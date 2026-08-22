use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    path::Path,
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
    domain::field::{
        Field, FieldStatus, FieldType, Requirement, UpstreamValidationError, ValidationIssue,
        map_validation_errors, stable_field_key,
    },
    image_processing::{self, ImageProcessingReport, ProcessedImage, ProcessingError},
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
            "body_is_unparseable": response.body_is_unparseable,
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
    } else if path == "/categories/taxonomy" {
        compatibility::SERVICE_ITEM_CREATION
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
    pub value: Value,
    pub label: String,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DraftModel {
    pub fields: Vec<Field>,
    pub options: Vec<FieldOption>,
    pub required_fields: Vec<String>,
    pub values: Map<String, Value>,
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
    pub option_count: usize,
    #[serde(default)]
    pub options_returned: usize,
    #[serde(default)]
    pub options_truncated: bool,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ComposerModelStatus {
    #[default]
    Available,
    Unavailable,
    Malformed,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PublicationDraftState {
    pub draft: DraftState,
    pub composer_model: ComposerModelStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicationCategory {
    pub category_id: String,
    pub selectable: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct PublicationRequirement {
    pub field: String,
    pub reason: String,
    pub source: String,
    pub command: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PublicationValidation {
    pub draft_id: String,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<PublicationRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub invalid: Vec<PublicationRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<PublicationRequirement>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unverifiable: Vec<PublicationRequirement>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DraftState {
    pub draft_id: String,
    #[serde(default)]
    pub etag: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
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

    fn merge_model(&mut self, model: DraftModel) -> Result<(), ApiError> {
        let mut field_names = self
            .fields
            .iter()
            .map(|field| field.key.clone())
            .collect::<BTreeSet<_>>();
        for field in model.fields {
            if !field_names.insert(field.key.clone()) {
                return Err(model_error(
                    "merge_models",
                    &field.key,
                    "multiple authoritative models defined the same field",
                ));
            }
            self.fields.push(field);
        }
        self.options.extend(model.options);
        for field in model.required_fields {
            if !self.required_fields.contains(&field) {
                self.required_fields.push(field);
            }
        }
        self.values.extend(model.values);
        Ok(())
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
    async fn publication_draft(&self, draft_id: &str) -> Result<PublicationDraftState, ApiError> {
        self.get_draft(draft_id)
            .await
            .map(|draft| PublicationDraftState {
                draft,
                composer_model: ComposerModelStatus::Available,
            })
    }
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
    async fn publication_categories(&self) -> Result<Vec<PublicationCategory>, ApiError>;
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

    async fn draft_request(
        &self,
        request: HttpRequest,
        require_authoritative_model: bool,
    ) -> Result<DraftState, ApiError> {
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
        let normalized = if require_authoritative_model {
            normalize_authoritative_draft_state(response.body, response.etag.as_deref())
        } else {
            normalize_draft_state(response.body, response.etag.as_deref())
        };
        normalized.map_err(|mut error| {
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
            return normalize_authoritative_draft_state(response.body, response.etag.as_deref());
        }
        self.observe_created_draft(&draft_id, &["create_draft", "establish_identity"])
            .await
    }

    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.draft_request(
            HttpRequest::read(format!("/adinput/ad/withModel/{draft_id}")),
            true,
        )
        .await
    }

    async fn publication_draft(&self, draft_id: &str) -> Result<PublicationDraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/adinput/ad/withModel/{draft_id}"
            )))
            .await?;
        if response.body_is_unparseable {
            return Err(malformed_read_response("publication_draft"));
        }
        normalize_publication_draft(response.body, response.etag.as_deref())
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
        self.draft_request(request, false).await
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
        self.draft_request(request, false).await
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
        self.draft_request(request, false).await
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

    async fn publication_categories(&self) -> Result<Vec<PublicationCategory>, ApiError> {
        let response = self.json(HttpRequest::read("/categories/taxonomy")).await?;
        normalize_publication_categories(&response.body)
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
        normalize_delivery_composer(response.body, draft_id)
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

fn normalize_delivery_composer(
    source: Value,
    draft_id: &str,
) -> Result<DeliveryComposer, ApiError> {
    let root = source.as_object().ok_or_else(|| {
        model_error(
            "delivery_composer",
            "$",
            "delivery composer must be an object",
        )
    })?;
    let context = root
        .get("context")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.context",
                "delivery context is unavailable or unrecognized",
            )
        })?;
    let observed_id = context.get("adId").and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    });
    if observed_id.as_deref() != Some(draft_id) {
        return Err(model_error(
            "delivery_composer",
            "$.context.adId",
            "delivery composer identifies a different draft",
        ));
    }
    let meetup_selected = context
        .get("meetup")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.context.meetup",
                "meetup selection state is unavailable or unrecognized",
            )
        })?;
    let shipping_selected = context
        .get("shipping")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.context.shipping",
                "shipping selection state is unavailable or unrecognized",
            )
        })?;
    let sections = root
        .get("sections")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.sections",
                "delivery sections are unavailable or unrecognized",
            )
        })?;
    if let Some(title) = sections
        .get("head")
        .and_then(Value::as_object)
        .and_then(|head| head.get("title"))
        && title
            .as_str()
            .is_none_or(|title| !safe_display_string(title))
    {
        return Err(model_error(
            "delivery_composer",
            "$.sections.head.title",
            "delivery field label is unavailable or unsafe",
        ));
    }
    let delivery_options = sections
        .get("deliveryOptions")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.sections.deliveryOptions",
                "delivery options are unavailable or unrecognized",
            )
        })?;

    let mut options = Vec::new();
    if let Some(meetup) = delivery_options.get("meetup") {
        let label = meetup
            .as_object()
            .and_then(|meetup| meetup.get("title"))
            .and_then(Value::as_str)
            .filter(|label| safe_display_string(label))
            .ok_or_else(|| {
                model_error(
                    "delivery_composer",
                    "$.sections.deliveryOptions.meetup.title",
                    "pickup option label is unavailable or unsafe",
                )
            })?;
        options.push(DeliveryOption {
            value: "pickup".to_owned(),
            label: label.to_owned(),
            mode: "pickup".to_owned(),
            package_size: None,
        });
    }
    let mut shipping_options_unavailable = false;
    if let Some(shipping) = delivery_options.get("shipping") {
        shipping
            .as_object()
            .and_then(|shipping| shipping.get("title"))
            .and_then(Value::as_str)
            .filter(|label| safe_display_string(label))
            .ok_or_else(|| {
                model_error(
                    "delivery_composer",
                    "$.sections.deliveryOptions.shipping.title",
                    "shipping option label is unavailable or unsafe",
                )
            })?;
        let mut shipping_options = Vec::new();
        if let Some(package_sizes) = sections
            .get("shipping")
            .and_then(Value::as_object)
            .and_then(|shipping| shipping.get("packageSizes"))
        {
            collect_delivery_package_options(
                package_sizes,
                "$.sections.shipping.packageSizes",
                0,
                &mut shipping_options,
            )?;
        }
        shipping_options.sort_by(|left, right| {
            let rank = |option: &DeliveryOption| match option.package_size.as_deref() {
                Some("SMALL") => 0,
                Some("MEDIUM") => 1,
                Some("LARGE") => 2,
                _ => 3,
            };
            rank(left)
                .cmp(&rank(right))
                .then_with(|| left.value.cmp(&right.value))
        });
        shipping_options_unavailable = shipping_options.is_empty();
        options.extend(shipping_options);
    }

    let mut machine_values = BTreeSet::new();
    for option in &options {
        if !machine_values.insert(option.value.clone()) {
            return Err(model_error(
                "delivery_composer",
                "$.sections",
                "delivery composer contains duplicate machine values",
            ));
        }
    }
    let mut selected = Vec::new();
    if meetup_selected {
        if !machine_values.contains("pickup") {
            return Err(model_error(
                "delivery_composer",
                "$.context.meetup",
                "selected pickup is absent from delivery options",
            ));
        }
        selected.push("pickup".to_owned());
    }
    if shipping_selected {
        let package_size = context
            .get("packageSize")
            .and_then(Value::as_str)
            .filter(|size| safe_machine_identifier(size))
            .ok_or_else(|| {
                model_error(
                    "delivery_composer",
                    "$.context.packageSize",
                    "selected shipping package is unavailable or unsafe",
                )
            })?;
        let value = format!("shipping:{}", package_size.to_ascii_lowercase());
        if !machine_values.contains(&value) {
            return Err(model_error(
                "delivery_composer",
                "$.context.packageSize",
                "selected shipping package is absent from delivery options",
            ));
        }
        selected.push(value);
    }

    let option_count = options.len();
    if option_count > MAX_OPTIONS_PER_FIELD {
        let selected_options = options
            .iter()
            .filter(|option| selected.contains(&option.value))
            .cloned()
            .collect::<Vec<_>>();
        let unselected_limit = MAX_OPTIONS_PER_FIELD.saturating_sub(selected_options.len());
        options = options
            .into_iter()
            .filter(|option| !selected.contains(&option.value))
            .take(unselected_limit)
            .chain(selected_options)
            .collect();
    }
    let options_returned = options.len();
    let available = option_count > 0;
    Ok(DeliveryComposer {
        state: DraftDelivery {
            source: "remote_delivery_composer".to_owned(),
            available,
            options,
            option_count,
            options_returned,
            options_truncated: option_count > options_returned,
            selected,
            unavailable_reason: if shipping_options_unavailable {
                Some("Shipping is offered, but package machine values are unavailable".to_owned())
            } else if !available {
                Some("Tori returned no delivery options for this draft".to_owned())
            } else {
                None
            },
        },
        source,
    })
}

fn collect_delivery_package_options(
    value: &Value,
    path: &str,
    depth: usize,
    options: &mut Vec<DeliveryOption>,
) -> Result<(), ApiError> {
    if depth > 8 {
        return Err(model_error(
            "delivery_composer",
            path,
            "shipping package option nesting exceeds the supported limit",
        ));
    }
    match value {
        Value::Object(package) if package.contains_key("size") => {
            let package_size = package
                .get("size")
                .and_then(Value::as_str)
                .filter(|size| safe_machine_identifier(size))
                .ok_or_else(|| {
                    model_error(
                        "delivery_composer",
                        path,
                        "package machine value is unavailable or unsafe",
                    )
                })?;
            let label = package
                .get("title")
                .and_then(Value::as_str)
                .filter(|label| safe_display_string(label))
                .ok_or_else(|| {
                    model_error(
                        "delivery_composer",
                        path,
                        "package option label is unavailable or unsafe",
                    )
                })?;
            options.push(DeliveryOption {
                value: format!("shipping:{}", package_size.to_ascii_lowercase()),
                label: label.to_owned(),
                mode: "shipping".to_owned(),
                package_size: Some(package_size.to_owned()),
            });
        }
        Value::Object(object) => {
            for (index, child) in object.values().enumerate() {
                collect_delivery_package_options(
                    child,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    options,
                )?;
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_delivery_package_options(
                    child,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    options,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
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

const MAX_OPTIONS_PER_FIELD: usize = 50;

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
    if let Ok(mut normalized) = serde_json::from_value::<DraftState>(body.clone()) {
        if let Some(etag) = response_etag {
            normalized.etag = etag.to_owned();
        }
        normalized.values = normalize_draft_values(normalized.values)?;
        if normalized.revision.is_none() {
            normalized.revision = normalized.values.get("revision").and_then(revision_value);
        }
        return Ok(normalized);
    }

    normalize_source_draft_state(body, response_etag)
}

fn normalize_authoritative_draft_state(
    body: Value,
    response_etag: Option<&str>,
) -> Result<DraftState, ApiError> {
    if body.get("model").is_some() {
        return normalize_source_draft_state(body, response_etag);
    }
    let root = body.as_object().ok_or_else(|| {
        model_error(
            "listing_composer",
            "$",
            "authoritative draft state must be an object",
        )
    })?;
    for key in ["fields", "options", "required_fields"] {
        if root.get(key).and_then(Value::as_array).is_none() {
            return Err(model_error(
                "listing_composer",
                &format!("$.{key}"),
                "authoritative normalized model data is unavailable or unrecognized",
            ));
        }
    }
    normalize_draft_state(body, response_etag)
}

fn normalize_publication_draft(
    body: Value,
    response_etag: Option<&str>,
) -> Result<PublicationDraftState, ApiError> {
    let composer_model = publication_composer_status(&body);
    match normalize_authoritative_draft_state(body.clone(), response_etag) {
        Ok(draft) => Ok(PublicationDraftState {
            draft,
            composer_model,
        }),
        Err(error)
            if composer_model != ComposerModelStatus::Available
                || error
                    .details
                    .as_deref()
                    .and_then(|details| details.get("path"))
                    .and_then(Value::as_str)
                    .is_some_and(|path| path.starts_with("$.model")) =>
        {
            publication_draft_without_model(body, response_etag).map(|draft| {
                PublicationDraftState {
                    draft,
                    composer_model: if composer_model == ComposerModelStatus::Available {
                        ComposerModelStatus::Malformed
                    } else {
                        composer_model
                    },
                }
            })
        }
        Err(error) => Err(error),
    }
}

fn publication_composer_status(body: &Value) -> ComposerModelStatus {
    if body.get("draft_id").is_some()
        && ["fields", "options", "required_fields"]
            .into_iter()
            .all(|key| body.get(key).and_then(Value::as_array).is_some())
    {
        return ComposerModelStatus::Available;
    }
    let Some(model) = body.get("model").filter(|model| !model.is_null()) else {
        return ComposerModelStatus::Unavailable;
    };
    let Some(model) = model.as_object() else {
        return ComposerModelStatus::Malformed;
    };
    match model.get("sections") {
        Some(Value::Array(sections)) if !sections.is_empty() => ComposerModelStatus::Available,
        Some(Value::Array(_)) | None | Some(Value::Null) => ComposerModelStatus::Unavailable,
        Some(_) => ComposerModelStatus::Malformed,
    }
}

fn publication_draft_without_model(
    body: Value,
    response_etag: Option<&str>,
) -> Result<DraftState, ApiError> {
    if body.get("draft_id").is_some() {
        return normalize_draft_state(body, response_etag);
    }
    let draft_id = draft_id_from_body(&body).ok_or_else(|| {
        model_error(
            "publication_validation",
            "$",
            "draft response did not contain an authoritative identity",
        )
    })?;
    let ad = body
        .get("ad")
        .and_then(Value::as_object)
        .ok_or_else(|| model_error("publication_validation", "$.ad", "ad data is unavailable"))?;
    let values = normalize_draft_values(
        ad.get("values")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| {
                model_error(
                    "publication_validation",
                    "$.ad.values",
                    "draft values are unavailable or unrecognized",
                )
            })?,
    )?;
    let etag = response_etag
        .or_else(|| ad.get("etag").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    let revision = extract_revision(ad, &values, &etag).ok();
    let images = normalize_draft_images(&values)?;
    Ok(DraftState {
        draft_id,
        etag,
        revision,
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

fn normalize_source_draft_state(
    body: Value,
    response_etag: Option<&str>,
) -> Result<DraftState, ApiError> {
    let draft_id = draft_id_from_body(&body).ok_or_else(|| {
        model_error(
            "listing_composer",
            "$",
            "draft response did not contain an authoritative identity",
        )
    })?;
    let ad = body
        .get("ad")
        .and_then(Value::as_object)
        .ok_or_else(|| model_error("listing_composer", "$.ad", "ad data is unavailable"))?;
    let values = normalize_draft_values(
        ad.get("values")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    "$.ad.values",
                    "draft values are unavailable or unrecognized",
                )
            })?,
    )?;
    let etag = response_etag
        .or_else(|| ad.get("etag").and_then(Value::as_str))
        .filter(|etag| !etag.is_empty())
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.ad.etag",
                "draft revision metadata is unavailable",
            )
        })?
        .to_owned();
    let revision = extract_revision(ad, &values, &etag)?;
    let model = body
        .get("model")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.model",
                "listing composer model is unavailable or unrecognized",
            )
        })?;
    let normalized_model = normalize_listing_model(model, &values)?;
    let images = normalize_draft_images(&values)?;
    let DraftModel {
        fields,
        options,
        required_fields,
        values: normalized_values,
    } = normalized_model;
    let mut values = values;
    values.extend(normalized_values);

    Ok(DraftState {
        draft_id,
        etag,
        revision: Some(revision),
        values,
        fields,
        options,
        required_fields,
        images,
        cleared_fields: Vec::new(),
        predictions: Vec::new(),
        delivery: None,
    })
}

fn normalize_listing_model(
    model: &Map<String, Value>,
    values: &Map<String, Value>,
) -> Result<DraftModel, ApiError> {
    let sections = model
        .get("sections")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.model.sections",
                "composer sections are unavailable or unrecognized",
            )
        })?;
    let mut normalized = DraftModel::default();
    let mut field_names = BTreeSet::new();
    for (section_index, section) in sections.iter().enumerate() {
        let path = format!("$.model.sections[{section_index}]");
        let section = section
            .as_object()
            .ok_or_else(|| model_error("listing_composer", &path, "section must be an object"))?;
        let section_name = match section.get("type") {
            Some(Value::String(name)) if safe_machine_identifier(name) => name.clone(),
            Some(_) => {
                return Err(model_error(
                    "listing_composer",
                    &format!("{path}.type"),
                    "section type is unavailable or unsafe",
                ));
            }
            None => format!("section_{section_index}"),
        };
        let content = section
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.content"),
                    "section content is unavailable or unrecognized",
                )
            })?;
        for (widget_index, widget) in content.iter().enumerate() {
            normalize_widget(
                widget,
                &format!("{path}.content[{widget_index}]"),
                &section_name,
                values,
                &mut field_names,
                &mut normalized,
            )?;
        }
    }
    normalized.required_fields = normalized
        .fields
        .iter()
        .filter(|field| field.requirement == Requirement::Required)
        .map(|field| field.key.clone())
        .collect();
    Ok(normalized)
}

fn normalize_widget(
    widget: &Value,
    path: &str,
    section: &str,
    values: &Map<String, Value>,
    field_names: &mut BTreeSet<String>,
    normalized: &mut DraftModel,
) -> Result<(), ApiError> {
    let widget = widget
        .as_object()
        .ok_or_else(|| model_error("listing_composer", path, "widget must be an object"))?;
    let id = required_model_string(widget, "id", path)?;
    let upstream_type = required_model_string(widget, "type", path)?;
    if !widget_is_applicable(widget, values, path)? {
        return Ok(());
    }

    if upstream_type == "complex" {
        let children = widget
            .get("children")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.children"),
                    "complex widget children are unavailable or unrecognized",
                )
            })?;
        for (child_index, child) in children.iter().enumerate() {
            normalize_widget(
                child,
                &format!("{path}.children[{child_index}]"),
                section,
                values,
                field_names,
                normalized,
            )?;
        }
        return Ok(());
    }

    if matches!(
        upstream_type.as_str(),
        "multi-image"
            | "image"
            | "static"
            | "info-text"
            | "attention"
            | "context-attention"
            | "section-title"
            | "proceed"
    ) {
        return Ok(());
    }

    if !field_names.insert(id.clone()) {
        return Err(model_error(
            "listing_composer",
            path,
            "composer contains duplicate applicable field names",
        ));
    }
    let label = match widget.get("label") {
        Some(Value::String(label)) if safe_display_string(label) => label.clone(),
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.label"),
                "field label is unavailable or unsafe",
            ));
        }
        None => id.clone(),
    };
    let mandatory = has_mandatory_rule(widget, path)?;
    let requirement = match widget.get("required") {
        Some(Value::Bool(true)) => Requirement::Required,
        Some(Value::Bool(false)) if mandatory => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.required"),
                "required state conflicts with mandatory validation",
            ));
        }
        Some(Value::Bool(false)) => Requirement::Optional,
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.required"),
                "required state must be a boolean",
            ));
        }
        None if mandatory => Requirement::Required,
        None => Requirement::Unknown,
    };
    let field_type = normalize_field_type(widget, &upstream_type, path)?;
    let value = model_field_value(values, &id);
    let mut field = Field::new(
        id.clone(),
        label,
        field_type.clone(),
        requirement,
        value,
        section,
    );
    let option_result = normalize_widget_options(widget, &upstream_type, &id, path)?;
    field.option_count = option_result.total;
    field.options_returned = option_result.options.len();
    field.options_truncated = option_result.total > option_result.options.len();
    if matches!(field_type, FieldType::Unknown(_)) {
        field.raw = Some(json!({
            "type": upstream_type,
            "sub_type": widget.get("sub-type").and_then(Value::as_str),
            "has_children": widget.get("children").is_some(),
            "has_options": widget.get("items").is_some() || widget.get("options").is_some()
        }));
    }
    normalized.options.extend(option_result.options);
    normalized.fields.push(field);
    Ok(())
}

fn required_model_string(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<String, ApiError> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| safe_machine_identifier(value))
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.{key}"),
                "machine identifier is unavailable or unsafe",
            )
        })?;
    Ok(value.to_owned())
}

fn safe_machine_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn safe_display_string(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

fn widget_is_applicable(
    widget: &Map<String, Value>,
    values: &Map<String, Value>,
    path: &str,
) -> Result<bool, ApiError> {
    match widget.get("hidden") {
        Some(Value::Bool(true)) => return Ok(false),
        Some(Value::Bool(false)) | None => {}
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.hidden"),
                "hidden state must be a boolean",
            ));
        }
    }
    if let Some(dependencies) = widget.get("dependencies") {
        let dependencies = dependencies.as_array().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.dependencies"),
                "dependencies must be an array",
            )
        })?;
        for dependency in dependencies {
            let dependency = dependency.as_str().ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.dependencies"),
                    "dependency names must be strings",
                )
            })?;
            if model_field_value(values, dependency)
                .as_ref()
                .is_none_or(|value| !value_is_present(value))
            {
                return Ok(false);
            }
        }
    }
    let Some(exclusive) = widget.get("exclusive-dependencies") else {
        return Ok(true);
    };
    let exclusive = exclusive.as_object().ok_or_else(|| {
        model_error(
            "listing_composer",
            &format!("{path}.exclusive-dependencies"),
            "exclusive dependencies must be an object",
        )
    })?;
    for (dependency, allowed) in exclusive {
        let allowed = allowed.as_array().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.exclusive-dependencies.{dependency}"),
                "exclusive dependency values must be an array",
            )
        })?;
        let Some(selected) = model_field_value(values, dependency) else {
            return Ok(false);
        };
        if !allowed
            .iter()
            .any(|allowed| values_semantically_equal(&selected, allowed))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn values_semantically_equal(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Value::Array(values), right) | (right, Value::Array(values)) => values
            .iter()
            .any(|value| values_semantically_equal(value, right)),
        (Value::String(left), Value::Number(right)) => left == &right.to_string(),
        (Value::Number(left), Value::String(right)) => &left.to_string() == right,
        _ => false,
    }
}

fn has_mandatory_rule(widget: &Map<String, Value>, path: &str) -> Result<bool, ApiError> {
    let Some(rules) = widget.get("validation-rules") else {
        return Ok(false);
    };
    let rules = rules.as_array().ok_or_else(|| {
        model_error(
            "listing_composer",
            &format!("{path}.validation-rules"),
            "validation rules must be an array",
        )
    })?;
    for (index, rule) in rules.iter().enumerate() {
        let rule = rule.as_object().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.validation-rules[{index}]"),
                "validation rule must be an object",
            )
        })?;
        if rule.get("type").and_then(Value::as_str) == Some("MANDATORY") {
            return Ok(true);
        }
    }
    Ok(false)
}

fn normalize_field_type(
    widget: &Map<String, Value>,
    upstream_type: &str,
    path: &str,
) -> Result<FieldType, ApiError> {
    let subtype = match widget.get("sub-type") {
        Some(Value::String(subtype)) if safe_machine_identifier(subtype) => Some(subtype.as_str()),
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.sub-type"),
                "field subtype is unavailable or unsafe",
            ));
        }
        None => None,
    };
    Ok(match upstream_type {
        "simple" => match subtype {
            None | Some("string") => FieldType::String,
            Some("multiline") => FieldType::Text,
            Some("number" | "decimal") => FieldType::Decimal,
            Some("integer") => FieldType::Integer,
            Some("boolean") => FieldType::Boolean,
            Some(subtype) => FieldType::Unknown(format!("simple:{subtype}")),
        },
        "html" => FieldType::Text,
        "post-code" => FieldType::String,
        "checkbox" => FieldType::Boolean,
        "select" if widget.get("multiple").and_then(Value::as_bool) == Some(true) => {
            FieldType::MultiSelect
        }
        "select" | "tree-select" | "managed" => FieldType::Select,
        "multi-select" => FieldType::MultiSelect,
        "date" => FieldType::Date,
        value => FieldType::Unknown(value.to_owned()),
    })
}

struct NormalizedOptions {
    options: Vec<FieldOption>,
    total: usize,
}

fn normalize_widget_options(
    widget: &Map<String, Value>,
    upstream_type: &str,
    field: &str,
    path: &str,
) -> Result<NormalizedOptions, ApiError> {
    let mut result = NormalizedOptions {
        options: Vec::new(),
        total: 0,
    };
    match upstream_type {
        "select" | "multi-select" => {
            let items = widget
                .get("items")
                .or_else(|| widget.get("options"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    model_error(
                        "listing_composer",
                        &format!("{path}.items"),
                        "select options are unavailable or unrecognized",
                    )
                })?;
            for (index, item) in items.iter().enumerate() {
                normalize_flat_option(item, field, &format!("{path}.items[{index}]"), &mut result)?;
            }
        }
        "managed" => {
            let nodes = widget
                .get("value-nodes")
                .or_else(|| widget.get("valueNodes"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    model_error(
                        "listing_composer",
                        &format!("{path}.value-nodes"),
                        "managed options are unavailable or unrecognized",
                    )
                })?;
            for (index, node) in nodes.iter().enumerate() {
                normalize_option_node(
                    node,
                    field,
                    &format!("{path}.value-nodes[{index}]"),
                    &mut result,
                )?;
            }
        }
        "tree-select" => {
            let root = widget.get("value").ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.value"),
                    "tree options are unavailable",
                )
            })?;
            normalize_option_node(root, field, &format!("{path}.value"), &mut result)?;
        }
        _ => {}
    }
    Ok(result)
}

fn normalize_flat_option(
    option: &Value,
    field: &str,
    path: &str,
    result: &mut NormalizedOptions,
) -> Result<(), ApiError> {
    let option = option
        .as_object()
        .ok_or_else(|| model_error("listing_composer", path, "option must be an object"))?;
    let value = option
        .get("value")
        .filter(|value| is_machine_value(value))
        .cloned()
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.value"),
                "option machine value is unavailable or unsafe",
            )
        })?;
    let label = option
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| safe_display_string(label))
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.label"),
                "option label is unavailable",
            )
        })?;
    push_option(result, field, value, label);
    Ok(())
}

fn normalize_option_node(
    node: &Value,
    field: &str,
    path: &str,
    result: &mut NormalizedOptions,
) -> Result<(), ApiError> {
    let node = node
        .as_object()
        .ok_or_else(|| model_error("listing_composer", path, "option node must be an object"))?;
    let children = match node.get("children") {
        Some(children) => Some(children.as_array().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.children"),
                "option node children must be an array",
            )
        })?),
        None => None,
    };
    let persistable = node
        .get("persistable")
        .and_then(Value::as_bool)
        .unwrap_or(children.is_none_or(|children| children.is_empty()));
    if persistable {
        let value = node
            .get("id")
            .or_else(|| node.get("value"))
            .filter(|value| is_machine_value(value))
            .cloned()
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.id"),
                    "option node machine value is unavailable or unsafe",
                )
            })?;
        let label = node
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| safe_display_string(label))
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.label"),
                    "option node label is unavailable",
                )
            })?;
        push_option(result, field, value, label);
    }
    if let Some(children) = children {
        for (index, child) in children.iter().enumerate() {
            normalize_option_node(child, field, &format!("{path}.children[{index}]"), result)?;
        }
    }
    Ok(())
}

fn push_option(result: &mut NormalizedOptions, field: &str, value: Value, label: &str) {
    result.total += 1;
    if result.options.len() < MAX_OPTIONS_PER_FIELD {
        result.options.push(FieldOption {
            field: field.to_owned(),
            value,
            label: label.to_owned(),
        });
    }
}

fn is_machine_value(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        }
        Value::Number(_) | Value::Bool(_) => true,
        _ => false,
    }
}

fn model_field_value(values: &Map<String, Value>, field: &str) -> Option<Value> {
    if let Some(value) = values.get(field) {
        return Some(value.clone());
    }
    match field {
        "price_amount" => values.get("price").cloned(),
        "price_max" => values.get("max_price").cloned(),
        "postal-code" => values
            .get("location")
            .and_then(Value::as_array)
            .and_then(|locations| locations.first())
            .and_then(Value::as_object)
            .and_then(|location| {
                location
                    .get("postal-code")
                    .or_else(|| location.get("postal_code"))
            })
            .cloned(),
        _ => None,
    }
}

fn value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => !values.is_empty() && values.iter().all(value_is_present),
        Value::Object(values) => !values.is_empty() && values.values().all(value_is_present),
        _ => true,
    }
}

fn normalize_draft_images(values: &Map<String, Value>) -> Result<Vec<DraftImage>, ApiError> {
    let Some(images) = values.get("multi_image") else {
        return Ok(Vec::new());
    };
    let images = images.as_array().ok_or_else(|| {
        model_error(
            "listing_composer",
            "$.ad.values.multi_image",
            "draft images must be an array",
        )
    })?;
    images
        .iter()
        .enumerate()
        .map(|(position, value)| {
            let path = format!("$.ad.values.multi_image[{position}]");
            let object = value.as_object().ok_or_else(|| {
                model_error("listing_composer", &path, "draft image must be an object")
            })?;
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .and_then(valid_image_location)
                .ok_or_else(|| {
                    model_error(
                        "listing_composer",
                        &format!("{path}.url"),
                        "draft image URL is unavailable or unsafe",
                    )
                })?;
            Ok(DraftImage {
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
        .collect()
}

fn extract_revision(
    ad: &Map<String, Value>,
    values: &Map<String, Value>,
    etag: &str,
) -> Result<String, ApiError> {
    let mut revisions = Vec::new();
    for key in ["checkout-url", "product-context-url"] {
        let Some(url) = ad.get(key) else {
            continue;
        };
        let url = url.as_str().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("$.ad.{key}"),
                "revision URL must be a string",
            )
        })?;
        let revision = revision_from_url(url).ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("$.ad.{key}"),
                "revision URL did not contain a safe revision",
            )
        })?;
        revisions.push(revision);
    }
    if let Some(ad_etag) = ad.get("etag") {
        let ad_etag = ad_etag.as_str().ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.ad.etag",
                "draft ETag must be a string",
            )
        })?;
        let revision = revision_from_etag(ad_etag).ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.ad.etag",
                "draft ETag did not contain a safe revision",
            )
        })?;
        revisions.push(revision);
    }
    if let Some(revision) = values.get("revision").and_then(revision_value) {
        revisions.push(revision);
    }
    let response_revision = revision_from_etag(etag).ok_or_else(|| {
        model_error(
            "listing_composer",
            "$.headers.etag",
            "response ETag did not contain a safe revision",
        )
    })?;
    revisions.push(response_revision);
    revisions.sort();
    revisions.dedup();
    match revisions.as_slice() {
        [revision] => Ok(revision.clone()),
        [] => Err(model_error(
            "listing_composer",
            "$.ad",
            "draft revision is unavailable",
        )),
        _ => Err(model_error(
            "listing_composer",
            "$.ad",
            "draft revision sources disagree",
        )),
    }
}

fn revision_from_url(value: &str) -> Option<String> {
    let query = value
        .split_once('?')?
        .1
        .split('#')
        .next()
        .unwrap_or_default();
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "adRevision")
        .map(|(_, value)| value.into_owned())
        .filter(|value| safe_revision(value))
}

fn revision_from_etag(etag: &str) -> Option<String> {
    let revision = etag
        .strip_prefix("W/\"")
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            etag.strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(etag);
    safe_revision(revision).then(|| revision.to_owned())
}

fn safe_revision(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn revision_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if safe_revision(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn delivery_draft_model(composer: &DeliveryComposer) -> DraftModel {
    let label = composer
        .source
        .pointer("/sections/head/title")
        .and_then(Value::as_str)
        .unwrap_or("Delivery");
    let selected = Value::Array(
        composer
            .state
            .selected
            .iter()
            .cloned()
            .map(Value::String)
            .collect(),
    );
    let options = composer
        .state
        .options
        .iter()
        .map(|option| FieldOption {
            field: "delivery".to_owned(),
            value: Value::String(option.value.clone()),
            label: option.label.clone(),
        })
        .collect::<Vec<_>>();
    let mut field = Field::new(
        "delivery",
        label,
        FieldType::MultiSelect,
        Requirement::Required,
        Some(selected.clone()),
        "delivery",
    );
    field.option_count = composer.state.option_count;
    field.options_returned = composer.state.options_returned;
    field.options_truncated = composer.state.options_truncated;
    let mut values = Map::new();
    values.insert("delivery".to_owned(), selected);
    DraftModel {
        fields: vec![field],
        options,
        required_fields: vec!["delivery".to_owned()],
        values,
    }
}

fn attach_delivery_model(
    state: &mut DraftState,
    composer: &DeliveryComposer,
) -> Result<(), ApiError> {
    state.merge_model(delivery_draft_model(composer))?;
    state.delivery = Some(composer.state.clone());
    Ok(())
}

fn normalize_publication_categories(body: &Value) -> Result<Vec<PublicationCategory>, ApiError> {
    let roots = body
        .get("categories")
        .and_then(Value::as_array)
        .ok_or_else(category_model_error)?;
    let mut categories = Vec::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        normalize_publication_category(root, &mut seen, &mut categories)?;
    }
    if categories.is_empty() {
        return Err(category_model_error());
    }
    categories.sort_by(|left, right| left.category_id.cmp(&right.category_id));
    Ok(categories)
}

fn normalize_publication_category(
    category: &Value,
    seen: &mut BTreeSet<String>,
    output: &mut Vec<PublicationCategory>,
) -> Result<(), ApiError> {
    let category = category.as_object().ok_or_else(category_model_error)?;
    let category_id = category
        .get("id")
        .or_else(|| category.get("category_id"))
        .and_then(publication_scalar_string)
        .ok_or_else(category_model_error)?;
    if !seen.insert(category_id.clone()) {
        return Err(category_model_error());
    }
    let children: &[Value] = match category.get("children") {
        Some(children) => children.as_array().ok_or_else(category_model_error)?,
        None => &[],
    };
    let selectable = match category
        .get("selectable")
        .or_else(|| category.get("isSelectable"))
    {
        Some(Value::Bool(selectable)) => *selectable,
        Some(_) => return Err(category_model_error()),
        None => children.is_empty(),
    };
    output.push(PublicationCategory {
        category_id,
        selectable,
    });
    for child in children {
        normalize_publication_category(child, seen, output)?;
    }
    Ok(())
}

fn category_model_error() -> ApiError {
    malformed_read_response("publication_category_taxonomy")
}

fn model_error(stage: &str, path: &str, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unrecognized_model",
        "Tori returned an unavailable or unrecognized draft model",
    );
    error.details = Some(Box::new(json!({
        "stage": stage,
        "path": path,
        "reason": reason,
    })));
    error
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_step: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persisted_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub absent_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indeterminate_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unattempted_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub manual_inspection_required: bool,
    pub upstream_transient: bool,
    pub safe_to_retry: bool,
    pub next_safe_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_state: Option<DraftState>,
}

fn is_false(value: &bool) -> bool {
    !value
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
                active_step: None,
                fields: Vec::new(),
                persisted_fields: Vec::new(),
                absent_fields: Vec::new(),
                indeterminate_fields: Vec::new(),
                unattempted_fields: Vec::new(),
                manual_inspection_required: false,
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
                active_step: None,
                fields: Vec::new(),
                persisted_fields: Vec::new(),
                absent_fields: Vec::new(),
                indeterminate_fields: Vec::new(),
                unattempted_fields: Vec::new(),
                manual_inspection_required: false,
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

    fn with_optional_listing_id(self, listing_id: Option<&str>) -> Self {
        match listing_id {
            Some(listing_id) => self.with_listing_id(listing_id),
            None => self,
        }
    }

    fn validation(completed_steps: &[String], report: PublicationValidation) -> Self {
        let repeatable = !report.pending.is_empty() || !report.unverifiable.is_empty();
        let next_safe_actions = report
            .missing
            .iter()
            .chain(&report.invalid)
            .chain(&report.pending)
            .chain(&report.unverifiable)
            .map(|requirement| requirement.command.clone())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let details = serde_json::to_value(&report).ok();
        Self {
            code: "draft.validation_failed".to_owned(),
            message: "The draft is not ready for publication".to_owned(),
            source: None,
            recovery: Some(Recovery {
                draft_id: report.draft_id,
                listing_id: None,
                completed_steps: completed_steps.to_vec(),
                active_step: None,
                fields: Vec::new(),
                persisted_fields: Vec::new(),
                absent_fields: Vec::new(),
                indeterminate_fields: Vec::new(),
                unattempted_fields: Vec::new(),
                manual_inspection_required: false,
                upstream_transient: repeatable,
                safe_to_retry: repeatable,
                next_safe_actions,
                fresh_state: None,
            }),
            details,
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
                active_step: Some("validate_delivery".to_owned()),
                fields: vec!["delivery".to_owned()],
                persisted_fields: Vec::new(),
                absent_fields: vec!["delivery".to_owned()],
                indeterminate_fields: Vec::new(),
                unattempted_fields: Vec::new(),
                manual_inspection_required: false,
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
            || step.starts_with("apply_")
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_processing: Vec<ImageProcessingReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AddImagesResult {
    #[serde(flatten)]
    pub draft: DraftState,
    pub image_processing: Vec<ImageProcessingReport>,
}

impl std::ops::Deref for AddImagesResult {
    type Target = DraftState;

    fn deref(&self) -> &Self::Target {
        &self.draft
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FieldMutationKind {
    Composer,
    Price,
    Delivery,
}

#[derive(Clone, Debug)]
struct FieldMutation {
    key: String,
    value: Value,
    step: String,
    fields: Vec<String>,
    kind: FieldMutationKind,
}

#[derive(Default)]
struct FieldProgress {
    persisted: Vec<String>,
    absent: Vec<String>,
}

struct AppliedFieldMutations {
    draft: DraftState,
    progress: FieldProgress,
}

struct FieldBoundary<'a> {
    step: &'a str,
    fields: &'a [String],
}

struct FieldOutcomes {
    persisted: Vec<String>,
    absent: Vec<String>,
    indeterminate: Vec<String>,
    unattempted: Vec<String>,
}

fn ordered_field_mutations(values: Map<String, Value>) -> Vec<FieldMutation> {
    let order = [
        "category",
        "title",
        "description",
        "trade_type",
        "price",
        "postal_code",
        "attributes",
        "delivery",
    ];
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.sort_by(|(left, _), (right, _)| {
        let rank = |key: &str| {
            if key == "delivery" {
                usize::MAX
            } else {
                order
                    .iter()
                    .position(|candidate| *candidate == key)
                    .unwrap_or(order.len() - 1)
            }
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
    values
        .into_iter()
        .map(|(key, value)| {
            let value = if key == "category" {
                normalize_category(value)
            } else {
                value
            };
            let stage_key: String = key
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || character == '_' {
                        character.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            let fields = if key == "attributes" {
                value
                    .as_object()
                    .filter(|attributes| !attributes.is_empty())
                    .map(|attributes| {
                        attributes
                            .keys()
                            .map(|field| format!("attributes.{field}"))
                            .collect()
                    })
                    .unwrap_or_else(|| vec![key.clone()])
            } else {
                vec![key.clone()]
            };
            let kind = match key.as_str() {
                "price" => FieldMutationKind::Price,
                "delivery" => FieldMutationKind::Delivery,
                _ => FieldMutationKind::Composer,
            };
            FieldMutation {
                step: format!("apply_{stage_key}"),
                fields,
                key,
                value,
                kind,
            }
        })
        .collect()
}

fn pending_fields(
    mutations: &[FieldMutation],
    progress: &FieldProgress,
    active_fields: &[String],
) -> Vec<String> {
    let classified = progress
        .persisted
        .iter()
        .chain(&progress.absent)
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let active = active_fields
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    mutations
        .iter()
        .flat_map(|mutation| mutation.fields.iter())
        .filter(|field| !classified.contains(field.as_str()) && !active.contains(field.as_str()))
        .cloned()
        .collect()
}

fn field_is_persisted(state: &DraftState, mutation: &FieldMutation, field: &str) -> bool {
    if mutation.kind == FieldMutationKind::Delivery {
        let requested = delivery_values(&mutation.value).unwrap_or_default();
        return state
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.selected == requested);
    }
    if mutation.key == "attributes" {
        let Some(attribute) = field.strip_prefix("attributes.") else {
            return state.values.get(&mutation.key) == Some(&mutation.value);
        };
        return state
            .values
            .get("attributes")
            .and_then(Value::as_object)
            .and_then(|attributes| attributes.get(attribute))
            == mutation
                .value
                .as_object()
                .and_then(|attributes| attributes.get(attribute));
    }
    let Some(observed) = state.values.get(&mutation.key) else {
        return false;
    };
    match mutation.key.as_str() {
        "price" => prices_equal(observed, &mutation.value),
        "trade_type" => {
            observed
                .as_str()
                .zip(mutation.value.as_str())
                .is_some_and(|(observed, requested)| {
                    composer_trade_type(observed) == composer_trade_type(requested)
                })
        }
        "category" => normalize_category(observed.clone()) == mutation.value,
        _ => observed == &mutation.value,
    }
}

fn classify_fields(state: &DraftState, mutation: &FieldMutation) -> (Vec<String>, Vec<String>) {
    mutation
        .fields
        .iter()
        .cloned()
        .partition(|field| field_is_persisted(state, mutation, field))
}

fn retry_field_action(draft_id: &str, fields: &[String]) -> String {
    let single_flag = match fields {
        [field] => match field.as_str() {
            "category" => Some("--category VALUE"),
            "title" => Some("--title VALUE"),
            "description" => Some("--description VALUE"),
            "price" => Some("--price VALUE"),
            "trade_type" => Some("--trade-type VALUE"),
            "postal_code" => Some("--postal-code VALUE"),
            "delivery" => Some("--delivery VALUE"),
            _ => None,
        },
        _ => None,
    };
    single_flag.map_or_else(
        || format!("flea draft update {draft_id} --input PATH_WITH_ONLY_ABSENT_FIELDS"),
        |flag| format!("flea draft update {draft_id} {flag}"),
    )
}

#[allow(clippy::too_many_arguments)]
fn field_recovery(
    draft_id: &str,
    completed_steps: &[String],
    boundary: FieldBoundary<'_>,
    outcomes: FieldOutcomes,
    upstream_transient: bool,
    safe_to_retry: bool,
    fresh_state: Option<DraftState>,
    force_inspection: bool,
) -> Recovery {
    let manual_inspection_required = force_inspection || !outcomes.indeterminate.is_empty();
    let next_safe_actions = if manual_inspection_required || outcomes.absent.is_empty() {
        vec![format!("flea draft show {draft_id}")]
    } else {
        vec![retry_field_action(draft_id, &outcomes.absent)]
    };
    Recovery {
        draft_id: draft_id.to_owned(),
        listing_id: None,
        completed_steps: completed_steps.to_vec(),
        active_step: Some(boundary.step.to_owned()),
        fields: boundary.fields.to_vec(),
        persisted_fields: outcomes.persisted,
        absent_fields: outcomes.absent,
        indeterminate_fields: outcomes.indeterminate,
        unattempted_fields: outcomes.unattempted,
        manual_inspection_required,
        upstream_transient,
        safe_to_retry,
        next_safe_actions,
        fresh_state,
    }
}

fn schema_validation_issues(state: &DraftState, mutation: &FieldMutation) -> Vec<ValidationIssue> {
    let requested = if mutation.key == "attributes" {
        mutation
            .value
            .as_object()
            .map(|attributes| {
                attributes
                    .iter()
                    .map(|(key, value)| (key.as_str(), value))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_else(|| vec![(mutation.key.as_str(), &mutation.value)])
    } else {
        vec![(mutation.key.as_str(), &mutation.value)]
    };
    requested
        .into_iter()
        .filter_map(|(key, value)| {
            let field = state.fields.iter().find(|field| field.key == key)?;
            let mut issue = schema_validation_issue(state, field, value)?;
            if mutation.key == "attributes" {
                issue.field = format!("attributes.{}", issue.field);
            }
            Some(issue)
        })
        .collect()
}

fn schema_validation_issue(
    state: &DraftState,
    field: &Field,
    value: &Value,
) -> Option<ValidationIssue> {
    if value.is_null() {
        return None;
    }
    let valid_shape = match field.field_type {
        FieldType::String | FieldType::Text | FieldType::Date => value.is_string(),
        FieldType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        FieldType::Decimal => value.as_f64().is_some_and(f64::is_finite),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Select => !value.is_array() && !value.is_object(),
        FieldType::MultiSelect => value.is_array(),
        FieldType::Unknown(_) => true,
    };
    if !valid_shape {
        return Some(ValidationIssue {
            field: field.key.clone(),
            code: "invalid_type".to_owned(),
            message: format!("expected {}", field_type_name(&field.field_type)),
            source: Some("local_schema".to_owned()),
            raw: None,
        });
    }
    if matches!(field.field_type, FieldType::Select | FieldType::MultiSelect) {
        let allowed = state
            .options
            .iter()
            .filter(|option| option.field == field.key)
            .filter_map(|option| match &option.value {
                Value::String(value) => Some(value.clone()),
                Value::Number(value) => Some(value.to_string()),
                Value::Bool(value) => Some(value.to_string()),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let supplied = value
            .as_array()
            .map(|values| values.iter().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![value]);
        if !allowed.is_empty()
            && supplied.iter().any(|value| {
                let candidate = match value {
                    Value::String(value) => Some(value.clone()),
                    Value::Number(value) => Some(value.to_string()),
                    Value::Bool(value) => Some(value.to_string()),
                    _ => None,
                };
                candidate.is_none_or(|value| !allowed.contains(value.as_str()))
            })
        {
            return Some(ValidationIssue {
                field: field.key.clone(),
                code: "invalid_option".to_owned(),
                message: "value is not present in the source-backed field options".to_owned(),
                source: Some("local_schema".to_owned()),
                raw: None,
            });
        }
    }
    None
}

fn field_type_name(field_type: &FieldType) -> &'static str {
    match field_type {
        FieldType::String | FieldType::Text => "a string",
        FieldType::Integer => "an integer",
        FieldType::Decimal => "a number",
        FieldType::Boolean => "a boolean",
        FieldType::Select => "one selectable value",
        FieldType::MultiSelect => "an array of selectable values",
        FieldType::Date => "a date string",
        FieldType::Unknown(_) => "a value accepted by the composer",
    }
}

fn structured_validation_issues(error: &ApiError, state: &DraftState) -> Vec<ValidationIssue> {
    let Some(upstream) = error
        .details
        .as_deref()
        .and_then(|details| details.get("upstream"))
    else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    collect_validation_errors(upstream, &mut errors);
    let mut issues = map_validation_errors(errors, &state.fields);
    for issue in &mut issues {
        issue.field = stable_field_key(&issue.field);
    }
    issues.sort_by(|left, right| {
        left.field
            .cmp(&right.field)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    issues.dedup_by(|left, right| {
        left.field == right.field && left.code == right.code && left.message == right.message
    });
    issues
}

fn collect_validation_errors(value: &Value, output: &mut Vec<UpstreamValidationError>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in [
        "errors",
        "field_errors",
        "fieldErrors",
        "validation_errors",
        "validationErrors",
        "invalid_params",
        "invalid-params",
        "violations",
        "validation",
        "fields",
    ] {
        let Some(errors) = object.get(key) else {
            continue;
        };
        match errors {
            Value::Array(errors) => {
                for error in errors {
                    collect_validation_error_item(error, None, output);
                }
            }
            Value::Object(errors) => {
                for (field, error) in errors {
                    match error {
                        Value::Array(errors) => {
                            for error in errors {
                                collect_validation_error_item(error, Some(field), output);
                            }
                        }
                        error => collect_validation_error_item(error, Some(field), output),
                    }
                }
            }
            _ => {}
        }
    }
    for key in ["error", "details"] {
        if let Some(nested) = object.get(key) {
            collect_validation_errors(nested, output);
        }
    }
}

fn collect_validation_error_item(
    value: &Value,
    fallback_field: Option<&str>,
    output: &mut Vec<UpstreamValidationError>,
) {
    if let Some(message) = value.as_str() {
        if let Some(field) = fallback_field {
            output.push(UpstreamValidationError {
                source: field.to_owned(),
                code: "invalid".to_owned(),
                message: message.to_owned(),
                raw: Some(value.clone()),
            });
        }
        return;
    }
    let Some(object) = value.as_object() else {
        return;
    };
    let source = [
        "field",
        "path",
        "name",
        "property",
        "parameter",
        "attribute",
        "key",
        "source",
    ]
    .into_iter()
    .find_map(|key| {
        object.get(key).and_then(|value| {
            value.as_str().or_else(|| {
                value.as_object().and_then(|source| {
                    ["pointer", "parameter", "field", "path"]
                        .into_iter()
                        .find_map(|key| source.get(key).and_then(Value::as_str))
                })
            })
        })
    })
    .or(fallback_field);
    let Some(source) = source else {
        return;
    };
    let message = ["message", "reason", "detail", "description"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .unwrap_or("Tori rejected the field");
    let code = ["code", "type", "kind"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .unwrap_or("invalid");
    output.push(UpstreamValidationError {
        source: source.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        raw: Some(value.clone()),
    });
}

fn mutation_is_ambiguous(error: &ApiError) -> bool {
    error.code == "mutation.uncertain"
        || error.status.is_none()
        || matches!(error.status, Some(408 | 425 | 500..=599))
}

fn field_error_details(
    stage: &str,
    fields: &[String],
    error: &ApiError,
    validation: &[ValidationIssue],
    observation: Option<Value>,
) -> Value {
    let mut details = json!({
        "stage": stage,
        "fields": fields,
        "status": error.status,
        "content_type": error.details.as_deref().and_then(|details| details.get("content_type")),
        "body_is_unparseable": error.details.as_deref().and_then(|details| details.get("body_is_unparseable")),
        "upstream_error": error.details,
    });
    let object = details
        .as_object_mut()
        .expect("field error details are an object");
    if !validation.is_empty() {
        object.insert(
            "field_errors".to_owned(),
            serde_json::to_value(validation).expect("validation issues serialize"),
        );
    }
    if let Some(observation) = observation {
        object.insert("observation".to_owned(), observation);
    }
    details
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
    fn local_field_validation_error(
        &self,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        issues: Vec<ValidationIssue>,
    ) -> WorkflowError {
        let (active_persisted, active_absent) = classify_fields(draft, mutation);
        let mut persisted = progress.persisted.clone();
        persisted.extend(active_persisted);
        let mut absent = progress.absent.clone();
        absent.extend(active_absent);
        let stage = mutation.step.replacen("apply_", "validate_", 1);
        let mut api = ApiError::new(
            "draft.validation_failed",
            "Draft fields do not match the source-backed composer schema",
        );
        api.details = Some(Box::new(json!({
            "stage": stage,
            "fields": mutation.fields,
            "field_errors": issues,
        })));
        WorkflowError {
            code: api.code.clone(),
            message: api.message.clone(),
            source: Some(api),
            recovery: Some(field_recovery(
                &draft.draft_id,
                completed,
                FieldBoundary {
                    step: &stage,
                    fields: &mutation.fields,
                },
                FieldOutcomes {
                    persisted,
                    absent,
                    indeterminate: Vec::new(),
                    unattempted: pending_fields(mutations, progress, &mutation.fields),
                },
                false,
                false,
                Some(draft.clone()),
                false,
            )),
            details: Some(json!({
                "stage": stage,
                "fields": mutation.fields,
                "field_errors": issues,
            })),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn observed_field_error(
        &self,
        draft_before: &DraftState,
        fresh: DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        error: ApiError,
        code: &str,
        message: &str,
        validation: &[ValidationIssue],
        safe_to_retry: bool,
    ) -> WorkflowError {
        let (active_persisted, active_absent) = classify_fields(&fresh, mutation);
        let mut persisted = progress.persisted.clone();
        persisted.extend(active_persisted);
        let mut absent = progress.absent.clone();
        absent.extend(active_absent);
        let observation = json!({
            "status": "succeeded",
            "etag_before": draft_before.etag,
            "etag_after": fresh.etag,
            "etag_changed": draft_before.etag != fresh.etag,
        });
        WorkflowError {
            code: code.to_owned(),
            message: message.to_owned(),
            source: Some(error.clone()),
            recovery: Some(field_recovery(
                &draft_before.draft_id,
                completed,
                FieldBoundary {
                    step: &mutation.step,
                    fields: &mutation.fields,
                },
                FieldOutcomes {
                    persisted,
                    absent,
                    indeterminate: Vec::new(),
                    unattempted: pending_fields(mutations, progress, &mutation.fields),
                },
                error.upstream_transient,
                safe_to_retry,
                Some(fresh),
                false,
            )),
            details: Some(field_error_details(
                &mutation.step,
                &mutation.fields,
                &error,
                validation,
                Some(observation),
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn unavailable_field_observation(
        &self,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        mutation_error: ApiError,
        observation_error: ApiError,
    ) -> WorkflowError {
        let validation = structured_validation_issues(&mutation_error, draft);
        let observation = json!({
            "status": "failed",
            "error": {
                "code": observation_error.code,
                "status": observation_error.status,
                "details": observation_error.details,
            },
            "guidance": "Inspect the authoritative draft before retrying any indeterminate field",
        });
        WorkflowError {
            code: "mutation.uncertain".to_owned(),
            message: "A draft field mutation returned an ambiguous response and authoritative state is unavailable".to_owned(),
            source: Some(mutation_error.clone()),
            recovery: Some(field_recovery(
                &draft.draft_id,
                completed,
                FieldBoundary {
                    step: &mutation.step,
                    fields: &mutation.fields,
                },
                FieldOutcomes {
                    persisted: progress.persisted.clone(),
                    absent: progress.absent.clone(),
                    indeterminate: mutation.fields.clone(),
                    unattempted: pending_fields(mutations, progress, &mutation.fields),
                },
                mutation_error.upstream_transient,
                false,
                None,
                true,
            )),
            details: Some(field_error_details(
                &mutation.step,
                &mutation.fields,
                &mutation_error,
                &validation,
                Some(observation),
            )),
        }
    }

    async fn field_mutation_error(
        &self,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        error: ApiError,
    ) -> WorkflowError {
        let mut validation = structured_validation_issues(&error, draft);
        for issue in &mut validation {
            let candidate = stable_field_key(&issue.field);
            if candidate == mutation.key {
                issue.field = mutation.key.clone();
            } else {
                let attribute = format!("attributes.{candidate}");
                if mutation.fields.contains(&attribute) {
                    issue.field = attribute;
                }
            }
        }
        if error.status == Some(412) {
            return match self.api.get_draft(&draft.draft_id).await {
                Ok(fresh) => {
                    let mut context = RetryContext::mutation(match mutation.kind {
                        FieldMutationKind::Price => OperationMethod::Patch,
                        FieldMutationKind::Delivery => OperationMethod::Post,
                        FieldMutationKind::Composer => OperationMethod::Put,
                    })
                    .with_etag()
                    .with_authoritative_observation();
                    if completed_steps_have_mutation(completed) {
                        context = context.with_completed_mutation_steps();
                    }
                    let classification = classify(FailureKind::PreconditionFailed, context);
                    self.observed_field_error(
                        draft,
                        fresh,
                        mutations,
                        mutation,
                        completed,
                        progress,
                        error,
                        "draft.conflict",
                        "The draft changed while the field update was being applied",
                        &validation,
                        classification.safe_to_retry,
                    )
                }
                Err(observation_error) => self.unavailable_field_observation(
                    draft,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    observation_error,
                ),
            };
        }
        if mutation_is_ambiguous(&error) {
            return match self.api.get_draft(&draft.draft_id).await {
                Ok(fresh) => self.observed_field_error(
                    draft,
                    fresh,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    "mutation.uncertain",
                    "A draft field mutation returned an ambiguous response",
                    &validation,
                    false,
                ),
                Err(observation_error) => self.unavailable_field_observation(
                    draft,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    observation_error,
                ),
            };
        }

        let (active_persisted, active_absent) = classify_fields(draft, mutation);
        let mut persisted = progress.persisted.clone();
        persisted.extend(active_persisted);
        let mut absent = progress.absent.clone();
        absent.extend(active_absent);
        let is_validation = error
            .status
            .is_some_and(|status| (400..500).contains(&status))
            && !validation.is_empty();
        WorkflowError {
            code: if is_validation {
                "draft.validation_failed".to_owned()
            } else {
                error.code.clone()
            },
            message: if is_validation {
                "Tori rejected one or more draft fields".to_owned()
            } else {
                error.message.clone()
            },
            source: Some(error.clone()),
            recovery: Some(field_recovery(
                &draft.draft_id,
                completed,
                FieldBoundary {
                    step: &mutation.step,
                    fields: &mutation.fields,
                },
                FieldOutcomes {
                    persisted,
                    absent,
                    indeterminate: Vec::new(),
                    unattempted: pending_fields(mutations, progress, &mutation.fields),
                },
                error.upstream_transient,
                false,
                Some(draft.clone()),
                false,
            )),
            details: Some(field_error_details(
                &mutation.step,
                &mutation.fields,
                &error,
                &validation,
                None,
            )),
        }
    }

    fn enrich_field_error(
        &self,
        mut error: WorkflowError,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        progress: &FieldProgress,
    ) -> WorkflowError {
        let validation_failure = matches!(
            error.code.as_str(),
            "draft.validation_failed"
                | "draft.invalid_delivery"
                | "draft.delivery_options_unavailable"
        );
        if let Some(recovery) = &mut error.recovery {
            recovery.active_step = Some(if validation_failure {
                "validate_delivery".to_owned()
            } else {
                mutation.step.clone()
            });
            recovery.fields = mutation.fields.clone();
            let active_persisted = std::mem::take(&mut recovery.persisted_fields);
            let active_absent = std::mem::take(&mut recovery.absent_fields);
            let active_indeterminate = std::mem::take(&mut recovery.indeterminate_fields);
            recovery.persisted_fields = progress.persisted.clone();
            recovery.persisted_fields.extend(active_persisted);
            recovery.absent_fields = progress.absent.clone();
            recovery.absent_fields.extend(active_absent);
            recovery.indeterminate_fields = active_indeterminate;
            recovery.unattempted_fields = pending_fields(mutations, progress, &mutation.fields);
            if validation_failure {
                if !recovery
                    .absent_fields
                    .iter()
                    .any(|field| mutation.fields.contains(field))
                {
                    recovery.absent_fields.extend(mutation.fields.clone());
                }
                recovery.next_safe_actions =
                    vec![retry_field_action(&draft.draft_id, &recovery.absent_fields)];
            } else if error.code == "mutation.uncertain" {
                if recovery
                    .persisted_fields
                    .iter()
                    .all(|field| !mutation.fields.contains(field))
                    && recovery
                        .absent_fields
                        .iter()
                        .all(|field| !mutation.fields.contains(field))
                    && recovery.indeterminate_fields.is_empty()
                {
                    recovery.indeterminate_fields = mutation.fields.clone();
                }
                recovery.manual_inspection_required = !recovery.indeterminate_fields.is_empty();
                if recovery.manual_inspection_required || recovery.absent_fields.is_empty() {
                    recovery.next_safe_actions =
                        vec![format!("flea draft show {}", draft.draft_id)];
                } else {
                    recovery.next_safe_actions =
                        vec![retry_field_action(&draft.draft_id, &recovery.absent_fields)];
                }
            }
            recovery.persisted_fields.sort();
            recovery.persisted_fields.dedup();
            recovery.absent_fields.sort();
            recovery.absent_fields.dedup();
        }
        let details = error.details.get_or_insert_with(|| json!({}));
        if let Some(details) = details.as_object_mut() {
            details.insert(
                "stage".to_owned(),
                Value::String(if validation_failure {
                    "validate_delivery".to_owned()
                } else {
                    mutation.step.clone()
                }),
            );
            details.insert(
                "fields".to_owned(),
                Value::Array(mutation.fields.iter().cloned().map(Value::String).collect()),
            );
        }
        error
    }

    async fn apply_field_mutations(
        &self,
        mut draft: DraftState,
        mutations: Vec<FieldMutation>,
        completed: &mut Vec<String>,
        workflow: &str,
        listing_id: Option<&str>,
    ) -> Result<AppliedFieldMutations, WorkflowError> {
        let mut progress = FieldProgress::default();
        let category_first = mutations
            .first()
            .is_some_and(|mutation| mutation.key == "category");
        let initial_validation_end = if category_first { 1 } else { mutations.len() };
        for mutation in &mutations[..initial_validation_end] {
            let issues = schema_validation_issues(&draft, mutation);
            if !issues.is_empty() {
                return Err(self
                    .local_field_validation_error(
                        &draft, &mutations, mutation, completed, &progress, issues,
                    )
                    .with_optional_listing_id(listing_id));
            }
        }

        for (index, mutation) in mutations.iter().enumerate() {
            if category_first && index == 1 {
                for pending in &mutations[index..] {
                    let issues = schema_validation_issues(&draft, pending);
                    if !issues.is_empty() {
                        return Err(self
                            .local_field_validation_error(
                                &draft, &mutations, pending, completed, &progress, issues,
                            )
                            .with_optional_listing_id(listing_id));
                    }
                }
            }
            let context = diagnostics::WorkflowContext {
                workflow,
                step: &mutation.step,
                draft_id: Some(&draft.draft_id),
                listing_id,
                fields: &mutation.fields,
            };
            diagnostics::workflow_step(&context, "started");
            match mutation.kind {
                FieldMutationKind::Composer => {
                    let mut values = draft.values.clone();
                    values.insert(mutation.key.clone(), mutation.value.clone());
                    match self
                        .api
                        .update_item(&draft.draft_id, &draft.etag, &values)
                        .await
                    {
                        Ok(updated) => {
                            diagnostics::workflow_step(&context, "completed");
                            completed.push(mutation.step.clone());
                            let (persisted, absent) = classify_fields(&updated, mutation);
                            progress.persisted.extend(persisted);
                            progress.absent.extend(absent);
                            draft = updated;
                        }
                        Err(error) => {
                            diagnostics::workflow_step(&context, "failed");
                            return Err(self
                                .field_mutation_error(
                                    &draft, &mutations, mutation, completed, &progress, error,
                                )
                                .await
                                .with_optional_listing_id(listing_id));
                        }
                    }
                }
                FieldMutationKind::Price => {
                    match self
                        .api
                        .update_sale_price(&draft.draft_id, &draft.etag, &mutation.value)
                        .await
                    {
                        Ok(_) => {
                            completed.push(mutation.step.clone());
                            match self.api.get_draft(&draft.draft_id).await {
                                Ok(fresh) if field_is_persisted(&fresh, mutation, "price") => {
                                    diagnostics::workflow_step(&context, "completed");
                                    completed.push("observe_price".to_owned());
                                    progress.persisted.push("price".to_owned());
                                    draft = fresh;
                                }
                                Ok(fresh) => {
                                    diagnostics::workflow_step(&context, "failed");
                                    let mut error = ApiError::new(
                                        "mutation.uncertain",
                                        "The authoritative draft price does not match the requested price",
                                    );
                                    error.details = Some(Box::new(json!({
                                        "stage": "observe_price",
                                        "requested_price": mutation.value,
                                        "observed_price": fresh.values.get("price"),
                                    })));
                                    return Err(self
                                        .observed_field_error(
                                            &draft,
                                            fresh,
                                            &mutations,
                                            mutation,
                                            completed,
                                            &progress,
                                            error,
                                            "mutation.uncertain",
                                            "The authoritative draft price does not match the requested price",
                                            &[],
                                            false,
                                        )
                                        .with_optional_listing_id(listing_id));
                                }
                                Err(observation_error) => {
                                    diagnostics::workflow_step(&context, "failed");
                                    let mutation_error = ApiError::new(
                                        "mutation.uncertain",
                                        "The price mutation succeeded but authoritative state is unavailable",
                                    );
                                    return Err(self
                                        .unavailable_field_observation(
                                            &draft,
                                            &mutations,
                                            mutation,
                                            completed,
                                            &progress,
                                            mutation_error,
                                            error_at_stage(observation_error, "observe_price"),
                                        )
                                        .with_optional_listing_id(listing_id));
                                }
                            }
                        }
                        Err(error) => {
                            diagnostics::workflow_step(&context, "failed");
                            return Err(self
                                .field_mutation_error(
                                    &draft, &mutations, mutation, completed, &progress, error,
                                )
                                .await
                                .with_optional_listing_id(listing_id));
                        }
                    }
                }
                FieldMutationKind::Delivery => {
                    match self
                        .apply_delivery_selection(draft.clone(), &mutation.value, completed)
                        .await
                    {
                        Ok(updated) => {
                            diagnostics::workflow_step(&context, "completed");
                            progress.persisted.extend(mutation.fields.clone());
                            draft = updated;
                        }
                        Err(error) => {
                            diagnostics::workflow_step(&context, "failed");
                            return Err(self
                                .enrich_field_error(error, &draft, &mutations, mutation, &progress)
                                .with_optional_listing_id(listing_id));
                        }
                    }
                }
            }
        }
        progress.persisted.sort();
        progress.persisted.dedup();
        progress.absent.sort();
        progress.absent.dedup();
        Ok(AppliedFieldMutations { draft, progress })
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
        if let Err(error) = self
            .api
            .apply_delivery(&draft_id, &composer, &selected)
            .await
        {
            if mutation_is_ambiguous(&error) {
                return match self.delivery_composer(&draft_id, completed).await {
                    Ok(observed) => {
                        let persisted = observed.state.selected == [selected.clone()];
                        state.delivery = Some(observed.state);
                        let mut workflow = WorkflowError::for_draft(
                            &draft_id,
                            completed,
                            error_at_stage(error, "apply_delivery"),
                            false,
                        );
                        if let Some(recovery) = &mut workflow.recovery {
                            recovery.active_step = Some("apply_delivery".to_owned());
                            recovery.fields = vec!["delivery".to_owned()];
                            if persisted {
                                recovery.persisted_fields = vec!["delivery".to_owned()];
                            } else {
                                recovery.absent_fields = vec!["delivery".to_owned()];
                                recovery.next_safe_actions =
                                    vec![retry_field_action(&draft_id, &recovery.absent_fields)];
                            }
                            recovery.fresh_state = Some(state);
                        }
                        Err(workflow)
                    }
                    Err(observation_error) => {
                        let mut workflow = WorkflowError::for_draft(
                            &draft_id,
                            completed,
                            error_at_stage(error, "apply_delivery"),
                            false,
                        );
                        if let Some(recovery) = &mut workflow.recovery {
                            recovery.active_step = Some("apply_delivery".to_owned());
                            recovery.fields = vec!["delivery".to_owned()];
                            recovery.indeterminate_fields = vec!["delivery".to_owned()];
                            recovery.manual_inspection_required = true;
                        }
                        workflow.details = Some(json!({
                            "stage": "apply_delivery",
                            "fields": ["delivery"],
                            "observation": {
                                "status": "failed",
                                "error": {
                                    "code": observation_error.code,
                                    "status": observation_error.source.as_ref().and_then(|source| source.status),
                                }
                            }
                        }));
                        Err(workflow)
                    }
                };
            }
            return Err(WorkflowError::for_draft(
                &draft_id,
                completed,
                error_at_stage(error, "apply_delivery"),
                false,
            ));
        }
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
        values: Map<String, Value>,
        images: Vec<PreparedImage>,
    ) -> Result<CreateResult, WorkflowError> {
        requested_sale_price(&values).map_err(WorkflowError::input)?;
        let draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["create_draft".to_owned()];
        let applied = self
            .apply_field_mutations(
                draft,
                ordered_field_mutations(values),
                &mut completed,
                "draft_create",
                None,
            )
            .await?;
        let mut draft = applied.draft;
        let mut image_processing = Vec::new();
        if !images.is_empty() {
            let result = self
                .add_prepared_images(&draft, images, &mut completed)
                .await?;
            draft = result.draft;
            image_processing = result.image_processing;
        }
        Ok(CreateResult {
            draft,
            completed_steps: completed,
            image_processing,
        })
    }

    pub async fn create_from_listing(
        &self,
        listing_id: &str,
    ) -> Result<CreateResult, WorkflowError> {
        let seed = self
            .api
            .source_listing(listing_id)
            .await
            .map_err(WorkflowError::before_creation)?;
        requested_sale_price(&seed.values).map_err(WorkflowError::input)?;
        let draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["load_source_listing".to_owned(), "create_draft".to_owned()];
        let applied = self
            .apply_field_mutations(
                draft,
                ordered_field_mutations(seed.values),
                &mut completed,
                "draft_create_from_listing",
                Some(listing_id),
            )
            .await?;
        let mut draft = applied.draft;

        let mut ordered = Vec::new();
        let mut image_processing = Vec::new();
        for source in seed.images {
            let image = prepare_image_bytes(source.bytes).map_err(|error| {
                WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                    .with_listing_id(listing_id)
            })?;
            image_processing.push(image.processing_report().clone());
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
        Ok(CreateResult {
            draft,
            completed_steps: completed,
            image_processing,
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
        let composer = self.delivery_composer(draft_id, &completed).await?;
        completed.push("fetch_delivery_options".to_owned());
        attach_delivery_model(&mut state, &composer)
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        Ok(state)
    }

    pub async fn validate(&self, draft_id: &str) -> Result<PublicationValidation, WorkflowError> {
        let publication = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut state = publication.draft;
        let delivery_verifiable = match self.api.delivery_composer(draft_id).await {
            Ok(composer) => attach_delivery_model(&mut state, &composer).is_ok(),
            Err(_) => false,
        };
        let categories = self.api.publication_categories().await.ok();
        Ok(evaluate_publication(
            &state,
            categories.as_deref(),
            publication.composer_model,
            delivery_verifiable,
        ))
    }

    pub async fn update(
        &self,
        draft_id: &str,
        patch: &Map<String, Value>,
    ) -> Result<UpdateResult, WorkflowError> {
        let current = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        let mut requested_values = current.values.clone();
        requested_values.extend(patch.clone());
        requested_values.remove("delivery");
        if patch.contains_key("price") {
            requested_sale_price(&requested_values)
                .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        }
        let requested_delivery = patch
            .get("delivery")
            .and_then(delivery_values)
            .unwrap_or_default();
        let applied = self
            .apply_field_mutations(
                current.clone(),
                ordered_field_mutations(patch.clone()),
                &mut completed,
                "draft_update",
                None,
            )
            .await?;
        let mut requested_fields = patch.keys().cloned().collect::<Vec<_>>();
        requested_fields.sort();
        Ok(UpdateResult {
            etag_changed: applied.draft.etag != current.etag,
            draft: applied.draft,
            requested_fields,
            requested_delivery,
            persisted_fields: applied.progress.persisted,
            ignored_fields: applied.progress.absent,
            completed_steps: completed,
        })
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
    ) -> Result<AddImagesResult, WorkflowError> {
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
    ) -> Result<AddImagesResult, WorkflowError> {
        let mut images = Vec::with_capacity(paths.len());
        for path in paths {
            let image = prepare_image(path.as_ref()).map_err(|error| {
                WorkflowError::for_draft(&state.draft_id, completed, error, false)
            })?;
            images.push(image);
        }
        self.add_prepared_images(state, images, completed).await
    }

    async fn add_prepared_images(
        &self,
        state: &DraftState,
        images: Vec<PreparedImage>,
        completed: &mut Vec<String>,
    ) -> Result<AddImagesResult, WorkflowError> {
        let mut existing = state.images.iter().collect::<Vec<_>>();
        existing.sort_by_key(|image| image.position);
        let mut ordered: Vec<UploadedImage> = existing
            .into_iter()
            .map(uploaded_from_draft_image)
            .collect();
        let mut image_processing = Vec::with_capacity(images.len());
        for image in images {
            image_processing.push(image.processing_report().clone());
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
        Ok(AddImagesResult {
            draft: updated,
            image_processing,
        })
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
        let publication = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        let composer_model = publication.composer_model;
        let mut state = publication.draft;
        completed.push("fetch_draft".to_owned());

        let composer = self.api.delivery_composer(draft_id).await.ok();
        let mut delivery_verifiable = match composer.as_ref() {
            Some(composer) if attach_delivery_model(&mut state, composer).is_ok() => {
                completed.push("fetch_delivery_options".to_owned());
                true
            }
            _ => false,
        };
        let categories = self.api.publication_categories().await.ok();
        if categories.is_some() {
            completed.push("fetch_category_taxonomy".to_owned());
        }
        let report = evaluate_publication(
            &state,
            categories.as_deref(),
            composer_model,
            delivery_verifiable,
        );
        if !report.missing.is_empty()
            || !report.invalid.is_empty()
            || !report.unverifiable.is_empty()
        {
            return Err(WorkflowError::validation(&completed, report));
        }
        completed.push("validate".to_owned());

        state = self.wait_for_images(state, &completed).await?;
        completed.push("wait_for_images".to_owned());
        if delivery_verifiable && state.delivery.is_none() {
            delivery_verifiable = composer
                .as_ref()
                .is_some_and(|composer| attach_delivery_model(&mut state, composer).is_ok());
        }
        let report = evaluate_publication(
            &state,
            categories.as_deref(),
            composer_model,
            delivery_verifiable,
        );
        if !report.ready {
            return Err(WorkflowError::validation(&completed, report));
        }
        let composer = composer.expect("ready publication has a delivery composer");
        let requested_delivery = composer.state.selected.clone();
        let delivery = requested_delivery
            .first()
            .expect("ready publication has one delivery selection")
            .clone();

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

        let revision = state.revision.clone().ok_or_else(|| {
            WorkflowError::for_draft(
                draft_id,
                &completed,
                model_error(
                    "listing_composer",
                    "$.ad.revision",
                    "draft revision is unavailable",
                ),
                false,
            )
        })?;
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
            if !state.images.is_empty()
                && state
                    .images
                    .iter()
                    .all(|image| image.state != ImageState::Processing)
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

pub struct PreparedImage {
    bytes: Vec<u8>,
    file_name: &'static str,
    width: u32,
    height: u32,
    report: ImageProcessingReport,
}

impl PreparedImage {
    pub const fn width(&self) -> u32 {
        self.width
    }

    pub const fn height(&self) -> u32 {
        self.height
    }

    pub fn source_format(&self) -> &str {
        &self.report.source_format
    }

    pub fn output_format(&self) -> &str {
        &self.report.uploaded_format
    }

    pub fn byte_len(&self) -> usize {
        self.bytes.len()
    }

    pub const fn metadata_stripped(&self) -> bool {
        self.report.metadata_stripped
    }

    pub const fn recompressed(&self) -> bool {
        self.report.recompressed
    }

    fn processing_report(&self) -> &ImageProcessingReport {
        &self.report
    }
}

impl From<ProcessedImage> for PreparedImage {
    fn from(image: ProcessedImage) -> Self {
        Self {
            bytes: image.bytes,
            file_name: image.file_name,
            width: image.width,
            height: image.height,
            report: image.report,
        }
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
            .field("report", &self.report)
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
    image_processing::preprocess_path(path)
        .map(PreparedImage::from)
        .map_err(image_processing_error)
}

fn prepare_image_bytes(bytes: Vec<u8>) -> Result<PreparedImage, ApiError> {
    image_processing::preprocess_bytes(bytes)
        .map(PreparedImage::from)
        .map_err(image_processing_error)
}

fn image_processing_error(error: ProcessingError) -> ApiError {
    let mut api_error = ApiError::new(error.code, error.message);
    api_error.details = error.details.map(Box::new);
    api_error
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

pub fn evaluate_publication(
    state: &DraftState,
    categories: Option<&[PublicationCategory]>,
    composer_model: ComposerModelStatus,
    delivery_verifiable: bool,
) -> PublicationValidation {
    let mut report = PublicationValidation {
        draft_id: state.draft_id.clone(),
        ready: false,
        missing: Vec::new(),
        invalid: Vec::new(),
        pending: Vec::new(),
        unverifiable: Vec::new(),
    };
    validate_publication_core(state, categories, delivery_verifiable, &mut report);
    validate_publication_composer(state, composer_model, &mut report);
    validate_publication_images(state, &mut report);
    for requirements in [
        &mut report.missing,
        &mut report.invalid,
        &mut report.pending,
        &mut report.unverifiable,
    ] {
        requirements.sort();
        requirements.dedup();
    }
    report.ready = report.missing.is_empty()
        && report.invalid.is_empty()
        && report.pending.is_empty()
        && report.unverifiable.is_empty();
    report
}

fn validate_publication_core(
    state: &DraftState,
    categories: Option<&[PublicationCategory]>,
    delivery_verifiable: bool,
    report: &mut PublicationValidation,
) {
    let category = publication_field_value(state, "category").and_then(publication_scalar_string);
    match category {
        None if publication_field_value(state, "category")
            .is_none_or(publication_value_missing) =>
        {
            report.missing.push(publication_issue(
                "category",
                "a category is required for publication",
                "publication_invariant",
                "flea category list".to_owned(),
            ));
        }
        None => report.invalid.push(publication_issue(
            "category",
            "the category must be a non-empty machine value",
            "publication_invariant",
            "flea category list".to_owned(),
        )),
        Some(category_id) => match categories {
            Some(categories) => match categories
                .iter()
                .find(|category| category.category_id == category_id)
            {
                Some(category) if category.selectable => {}
                Some(_) => report.invalid.push(publication_issue(
                    "category",
                    "the selected category cannot contain listings",
                    "category_taxonomy",
                    "flea category list".to_owned(),
                )),
                None => report.invalid.push(publication_issue(
                    "category",
                    "the selected category is absent from the current taxonomy",
                    "category_taxonomy",
                    "flea category list".to_owned(),
                )),
            },
            None => report.unverifiable.push(publication_issue(
                "category",
                "category selectability could not be verified",
                "category_taxonomy",
                format!("flea draft validate {}", state.draft_id),
            )),
        },
    }

    validate_publication_text(state, "title", report);
    validate_publication_text(state, "description", report);

    let trade_type = publication_field_value(state, "trade_type");
    let trade_type = match trade_type.and_then(Value::as_str) {
        None if trade_type.is_none_or(publication_value_missing) => {
            report.missing.push(publication_core_issue(
                state,
                "trade_type",
                "a trade type is required for publication",
            ));
            None
        }
        Some("sell" | "SELL" | "1") => Some("sell"),
        Some("give_away" | "GIVE_AWAY" | "2") => Some("give_away"),
        Some("wanted" | "WANTED" | "3") => Some("wanted"),
        _ => {
            report.invalid.push(publication_core_issue(
                state,
                "trade_type",
                "the trade type must identify a sale, give-away, or wanted listing",
            ));
            None
        }
    };

    let price = publication_field_value(state, "price");
    match trade_type {
        Some("sell") => match price.and_then(publication_numeric_value) {
            None if price.is_none_or(publication_value_missing) => report.missing.push(
                publication_core_issue(state, "price", "sale listings require a price"),
            ),
            Some(price) if price > 0.0 => {}
            _ => report.invalid.push(publication_core_issue(
                state,
                "price",
                "a sale price must be a positive number",
            )),
        },
        Some("give_away") if price.is_some_and(|price| !publication_value_missing(price)) => {
            report.invalid.push(publication_core_issue(
                state,
                "price",
                "give-away listings cannot include a sale price",
            ));
        }
        Some("wanted") if price.is_some_and(|price| publication_numeric_value(price).is_none()) => {
            report.invalid.push(publication_core_issue(
                state,
                "price",
                "the price must be numeric when supplied",
            ));
        }
        _ => {}
    }

    match publication_field_value(state, "postal_code") {
        None => report.missing.push(publication_core_issue(
            state,
            "postal_code",
            "a postal location is required for publication",
        )),
        Some(Value::String(postal_code))
            if postal_code.len() == 5
                && postal_code
                    .bytes()
                    .all(|character| character.is_ascii_digit()) => {}
        Some(_) => report.invalid.push(publication_core_issue(
            state,
            "postal_code",
            "the postal location must contain a five-digit postal code",
        )),
    }

    if !delivery_verifiable {
        report.unverifiable.push(publication_issue(
            "delivery",
            "delivery configuration could not be verified",
            "delivery_composer",
            format!("flea draft validate {}", state.draft_id),
        ));
    } else {
        match state.delivery.as_ref() {
            Some(delivery) if delivery.selected.is_empty() => {
                report.missing.push(publication_core_issue(
                    state,
                    "delivery",
                    "explicit delivery intent is required for publication",
                ))
            }
            Some(delivery)
                if delivery.selected.len() == 1
                    && delivery
                        .options
                        .iter()
                        .any(|option| option.value == delivery.selected[0]) => {}
            Some(_) => report.invalid.push(publication_issue(
                "delivery",
                "the selected delivery value is unavailable or ambiguous",
                "delivery_composer",
                format!("flea draft show {}", state.draft_id),
            )),
            None => report.unverifiable.push(publication_issue(
                "delivery",
                "delivery configuration could not be verified",
                "delivery_composer",
                format!("flea draft validate {}", state.draft_id),
            )),
        }
    }
}

fn validate_publication_text(state: &DraftState, field: &str, report: &mut PublicationValidation) {
    match publication_field_value(state, field) {
        None => report.missing.push(publication_core_issue(
            state,
            field,
            &format!("a {field} is required for publication"),
        )),
        Some(Value::String(value)) if !value.trim().is_empty() => {}
        Some(Value::String(_)) => report.missing.push(publication_core_issue(
            state,
            field,
            &format!("a {field} is required for publication"),
        )),
        Some(_) => report.invalid.push(publication_core_issue(
            state,
            field,
            &format!("the {field} must be text"),
        )),
    }
}

fn validate_publication_composer(
    state: &DraftState,
    composer_model: ComposerModelStatus,
    report: &mut PublicationValidation,
) {
    if composer_model != ComposerModelStatus::Available {
        let reason = match composer_model {
            ComposerModelStatus::Unavailable => "listing-composer requirements are unavailable",
            ComposerModelStatus::Malformed => "listing-composer requirements are malformed",
            ComposerModelStatus::Available => unreachable!(),
        };
        report.unverifiable.push(publication_issue(
            "composer_model",
            reason,
            "listing_composer",
            format!("flea draft validate {}", state.draft_id),
        ));
        return;
    }

    for field in &state.fields {
        let publication_field = publication_field_name(&field.key);
        if field.requirement == Requirement::Required
            && !publication_report_contains(report, publication_field)
            && publication_field_value(state, publication_field)
                .is_none_or(publication_value_missing)
        {
            report.missing.push(publication_issue(
                publication_field,
                "the selected category requires this field",
                "listing_composer",
                format!("flea draft show {}", state.draft_id),
            ));
            continue;
        }
        if field.status == FieldStatus::Invalid
            && !publication_report_contains(report, publication_field)
        {
            report.invalid.push(publication_issue(
                publication_field,
                field
                    .validation_message
                    .as_deref()
                    .unwrap_or("the listing composer rejected this value"),
                "listing_composer",
                format!("flea draft show {}", state.draft_id),
            ));
            continue;
        }
        if field.requirement == Requirement::Required
            && matches!(field.field_type, FieldType::Unknown(_))
            && !publication_report_contains(report, publication_field)
        {
            report.unverifiable.push(publication_issue(
                publication_field,
                "the required listing-composer field has an unknown type",
                "listing_composer",
                format!("flea draft show {}", state.draft_id),
            ));
        }
    }

    for field in &state.fields {
        let publication_field = publication_field_name(&field.key);
        if publication_report_contains(report, publication_field) {
            continue;
        }
        let options = state
            .options
            .iter()
            .filter(|option| option.field == field.key)
            .collect::<Vec<_>>();
        if options.is_empty() {
            continue;
        }
        let Some(value) = publication_field_value(state, publication_field) else {
            continue;
        };
        let valid = match value {
            Value::Array(values) => values.iter().all(|value| {
                options
                    .iter()
                    .any(|option| values_semantically_equal(value, &option.value))
            }),
            value => options
                .iter()
                .any(|option| values_semantically_equal(value, &option.value)),
        };
        if !valid && field.options_truncated {
            report.unverifiable.push(publication_issue(
                publication_field,
                "the listing-composer options are truncated",
                "listing_composer",
                format!("flea draft show {}", state.draft_id),
            ));
        } else if !valid {
            report.invalid.push(publication_issue(
                publication_field,
                "the value is not an option in the current listing composer",
                "listing_composer",
                format!("flea draft show {}", state.draft_id),
            ));
        }
    }
}

fn validate_publication_images(state: &DraftState, report: &mut PublicationValidation) {
    if state.images.is_empty() {
        report.missing.push(publication_issue(
            "images",
            "at least one image is required for publication",
            "publication_invariant",
            format!("flea draft image add {} PATH", state.draft_id),
        ));
        return;
    }
    let mut images = state.images.iter().collect::<Vec<_>>();
    images.sort_by(|left, right| {
        left.position
            .cmp(&right.position)
            .then_with(|| left.image_id.cmp(&right.image_id))
    });
    let pending = images
        .iter()
        .filter(|image| image.state == ImageState::Processing)
        .map(|image| image.image_id.as_str())
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        report.pending.push(publication_issue(
            "images",
            format!("image processing is pending: {}", pending.join(", ")),
            "image_processing",
            format!("flea draft validate {}", state.draft_id),
        ));
    }
    let failed = images
        .iter()
        .filter(|image| image.state == ImageState::Failed)
        .map(|image| image.image_id.as_str())
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        report.invalid.push(publication_issue(
            "images",
            format!("image processing rejected: {}", failed.join(", ")),
            "image_processing",
            format!("flea draft show {}", state.draft_id),
        ));
    }
}

fn publication_field_value<'a>(state: &'a DraftState, field: &str) -> Option<&'a Value> {
    let aliases: &[&str] = match field {
        "category" => &["category", "category_id", "categoryId"],
        "title" => &["title", "subject", "heading"],
        "description" => &["description", "body", "text"],
        "trade_type" => &["trade_type", "trade-type", "tradeType"],
        "price" => &["price", "price_amount", "price_max"],
        "postal_code" => &[
            "postal_code",
            "postal-code",
            "post-code",
            "postcode",
            "postalCode",
        ],
        "images" => &["multi_image", "multi-image", "image", "images"],
        "delivery" => &["delivery"],
        _ => &[],
    };
    if let Some(value) = aliases.iter().find_map(|alias| state.values.get(*alias)) {
        return Some(value);
    }
    if let Some(value) = state.values.get(field) {
        return Some(value);
    }
    if field == "postal_code"
        && let Some(value) = publication_postal_value(state.values.get("location")?)
    {
        return Some(value);
    }
    state
        .fields
        .iter()
        .find(|model_field| publication_field_name(&model_field.key) == field)
        .and_then(|model_field| model_field.value.as_ref())
}

fn publication_postal_value(location: &Value) -> Option<&Value> {
    let location = match location {
        Value::Array(locations) => locations.first()?,
        location => location,
    };
    let location = location.as_object()?;
    ["postal_code", "postal-code", "postalCode"]
        .into_iter()
        .find_map(|key| location.get(key))
}

fn publication_field_name(field: &str) -> &str {
    match field {
        "subject" | "heading" => "title",
        "body" | "text" => "description",
        "categoryId" | "category_id" => "category",
        "tradeType" | "trade-type" => "trade_type",
        "price_amount" | "price_max" => "price",
        "postalCode" | "postal-code" | "post-code" | "postcode" => "postal_code",
        "multi_image" | "multi-image" | "image" => "images",
        field => field,
    }
}

fn publication_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn publication_value_missing(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

fn publication_numeric_value(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn publication_issue(
    field: impl Into<String>,
    reason: impl Into<String>,
    source: impl Into<String>,
    command: String,
) -> PublicationRequirement {
    PublicationRequirement {
        field: field.into(),
        reason: reason.into(),
        source: source.into(),
        command,
    }
}

fn publication_core_issue(state: &DraftState, field: &str, reason: &str) -> PublicationRequirement {
    let option = match field {
        "category" => "--category VALUE",
        "title" => "--title VALUE",
        "description" => "--description VALUE",
        "trade_type" => "--trade-type VALUE",
        "price" => "--price VALUE",
        "postal_code" => "--postal-code VALUE",
        "delivery" => "--delivery VALUE",
        _ => "--input PATH",
    };
    publication_issue(
        field,
        reason,
        "publication_invariant",
        format!("flea draft update {} {option}", state.draft_id),
    )
}

fn publication_report_contains(report: &PublicationValidation, field: &str) -> bool {
    report
        .missing
        .iter()
        .chain(&report.invalid)
        .chain(&report.pending)
        .chain(&report.unverifiable)
        .any(|requirement| requirement.field == field)
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
