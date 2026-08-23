use crate::diagnostics;
use crate::domain::observation::Observation;
use crate::domain::observation::ObservationOperation;
use crate::domain::observation::ObservationSource;
use crate::marketplace::tori::client::HttpFailure;
use crate::marketplace::tori::client::RequestSpec;
use crate::marketplace::tori::client::ToriClient;
use crate::marketplace::tori::client::compatibility;
use crate::retry::FailureKind;
use crate::retry::OperationMethod;
use crate::retry::RetryClassification;
use crate::retry::RetryContext;
use crate::retry::classify;
use crate::transport::MultipartPart;
use reqwest::Method as ReqwestMethod;
use reqwest::header::CONTENT_TYPE;
use reqwest::header::HeaderValue;
use reqwest::header::LOCATION;
use serde_json::Value;
use serde_json::json;
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryPolicy {
    BoundedRead,
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Method {
    Get,
    Post,
    Patch,
    Put,
    Delete,
}

impl Method {
    pub(super) const fn is_mutation(&self) -> bool {
        !matches!(self, Self::Get)
    }

    pub(super) const fn retry_method(&self) -> OperationMethod {
        match self {
            Self::Get => OperationMethod::Get,
            Self::Post => OperationMethod::Post,
            Self::Patch => OperationMethod::Patch,
            Self::Put => OperationMethod::Put,
            Self::Delete => OperationMethod::Delete,
        }
    }
}

#[derive(Clone, PartialEq)]
pub enum RequestBody {
    Empty,
    Json(Value),
    Form(Vec<(String, String)>),
    Image {
        bytes: Vec<u8>,
        file_name: String,
        mime_type: String,
        width: u32,
        height: u32,
    },
}

impl fmt::Debug for RequestBody {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("Empty"),
            Self::Json(value) => {
                let mut redacted = value.clone();
                diagnostics::redact_diagnostic_value(&mut redacted);
                formatter.debug_tuple("Json").field(&redacted).finish()
            }
            Self::Form(values) => formatter
                .debug_tuple("Form")
                .field(&values.iter().map(|(key, _)| key).collect::<Vec<_>>())
                .finish(),
            Self::Image {
                bytes,
                mime_type,
                width,
                height,
                ..
            } => formatter
                .debug_struct("Image")
                .field("byte_len", &bytes.len())
                .field("mime_type", mime_type)
                .field("width", width)
                .field("height", height)
                .finish(),
        }
    }
}

#[derive(Clone, PartialEq)]
pub struct HttpRequest {
    pub method: Method,
    pub path: String,
    pub observation_source: ObservationSource,
    pub service: Option<&'static str>,
    pub if_match: Option<String>,
    pub retry: RetryPolicy,
    pub body: RequestBody,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("request_target", &"[REDACTED]")
            .field("observation_source", &self.observation_source)
            .field("has_if_match", &self.if_match.is_some())
            .field("retry", &self.retry)
            .field("body", &self.body)
            .finish()
    }
}

impl HttpRequest {
    pub(crate) fn read(source: ObservationSource, path: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            path: path.into(),
            observation_source: source,
            service: None,
            if_match: None,
            retry: RetryPolicy::BoundedRead,
            body: RequestBody::Empty,
        }
    }

    pub(super) fn mutation(
        source: ObservationSource,
        method: Method,
        path: impl Into<String>,
        body: RequestBody,
    ) -> Self {
        Self {
            method,
            path: path.into(),
            observation_source: source,
            service: None,
            if_match: None,
            retry: RetryPolicy::Never,
            body,
        }
    }

    pub(crate) fn with_service(mut self, service: &'static str) -> Self {
        self.service = Some(service);
        self
    }

    pub(crate) fn retry_context(&self) -> RetryContext {
        let method = self.method.retry_method();
        if self.method.is_mutation() {
            RetryContext::mutation(method)
        } else {
            RetryContext::read(method)
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub body: Value,
    pub body_is_unparseable: bool,
}

#[derive(Clone, PartialEq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub upstream_transient: bool,
    pub safe_to_retry: bool,
    pub observation: Option<Observation>,
    pub status: Option<u16>,
    pub details: Option<Box<Value>>,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            upstream_transient: false,
            safe_to_retry: false,
            observation: None,
            status: None,
            details: None,
        }
    }

    pub(crate) fn response(
        response: &HttpResponse,
        context: RetryContext,
        source: ObservationSource,
    ) -> Self {
        let message = response
            .body
            .get("message")
            .and_then(Value::as_str)
            .map(diagnostics::redact_text)
            .unwrap_or_else(|| "Tori rejected the request".to_owned());
        let mut upstream = response.body.clone();
        diagnostics::redact_diagnostic_value(&mut upstream);
        let classification = classify(FailureKind::HttpStatus(response.status), context);
        let (code, message) = if classification.upstream_transient
            && !classification.safe_to_retry
            && !context.method.is_read()
        {
            (
                "mutation.uncertain",
                "The upstream failure may be temporary, but the mutation outcome is unknown"
                    .to_owned(),
            )
        } else {
            ("upstream.request_failed", message)
        };
        let operation = if context.method.is_read() {
            ObservationOperation::Read
        } else {
            ObservationOperation::Mutation
        };
        let observation = if response.status == 404 && context.method.is_read() {
            Observation::confirmed_absent(source, Some(response.status))
        } else if classification.upstream_transient {
            Observation::temporarily_unavailable(source, Some(response.status), true)
        } else {
            Observation::unrecognized_response(source, Some(response.status))
        };
        let mut error = Self::new(code, message).with_observation(observation, operation);
        if response.status == 404 && context.method.is_read() {
            error.code = if source == ObservationSource::DraftDetail {
                "draft.not_found".to_owned()
            } else {
                "remote.not_found".to_owned()
            };
            error.message = "The remote resource was not found".to_owned();
        }
        error.status = Some(response.status);
        error.details = Some(Box::new(json!({
            "status": response.status,
            "content_type": response.content_type,
            "body_is_unparseable": response.body_is_unparseable,
            "upstream": upstream
        })));
        error
    }

    pub(super) fn retry_classification(mut self, classification: RetryClassification) -> Self {
        self.upstream_transient = classification.upstream_transient;
        self.safe_to_retry = classification.safe_to_retry;
        self
    }

    pub(crate) fn with_observation(
        mut self,
        observation: Observation,
        operation: ObservationOperation,
    ) -> Self {
        let classification = observation.retry_classification(operation);
        self.upstream_transient = classification.upstream_transient;
        self.safe_to_retry = classification.safe_to_retry;
        self.observation = Some(observation);
        self
    }
}

impl fmt::Debug for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut details = self.details.as_deref().cloned();
        if let Some(details) = &mut details {
            diagnostics::redact_diagnostic_value(details);
        }
        formatter
            .debug_struct("ApiError")
            .field("code", &self.code)
            .field(
                "message",
                &diagnostics::redact_diagnostic_text(&self.message),
            )
            .field("upstream_transient", &self.upstream_transient)
            .field("safe_to_retry", &self.safe_to_retry)
            .field("observation", &self.observation)
            .field("status", &self.status)
            .field("details", &details)
            .finish()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ApiError {}

#[allow(async_fn_in_trait)]
pub trait AdInputProtocol: Send + Sync {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, ApiError>;
}

impl<C: ToriClient> AdInputProtocol for C {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        let service = request
            .service
            .unwrap_or_else(|| service_for_path(&request.path));
        let observation_source = request.observation_source;
        let retry_context = request.retry_context();
        let method = match request.method {
            Method::Get => ReqwestMethod::GET,
            Method::Post => ReqwestMethod::POST,
            Method::Patch => ReqwestMethod::PATCH,
            Method::Put => ReqwestMethod::PUT,
            Method::Delete => ReqwestMethod::DELETE,
        };
        let mut spec = RequestSpec::new(method, request.path.clone(), service);
        if request.path.starts_with("/adinput/") {
            spec = spec.adinput();
        }
        spec = match request.body {
            RequestBody::Empty if request.method.is_mutation() => spec.empty_body(),
            RequestBody::Empty => spec,
            RequestBody::Json(value) => spec.body(
                serde_json::to_vec(&value).map_err(|error| {
                    ApiError::new("upstream.request_encoding_failed", error.to_string())
                })?,
                HeaderValue::from_static("application/json"),
            ),
            RequestBody::Form(values) => {
                let mut serializer = url::form_urlencoded::Serializer::new(String::new());
                for (key, value) in values {
                    serializer.append_pair(&key, &value);
                }
                spec.body(
                    serializer.finish().into_bytes(),
                    HeaderValue::from_static("application/x-www-form-urlencoded"),
                )
            }
            RequestBody::Image {
                bytes,
                file_name,
                mime_type,
                ..
            } => {
                spec.headers.insert(
                    "upload-draft-interop-version",
                    HeaderValue::from_static(compatibility::UPLOAD_DRAFT_INTEROP_VERSION),
                );
                spec.headers
                    .insert("upload-complete", HeaderValue::from_static("?1"));
                spec.multipart(vec![
                    MultipartPart::bytes("file", bytes)
                        .file_name(file_name)
                        .mime_type(mime_type),
                ])
            }
        };
        if let Some(etag) = request.if_match {
            spec = spec.if_match(HeaderValue::from_str(&etag).map_err(|_| {
                ApiError::new("upstream.invalid_etag", "Tori returned an invalid ETag")
            })?);
        }
        let response = ToriClient::execute(self, spec).await.map_err(|error| {
            let failure = HttpFailure::from(error);
            let classification = failure.retry_classification(retry_context);
            let (code, message) = if classification.upstream_transient
                && !classification.safe_to_retry
                && !retry_context.method.is_read()
            {
                (
                    "mutation.uncertain",
                    "The upstream failure may be temporary, but the mutation outcome is unknown"
                        .to_owned(),
                )
            } else {
                ("upstream.request_failed", failure.to_string())
            };
            let operation = if retry_context.method.is_read() {
                ObservationOperation::Read
            } else {
                ObservationOperation::Mutation
            };
            let observation = if classification.upstream_transient {
                Observation::temporarily_unavailable(observation_source, None, false)
            } else {
                Observation::unrecognized_response(observation_source, None)
            };
            ApiError::new(code, message).with_observation(observation, operation)
        })?;
        let (body, body_is_unparseable) = if response.body.is_empty() {
            (Value::Null, false)
        } else {
            match serde_json::from_slice(&response.body) {
                Ok(body) => (body, false),
                Err(_) => (Value::Null, true),
            }
        };
        Ok(HttpResponse {
            status: response.status.as_u16(),
            etag: response
                .etag()
                .and_then(|etag| etag.to_str().ok())
                .map(str::to_owned),
            content_type: response
                .headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(safe_content_type),
            location: response
                .headers
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            body,
            body_is_unparseable,
        })
    }
}

fn service_for_path(path: &str) -> &'static str {
    if path.starts_with("/adinput/") {
        compatibility::SERVICE_ADINPUT
    } else if path.starts_with("/ui/addelivery") || path.contains("/delivery") {
        compatibility::SERVICE_DELIVERY
    } else if path == "/categories/taxonomy" || path.starts_with("/items/") {
        compatibility::SERVICE_ITEM_CREATION
    } else if path.starts_with("/orders/") {
        compatibility::SERVICE_ORDER_PAYMENT
    } else if path.starts_with("/tracking/") {
        compatibility::SERVICE_BILLING_TRACKING
    } else if path.starts_with("/search?")
        || path.starts_with("/my/listings/")
        || path.starts_with("/listings/")
        || path.strip_prefix('/').is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        compatibility::SERVICE_AD_SUMMARIES
    } else {
        compatibility::SERVICE_ADINPUT
    }
}

fn safe_content_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or("unknown")
        .trim()
        .to_ascii_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::tori::client::{HttpError, RequestSpec};
    use crate::transport::{TransportError, TransportErrorKind};
    use std::future::Future;
    use std::pin::Pin;

    struct FailingClient {
        error: TransportErrorKind,
    }

    impl ToriClient for FailingClient {
        fn execute(
            &self,
            _request: RequestSpec,
        ) -> Pin<
            Box<
                dyn Future<
                        Output = Result<crate::marketplace::tori::client::HttpResponse, HttpError>,
                    > + Send
                    + '_,
            >,
        > {
            let error = self.error;
            Box::pin(async move { Err(HttpError::Transport(TransportError::request(error))) })
        }
    }

    fn response(status: u16) -> HttpResponse {
        HttpResponse {
            status,
            etag: None,
            content_type: Some("application/json".to_owned()),
            location: None,
            body: json!({ "message": "fixture" }),
            body_is_unparseable: false,
        }
    }

    #[test]
    fn response_errors_preserve_each_typed_source_and_read_retry_behavior() {
        let sources = [
            ObservationSource::AuthenticatedListingCollection,
            ObservationSource::CategoryTaxonomy,
            ObservationSource::DeliveryComposer,
            ObservationSource::DraftCategoryPredictions,
            ObservationSource::DraftCreation,
            ObservationSource::DraftDetail,
            ObservationSource::DraftService,
            ObservationSource::ListingCopyEligibility,
            ObservationSource::ListingDetail,
            ObservationSource::PublicationConfirmation,
            ObservationSource::PublicationProductContext,
        ];

        for source in sources {
            let error = ApiError::response(
                &response(503),
                RetryContext::read(OperationMethod::Get),
                source,
            );

            assert_eq!(error.observation.unwrap().source, source.as_str());
            assert!(error.upstream_transient);
            assert!(error.safe_to_retry);
        }
    }

    #[test]
    fn typed_draft_detail_preserves_not_found_classification() {
        let error = ApiError::response(
            &response(404),
            RetryContext::read(OperationMethod::Get),
            ObservationSource::DraftDetail,
        );

        assert_eq!(error.code, "draft.not_found");
        assert_eq!(
            error.observation.unwrap().source,
            ObservationSource::DraftDetail.as_str()
        );
        assert!(!error.upstream_transient);
        assert!(!error.safe_to_retry);
    }

    #[tokio::test]
    async fn transport_errors_use_request_metadata_instead_of_request_paths() {
        let transport = FailingClient {
            error: TransportErrorKind::Connection,
        };
        let read = AdInputProtocol::execute(
            &transport,
            HttpRequest::read(
                ObservationSource::CategoryTaxonomy,
                "/path-with-no-source-semantics",
            ),
        )
        .await
        .unwrap_err();
        let mutation = AdInputProtocol::execute(
            &transport,
            HttpRequest::mutation(
                ObservationSource::PublicationConfirmation,
                Method::Post,
                "/another-unrelated-path",
                RequestBody::Empty,
            ),
        )
        .await
        .unwrap_err();

        assert_eq!(
            read.observation.unwrap().source,
            ObservationSource::CategoryTaxonomy.as_str()
        );
        assert!(read.upstream_transient);
        assert!(read.safe_to_retry);
        assert_eq!(mutation.code, "mutation.uncertain");
        assert_eq!(
            mutation.observation.unwrap().source,
            ObservationSource::PublicationConfirmation.as_str()
        );
        assert!(mutation.upstream_transient);
        assert!(!mutation.safe_to_retry);
    }
}
