use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fmt,
    fs::File,
    io::{Cursor, Read},
    path::Path,
    process::Command,
    time::Duration,
};

use reqwest::{
    Method as ReqwestMethod,
    header::{CONTENT_TYPE, HeaderValue, LOCATION},
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    api::client::{
        HttpError, MultipartPart, RequestSpec, ToriClient, TransportErrorKind, compatibility,
    },
    diagnostics,
    domain::field::Field,
    retry::{FailureKind, OperationMethod, RetryClassification, RetryContext, classify},
};

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
    const fn is_mutation(&self) -> bool {
        !matches!(self, Self::Get)
    }

    const fn retry_method(&self) -> OperationMethod {
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
                diagnostics::redact_value(&mut redacted);
                formatter.debug_tuple("Json").field(&redacted).finish()
            }
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
            .field("has_if_match", &self.if_match.is_some())
            .field("retry", &self.retry)
            .field("body", &self.body)
            .finish()
    }
}

impl HttpRequest {
    fn read(path: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            path: path.into(),
            if_match: None,
            retry: RetryPolicy::BoundedRead,
            body: RequestBody::Empty,
        }
    }

    fn mutation(method: Method, path: impl Into<String>, body: RequestBody) -> Self {
        Self {
            method,
            path: path.into(),
            if_match: None,
            retry: RetryPolicy::Never,
            body,
        }
    }

    fn retry_context(&self) -> RetryContext {
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
            status: None,
            details: None,
        }
    }

    fn response(response: &HttpResponse, context: RetryContext) -> Self {
        let message = response
            .body
            .get("message")
            .and_then(Value::as_str)
            .map(diagnostics::redact_text)
            .unwrap_or_else(|| "Tori rejected the request".to_owned());
        let mut upstream = response.body.clone();
        diagnostics::redact_value(&mut upstream);
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
        let mut error = Self::new(code, message).retry_classification(classification);
        error.status = Some(response.status);
        error.details = Some(Box::new(json!({
            "status": response.status,
            "content_type": response.content_type,
            "upstream": upstream
        })));
        error
    }

    fn retry_classification(mut self, classification: RetryClassification) -> Self {
        self.upstream_transient = classification.upstream_transient;
        self.safe_to_retry = classification.safe_to_retry;
        self
    }
}

impl fmt::Debug for ApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut details = self.details.as_deref().cloned();
        if let Some(details) = &mut details {
            diagnostics::redact_value(details);
        }
        formatter
            .debug_struct("ApiError")
            .field("code", &self.code)
            .field("message", &diagnostics::redact_text(&self.message))
            .field("upstream_transient", &self.upstream_transient)
            .field("safe_to_retry", &self.safe_to_retry)
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
pub trait HttpTransport: Send + Sync {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, ApiError>;
}

pub struct ClientTransport<C> {
    client: C,
}

impl<C> ClientTransport<C> {
    pub const fn new(client: C) -> Self {
        Self { client }
    }
}

impl<C: ToriClient> HttpTransport for ClientTransport<C> {
    async fn execute(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        let service = service_for_path(&request.path);
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
            RequestBody::Empty if spec.method == ReqwestMethod::POST => spec.empty_body(),
            RequestBody::Empty => spec,
            RequestBody::Json(value) => spec.body(
                serde_json::to_vec(&value).map_err(|error| {
                    ApiError::new("upstream.request_encoding_failed", error.to_string())
                })?,
                HeaderValue::from_static("application/json"),
            ),
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
        let response = self.client.execute(spec).await.map_err(|error| {
            let failure = match &error {
                HttpError::Transport(transport)
                    if matches!(
                        transport.kind,
                        TransportErrorKind::Timeout | TransportErrorKind::Connection
                    ) =>
                {
                    FailureKind::Transport
                }
                HttpError::InvalidRequest
                | HttpError::ResponseTooLarge
                | HttpError::Transport(_) => FailureKind::Local,
            };
            let classification = classify(failure, retry_context);
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
                ("upstream.request_failed", error.to_string())
            };
            ApiError::new(code, message).retry_classification(classification)
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
    if path.ends_with("/upload") || path.ends_with("/update") {
        ""
    } else if path.starts_with("/ui/addelivery") || path.contains("/delivery") {
        compatibility::SERVICE_DELIVERY
    } else if path.contains("/products") || path.contains("/publish") {
        compatibility::SERVICE_ORDER_PAYMENT
    } else if path.contains("/tracking/") {
        compatibility::SERVICE_BILLING_TRACKING
    } else if path.starts_with("/listings/") {
        compatibility::SERVICE_AD_SUMMARIES
    } else if path.ends_with("/item") || path.starts_with("/items/") {
        compatibility::SERVICE_ITEM_CREATION
    } else {
        compatibility::SERVICE_ADINPUT
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ImageState {
    Processing,
    Ready,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftImage {
    pub image_id: String,
    pub position: usize,
    pub state: ImageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CategoryPrediction {
    pub category: String,
    pub confidence: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct FieldOption {
    pub field: String,
    pub value: String,
    pub label: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DeliveryOption {
    pub value: String,
    pub label: String,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_size: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftDelivery {
    pub source: String,
    pub available: bool,
    #[serde(default)]
    pub options: Vec<DeliveryOption>,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, PartialEq)]
pub struct DeliveryComposer {
    pub state: DraftDelivery,
    source: Value,
}

impl fmt::Debug for DeliveryComposer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeliveryComposer")
            .field("state", &self.state)
            .field("source", &"[REDACTED]")
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftState {
    pub draft_id: String,
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub values: Map<String, Value>,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub options: Vec<FieldOption>,
    #[serde(default)]
    pub required_fields: Vec<String>,
    #[serde(default)]
    pub images: Vec<DraftImage>,
    #[serde(default)]
    pub cleared_fields: Vec<String>,
    #[serde(default)]
    pub predictions: Vec<CategoryPrediction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DraftDelivery>,
}

impl DraftState {
    fn category_is_unset(&self) -> bool {
        self.values.get("category").is_none_or(Value::is_null)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UploadedImage {
    pub image_id: String,
    pub state: ImageState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default)]
    pub width: u32,
    #[serde(default)]
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ListingDraftSeed {
    pub listing_id: String,
    pub values: Map<String, Value>,
    #[serde(default)]
    pub images: Vec<SourceImage>,
}

#[derive(Clone, Serialize, Deserialize, PartialEq)]
pub struct SourceImage {
    pub file_name: String,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for SourceImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SourceImage")
            .field("file_name", &self.file_name)
            .field("byte_len", &self.bytes.len())
            .finish()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProductContext {
    pub revision: String,
    pub context: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Publication {
    pub listing_id: String,
    pub revision: String,
    pub state: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Confirmation {
    pub order_id: String,
    #[serde(default)]
    pub details: Value,
}

#[allow(async_fn_in_trait)]
pub trait AdInputApi: Send + Sync {
    async fn create_draft(&self) -> Result<DraftState, ApiError>;
    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError>;
    async fn update_item(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<DraftState, ApiError>;
    async fn update_sale_price(
        &self,
        draft_id: &str,
        etag: &str,
        price: &Value,
    ) -> Result<String, ApiError>;
    async fn submit_adinput(
        &self,
        draft_id: &str,
        etag: &str,
        state: &DraftState,
    ) -> Result<DraftState, ApiError>;
    async fn delete_draft(&self, draft_id: &str) -> Result<(), ApiError>;
    async fn upload_image(
        &self,
        draft_id: &str,
        file_name: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<UploadedImage, ApiError>;
    async fn set_images(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
        images: &[UploadedImage],
    ) -> Result<DraftState, ApiError>;
    async fn category_predictions(
        &self,
        draft_id: &str,
    ) -> Result<Vec<CategoryPrediction>, ApiError>;
    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError>;
    async fn delivery_composer(&self, draft_id: &str) -> Result<DeliveryComposer, ApiError>;
    async fn apply_delivery(
        &self,
        draft_id: &str,
        composer: &DeliveryComposer,
        delivery: &str,
    ) -> Result<(), ApiError>;
    async fn product_context(
        &self,
        draft_id: &str,
        revision: &str,
    ) -> Result<ProductContext, ApiError>;
    async fn publish_basic(
        &self,
        draft_id: &str,
        context: &ProductContext,
    ) -> Result<Publication, ApiError>;
    async fn confirmation(&self, listing_id: &str) -> Result<Confirmation, ApiError>;
    async fn track_confirmation(&self, confirmation: &Confirmation) -> Result<(), ApiError>;
    async fn observed_listing(&self, listing_id: &str) -> Result<Value, ApiError>;
}

pub struct HttpAdInputApi<T> {
    transport: T,
}

impl<T> HttpAdInputApi<T> {
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T: HttpTransport> HttpAdInputApi<T> {
    async fn json(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        debug_assert!(!request.method.is_mutation() || request.retry == RetryPolicy::Never);
        let retry_context = request.retry_context();
        let response = self.transport.execute(request).await?;
        if (200..300).contains(&response.status) {
            Ok(response)
        } else {
            Err(ApiError::response(&response, retry_context))
        }
    }

    async fn draft_request(&self, request: HttpRequest) -> Result<DraftState, ApiError> {
        let is_mutation = request.method.is_mutation();
        let retry_context = request.retry_context();
        let response = self.json(request).await?;
        if response.body_is_unparseable {
            let mut error = unexpected_representation("receive_draft_state", &response)
                .retry_classification(classify(FailureKind::MalformedSuccess, retry_context));
            if is_mutation {
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "The draft mutation may have succeeded, but its resulting state is unknown"
                        .to_owned();
            }
            return Err(error);
        }
        normalize_draft_state(response.body, response.etag.as_deref()).map_err(|mut error| {
            let classification = classify(FailureKind::MalformedSuccess, retry_context);
            error.upstream_transient = classification.upstream_transient;
            error.safe_to_retry = classification.safe_to_retry;
            if is_mutation {
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "The draft mutation may have succeeded, but its resulting state is unknown"
                        .to_owned();
                error.status = Some(response.status);
            }
            error
        })
    }

    async fn observe_created_draft(
        &self,
        draft_id: &str,
        completed_steps: &[&str],
    ) -> Result<DraftState, ApiError> {
        self.get_draft(draft_id).await.map_err(|mut error| {
            error.details = Some(Box::new(json!({
                "stage": "observe_created_draft",
                "draft_id": draft_id,
                "completed_steps": completed_steps,
                "recovery_guidance": format!(
                    "Inspect the draft with `flea draft show {draft_id}`; do not repeat creation"
                )
            })));
            error
        })
    }
}

#[allow(async_fn_in_trait)]
impl<T: HttpTransport> AdInputApi for HttpAdInputApi<T> {
    async fn create_draft(&self) -> Result<DraftState, ApiError> {
        let request = HttpRequest::mutation(
            Method::Post,
            "/adinput/ad/withModel/recommerce",
            RequestBody::Empty,
        );
        let retry_context = request.retry_context();
        let response = self.transport.execute(request).await?;
        if response.status == 303 {
            let draft_id =
                draft_id_from_location(response.location.as_deref()).ok_or_else(|| {
                    uncertain_creation(&response, "redirect response did not identify a draft")
                })?;
            return self
                .observe_created_draft(&draft_id, &["create_draft", "establish_identity"])
                .await;
        }
        if !(200..300).contains(&response.status) {
            return Err(ApiError::response(&response, retry_context));
        }
        if response.body_is_unparseable {
            return Err(uncertain_creation(
                &response,
                "successful response was not valid JSON",
            ));
        }

        let body_id = draft_id_from_body(&response.body);
        let location_id = draft_id_from_location(response.location.as_deref());
        if body_id.is_some() && location_id.is_some() && body_id != location_id {
            return Err(uncertain_creation(
                &response,
                "response body and Location identified different drafts",
            ));
        }
        let draft_id = body_id.or(location_id).ok_or_else(|| {
            uncertain_creation(&response, "successful response did not identify a draft")
        })?;

        if draft_id_from_body(&response.body).is_some() {
            return normalize_draft_state(response.body, response.etag.as_deref());
        }
        self.observe_created_draft(&draft_id, &["create_draft", "establish_identity"])
            .await
    }

    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.draft_request(HttpRequest::read(format!(
            "/adinput/ad/withModel/{draft_id}"
        )))
        .await
    }

    async fn update_item(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let values = composer_values(values)?;
        let mut request = HttpRequest::mutation(
            Method::Put,
            format!("/adinput/ad/recommerce/{draft_id}/update"),
            RequestBody::Json(Value::Object(values)),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request).await
    }

    async fn update_sale_price(
        &self,
        draft_id: &str,
        etag: &str,
        price: &Value,
    ) -> Result<String, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        validate_price(price)?;
        let mut request = HttpRequest::mutation(
            Method::Patch,
            format!("/items/{draft_id}"),
            RequestBody::Json(json!({
                "data": {
                    "price": {
                        "price_amount": price
                    }
                }
            })),
        );
        request.if_match = Some(etag.to_owned());
        let response = self
            .json(request)
            .await
            .map_err(|error| error_at_stage(error, "apply_price"))?;
        normalize_item_update(response, draft_id)
    }

    async fn submit_adinput(
        &self,
        draft_id: &str,
        etag: &str,
        state: &DraftState,
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut body = serde_json::to_value(state).expect("draft state serializes");
        if let Some(body) = body.as_object_mut() {
            body.remove("delivery");
        }
        let mut request = HttpRequest::mutation(
            Method::Put,
            format!("/drafts/{draft_id}/adinput"),
            RequestBody::Json(body),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request).await
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.json(HttpRequest::mutation(
            Method::Delete,
            format!("/drafts/{draft_id}"),
            RequestBody::Empty,
        ))
        .await
        .map(|_| ())
    }

    async fn upload_image(
        &self,
        draft_id: &str,
        file_name: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<UploadedImage, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mime_type = if file_name.ends_with(".png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        let response = self
            .json(HttpRequest::mutation(
                Method::Post,
                format!("/adinput/ad/recommerce/{draft_id}/upload"),
                RequestBody::Image {
                    bytes,
                    file_name: file_name.to_owned(),
                    mime_type: mime_type.to_owned(),
                    width,
                    height,
                },
            ))
            .await?;
        let location = response
            .location
            .as_deref()
            .or_else(|| response.body.get("location").and_then(Value::as_str))
            .and_then(valid_image_location)
            .ok_or_else(|| {
                let mut error = unexpected_representation("upload_image", &response);
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "Image upload succeeded without an authoritative image location".to_owned();
                error
            })?;
        Ok(UploadedImage {
            image_id: location.clone(),
            state: ImageState::Processing,
            url: Some(location),
            width,
            height,
            mime_type: Some(mime_type.to_owned()),
        })
    }

    async fn set_images(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
        images: &[UploadedImage],
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut values = composer_values(values)?;
        let mut image = Vec::with_capacity(images.len());
        let mut multi_image = Vec::with_capacity(images.len());
        for uploaded in images {
            let url = uploaded
                .url
                .as_deref()
                .and_then(valid_image_location)
                .ok_or_else(|| {
                    ApiError::new("draft.invalid_image", "Tori image location is invalid")
                })?;
            let path = url
                .strip_prefix("https://img.tori.net/dynamic/default/")
                .expect("validated image location has the canonical prefix");
            let mime_type = uploaded.mime_type.as_deref().unwrap_or("image/jpeg");
            image.push(json!({
                "height": uploaded.height.to_string(),
                "type": mime_type,
                "uri": path,
                "width": uploaded.width.to_string()
            }));
            multi_image.push(json!({
                "description": "",
                "height": uploaded.height,
                "path": path,
                "type": mime_type,
                "url": url,
                "width": uploaded.width
            }));
        }
        values.insert("image".to_owned(), Value::Array(image));
        values.insert("multi_image".to_owned(), Value::Array(multi_image));
        let mut request = HttpRequest::mutation(
            Method::Put,
            format!("/adinput/ad/recommerce/{draft_id}/update"),
            RequestBody::Json(Value::Object(values)),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request).await
    }

    async fn category_predictions(
        &self,
        draft_id: &str,
    ) -> Result<Vec<CategoryPrediction>, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/drafts/{draft_id}/category-predictions"
            )))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|_| malformed_read_response("category_predictions"))
    }

    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/listings/{listing_id}/draft-source"
            )))
            .await?;
        serde_json::from_value(response.body).map_err(|_| malformed_read_response("source_listing"))
    }

    async fn delivery_composer(&self, draft_id: &str) -> Result<DeliveryComposer, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let draft_id_query: String =
            url::form_urlencoded::byte_serialize(draft_id.as_bytes()).collect();
        let response = self
            .json(HttpRequest::read(format!(
                "/ui/addelivery?adId={draft_id_query}&editMode=false"
            )))
            .await?;
        Ok(normalize_delivery_composer(response.body))
    }

    async fn apply_delivery(
        &self,
        draft_id: &str,
        composer: &DeliveryComposer,
        delivery: &str,
    ) -> Result<(), ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let body = if delivery == "pickup" {
            json!({
                "meetup": true,
                "shipping": false,
                "sellerPaysShipping": false,
                "client": "ANDROID",
                "buyNow": false
            })
        } else {
            let package_size = composer
                .state
                .options
                .iter()
                .find(|option| option.value == delivery && option.mode == "shipping")
                .and_then(|option| option.package_size.as_deref())
                .ok_or_else(|| invalid_delivery_api(&composer.state, delivery))?;
            let address = composer
                .source
                .pointer("/sections/shipping/address")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    shipping_unavailable(&composer.state, "seller address is missing")
                })?;
            let required_string = |key: &str| {
                address
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        shipping_unavailable(
                            &composer.state,
                            &format!("seller address field `{key}` is missing"),
                        )
                    })
            };
            let postal_code = required_string("postalCode")?;
            let city = required_string("city")?;
            let name = required_string("name")?;
            let phone_number = address
                .get("phoneNumber")
                .or_else(|| address.get("mobilePhone"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    shipping_unavailable(&composer.state, "seller phone number is missing")
                })?;
            let street_name = address
                .get("streetName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let street_no = address
                .get("streetNo")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("streetName", street_name);
            query.append_pair("streetNo", street_no);
            query.append_pair("postalCode", &postal_code);
            query.append_pair("city", &city);
            query.append_pair("adId", draft_id);
            query.append_pair("size", package_size);
            query.append_pair("name", &name);
            let response = self
                .json(HttpRequest::read(format!(
                    "/ui/addelivery/shipping?{}",
                    query.finish()
                )))
                .await?;
            let products = shipping_products(&response.body);
            if products.is_empty() {
                return Err(shipping_unavailable(
                    &composer.state,
                    "no shipping providers support the selected package size",
                ));
            }
            let context = composer.source.get("context").and_then(Value::as_object);
            let seller_pays_shipping = context
                .and_then(|context| context.get("sellerPaysShipping"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let buy_now = context
                .and_then(|context| {
                    context
                        .get("buyNow")
                        .and_then(Value::as_bool)
                        .filter(|selected| *selected)
                        .or_else(|| context.get("defaultBuyNow").and_then(Value::as_bool))
                })
                .unwrap_or(false);
            let save_address = composer
                .source
                .pointer("/sections/shipping/checkBoxes/saveAddress/checked")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let shipping_info = json!({
                "size": package_size,
                "streetName": street_name,
                "streetNo": street_no,
                "houseType": address.get("houseType").cloned().unwrap_or(Value::Null),
                "floorType": address.get("floorType").cloned().unwrap_or(Value::Null),
                "floorNo": address.get("floorNo").cloned().unwrap_or(Value::Null),
                "flatNo": address.get("flatNo").cloned().unwrap_or(Value::Null),
                "deliveryPointId": address.get("deliveryPointId").cloned().unwrap_or(Value::Null),
                "postalCode": postal_code,
                "city": city,
                "products": products,
                "saveAddress": save_address,
                "address": address.get("address").cloned().unwrap_or(Value::Null),
                "name": name,
                "phoneNumber": phone_number
            });
            json!({
                "meetup": false,
                "shipping": true,
                "sellerPaysShipping": seller_pays_shipping,
                "shippingInfo": shipping_info,
                "client": "ANDROID",
                "buyNow": buy_now
            })
        };
        self.json(HttpRequest::mutation(
            Method::Post,
            format!("/ads/{draft_id}/delivery"),
            RequestBody::Json(body),
        ))
        .await
        .map(|_| ())
    }

    async fn product_context(
        &self,
        draft_id: &str,
        revision: &str,
    ) -> Result<ProductContext, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let revision: String = url::form_urlencoded::byte_serialize(revision.as_bytes()).collect();
        let response = self
            .json(HttpRequest::read(format!(
                "/drafts/{draft_id}/products?revision={revision}"
            )))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|_| malformed_read_response("product_context"))
    }

    async fn publish_basic(
        &self,
        draft_id: &str,
        context: &ProductContext,
    ) -> Result<Publication, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::mutation(
                Method::Post,
                format!("/drafts/{draft_id}/publish"),
                RequestBody::Json(json!({
                    "package": "basic",
                    "revision": context.revision,
                    "context": context.context,
                })),
            ))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|_| uncertain_mutation_response("publish_basic"))
    }

    async fn confirmation(&self, listing_id: &str) -> Result<Confirmation, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/listings/{listing_id}/confirmation"
            )))
            .await?;
        serde_json::from_value(response.body).map_err(|_| malformed_read_response("confirmation"))
    }

    async fn track_confirmation(&self, confirmation: &Confirmation) -> Result<(), ApiError> {
        self.json(HttpRequest::mutation(
            Method::Post,
            "/tracking/confirmation",
            RequestBody::Json(json!({ "order_id": confirmation.order_id })),
        ))
        .await
        .map(|_| ())
    }

    async fn observed_listing(&self, listing_id: &str) -> Result<Value, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        self.json(HttpRequest::read(format!("/listings/{listing_id}")))
            .await
            .map(|response| response.body)
    }
}

fn validate_price(price: &Value) -> Result<(), ApiError> {
    if !price.is_number()
        || price
            .as_f64()
            .is_none_or(|amount| !amount.is_finite() || amount < 0.0)
    {
        return Err(ApiError::new(
            "draft.invalid_price",
            "Price must be a non-negative number",
        ));
    }
    Ok(())
}

fn composer_trade_type(value: &str) -> &str {
    match value {
        "sell" | "SELL" => "1",
        "give_away" | "GIVE_AWAY" => "2",
        "wanted" | "WANTED" => "3",
        value => value,
    }
}

fn composer_values(values: &Map<String, Value>) -> Result<Map<String, Value>, ApiError> {
    let mut encoded = values.clone();
    if let Some(trade_type) = encoded.get_mut("trade_type")
        && let Some(value) = trade_type.as_str()
    {
        *trade_type = Value::String(composer_trade_type(value).to_owned());
    }

    let Some(price) = encoded.remove("price") else {
        return Ok(encoded);
    };
    validate_price(&price)?;
    let price_text = price.to_string();
    match encoded.get("trade_type").and_then(Value::as_str) {
        Some("1") => {
            encoded.insert("price".to_owned(), json!([{ "price_amount": price_text }]));
        }
        Some("2") => {}
        Some("3") => {
            encoded.insert("price".to_owned(), json!([{ "price_max": price_text }]));
        }
        _ => {
            return Err(ApiError::new(
                "draft.price_trade_type_conflict",
                "Price requires a recognized sale or wanted trade type",
            ));
        }
    }
    Ok(encoded)
}

fn error_at_stage(mut error: ApiError, stage: &str) -> ApiError {
    let mut details = error
        .details
        .take()
        .map(|details| *details)
        .unwrap_or_else(|| json!({}));
    if let Some(details) = details.as_object_mut() {
        details.insert("stage".to_owned(), Value::String(stage.to_owned()));
    }
    error.details = Some(Box::new(details));
    error
}

fn uncertain_item_update(response: &HttpResponse, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The price mutation may have succeeded, but its resulting revision is unknown",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": "apply_price",
        "status": response.status,
        "content_type": response.content_type,
        "reason": reason,
    })));
    error
}

fn normalize_item_update(response: HttpResponse, draft_id: &str) -> Result<String, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_item_update(
            &response,
            "successful response was not valid JSON",
        ));
    }
    let response_id = response.body.get("id").and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    });
    if response_id.as_deref() != Some(draft_id) {
        return Err(uncertain_item_update(
            &response,
            "successful response identified a different item",
        ));
    }
    response
        .body
        .get("etag")
        .and_then(Value::as_str)
        .filter(|etag| !etag.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            uncertain_item_update(
                &response,
                "successful response did not contain an authoritative ETag",
            )
        })
}

fn normalize_delivery_composer(source: Value) -> DeliveryComposer {
    let mut options = Vec::new();
    if let Some(meetup) = source.pointer("/sections/deliveryOptions/meetup") {
        options.push(DeliveryOption {
            value: "pickup".to_owned(),
            label: meetup
                .get("title")
                .and_then(Value::as_str)
                .unwrap_or("Pickup or direct arrangement")
                .to_owned(),
            mode: "pickup".to_owned(),
            package_size: None,
        });
    }
    if source
        .pointer("/sections/deliveryOptions/shipping")
        .is_some()
    {
        let mut shipping = source
            .pointer("/sections/shipping/packageSizes")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(_, size)| {
                let package_size = size.get("size")?.as_str()?.trim();
                if package_size.is_empty() {
                    return None;
                }
                let normalized = package_size.to_ascii_lowercase();
                Some(DeliveryOption {
                    value: format!("shipping:{normalized}"),
                    label: size
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or(package_size)
                        .to_owned(),
                    mode: "shipping".to_owned(),
                    package_size: Some(package_size.to_owned()),
                })
            })
            .collect::<Vec<_>>();
        shipping.sort_by_key(|option| match option.package_size.as_deref() {
            Some("SMALL") => 0,
            Some("MEDIUM") => 1,
            Some("LARGE") => 2,
            _ => 3,
        });
        options.extend(shipping);
    }
    let context = source.get("context").and_then(Value::as_object);
    let selected = if context
        .and_then(|context| context.get("shipping"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        context
            .and_then(|context| context.get("packageSize"))
            .and_then(Value::as_str)
            .map(|size| vec![format!("shipping:{}", size.to_ascii_lowercase())])
            .unwrap_or_default()
    } else if context
        .and_then(|context| context.get("meetup"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        vec!["pickup".to_owned()]
    } else {
        Vec::new()
    };
    let available = !options.is_empty();
    DeliveryComposer {
        state: DraftDelivery {
            source: "remote_delivery_composer".to_owned(),
            available,
            options,
            selected,
            unavailable_reason: (!available)
                .then(|| "Tori returned no delivery options for this draft".to_owned()),
        },
        source,
    }
}

fn shipping_products(body: &Value) -> Vec<String> {
    let mut products = body
        .pointer("/sections/shipping/providers/options")
        .or_else(|| body.pointer("/sections/providers/options"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| option.get("product").and_then(Value::as_str))
        .filter(|product| !product.trim().is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    products.sort();
    products.dedup();
    products
}

fn allowed_delivery_values(state: &DraftDelivery) -> Vec<String> {
    state
        .options
        .iter()
        .map(|option| option.value.clone())
        .collect()
}

fn invalid_delivery_api(state: &DraftDelivery, requested: &str) -> ApiError {
    let mut error = ApiError::new(
        "draft.invalid_delivery",
        "The requested delivery value is unavailable for this draft",
    );
    error.details = Some(Box::new(json!({
        "requested_values": [requested],
        "allowed_values": allowed_delivery_values(state),
    })));
    error
}

fn shipping_unavailable(state: &DraftDelivery, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "draft.delivery_options_unavailable",
        "Shipping cannot be configured from the current delivery composer",
    );
    error.details = Some(Box::new(json!({
        "reason": reason,
        "allowed_values": allowed_delivery_values(state),
        "recovery_guidance": "Open the draft delivery composer in Tori and complete the seller address"
    })));
    error
}

fn safe_content_type(value: &str) -> String {
    value
        .split(';')
        .next()
        .unwrap_or("unknown")
        .trim()
        .to_ascii_lowercase()
}

fn unexpected_representation(stage: &str, response: &HttpResponse) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an unsupported response representation",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": stage,
        "status": response.status,
        "content_type": response.content_type,
    })));
    error
}

fn malformed_read_response(stage: &str) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an invalid success response",
    )
    .retry_classification(classify(
        FailureKind::MalformedSuccess,
        RetryContext::read(OperationMethod::Get),
    ));
    error.details = Some(Box::new(json!({ "stage": stage })));
    error
}

fn uncertain_mutation_response(stage: &str) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The mutation may have succeeded, but its resulting state is unknown",
    );
    error.details = Some(Box::new(json!({
        "stage": stage,
        "recovery_guidance": "Inspect authoritative state before continuing; do not repeat the mutation"
    })));
    error
}

fn uncertain_creation(response: &HttpResponse, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "Draft creation may have succeeded, but its remote identity could not be established",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": "create_draft",
        "status": response.status,
        "content_type": response.content_type,
        "completed_steps": [],
        "reason": reason,
        "recovery_guidance": "Inspect drafts in Tori before continuing; do not repeat draft creation"
    })));
    error
}

fn draft_id_from_body(body: &Value) -> Option<String> {
    let ad = body.get("ad").unwrap_or(body);
    [ad.get("id"), ad.get("draft_id"), body.get("draft_id")]
        .into_iter()
        .flatten()
        .find_map(|value| match value {
            Value::String(value) if validate_resource_id(value, "draft").is_ok() => {
                Some(value.clone())
            }
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
}

fn draft_id_from_location(location: Option<&str>) -> Option<String> {
    let location = location?;
    let path = if location.starts_with('/') {
        location.to_owned()
    } else {
        let parsed = url::Url::parse(location).ok()?;
        if parsed.scheme() != "https" || parsed.host_str()? != "apps-adinput.svc.tori.fi" {
            return None;
        }
        parsed.path().to_owned()
    };
    let id = path
        .strip_prefix("/adinput/ad/recommerce/")?
        .trim_end_matches('/');
    (!id.contains('/') && validate_resource_id(id, "draft").is_ok()).then(|| id.to_owned())
}

fn valid_image_location(location: &str) -> Option<String> {
    let parsed = url::Url::parse(location).ok()?;
    (parsed.scheme() == "https"
        && parsed.host_str() == Some("img.tori.net")
        && parsed.path().starts_with("/dynamic/default/")
        && parsed.query().is_none()
        && parsed.fragment().is_none())
    .then(|| location.to_owned())
}

fn normalize_draft_values(mut values: Map<String, Value>) -> Result<Map<String, Value>, ApiError> {
    let Some(price) = values.remove("price") else {
        return Ok(values);
    };
    let normalized = if price.is_number() {
        price
    } else {
        let entries = price.as_array().ok_or_else(invalid_source_price)?;
        let [entry] = entries.as_slice() else {
            return Err(invalid_source_price());
        };
        let object = entry.as_object().ok_or_else(invalid_source_price)?;
        if object.len() != 1 {
            return Err(invalid_source_price());
        }
        let amount = object
            .get("price_amount")
            .or_else(|| object.get("price_max"))
            .and_then(Value::as_str)
            .ok_or_else(invalid_source_price)?;
        serde_json::from_str::<Value>(amount).map_err(|_| invalid_source_price())?
    };
    if validate_price(&normalized).is_err() {
        return Err(invalid_source_price());
    }
    values.insert("price".to_owned(), normalized);
    Ok(values)
}

fn invalid_source_price() -> ApiError {
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an unsupported price representation",
    );
    error.details = Some(Box::new(json!({ "stage": "normalize_price" })));
    error
}

fn normalize_draft_state(body: Value, response_etag: Option<&str>) -> Result<DraftState, ApiError> {
    if let Ok(mut legacy) = serde_json::from_value::<DraftState>(body.clone()) {
        if let Some(etag) = response_etag {
            legacy.etag = etag.to_owned();
        }
        legacy.values = normalize_draft_values(legacy.values)?;
        return Ok(legacy);
    }
    let draft_id = draft_id_from_body(&body).ok_or_else(|| {
        ApiError::new(
            "upstream.unexpected_response",
            "Tori draft response did not contain an authoritative identity",
        )
    })?;
    let ad = body.get("ad").unwrap_or(&body);
    let values = normalize_draft_values(
        ad.get("values")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
    )?;
    let etag = response_etag
        .or_else(|| ad.get("etag").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    let images = values
        .get("multi_image")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(position, value)| {
            let object = value.as_object()?;
            let url = object.get("url").and_then(Value::as_str)?.to_owned();
            Some(DraftImage {
                image_id: url.clone(),
                position,
                state: ImageState::Ready,
                url: Some(url),
                width: object
                    .get("width")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
                height: object
                    .get("height")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
                mime_type: object
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                failure: None,
            })
        })
        .collect();
    Ok(DraftState {
        draft_id,
        etag,
        values,
        fields: Vec::new(),
        options: Vec::new(),
        required_fields: Vec::new(),
        images,
        cleared_fields: Vec::new(),
        predictions: Vec::new(),
        delivery: None,
    })
}

fn validate_resource_id(value: &str, resource: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ApiError::new(
            format!("{resource}.invalid_id"),
            format!("The {resource} ID is invalid"),
        ));
    }
    Ok(())
}

#[derive(Clone, Debug)]
pub struct WorkflowConfig {
    pub image_processing_timeout: Duration,
    pub image_poll_interval: Duration,
    pub image_poll_limit: usize,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            image_processing_timeout: Duration::from_secs(120),
            image_poll_interval: Duration::from_secs(2),
            image_poll_limit: 60,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Recovery {
    pub draft_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_id: Option<String>,
    pub completed_steps: Vec<String>,
    pub upstream_transient: bool,
    pub safe_to_retry: bool,
    pub next_safe_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_state: Option<DraftState>,
}

#[derive(Clone, PartialEq)]
pub struct WorkflowError {
    pub code: String,
    pub message: String,
    pub source: Option<ApiError>,
    pub recovery: Option<Recovery>,
    pub details: Option<Value>,
}

impl WorkflowError {
    fn input(error: ApiError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery: None,
            details: None,
        }
    }

    fn before_creation(error: ApiError) -> Self {
        let recovery = error.details.as_deref().and_then(|details| {
            let draft_id = details.get("draft_id")?.as_str()?.to_owned();
            let completed_steps = details
                .get("completed_steps")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            Some(Recovery {
                next_safe_actions: vec![format!("flea draft show {draft_id}")],
                draft_id,
                listing_id: None,
                completed_steps,
                upstream_transient: error.upstream_transient,
                safe_to_retry: false,
                fresh_state: None,
            })
        });
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery,
            details: None,
        }
    }

    fn for_draft(
        draft_id: &str,
        completed_steps: &[String],
        error: ApiError,
        safe_to_retry: bool,
    ) -> Self {
        let safe_to_retry =
            safe_to_retry && error.safe_to_retry && !completed_steps_have_mutation(completed_steps);
        let upstream_transient = error.upstream_transient;
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery: Some(Recovery {
                draft_id: draft_id.to_owned(),
                listing_id: None,
                completed_steps: completed_steps.to_vec(),
                upstream_transient,
                safe_to_retry,
                next_safe_actions: vec![format!("flea draft show {draft_id}")],
                fresh_state: None,
            }),
            details: None,
        }
    }

    fn with_listing_id(mut self, listing_id: &str) -> Self {
        if let Some(recovery) = &mut self.recovery {
            recovery.listing_id = Some(listing_id.to_owned());
        }
        self
    }

    fn price_observation(
        draft_id: &str,
        completed_steps: &[String],
        requested: &Value,
        fresh_state: DraftState,
    ) -> Self {
        let observed = fresh_state.values.get("price").cloned();
        let mut error = ApiError::new(
            "mutation.uncertain",
            "The authoritative draft price does not match the requested price",
        );
        error.details = Some(Box::new(json!({
            "stage": "observe_price",
            "requested_price": requested,
            "observed_price": observed,
        })));
        let mut workflow = Self::for_draft(draft_id, completed_steps, error, false);
        if let Some(recovery) = &mut workflow.recovery {
            recovery.fresh_state = Some(fresh_state);
        }
        workflow
    }

    fn validation(draft_id: &str, completed_steps: &[String], missing: Vec<String>) -> Self {
        Self {
            code: "draft.validation_failed".to_owned(),
            message: "Required fields are missing or invalid".to_owned(),
            source: None,
            recovery: Some(Recovery {
                draft_id: draft_id.to_owned(),
                listing_id: None,
                completed_steps: completed_steps.to_vec(),
                upstream_transient: false,
                safe_to_retry: false,
                next_safe_actions: vec![format!("flea draft update {draft_id} --input PATH")],
                fresh_state: None,
            }),
            details: Some(json!({ "missing_fields": missing })),
        }
    }

    fn delivery_validation(
        draft_id: &str,
        completed_steps: &[String],
        delivery: &DraftDelivery,
        requested: Vec<String>,
    ) -> Self {
        let allowed = allowed_delivery_values(delivery);
        let next_safe_actions = allowed
            .first()
            .map(|value| format!("flea draft update {draft_id} --delivery {value}"))
            .into_iter()
            .chain(std::iter::once(format!("flea draft show {draft_id}")))
            .collect();
        let missing = requested.is_empty();
        Self {
            code: if missing {
                "draft.validation_failed".to_owned()
            } else {
                "draft.invalid_delivery".to_owned()
            },
            message: if missing {
                "An explicit delivery selection is required".to_owned()
            } else {
                "The requested delivery value is unavailable for this draft".to_owned()
            },
            source: None,
            recovery: Some(Recovery {
                draft_id: draft_id.to_owned(),
                listing_id: None,
                completed_steps: completed_steps.to_vec(),
                upstream_transient: false,
                safe_to_retry: false,
                next_safe_actions,
                fresh_state: None,
            }),
            details: Some(json!({
                "missing_fields": if missing { vec!["delivery"] } else { Vec::<&str>::new() },
                "requested_values": requested,
                "allowed_values": allowed,
                "options_available": delivery.available,
                "unavailable_reason": delivery.unavailable_reason,
                "recovery_guidance": if delivery.available {
                    "Select one of the allowed machine values"
                } else {
                    "Open the draft delivery composer in Tori and make delivery options available"
                },
            })),
        }
    }
}

pub(crate) fn completed_steps_have_mutation(completed_steps: &[String]) -> bool {
    completed_steps.iter().any(|step| {
        step == "create_draft"
            || step == "apply_category"
            || step == "apply_fields"
            || step == "copy_fields"
            || step.starts_with("upload_image:")
            || matches!(
                step.as_str(),
                "attach_images"
                    | "update_item_fields"
                    | "apply_price"
                    | "patch_item_fields"
                    | "submit_adinput"
                    | "apply_delivery"
                    | "publish_basic"
                    | "track_confirmation"
            )
    })
}

impl fmt::Debug for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut details = self.details.clone();
        let mut recovery = self
            .recovery
            .as_ref()
            .and_then(|recovery| serde_json::to_value(recovery).ok());
        if let Some(details) = &mut details {
            diagnostics::redact_value(details);
        }
        if let Some(recovery) = &mut recovery {
            diagnostics::redact_value(recovery);
        }
        formatter
            .debug_struct("WorkflowError")
            .field("code", &self.code)
            .field("message", &diagnostics::redact_text(&self.message))
            .field("source", &self.source)
            .field("recovery", &recovery)
            .field("details", &details)
            .finish()
    }
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowError {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CreateResult {
    pub draft: DraftState,
    pub completed_steps: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UpdateResult {
    pub draft: DraftState,
    pub requested_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_delivery: Vec<String>,
    pub persisted_fields: Vec<String>,
    pub ignored_fields: Vec<String>,
    pub etag_changed: bool,
    pub completed_steps: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PublishResult {
    pub draft_id: String,
    pub listing_id: String,
    pub revision: String,
    pub state: String,
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub observed_listing: Value,
}

pub struct DraftWorkflow<A> {
    api: A,
    config: WorkflowConfig,
}

impl<A> DraftWorkflow<A> {
    pub fn new(api: A, config: WorkflowConfig) -> Self {
        Self { api, config }
    }
}

fn requested_sale_price(values: &Map<String, Value>) -> Result<Option<Value>, ApiError> {
    let Some(price) = values.get("price") else {
        return Ok(None);
    };
    validate_price(price)?;
    let trade_type = values
        .get("trade_type")
        .and_then(Value::as_str)
        .map(composer_trade_type);
    if trade_type != Some("1") {
        return Err(ApiError::new(
            "draft.price_trade_type_conflict",
            "Sale price requires the sale trade type",
        ));
    }
    Ok(Some(price.clone()))
}

fn prices_equal(left: &Value, right: &Value) -> bool {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

impl<A: AdInputApi> DraftWorkflow<A> {
    async fn observe_price(
        &self,
        draft_id: &str,
        requested: &Value,
        completed: &[String],
    ) -> Result<DraftState, WorkflowError> {
        let fresh = self.api.get_draft(draft_id).await.map_err(|error| {
            WorkflowError::for_draft(
                draft_id,
                completed,
                error_at_stage(error, "observe_price"),
                true,
            )
        })?;
        if fresh
            .values
            .get("price")
            .is_some_and(|observed| prices_equal(observed, requested))
        {
            Ok(fresh)
        } else {
            Err(WorkflowError::price_observation(
                draft_id, completed, requested, fresh,
            ))
        }
    }

    async fn delivery_composer(
        &self,
        draft_id: &str,
        completed: &[String],
    ) -> Result<DeliveryComposer, WorkflowError> {
        self.api
            .delivery_composer(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, completed, error, true))
    }

    async fn apply_delivery_selection(
        &self,
        mut state: DraftState,
        requested: &Value,
        completed: &mut Vec<String>,
    ) -> Result<DraftState, WorkflowError> {
        let draft_id = state.draft_id.clone();
        let composer = self.delivery_composer(&draft_id, completed).await?;
        completed.push("fetch_delivery_options".to_owned());
        let requested_values = delivery_values(requested).unwrap_or_default();
        let selected = requested_values
            .first()
            .filter(|_| requested_values.len() == 1)
            .filter(|requested| {
                composer
                    .state
                    .options
                    .iter()
                    .any(|option| option.value.as_str() == requested.as_str())
            })
            .cloned()
            .ok_or_else(|| {
                WorkflowError::delivery_validation(
                    &draft_id,
                    completed,
                    &composer.state,
                    requested_values.clone(),
                )
            })?;
        self.api
            .apply_delivery(&draft_id, &composer, &selected)
            .await
            .map_err(|error| WorkflowError::for_draft(&draft_id, completed, error, false))?;
        completed.push("apply_delivery".to_owned());
        let observed = self.delivery_composer(&draft_id, completed).await?;
        if observed.state.selected != [selected.clone()] {
            let mut error = ApiError::new(
                "mutation.uncertain",
                "Tori accepted the delivery mutation without returning the requested state",
            );
            error.details = Some(Box::new(json!({
                "requested_values": [selected],
                "observed_values": observed.state.selected.clone(),
                "allowed_values": allowed_delivery_values(&observed.state),
                "recovery_guidance": format!("Inspect the draft with `flea draft show {draft_id}`; do not repeat publication")
            })));
            return Err(WorkflowError::for_draft(&draft_id, completed, error, false));
        }
        completed.push("observe_delivery".to_owned());
        state.delivery = Some(observed.state);
        Ok(state)
    }

    pub async fn create(
        &self,
        values: Map<String, Value>,
        image_paths: &[impl AsRef<Path>],
    ) -> Result<CreateResult, WorkflowError> {
        let mut images = Vec::with_capacity(image_paths.len());
        for path in image_paths {
            match prepare_image(path.as_ref()) {
                Ok(image) => images.push(image),
                Err(error) => return Err(WorkflowError::before_creation(error)),
            }
        }
        self.create_prepared(values, images).await
    }

    pub async fn create_prepared(
        &self,
        mut values: Map<String, Value>,
        images: Vec<PreparedImage>,
    ) -> Result<CreateResult, WorkflowError> {
        let price = requested_sale_price(&values).map_err(WorkflowError::input)?;
        values.remove("price");
        let delivery = values.remove("delivery");
        let mut draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["create_draft".to_owned()];

        if let Some(category) = values.remove("category") {
            let mut category_values = draft.values.clone();
            category_values.insert("category".to_owned(), normalize_category(category));
            draft = self
                .api
                .update_item(&draft.draft_id, &draft.etag, &category_values)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                })?;
            completed.push("apply_category".to_owned());
        }
        if !values.is_empty() {
            let mut merged_values = draft.values.clone();
            merged_values.extend(values);
            draft = self
                .api
                .update_item(&draft.draft_id, &draft.etag, &merged_values)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                })?;
            completed.push("apply_fields".to_owned());
        }
        if let Some(price) = price {
            self.api
                .update_sale_price(&draft.draft_id, &draft.etag, &price)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                })?;
            completed.push("apply_price".to_owned());
            draft = self
                .observe_price(&draft.draft_id, &price, &completed)
                .await?;
            completed.push("observe_price".to_owned());
        }
        if !images.is_empty() {
            draft = self
                .add_prepared_images(&draft, images, &mut completed)
                .await?;
        }
        if let Some(delivery) = delivery {
            draft = self
                .apply_delivery_selection(draft, &delivery, &mut completed)
                .await?;
        }
        Ok(CreateResult {
            draft,
            completed_steps: completed,
        })
    }

    pub async fn create_from_listing(
        &self,
        listing_id: &str,
    ) -> Result<CreateResult, WorkflowError> {
        let mut seed = self
            .api
            .source_listing(listing_id)
            .await
            .map_err(WorkflowError::before_creation)?;
        let price = requested_sale_price(&seed.values).map_err(WorkflowError::input)?;
        seed.values.remove("price");
        let mut draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["load_source_listing".to_owned(), "create_draft".to_owned()];
        let mut seed_values = seed.values;
        let delivery = seed_values.remove("delivery");
        let mut copied_values = draft.values.clone();
        copied_values.extend(seed_values);
        draft = self
            .api
            .update_item(&draft.draft_id, &draft.etag, &copied_values)
            .await
            .map_err(|error| {
                WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                    .with_listing_id(listing_id)
            })?;
        completed.push("copy_fields".to_owned());
        if let Some(price) = price {
            self.api
                .update_sale_price(&draft.draft_id, &draft.etag, &price)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                        .with_listing_id(listing_id)
                })?;
            completed.push("apply_price".to_owned());
            draft = self
                .observe_price(&draft.draft_id, &price, &completed)
                .await
                .map_err(|error| error.with_listing_id(listing_id))?;
            completed.push("observe_price".to_owned());
        }

        let mut ordered = Vec::new();
        for source in seed.images {
            let image = sanitize_image(&source.bytes).map_err(|error| {
                WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                    .with_listing_id(listing_id)
            })?;
            let uploaded = self
                .api
                .upload_image(
                    &draft.draft_id,
                    &image.file_name,
                    image.bytes,
                    image.width,
                    image.height,
                )
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                        .with_listing_id(listing_id)
                })?;
            ordered.push(uploaded);
            completed.push(format!("upload_image:{}", ordered.len() - 1));
        }
        if !ordered.is_empty() {
            draft = self
                .api
                .set_images(&draft.draft_id, &draft.etag, &draft.values, &ordered)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                        .with_listing_id(listing_id)
                })?;
            completed.push("attach_images".to_owned());
        }
        if let Some(delivery) = delivery {
            draft = self
                .apply_delivery_selection(draft, &delivery, &mut completed)
                .await
                .map_err(|error| error.with_listing_id(listing_id))?;
        }
        Ok(CreateResult {
            draft,
            completed_steps: completed,
        })
    }

    pub async fn show(&self, draft_id: &str) -> Result<DraftState, WorkflowError> {
        let mut state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        if state.category_is_unset() && !state.images.is_empty() {
            state.predictions = self
                .api
                .category_predictions(draft_id)
                .await
                .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
            completed.push("fetch_category_predictions".to_owned());
        }
        state.delivery = Some(self.delivery_composer(draft_id, &completed).await?.state);
        Ok(state)
    }

    pub async fn update(
        &self,
        draft_id: &str,
        patch: &Map<String, Value>,
    ) -> Result<UpdateResult, WorkflowError> {
        if let Some(price) = patch.get("price") {
            validate_price(price).map_err(WorkflowError::input)?;
        }
        let current = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        let mut values = current.values.clone();
        values.extend(patch.clone());
        values.remove("delivery");
        let price = if patch.contains_key("price") {
            requested_sale_price(&values)
                .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?
        } else {
            None
        };
        let mut item_patch = patch.clone();
        let delivery = item_patch.remove("delivery");
        item_patch.remove("price");
        let requested_delivery = delivery
            .as_ref()
            .and_then(delivery_values)
            .unwrap_or_default();
        let mut state = current.clone();
        let mut persisted_fields = Vec::new();
        let mut ignored_fields = Vec::new();

        if !item_patch.is_empty() {
            if price.is_some() {
                values.remove("price");
            }
            state = match self.api.update_item(draft_id, &state.etag, &values).await {
                Ok(state) => state,
                Err(error) if error.status == Some(412) => {
                    return Err(self
                        .update_conflict(draft_id, &completed, OperationMethod::Put)
                        .await);
                }
                Err(error) => {
                    return Err(WorkflowError::for_draft(draft_id, &completed, error, false));
                }
            };
            completed.push("update_item_fields".to_owned());
            for (field, requested) in &item_patch {
                if state.values.get(field) == Some(requested) {
                    persisted_fields.push(field.clone());
                } else {
                    ignored_fields.push(field.clone());
                }
            }
        }

        if let Some(price) = price {
            match self
                .api
                .update_sale_price(draft_id, &state.etag, &price)
                .await
            {
                Ok(_) => {}
                Err(error) if error.status == Some(412) => {
                    return Err(self
                        .update_conflict(draft_id, &completed, OperationMethod::Patch)
                        .await);
                }
                Err(error) => {
                    return Err(WorkflowError::for_draft(draft_id, &completed, error, false));
                }
            }
            completed.push("apply_price".to_owned());
            state = self.observe_price(draft_id, &price, &completed).await?;
            completed.push("observe_price".to_owned());
            persisted_fields.push("price".to_owned());
        }

        if let Some(delivery) = delivery {
            state = self
                .apply_delivery_selection(state, &delivery, &mut completed)
                .await?;
            persisted_fields.push("delivery".to_owned());
        }
        let etag_changed = state.etag != current.etag;
        let mut requested_fields = patch.keys().cloned().collect::<Vec<_>>();
        requested_fields.sort();
        persisted_fields.sort();
        ignored_fields.sort();
        Ok(UpdateResult {
            draft: state,
            requested_fields,
            requested_delivery,
            persisted_fields,
            ignored_fields,
            etag_changed,
            completed_steps: completed,
        })
    }

    async fn update_conflict(
        &self,
        draft_id: &str,
        completed: &[String],
        method: OperationMethod,
    ) -> WorkflowError {
        let fresh = match self.api.get_draft(draft_id).await {
            Ok(fresh) => fresh,
            Err(error) => return WorkflowError::for_draft(draft_id, completed, error, true),
        };
        let mut conflict = ApiError::new(
            "draft.conflict",
            "The draft changed while the update was being applied",
        );
        conflict.status = Some(412);
        let mut context = RetryContext::mutation(method)
            .with_etag()
            .with_authoritative_observation();
        if completed_steps_have_mutation(completed) {
            context = context.with_completed_mutation_steps();
        }
        let classification = classify(FailureKind::PreconditionFailed, context);
        let mut workflow = WorkflowError::for_draft(draft_id, completed, conflict, false);
        if let Some(recovery) = &mut workflow.recovery {
            recovery.upstream_transient = classification.upstream_transient;
            recovery.safe_to_retry = classification.safe_to_retry;
            recovery.fresh_state = Some(fresh);
            recovery.next_safe_actions = vec![format!("flea draft show {draft_id}")];
        }
        workflow
    }

    pub async fn delete(&self, draft_id: &str) -> Result<(), WorkflowError> {
        self.api
            .delete_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, false))
    }

    pub async fn add_images(
        &self,
        draft_id: &str,
        paths: &[impl AsRef<Path>],
    ) -> Result<DraftState, WorkflowError> {
        let state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        self.add_images_from_paths(&state, paths, &mut completed)
            .await
    }

    async fn add_images_from_paths(
        &self,
        state: &DraftState,
        paths: &[impl AsRef<Path>],
        completed: &mut Vec<String>,
    ) -> Result<DraftState, WorkflowError> {
        let mut existing = state.images.iter().collect::<Vec<_>>();
        existing.sort_by_key(|image| image.position);
        let mut ordered: Vec<UploadedImage> = existing
            .into_iter()
            .map(uploaded_from_draft_image)
            .collect();
        for path in paths {
            let image = prepare_image(path.as_ref()).map_err(|error| {
                WorkflowError::for_draft(&state.draft_id, completed, error, false)
            })?;
            let uploaded = self.upload_prepared_image(state, image, completed).await?;
            ordered.push(uploaded);
            completed.push(format!("upload_image:{}", ordered.len() - 1));
        }
        let updated = self
            .api
            .set_images(&state.draft_id, &state.etag, &state.values, &ordered)
            .await
            .map_err(|error| WorkflowError::for_draft(&state.draft_id, completed, error, false))?;
        completed.push("attach_images".to_owned());
        Ok(updated)
    }

    async fn add_prepared_images(
        &self,
        state: &DraftState,
        images: Vec<PreparedImage>,
        completed: &mut Vec<String>,
    ) -> Result<DraftState, WorkflowError> {
        let mut existing = state.images.iter().collect::<Vec<_>>();
        existing.sort_by_key(|image| image.position);
        let mut ordered: Vec<UploadedImage> = existing
            .into_iter()
            .map(uploaded_from_draft_image)
            .collect();
        for image in images {
            let uploaded = self.upload_prepared_image(state, image, completed).await?;
            ordered.push(uploaded);
            completed.push(format!("upload_image:{}", ordered.len() - 1));
        }
        let updated = self
            .api
            .set_images(&state.draft_id, &state.etag, &state.values, &ordered)
            .await
            .map_err(|error| WorkflowError::for_draft(&state.draft_id, completed, error, false))?;
        completed.push("attach_images".to_owned());
        Ok(updated)
    }

    async fn upload_prepared_image(
        &self,
        state: &DraftState,
        image: PreparedImage,
        completed: &[String],
    ) -> Result<UploadedImage, WorkflowError> {
        self.api
            .upload_image(
                &state.draft_id,
                &image.file_name,
                image.bytes,
                image.width,
                image.height,
            )
            .await
            .map_err(|error| WorkflowError::for_draft(&state.draft_id, completed, error, false))
    }

    pub async fn remove_images(
        &self,
        draft_id: &str,
        remove: &[String],
    ) -> Result<DraftState, WorkflowError> {
        let state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut retained = state
            .images
            .iter()
            .filter(|image| !remove.contains(&image.image_id))
            .collect::<Vec<_>>();
        retained.sort_by_key(|image| image.position);
        let ordered: Vec<UploadedImage> = retained
            .into_iter()
            .map(uploaded_from_draft_image)
            .collect();
        self.api
            .set_images(draft_id, &state.etag, &state.values, &ordered)
            .await
            .map_err(|error| {
                WorkflowError::for_draft(draft_id, &["fetch_draft".to_owned()], error, false)
            })
    }

    pub async fn publish(&self, draft_id: &str) -> Result<PublishResult, WorkflowError> {
        let mut completed = Vec::new();
        let mut state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        completed.push("fetch_draft".to_owned());
        let composer = self.delivery_composer(draft_id, &completed).await?;
        state.delivery = Some(composer.state.clone());
        completed.push("fetch_delivery_options".to_owned());

        let missing = missing_required_fields(&state);
        if !missing.is_empty() {
            return Err(WorkflowError::validation(draft_id, &completed, missing));
        }
        let requested_delivery = composer.state.selected.clone();
        let delivery = requested_delivery
            .first()
            .filter(|_| requested_delivery.len() == 1)
            .filter(|requested| {
                composer
                    .state
                    .options
                    .iter()
                    .any(|option| option.value.as_str() == requested.as_str())
            })
            .cloned()
            .ok_or_else(|| {
                WorkflowError::delivery_validation(
                    draft_id,
                    &completed,
                    &composer.state,
                    requested_delivery,
                )
            })?;
        completed.push("validate".to_owned());

        state = self.wait_for_images(state, &completed).await?;
        completed.push("wait_for_images".to_owned());

        state.values.remove("delivery");
        self.api
            .update_item(draft_id, &state.etag, &state.values)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        completed.push("patch_item_fields".to_owned());

        state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        completed.push("fetch_fresh_etag".to_owned());

        state = self
            .api
            .submit_adinput(draft_id, &state.etag, &state)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        completed.push("submit_adinput".to_owned());

        let revision = state
            .values
            .get("revision")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                WorkflowError::for_draft(
                    draft_id,
                    &completed,
                    ApiError::new("upstream.unexpected_response", "ad revision is missing"),
                    false,
                )
            })?
            .to_owned();
        self.api
            .apply_delivery(draft_id, &composer, &delivery)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        completed.push("apply_delivery".to_owned());
        let observed_delivery = self.delivery_composer(draft_id, &completed).await?;
        if observed_delivery.state.selected != [delivery.clone()] {
            let mut error = ApiError::new(
                "mutation.uncertain",
                "Tori accepted the delivery mutation without returning the requested state",
            );
            error.details = Some(Box::new(json!({
                "requested_values": [delivery],
                "observed_values": observed_delivery.state.selected.clone(),
                "allowed_values": allowed_delivery_values(&observed_delivery.state),
                "recovery_guidance": format!("Inspect the draft with `flea draft show {draft_id}`; do not repeat publication")
            })));
            return Err(WorkflowError::for_draft(draft_id, &completed, error, false));
        }
        completed.push("observe_delivery".to_owned());

        let context = self
            .api
            .product_context(draft_id, &revision)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        completed.push("fetch_product_context".to_owned());

        let publication = self
            .api
            .publish_basic(draft_id, &context)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        completed.push("publish_basic".to_owned());

        let mut warnings = Vec::new();
        match self.api.confirmation(&publication.listing_id).await {
            Ok(confirmation) => {
                completed.push("fetch_confirmation".to_owned());
                if let Err(error) = self.api.track_confirmation(&confirmation).await {
                    warnings.push(format!("confirmation tracking failed: {}", error.message));
                } else {
                    completed.push("track_confirmation".to_owned());
                }
            }
            Err(error) => warnings.push(format!("confirmation fetch failed: {}", error.message)),
        }

        let observed_listing = match self.api.observed_listing(&publication.listing_id).await {
            Ok(listing) => listing,
            Err(error) => {
                let mut workflow = WorkflowError::for_draft(draft_id, &completed, error, true);
                if let Some(recovery) = &mut workflow.recovery {
                    recovery.listing_id = Some(publication.listing_id.clone());
                    recovery.next_safe_actions =
                        vec![format!("flea listing show {}", publication.listing_id)];
                }
                workflow.details = Some(json!({
                    "listing_id": publication.listing_id,
                    "revision": publication.revision,
                }));
                return Err(workflow);
            }
        };
        completed.push("fetch_observed_listing".to_owned());

        Ok(PublishResult {
            draft_id: draft_id.to_owned(),
            listing_id: publication.listing_id,
            revision: publication.revision,
            state: publication.state,
            completed_steps: completed,
            warnings,
            observed_listing,
        })
    }

    async fn wait_for_images(
        &self,
        mut state: DraftState,
        completed: &[String],
    ) -> Result<DraftState, WorkflowError> {
        let started = tokio::time::Instant::now();
        for poll in 0..=self.config.image_poll_limit {
            if let Some(image) = state
                .images
                .iter()
                .find(|image| image.state == ImageState::Failed)
            {
                let mut error = ApiError::new("draft.image_failed", "An image failed processing");
                error.details = Some(Box::new(
                    json!({ "image_id": image.image_id, "failure": image.failure }),
                ));
                return Err(WorkflowError::for_draft(
                    &state.draft_id,
                    completed,
                    error,
                    false,
                ));
            }
            if state
                .images
                .iter()
                .all(|image| image.state == ImageState::Ready)
            {
                return Ok(state);
            }
            if poll == self.config.image_poll_limit
                || started.elapsed() >= self.config.image_processing_timeout
            {
                return Err(image_processing_timeout(&state.draft_id, completed));
            }
            let remaining = self
                .config
                .image_processing_timeout
                .saturating_sub(started.elapsed());
            tokio::time::sleep(self.config.image_poll_interval.min(remaining)).await;
            let remaining = self
                .config
                .image_processing_timeout
                .saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(image_processing_timeout(&state.draft_id, completed));
            }
            state = tokio::time::timeout(remaining, self.api.get_draft(&state.draft_id))
                .await
                .map_err(|_| image_processing_timeout(&state.draft_id, completed))?
                .map_err(|error| {
                    WorkflowError::for_draft(&state.draft_id, completed, error, true)
                })?;
        }
        unreachable!("bounded image loop always returns")
    }
}

fn image_processing_timeout(draft_id: &str, completed: &[String]) -> WorkflowError {
    let mut error = ApiError::new(
        "draft.image_processing",
        "Images did not finish processing before the bounded timeout",
    );
    error.upstream_transient = true;
    error.safe_to_retry = true;
    WorkflowError::for_draft(draft_id, completed, error, true)
}

const MAX_IMAGE_INPUT_BYTES: u64 = 25 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 12_000;
const MAX_IMAGE_PIXELS: u64 = 40_000_000;

pub struct PreparedImage {
    bytes: Vec<u8>,
    file_name: String,
    width: u32,
    height: u32,
    source_format: &'static str,
    metadata_stripped: bool,
}

impl PreparedImage {
    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub const fn source_format(&self) -> &'static str {
        self.source_format
    }

    pub fn output_format(&self) -> &'static str {
        if self.file_name.ends_with(".png") {
            "png"
        } else {
            "jpeg"
        }
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    pub const fn metadata_stripped(&self) -> bool {
        self.metadata_stripped
    }
}

impl fmt::Debug for PreparedImage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedImage")
            .field("byte_len", &self.bytes.len())
            .field("file_name", &self.file_name)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("source_format", &self.source_format)
            .field("metadata_stripped", &self.metadata_stripped)
            .finish()
    }
}

pub fn normalize_category(category: Value) -> Value {
    match category {
        Value::String(id) => id
            .parse::<u64>()
            .map(Value::from)
            .unwrap_or(Value::String(id)),
        category => category,
    }
}

pub fn prepare_image(path: &Path) -> Result<PreparedImage, ApiError> {
    let metadata = path.metadata().map_err(|_| {
        ApiError::new(
            "draft.image_read_failed",
            "Image file does not exist or cannot be read",
        )
    })?;
    if !metadata.is_file() {
        return Err(ApiError::new(
            "draft.image_read_failed",
            "Image path must identify a regular file",
        ));
    }
    if metadata.len() > MAX_IMAGE_INPUT_BYTES {
        return Err(ApiError::new(
            "draft.invalid_image",
            "Image file exceeds the 25 MiB local processing limit",
        ));
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or_default());
    File::open(path)
        .and_then(|file| file.take(MAX_IMAGE_INPUT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| {
            ApiError::new(
                "draft.image_read_failed",
                "Image file does not exist or cannot be read",
            )
        })?;
    if bytes.len() as u64 > MAX_IMAGE_INPUT_BYTES {
        return Err(ApiError::new(
            "draft.invalid_image",
            "Image file exceeds the 25 MiB local processing limit",
        ));
    }

    if is_heif(&bytes) {
        let converted = convert_heif(path)?;
        return sanitize_raster(&converted, image::ImageFormat::Jpeg, "heic");
    }
    let format = image::guess_format(&bytes).map_err(|_| {
        ApiError::new(
            "draft.invalid_image",
            "Image must be JPEG, PNG, HEIC, or HEIF",
        )
    })?;
    match format {
        image::ImageFormat::Jpeg => sanitize_raster(&bytes, format, "jpeg"),
        image::ImageFormat::Png => sanitize_raster(&bytes, format, "png"),
        _ => Err(ApiError::new(
            "draft.invalid_image",
            "Image must be JPEG, PNG, HEIC, or HEIF",
        )),
    }
}

fn sanitize_image(bytes: &[u8]) -> Result<PreparedImage, ApiError> {
    let format = image::guess_format(bytes)
        .map_err(|_| ApiError::new("draft.invalid_image", "Image must be JPEG or PNG"))?;
    match format {
        image::ImageFormat::Jpeg => sanitize_raster(bytes, format, "jpeg"),
        image::ImageFormat::Png => sanitize_raster(bytes, format, "png"),
        _ => Err(ApiError::new(
            "draft.invalid_image",
            "Image must be JPEG or PNG",
        )),
    }
}

fn sanitize_raster(
    bytes: &[u8],
    format: image::ImageFormat,
    source_format: &'static str,
) -> Result<PreparedImage, ApiError> {
    let dimensions = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_dimensions()
        .map_err(|_| ApiError::new("draft.invalid_image", "Image data is invalid"))?;
    validate_image_dimensions(dimensions.0, dimensions.1)?;
    let decoded = image::load_from_memory_with_format(bytes, format)
        .map_err(|_| ApiError::new("draft.invalid_image", "Image data is invalid"))?;
    let (output_format, file_name) = if format == image::ImageFormat::Png {
        (image::ImageFormat::Png, "image.png")
    } else {
        (image::ImageFormat::Jpeg, "image.jpg")
    };
    let mut output = Cursor::new(Vec::new());
    decoded
        .write_to(&mut output, output_format)
        .map_err(|_| ApiError::new("draft.invalid_image", "Image conversion failed"))?;
    Ok(PreparedImage {
        bytes: output.into_inner(),
        file_name: file_name.to_owned(),
        width: dimensions.0,
        height: dimensions.1,
        source_format,
        metadata_stripped: true,
    })
}

fn validate_image_dimensions(width: u32, height: u32) -> Result<(), ApiError> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > MAX_IMAGE_DIMENSION
        || height > MAX_IMAGE_DIMENSION
        || pixels > MAX_IMAGE_PIXELS
    {
        return Err(ApiError::new(
            "draft.invalid_image",
            "Image dimensions exceed the local 12000 pixel or 40 megapixel limit",
        ));
    }
    Ok(())
}

fn is_heif(bytes: &[u8]) -> bool {
    if bytes.len() < 16 || &bytes[4..8] != b"ftyp" {
        return false;
    }
    bytes[8..bytes.len().min(64)].chunks_exact(4).any(|brand| {
        matches!(
            brand,
            b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1"
        )
    })
}

fn convert_heif(path: &Path) -> Result<Vec<u8>, ApiError> {
    convert_heif_in(path, None)
}

fn convert_heif_in(path: &Path, temporary_parent: Option<&Path>) -> Result<Vec<u8>, ApiError> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("flea-image-");
    let temporary = match temporary_parent {
        Some(parent) => builder.tempdir_in(parent),
        None => builder.tempdir(),
    }
    .map_err(|_| image_processing_unavailable())?;
    let output_path = temporary.path().join("converted.jpg");
    let mut failures = Vec::new();

    #[cfg(target_os = "macos")]
    {
        match run_converter(
            OsStr::new("sips"),
            [
                OsStr::new("-s"),
                OsStr::new("format"),
                OsStr::new("jpeg"),
                path.as_os_str(),
                OsStr::new("--out"),
                output_path.as_os_str(),
            ],
            temporary.path(),
        ) {
            Ok(()) => return read_converted_image(&output_path),
            Err(error) => failures.push(error),
        }
    }

    match run_converter(
        OsStr::new("heif-convert"),
        [path.as_os_str(), output_path.as_os_str()],
        temporary.path(),
    ) {
        Ok(()) => read_converted_image(&output_path),
        Err(error) => {
            failures.push(error);
            if failures
                .iter()
                .all(|failure| failure.kind() == std::io::ErrorKind::NotFound)
            {
                Err(image_processing_unavailable())
            } else {
                Err(ApiError::new(
                    "draft.invalid_image",
                    "HEIC or HEIF image conversion failed",
                ))
            }
        }
    }
}

fn run_converter<'a>(
    program: &OsStr,
    arguments: impl IntoIterator<Item = &'a OsStr>,
    temporary_path: &Path,
) -> Result<(), std::io::Error> {
    let status = Command::new(program)
        .args(arguments)
        .env("MAGICK_TEMPORARY_PATH", temporary_path)
        .env("TMPDIR", temporary_path)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other("image converter failed"))
    }
}

fn read_converted_image(path: &Path) -> Result<Vec<u8>, ApiError> {
    let metadata = path
        .metadata()
        .map_err(|_| ApiError::new("draft.invalid_image", "HEIC or HEIF conversion failed"))?;
    if metadata.len() > MAX_IMAGE_INPUT_BYTES {
        return Err(ApiError::new(
            "draft.invalid_image",
            "Converted image exceeds the 25 MiB local processing limit",
        ));
    }
    std::fs::read(path)
        .map_err(|_| ApiError::new("draft.invalid_image", "HEIC or HEIF conversion failed"))
}

fn image_processing_unavailable() -> ApiError {
    ApiError::new(
        "draft.heif_decoder_unavailable",
        "HEIC and HEIF preview requires macOS ImageIO or the `heif-convert` command",
    )
}

fn uploaded_from_draft_image(image: &DraftImage) -> UploadedImage {
    UploadedImage {
        image_id: image.image_id.clone(),
        state: image.state.clone(),
        url: image.url.clone().or_else(|| Some(image.image_id.clone())),
        width: image.width,
        height: image.height,
        mime_type: image.mime_type.clone(),
    }
}

fn missing_required_fields(state: &DraftState) -> Vec<String> {
    state
        .required_fields
        .iter()
        .filter(|key| key.as_str() != "delivery")
        .filter(|key| {
            state
                .values
                .get(*key)
                .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty))
        })
        .cloned()
        .collect()
}

fn delivery_values(delivery: &Value) -> Option<Vec<String>> {
    match delivery {
        Value::String(value) if !value.trim().is_empty() => Some(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
            })
            .collect(),
        _ => None,
    }
}

pub fn ordered_image_states(images: &[DraftImage]) -> BTreeMap<usize, (&str, &ImageState)> {
    images
        .iter()
        .map(|image| (image.position, (image.image_id.as_str(), &image.state)))
        .collect()
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn recognizes_heic_and_heif_file_type_brands() {
        for brand in [b"heic", b"heix", b"mif1"] {
            let mut bytes = b"\0\0\0\x18ftyp".to_vec();
            bytes.extend_from_slice(brand);
            bytes.extend_from_slice(b"\0\0\0\0");
            assert!(is_heif(&bytes));
        }
        assert!(!is_heif(b"not an ISO media file"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn heic_preview_converts_strips_metadata_and_cleans_artifacts() {
        let source = tempfile::tempdir().unwrap();
        let png_path = source.path().join("source.png");
        let heic_path = source.path().join("source.heic");
        image::DynamicImage::new_rgb8(4, 6).save(&png_path).unwrap();
        let status = Command::new("sips")
            .args([OsStr::new("-s"), OsStr::new("format"), OsStr::new("heic")])
            .arg(&png_path)
            .arg(OsStr::new("--out"))
            .arg(&heic_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "macOS ImageIO must encode the HEIC fixture"
        );
        let original = std::fs::read(&heic_path).unwrap();
        let artifacts = tempfile::tempdir().unwrap();

        let converted = convert_heif_in(&heic_path, Some(artifacts.path())).unwrap();
        let prepared = sanitize_raster(&converted, image::ImageFormat::Jpeg, "heic").unwrap();

        assert_eq!(prepared.width(), 4);
        assert_eq!(prepared.height(), 6);
        assert_eq!(prepared.output_format(), "jpeg");
        assert!(prepared.metadata_stripped());
        assert!(!prepared.bytes.windows(4).any(|window| window == b"Exif"));
        assert_eq!(std::fs::read(&heic_path).unwrap(), original);
        assert_eq!(artifacts.path().read_dir().unwrap().count(), 0);
    }
}
