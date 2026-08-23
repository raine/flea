use std::{
    fmt,
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use reqwest::{
    Method, StatusCode,
    header::{
        ACCEPT, ACCEPT_LANGUAGE, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, COOKIE, ETAG, HOST,
        HeaderMap, HeaderName, HeaderValue, IF_MATCH, SET_COOKIE, USER_AGENT,
    },
};
use url::Position;

use crate::{
    marketplace::tori::signing::{SigningContext, sign},
    retry::{FailureKind, OperationMethod, RetryContext, classify},
    transport::{
        MultipartPart, RequestBody, ReqwestTransport, Transport, TransportError,
        TransportErrorKind, TransportRequest,
    },
};

pub mod compatibility {
    pub const GATEWAY_BASE_URL: &str = "https://apps-gw-poc.svc.tori.fi";
    pub const ADINPUT_BASE_URL: &str = "https://apps-adinput.svc.tori.fi";

    pub const APP_VERSION: &str = "26.4.0";
    pub const APP_BUILD_NUMBER: &str = "26357";
    pub const OS_NAME: &str = "Android";
    pub const OS_VERSION: &str = "14";
    pub const DEVICE: &str = "Pixel 6";
    pub const APP_BRAND: &str = "Tori";
    pub const DEVICE_INFO: &str = "Android, mobile";
    pub const ADINPUT_VERSION: &str = "boatmotor";
    pub const USER_AGENT: &str = "ToriApp_And/26.4.0 (Linux; U; Android 14; en_us; Pixel 6 Build/UP1A.231005.007) ToriNativeApp(UA spoofed for tracking) ToriApp_And";

    pub const SERVICE_ADINPUT: &str = "APPS-ADINPUT";
    pub const SERVICE_ITEM_CREATION: &str = "RC-ITEM-CREATION-FLOW-API";
    pub const SERVICE_DELIVERY: &str = "TJT-API";
    pub const SERVICE_AD_ACTION: &str = "ITEM-ACTION";
    pub const SERVICE_AD_SUMMARIES: &str = "AD-SUMMARIES";
    pub const SERVICE_ADVIEW: &str = "ADVIEW-PROVIDER-RC";
    pub const SERVICE_BILLING_TRACKING: &str = "BILLING-TRACKING-SERVICE";
    pub const SERVICE_ORDER_PAYMENT: &str = "ORDER-PAYMENT-SERVER";
    pub const SERVICE_SEARCH: &str = "SEARCH-QUEST";
    pub const SERVICE_FAVORITES: &str = "FAVORITE-MANAGEMENT";
    pub const SERVICE_SAVED_SEARCHES: &str = "SEARCH-SAVEDSEARCH";

    pub const UPLOAD_DRAFT_INTEROP_VERSION: &str = "6";
}

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_GET_RETRIES: usize = 2;
const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_millis(100);
const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(2);
const MAX_GET_RETRIES: usize = 8;
const MAX_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);

#[derive(Debug, Clone)]
pub struct ClientConfig {
    pub gateway_base_url: String,
    pub adinput_base_url: String,
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
    /// Number of attempts after the initial request when replay is classified as safe.
    pub max_get_retries: usize,
    pub retry_base_delay: Duration,
    pub retry_max_delay: Duration,
    pub include_device_identity: bool,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            gateway_base_url: compatibility::GATEWAY_BASE_URL.to_owned(),
            adinput_base_url: compatibility::ADINPUT_BASE_URL.to_owned(),
            request_timeout: DEFAULT_TIMEOUT,
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            max_get_retries: DEFAULT_GET_RETRIES,
            retry_base_delay: DEFAULT_RETRY_BASE_DELAY,
            retry_max_delay: DEFAULT_RETRY_MAX_DELAY,
            include_device_identity: true,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeviceIdentity {
    pub installation_id: String,
    pub ab_test_device_id: String,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ApiHost {
    #[default]
    Gateway,
    Adinput,
}

fn signing_bytes(body: &RequestBody) -> &[u8] {
    match body {
        RequestBody::Bytes(bytes) => bytes,
        RequestBody::Empty | RequestBody::Multipart(_) => &[],
    }
}

#[derive(Clone)]
pub struct RequestSpec {
    pub method: Method,
    pub host: ApiHost,
    /// The path and raw query exactly as they must be sent and signed.
    pub path_and_query: String,
    pub service: String,
    pub body: RequestBody,
    pub content_type: Option<HeaderValue>,
    pub if_match: Option<HeaderValue>,
    pub content_length_zero: bool,
    pub headers: HeaderMap,
    idempotency_contract: bool,
    idempotency_key: bool,
}

impl fmt::Debug for RequestSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestSpec")
            .field("method", &self.method)
            .field("host", &self.host)
            .field("request_target", &"[REDACTED]")
            .field("service", &self.service)
            .field("body", &self.body)
            .field("content_type", &self.content_type)
            .field("has_if_match", &self.if_match.is_some())
            .field("content_length_zero", &self.content_length_zero)
            .field("idempotency_contract", &self.idempotency_contract)
            .field("has_idempotency_key", &self.idempotency_key)
            .field("header_names", &header_names(&self.headers))
            .finish()
    }
}

impl RequestSpec {
    pub fn new(
        method: Method,
        path_and_query: impl Into<String>,
        service: impl Into<String>,
    ) -> Self {
        Self {
            method,
            host: ApiHost::Gateway,
            path_and_query: path_and_query.into(),
            service: service.into(),
            body: RequestBody::Empty,
            content_type: None,
            if_match: None,
            content_length_zero: false,
            headers: HeaderMap::new(),
            idempotency_contract: false,
            idempotency_key: false,
        }
    }

    pub fn adinput(mut self) -> Self {
        self.host = ApiHost::Adinput;
        self
    }

    pub fn body(mut self, body: impl Into<Vec<u8>>, content_type: HeaderValue) -> Self {
        self.body = RequestBody::Bytes(body.into());
        self.content_type = Some(content_type);
        self
    }

    pub fn empty_body(mut self) -> Self {
        self.body = RequestBody::Bytes(Vec::new());
        self.content_length_zero = true;
        self
    }

    pub fn multipart(mut self, parts: Vec<MultipartPart>) -> Self {
        self.body = RequestBody::Multipart(parts);
        self
    }

    pub fn if_match(mut self, etag: HeaderValue) -> Self {
        self.if_match = Some(etag);
        self
    }

    pub fn with_source_backed_idempotency_contract(mut self) -> Self {
        self.idempotency_contract = true;
        self
    }

    pub fn with_source_backed_idempotency_key(
        mut self,
        header_name: HeaderName,
        header_value: HeaderValue,
    ) -> Self {
        self.headers.insert(header_name, header_value);
        self.idempotency_key = true;
        self
    }

    fn retry_context(&self) -> RetryContext {
        let method = OperationMethod::from_reqwest(&self.method);
        let mut context = if matches!(method, OperationMethod::Get | OperationMethod::Head) {
            RetryContext::read(method)
        } else {
            RetryContext::mutation(method)
        };
        if self.idempotency_contract {
            context = context.with_idempotency_contract();
        }
        if self.idempotency_key {
            context = context.with_idempotency_key();
        }
        context
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("invalid HTTP request")]
    InvalidRequest,
    #[error("HTTP response exceeded the configured size bound")]
    ResponseTooLarge,
    #[error(transparent)]
    Transport(#[from] TransportError),
}

#[derive(Clone, Copy, Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum HttpAdapterFailure {
    #[error("invalid HTTP request")]
    InvalidRequest,
    #[error("HTTP response exceeded the configured size bound")]
    ResponseTooLarge,
    #[error("HTTP transport failed")]
    OtherTransport,
}

#[derive(Clone, Debug, thiserror::Error, Eq, PartialEq)]
pub(crate) enum HttpFailure {
    #[error(transparent)]
    Transport(TransportError),
    #[error(transparent)]
    Local(HttpAdapterFailure),
}

impl HttpFailure {
    pub(crate) const fn failure_kind(&self) -> FailureKind {
        match self {
            Self::Transport(_) => FailureKind::Transport,
            Self::Local(_) => FailureKind::Local,
        }
    }

    pub(crate) fn retry_classification(
        &self,
        context: RetryContext,
    ) -> crate::retry::RetryClassification {
        classify(self.failure_kind(), context)
    }
}

impl From<HttpError> for HttpFailure {
    fn from(error: HttpError) -> Self {
        match error {
            HttpError::Transport(transport)
                if matches!(
                    transport.kind,
                    TransportErrorKind::Timeout | TransportErrorKind::Connection
                ) =>
            {
                Self::Transport(transport)
            }
            HttpError::InvalidRequest => Self::Local(HttpAdapterFailure::InvalidRequest),
            HttpError::ResponseTooLarge => Self::Local(HttpAdapterFailure::ResponseTooLarge),
            HttpError::Transport(_) => Self::Local(HttpAdapterFailure::OtherTransport),
        }
    }
}

pub(crate) fn map_http_error<T: From<HttpFailure>>(error: HttpError) -> T {
    HttpFailure::from(error).into()
}

#[derive(Clone)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("header_names", &header_names(&self.headers))
            .field("body_len", &self.body.len())
            .finish()
    }
}

impl HttpResponse {
    pub fn etag(&self) -> Option<&HeaderValue> {
        self.headers.get(ETAG)
    }
}

pub trait ToriClient: Send + Sync {
    fn execute(
        &self,
        request: RequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>>;
}

impl<T: ToriClient + ?Sized> ToriClient for Arc<T> {
    fn execute(
        &self,
        request: RequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>> {
        self.as_ref().execute(request)
    }
}

pub struct HttpClient<T: Transport = ReqwestTransport> {
    config: ClientConfig,
    identity: DeviceIdentity,
    bearer: Option<String>,
    transport: Arc<T>,
}

impl HttpClient<ReqwestTransport> {
    pub fn new(config: ClientConfig, identity: DeviceIdentity, bearer: Option<String>) -> Self {
        Self::with_transport(config, identity, bearer, ReqwestTransport::default())
    }
}

impl<T: Transport> HttpClient<T> {
    pub fn with_transport(
        config: ClientConfig,
        identity: DeviceIdentity,
        bearer: Option<String>,
        transport: T,
    ) -> Self {
        Self {
            config,
            identity,
            bearer,
            transport: Arc::new(transport),
        }
    }

    pub async fn send(&self, request: RequestSpec) -> Result<HttpResponse, HttpError> {
        validate_path(&request.path_and_query)?;
        let retry_context = request.retry_context();
        let max_attempts = self.config.max_get_retries.min(MAX_GET_RETRIES) + 1;
        let mut attempt = 0;

        loop {
            let transport_request = self.prepare(&request)?;
            match self.transport.execute(transport_request).await {
                Ok(response) => {
                    if response.body.len() > self.config.max_response_bytes {
                        return Err(HttpError::ResponseTooLarge);
                    }
                    let classification = classify(
                        FailureKind::HttpStatus(response.status.as_u16()),
                        retry_context,
                    );
                    if attempt + 1 < max_attempts
                        && classification.upstream_transient
                        && classification.safe_to_retry
                    {
                        self.sleep_before_retry(attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Ok(HttpResponse {
                        status: response.status,
                        headers: response.headers,
                        body: response.body,
                    });
                }
                Err(error) => {
                    let classification = retry_transport_classification(error.kind, retry_context);
                    if attempt + 1 < max_attempts
                        && classification.upstream_transient
                        && classification.safe_to_retry
                    {
                        self.sleep_before_retry(attempt).await;
                        attempt += 1;
                        continue;
                    }
                    return Err(error.into());
                }
            }
        }
    }

    fn prepare(&self, request: &RequestSpec) -> Result<TransportRequest, HttpError> {
        let base_url = match request.host {
            ApiHost::Gateway => &self.config.gateway_base_url,
            ApiHost::Adinput => &self.config.adinput_base_url,
        };
        validate_custom_headers(&request.headers)?;
        let base_url = base_url.trim_end_matches('/');
        let url = format!("{base_url}{}", request.path_and_query);
        validate_canonical_url(base_url, &url, &request.path_and_query)?;

        let signature = sign(SigningContext {
            method: request.method.as_str(),
            path_and_query: &request.path_and_query,
            service: &request.service,
            body: signing_bytes(&request.body),
        });
        let mut headers = compatibility_headers(
            self.config
                .include_device_identity
                .then_some(&self.identity),
        )?;
        headers.extend(request.headers.clone());
        if let Some(bearer) = &self.bearer {
            let value = HeaderValue::from_str(&format!("Bearer {bearer}"))
                .map_err(|_| HttpError::InvalidRequest)?;
            headers.insert(AUTHORIZATION, value);
        }
        if !request.service.is_empty() {
            insert_header(&mut headers, "finn-gw-service", &request.service)?;
        }
        insert_header(&mut headers, "finn-gw-key", signature.as_header_value())?;
        if let Some(content_type) = &request.content_type {
            headers.insert(CONTENT_TYPE, content_type.clone());
        }
        if let Some(etag) = &request.if_match {
            headers.insert(IF_MATCH, etag.clone());
        }
        if request.content_length_zero {
            headers.insert(CONTENT_LENGTH, HeaderValue::from_static("0"));
        }

        Ok(TransportRequest {
            method: request.method.clone(),
            url,
            headers,
            body: request.body.clone(),
            deadline: self.config.request_timeout.min(MAX_REQUEST_TIMEOUT),
            max_response_bytes: self.config.max_response_bytes,
        })
    }

    async fn sleep_before_retry(&self, attempt: usize) {
        let delay = retry_delay(&self.config, attempt);
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
    }
}

impl<T: Transport + 'static> ToriClient for HttpClient<T> {
    fn execute(
        &self,
        request: RequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>> {
        Box::pin(self.send(request))
    }
}

fn header_names(headers: &HeaderMap) -> Vec<&str> {
    headers.keys().map(HeaderName::as_str).collect()
}

fn compatibility_headers(identity: Option<&DeviceIdentity>) -> Result<HeaderMap, HttpError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        USER_AGENT,
        HeaderValue::from_static(compatibility::USER_AGENT),
    );
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/json; charset=UTF-8"),
    );
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("en-US,en;q=0.9"));
    insert_header(&mut headers, "finn-device-info", compatibility::DEVICE_INFO)?;
    if let Some(identity) = identity {
        insert_header(
            &mut headers,
            "finn-app-installation-id",
            &identity.installation_id,
        )?;
        insert_header(
            &mut headers,
            "ab-test-device-id",
            &identity.ab_test_device_id,
        )?;
    }
    insert_header(&mut headers, "x-nmp-os-name", compatibility::OS_NAME)?;
    insert_header(&mut headers, "x-nmp-os-version", compatibility::OS_VERSION)?;
    insert_header(
        &mut headers,
        "x-nmp-app-version-name",
        compatibility::APP_VERSION,
    )?;
    insert_header(
        &mut headers,
        "x-nmp-app-build-number",
        compatibility::APP_BUILD_NUMBER,
    )?;
    insert_header(&mut headers, "buildnumber", compatibility::APP_BUILD_NUMBER)?;
    insert_header(&mut headers, "x-nmp-app-brand", compatibility::APP_BRAND)?;
    insert_header(&mut headers, "x-nmp-device", compatibility::DEVICE)?;
    insert_header(
        &mut headers,
        "x-finn-apps-adinput-version-name",
        compatibility::ADINPUT_VERSION,
    )?;
    for name in [
        "cmp-analytics",
        "cmp-personalisation",
        "cmp-marketing",
        "cmp-advertising",
    ] {
        insert_header(&mut headers, name, "1")?;
    }
    Ok(headers)
}

fn insert_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &str,
) -> Result<(), HttpError> {
    let value = HeaderValue::from_str(value).map_err(|_| HttpError::InvalidRequest)?;
    headers.insert(HeaderName::from_static(name), value);
    Ok(())
}

fn validate_path(path_and_query: &str) -> Result<(), HttpError> {
    if !path_and_query.starts_with('/')
        || path_and_query.starts_with("//")
        || path_and_query.contains(['#', '\\'])
        || path_and_query.chars().any(char::is_control)
    {
        return Err(HttpError::InvalidRequest);
    }
    Ok(())
}

fn validate_canonical_url(
    base_url: &str,
    url: &str,
    path_and_query: &str,
) -> Result<(), HttpError> {
    let base = reqwest::Url::parse(base_url).map_err(|_| HttpError::InvalidRequest)?;
    if !matches!(base.scheme(), "https" | "http")
        || base.cannot_be_a_base()
        || base.username() != ""
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
        || base.path() != "/"
    {
        return Err(HttpError::InvalidRequest);
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| HttpError::InvalidRequest)?;
    if parsed.scheme() != base.scheme()
        || parsed.host_str() != base.host_str()
        || parsed.port_or_known_default() != base.port_or_known_default()
        || &parsed[Position::BeforePath..] != path_and_query
    {
        return Err(HttpError::InvalidRequest);
    }
    Ok(())
}

fn validate_custom_headers(headers: &HeaderMap) -> Result<(), HttpError> {
    let reserved = [
        AUTHORIZATION,
        CONTENT_LENGTH,
        CONTENT_TYPE,
        COOKIE,
        HOST,
        IF_MATCH,
        SET_COOKIE,
        HeaderName::from_static("finn-gw-key"),
        HeaderName::from_static("finn-gw-service"),
    ];
    if reserved.iter().any(|name| headers.contains_key(name)) {
        return Err(HttpError::InvalidRequest);
    }
    Ok(())
}

fn retry_transport_classification(
    kind: TransportErrorKind,
    context: RetryContext,
) -> crate::retry::RetryClassification {
    HttpFailure::from(HttpError::Transport(TransportError::request(kind)))
        .retry_classification(context)
}

fn retry_delay(config: &ClientConfig, attempt: usize) -> Duration {
    if config.retry_base_delay.is_zero() {
        return Duration::ZERO;
    }
    let exponent = u32::try_from(attempt.min(31)).expect("bounded attempt");
    let ceiling = config
        .retry_base_delay
        .saturating_mul(2_u32.saturating_pow(exponent))
        .min(config.retry_max_delay)
        .min(MAX_RETRY_DELAY);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos() as u64;
    let half = ceiling / 2;
    half.saturating_add(Duration::from_nanos(nanos % (half.as_nanos() as u64 + 1)))
}

#[cfg(test)]
mod tests {
    use super::{HttpAdapterFailure, HttpError, HttpFailure};
    use crate::{
        retry::{OperationMethod, RetryContext},
        transport::{TransportError, TransportErrorKind},
    };

    #[test]
    fn http_failures_carry_transport_and_local_policy_in_types() {
        for kind in [TransportErrorKind::Timeout, TransportErrorKind::Connection] {
            let failure = HttpFailure::from(HttpError::Transport(TransportError::request(kind)));
            assert!(matches!(failure, HttpFailure::Transport(_)));
            let retry = failure.retry_classification(RetryContext::read(OperationMethod::Get));
            assert!(retry.upstream_transient);
            assert!(retry.safe_to_retry);
        }

        for (error, expected) in [
            (
                HttpError::InvalidRequest,
                HttpAdapterFailure::InvalidRequest,
            ),
            (
                HttpError::ResponseTooLarge,
                HttpAdapterFailure::ResponseTooLarge,
            ),
            (
                HttpError::Transport(TransportError::request(TransportErrorKind::Other)),
                HttpAdapterFailure::OtherTransport,
            ),
        ] {
            let failure = HttpFailure::from(error);
            assert_eq!(failure, HttpFailure::Local(expected));
            let retry = failure.retry_classification(RetryContext::read(OperationMethod::Get));
            assert!(!retry.upstream_transient);
            assert!(retry.safe_to_retry);
        }
    }
}
