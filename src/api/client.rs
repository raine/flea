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

use super::signing::{SigningContext, sign};

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
    pub const SERVICE_AD_ACTION: &str = "AD-ACTION";
    pub const SERVICE_AD_SUMMARIES: &str = "AD-SUMMARIES";
    pub const SERVICE_ADVIEW: &str = "ADVIEW-PROVIDER-RC";
    pub const SERVICE_BILLING_TRACKING: &str = "BILLING-TRACKING-SERVICE";
    pub const SERVICE_ORDER_PAYMENT: &str = "ORDER-PAYMENT-SERVER";
    pub const SERVICE_LOGIN: &str = "LOGIN-SERVER-AUTH";
    pub const SERVICE_SEARCH: &str = "SEARCH-QUEST";

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
    /// Number of attempts after the initial GET or HEAD request.
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

#[derive(Clone, Eq, PartialEq)]
pub struct MultipartPart {
    pub name: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    bytes: Vec<u8>,
}

impl MultipartPart {
    pub fn bytes(name: impl Into<String>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            name: name.into(),
            file_name: None,
            mime_type: None,
            bytes: bytes.into(),
        }
    }

    pub fn file_name(mut self, file_name: impl Into<String>) -> Self {
        self.file_name = Some(file_name.into());
        self
    }

    pub fn mime_type(mut self, mime_type: impl Into<String>) -> Self {
        self.mime_type = Some(mime_type.into());
        self
    }

    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

impl fmt::Debug for MultipartPart {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MultipartPart")
            .field("name", &self.name)
            .field("file_name", &self.file_name)
            .field("mime_type", &self.mime_type)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Default, Eq, PartialEq)]
pub enum RequestBody {
    #[default]
    Empty,
    Bytes(Vec<u8>),
    Multipart(Vec<MultipartPart>),
}

impl RequestBody {
    fn signing_bytes(&self) -> &[u8] {
        match self {
            Self::Bytes(bytes) => bytes,
            Self::Empty | Self::Multipart(_) => &[],
        }
    }
}

impl fmt::Debug for RequestBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Empty"),
            Self::Bytes(bytes) => formatter
                .debug_struct("Bytes")
                .field("byte_len", &bytes.len())
                .finish(),
            Self::Multipart(parts) => formatter.debug_tuple("Multipart").field(parts).finish(),
        }
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
}

#[derive(Clone)]
pub struct TransportRequest {
    pub method: Method,
    pub url: String,
    pub path_and_query: String,
    pub headers: HeaderMap,
    pub body: RequestBody,
    pub deadline: Duration,
    pub max_response_bytes: usize,
}

impl fmt::Debug for TransportRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportRequest")
            .field("method", &self.method)
            .field("request_target", &"[REDACTED]")
            .field("header_names", &header_names(&self.headers))
            .field("body", &self.body)
            .field("deadline", &self.deadline)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

#[derive(Clone)]
pub struct TransportResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportErrorKind {
    Timeout,
    Connection,
    Other,
}

#[derive(Clone, thiserror::Error)]
#[error("HTTP transport {kind}")]
pub struct TransportError {
    pub kind: TransportErrorKind,
}

impl fmt::Debug for TransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportError")
            .field("kind", &self.kind)
            .finish()
    }
}

impl fmt::Display for TransportErrorKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Timeout => "timed out",
            Self::Connection => "connection failed",
            Self::Other => "failed",
        })
    }
}

pub type TransportFuture<'a> =
    Pin<Box<dyn Future<Output = Result<TransportResponse, TransportError>> + Send + 'a>>;

pub trait Transport: Send + Sync {
    fn execute(&self, request: TransportRequest) -> TransportFuture<'_>;
}

#[derive(Clone)]
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl Default for ReqwestTransport {
    fn default() -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .expect("static HTTP client configuration is valid");
        Self { client }
    }
}

impl Transport for ReqwestTransport {
    fn execute(&self, request: TransportRequest) -> TransportFuture<'_> {
        Box::pin(async move {
            let TransportRequest {
                method,
                url,
                headers,
                body,
                deadline,
                max_response_bytes,
                ..
            } = request;
            let mut builder = self
                .client
                .request(method, url)
                .headers(headers)
                .timeout(deadline);
            builder = match body {
                RequestBody::Empty => builder,
                RequestBody::Bytes(bytes) => builder.body(bytes),
                RequestBody::Multipart(parts) => {
                    let mut form = reqwest::multipart::Form::new();
                    for part in parts {
                        let mut reqwest_part = reqwest::multipart::Part::bytes(part.bytes);
                        if let Some(file_name) = part.file_name {
                            reqwest_part = reqwest_part.file_name(file_name);
                        }
                        if let Some(mime_type) = part.mime_type {
                            reqwest_part =
                                reqwest_part
                                    .mime_str(&mime_type)
                                    .map_err(|_| TransportError {
                                        kind: TransportErrorKind::Other,
                                    })?;
                        }
                        form = form.part(part.name, reqwest_part);
                    }
                    builder.multipart(form)
                }
            };

            let mut response = builder.send().await.map_err(classify_reqwest_error)?;
            if response
                .content_length()
                .is_some_and(|len| len > max_response_bytes as u64)
            {
                return Err(TransportError {
                    kind: TransportErrorKind::Other,
                });
            }

            let status = response.status();
            let headers = response.headers().clone();
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(classify_reqwest_error)? {
                if bytes.len().saturating_add(chunk.len()) > max_response_bytes {
                    return Err(TransportError {
                        kind: TransportErrorKind::Other,
                    });
                }
                bytes.extend_from_slice(&chunk);
            }

            Ok(TransportResponse {
                status,
                headers,
                body: bytes,
            })
        })
    }
}

fn classify_reqwest_error(error: reqwest::Error) -> TransportError {
    let kind = if error.is_timeout() {
        TransportErrorKind::Timeout
    } else if error.is_connect() {
        TransportErrorKind::Connection
    } else {
        TransportErrorKind::Other
    };
    TransportError { kind }
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

#[derive(Clone)]
pub struct HttpResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl fmt::Debug for TransportResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TransportResponse")
            .field("status", &self.status)
            .field("header_names", &header_names(&self.headers))
            .field("body_len", &self.body.len())
            .finish()
    }
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
        let retryable_method = matches!(request.method, Method::GET | Method::HEAD);
        let max_attempts = if retryable_method {
            self.config.max_get_retries.min(MAX_GET_RETRIES) + 1
        } else {
            1
        };
        let mut attempt = 0;

        loop {
            let transport_request = self.prepare(&request)?;
            match self.transport.execute(transport_request).await {
                Ok(response) => {
                    if response.body.len() > self.config.max_response_bytes {
                        return Err(HttpError::ResponseTooLarge);
                    }
                    if attempt + 1 < max_attempts && is_transient_status(response.status) {
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
                Err(error) if attempt + 1 < max_attempts && is_transient_error(error.kind) => {
                    self.sleep_before_retry(attempt).await;
                    attempt += 1;
                }
                Err(error) => return Err(error.into()),
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
            body: request.body.signing_bytes(),
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
            path_and_query: request.path_and_query.clone(),
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

fn is_transient_status(status: StatusCode) -> bool {
    matches!(
        status,
        StatusCode::REQUEST_TIMEOUT
            | StatusCode::TOO_EARLY
            | StatusCode::TOO_MANY_REQUESTS
            | StatusCode::INTERNAL_SERVER_ERROR
            | StatusCode::BAD_GATEWAY
            | StatusCode::SERVICE_UNAVAILABLE
            | StatusCode::GATEWAY_TIMEOUT
    )
}

fn is_transient_error(kind: TransportErrorKind) -> bool {
    matches!(
        kind,
        TransportErrorKind::Timeout | TransportErrorKind::Connection
    )
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
