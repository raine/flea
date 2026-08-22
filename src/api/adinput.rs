use std::{collections::BTreeMap, fmt, path::Path, time::Duration};

use reqwest::{Method as ReqwestMethod, header::HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    api::client::{MultipartPart, RequestSpec, ToriClient, compatibility},
    diagnostics,
    domain::field::Field,
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
}

#[derive(Clone, PartialEq)]
pub enum RequestBody {
    Empty,
    Json(Value),
    Image {
        bytes: Vec<u8>,
        file_name: String,
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
                file_name,
                width,
                height,
            } => formatter
                .debug_struct("Image")
                .field("byte_len", &bytes.len())
                .field("file_name", file_name)
                .field("width", width)
                .field("height", height)
                .finish(),
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    pub method: Method,
    pub path: String,
    pub if_match: Option<String>,
    pub retry: RetryPolicy,
    pub body: RequestBody,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct HttpResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Value,
}

#[derive(Clone, PartialEq)]
pub struct ApiError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub status: Option<u16>,
    pub details: Option<Box<Value>>,
}

impl ApiError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
            status: None,
            details: None,
        }
    }

    fn response(response: &HttpResponse) -> Self {
        let message = response
            .body
            .get("message")
            .and_then(Value::as_str)
            .map(diagnostics::redact_text)
            .unwrap_or_else(|| "Tori rejected the request".to_owned());
        let mut details = response.body.clone();
        diagnostics::redact_value(&mut details);
        let mut error = Self::new("upstream.request_failed", message);
        error.status = Some(response.status);
        error.retryable = response.status >= 500;
        error.details = Some(Box::new(details));
        error
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
            .field("retryable", &self.retryable)
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
        let method = match request.method {
            Method::Get => ReqwestMethod::GET,
            Method::Post => ReqwestMethod::POST,
            Method::Patch => ReqwestMethod::PATCH,
            Method::Put => ReqwestMethod::PUT,
            Method::Delete => ReqwestMethod::DELETE,
        };
        let mut spec = RequestSpec::new(method, request.path.clone(), service);
        if service == compatibility::SERVICE_ADINPUT {
            spec = spec.adinput();
        }
        spec = match request.body {
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
                width,
                height,
            } => spec.multipart(vec![
                MultipartPart::bytes("image", bytes)
                    .file_name(file_name)
                    .mime_type("application/octet-stream"),
                MultipartPart::bytes("width", width.to_string()),
                MultipartPart::bytes("height", height.to_string()),
            ]),
        };
        if let Some(etag) = request.if_match {
            spec = spec.if_match(HeaderValue::from_str(&etag).map_err(|_| {
                ApiError::new("upstream.invalid_etag", "Tori returned an invalid ETag")
            })?);
        }
        let response = self.client.execute(spec).await.map_err(|error| {
            let mut api = ApiError::new("upstream.request_failed", error.to_string());
            api.retryable = matches!(request.retry, RetryPolicy::BoundedRead);
            api
        })?;
        let body = if response.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&response.body)
                .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))?
        };
        Ok(HttpResponse {
            status: response.status.as_u16(),
            etag: response
                .etag()
                .and_then(|etag| etag.to_str().ok())
                .map(str::to_owned),
            body,
        })
    }
}

fn service_for_path(path: &str) -> &'static str {
    if path.contains("/delivery") {
        compatibility::SERVICE_DELIVERY
    } else if path.contains("/products") || path.contains("/publish") {
        compatibility::SERVICE_ORDER_PAYMENT
    } else if path.contains("/tracking/") {
        compatibility::SERVICE_BILLING_TRACKING
    } else if path.starts_with("/listings/") {
        compatibility::SERVICE_AD_SUMMARIES
    } else if path.ends_with("/item") {
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
        image_ids: &[String],
    ) -> Result<DraftState, ApiError>;
    async fn category_predictions(
        &self,
        draft_id: &str,
    ) -> Result<Vec<CategoryPrediction>, ApiError>;
    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError>;
    async fn apply_delivery(
        &self,
        draft_id: &str,
        revision: &str,
        delivery: &Value,
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
        let response = self.transport.execute(request).await?;
        if (200..300).contains(&response.status) {
            Ok(response)
        } else {
            Err(ApiError::response(&response))
        }
    }

    async fn draft_request(&self, request: HttpRequest) -> Result<DraftState, ApiError> {
        let response = self.json(request).await?;
        let mut draft: DraftState = serde_json::from_value(response.body)
            .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))?;
        if let Some(etag) = response.etag {
            draft.etag = etag;
        }
        Ok(draft)
    }
}

#[allow(async_fn_in_trait)]
impl<T: HttpTransport> AdInputApi for HttpAdInputApi<T> {
    async fn create_draft(&self) -> Result<DraftState, ApiError> {
        self.draft_request(HttpRequest::mutation(
            Method::Post,
            "/drafts",
            RequestBody::Json(json!({ "type": "recommerce" })),
        ))
        .await
    }

    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.draft_request(HttpRequest::read(format!("/drafts/{draft_id}/with-model")))
            .await
    }

    async fn update_item(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut request = HttpRequest::mutation(
            Method::Patch,
            format!("/drafts/{draft_id}/item"),
            RequestBody::Json(Value::Object(values.clone())),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request).await
    }

    async fn submit_adinput(
        &self,
        draft_id: &str,
        etag: &str,
        state: &DraftState,
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut request = HttpRequest::mutation(
            Method::Put,
            format!("/drafts/{draft_id}/adinput"),
            RequestBody::Json(serde_json::to_value(state).expect("draft state serializes")),
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
        let response = self
            .json(HttpRequest::mutation(
                Method::Post,
                format!("/drafts/{draft_id}/images"),
                RequestBody::Image {
                    bytes,
                    file_name: file_name.to_owned(),
                    width,
                    height,
                },
            ))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))
    }

    async fn set_images(
        &self,
        draft_id: &str,
        etag: &str,
        image_ids: &[String],
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut request = HttpRequest::mutation(
            Method::Put,
            format!("/drafts/{draft_id}/images/order"),
            RequestBody::Json(json!({ "image_ids": image_ids })),
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
            .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))
    }

    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/listings/{listing_id}/draft-source"
            )))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))
    }

    async fn apply_delivery(
        &self,
        draft_id: &str,
        revision: &str,
        delivery: &Value,
    ) -> Result<(), ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.json(HttpRequest::mutation(
            Method::Put,
            format!("/drafts/{draft_id}/delivery"),
            RequestBody::Json(json!({ "revision": revision, "delivery": delivery })),
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
            .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))
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
            .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))
    }

    async fn confirmation(&self, listing_id: &str) -> Result<Confirmation, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/listings/{listing_id}/confirmation"
            )))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|error| ApiError::new("upstream.unexpected_response", error.to_string()))
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
    pub retryable: bool,
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
    fn before_creation(error: ApiError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery: None,
            details: None,
        }
    }

    fn for_draft(
        draft_id: &str,
        completed_steps: &[String],
        error: ApiError,
        retryable: bool,
    ) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery: Some(Recovery {
                draft_id: draft_id.to_owned(),
                listing_id: None,
                completed_steps: completed_steps.to_vec(),
                retryable,
                next_safe_actions: vec![format!("tori draft show {draft_id}")],
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

    fn validation(draft_id: &str, completed_steps: &[String], missing: Vec<String>) -> Self {
        Self {
            code: "draft.validation_failed".to_owned(),
            message: "Required fields are missing or invalid".to_owned(),
            source: None,
            recovery: Some(Recovery {
                draft_id: draft_id.to_owned(),
                listing_id: None,
                completed_steps: completed_steps.to_vec(),
                retryable: false,
                next_safe_actions: vec![format!("tori draft update {draft_id} --input PATH")],
                fresh_state: None,
            }),
            details: Some(json!({ "missing_fields": missing })),
        }
    }
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

impl<A: AdInputApi> DraftWorkflow<A> {
    pub async fn create(
        &self,
        mut values: Map<String, Value>,
        image_paths: &[impl AsRef<Path>],
    ) -> Result<CreateResult, WorkflowError> {
        let mut draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["create_draft".to_owned()];

        if let Some(category) = values.remove("category") {
            let category = match category {
                Value::String(id) => id
                    .parse::<u64>()
                    .map(Value::from)
                    .unwrap_or(Value::String(id)),
                category => category,
            };
            let category_values = Map::from_iter([("category".to_owned(), category)]);
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
            draft = self
                .api
                .update_item(&draft.draft_id, &draft.etag, &values)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                })?;
            completed.push("apply_fields".to_owned());
        }
        if !image_paths.is_empty() {
            draft = self
                .add_images_from_paths(&draft, image_paths, &mut completed)
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
        let seed = self
            .api
            .source_listing(listing_id)
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["load_source_listing".to_owned(), "create_draft".to_owned()];
        draft = self
            .api
            .update_item(&draft.draft_id, &draft.etag, &seed.values)
            .await
            .map_err(|error| {
                WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                    .with_listing_id(listing_id)
            })?;
        completed.push("copy_fields".to_owned());

        let mut ordered = Vec::new();
        for source in seed.images {
            let (width, height) = image_dimensions(&source.bytes).map_err(|error| {
                WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                    .with_listing_id(listing_id)
            })?;
            let uploaded = self
                .api
                .upload_image(
                    &draft.draft_id,
                    &source.file_name,
                    source.bytes,
                    width,
                    height,
                )
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                        .with_listing_id(listing_id)
                })?;
            ordered.push(uploaded.image_id);
            completed.push(format!("upload_image:{}", ordered.len() - 1));
        }
        if !ordered.is_empty() {
            draft = self
                .api
                .set_images(&draft.draft_id, &draft.etag, &ordered)
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
        })
    }

    pub async fn show(&self, draft_id: &str) -> Result<DraftState, WorkflowError> {
        let mut state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        if state.category_is_unset() && !state.images.is_empty() {
            state.predictions = self
                .api
                .category_predictions(draft_id)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(draft_id, &["fetch_draft".to_owned()], error, true)
                })?;
        }
        Ok(state)
    }

    pub async fn update(
        &self,
        draft_id: &str,
        patch: &Map<String, Value>,
    ) -> Result<DraftState, WorkflowError> {
        let current = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let completed = vec!["fetch_draft".to_owned()];
        match self.api.update_item(draft_id, &current.etag, patch).await {
            Ok(state) => Ok(state),
            Err(error) if error.status == Some(412) => {
                let fresh = self.api.get_draft(draft_id).await.map_err(|fresh_error| {
                    WorkflowError::for_draft(draft_id, &completed, fresh_error, true)
                })?;
                let mut conflict = ApiError::new(
                    "draft.conflict",
                    "The draft changed while the update was being applied",
                );
                conflict.status = Some(412);
                let mut workflow = WorkflowError::for_draft(draft_id, &completed, conflict, false);
                if let Some(recovery) = &mut workflow.recovery {
                    recovery.fresh_state = Some(fresh);
                    recovery.next_safe_actions = vec![format!("tori draft show {draft_id}")];
                }
                Err(workflow)
            }
            Err(error) => Err(WorkflowError::for_draft(draft_id, &completed, error, false)),
        }
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
        let mut ordered: Vec<String> = existing
            .into_iter()
            .map(|image| image.image_id.clone())
            .collect();
        for path in paths {
            let path = path.as_ref();
            let bytes = std::fs::read(path).map_err(|error| {
                WorkflowError::for_draft(
                    &state.draft_id,
                    completed,
                    ApiError::new("draft.image_read_failed", error.to_string()),
                    false,
                )
            })?;
            let (width, height) = image_dimensions(&bytes).map_err(|error| {
                WorkflowError::for_draft(&state.draft_id, completed, error, false)
            })?;
            let file_name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("image");
            let uploaded = self
                .api
                .upload_image(&state.draft_id, file_name, bytes, width, height)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&state.draft_id, completed, error, false)
                })?;
            ordered.push(uploaded.image_id);
            completed.push(format!("upload_image:{}", ordered.len() - 1));
        }
        let updated = self
            .api
            .set_images(&state.draft_id, &state.etag, &ordered)
            .await
            .map_err(|error| WorkflowError::for_draft(&state.draft_id, completed, error, false))?;
        completed.push("attach_images".to_owned());
        Ok(updated)
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
        let ordered: Vec<String> = retained
            .into_iter()
            .map(|image| image.image_id.clone())
            .collect();
        self.api
            .set_images(draft_id, &state.etag, &ordered)
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

        let missing = missing_required_fields(&state);
        if !missing.is_empty() {
            return Err(WorkflowError::validation(draft_id, &completed, missing));
        }
        let delivery = state.values.get("delivery").cloned().ok_or_else(|| {
            WorkflowError::validation(draft_id, &completed, vec!["delivery".to_owned()])
        })?;
        if !delivery_is_explicit(&delivery) {
            return Err(WorkflowError::validation(
                draft_id,
                &completed,
                vec!["delivery".to_owned()],
            ));
        }
        completed.push("validate".to_owned());

        state = self.wait_for_images(state, &completed).await?;
        completed.push("wait_for_images".to_owned());

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
            .apply_delivery(draft_id, &revision, &delivery)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        completed.push("apply_delivery".to_owned());

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
                        vec![format!("tori listing show {}", publication.listing_id)];
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
    error.retryable = true;
    WorkflowError::for_draft(draft_id, completed, error, true)
}

fn image_dimensions(bytes: &[u8]) -> Result<(u32, u32), ApiError> {
    let image = image::load_from_memory(bytes)
        .map_err(|error| ApiError::new("draft.invalid_image", error.to_string()))?;
    Ok((image.width(), image.height()))
}

fn missing_required_fields(state: &DraftState) -> Vec<String> {
    state
        .required_fields
        .iter()
        .filter(|key| {
            state
                .values
                .get(*key)
                .is_none_or(|value| value.is_null() || value.as_str().is_some_and(str::is_empty))
        })
        .cloned()
        .collect()
}

fn delivery_is_explicit(delivery: &Value) -> bool {
    match delivery {
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => !values.is_empty() && values.iter().all(delivery_is_explicit),
        Value::Object(values) => {
            !values.is_empty() && values.values().all(|value| !value.is_null())
        }
        _ => false,
    }
}

pub fn ordered_image_states(images: &[DraftImage]) -> BTreeMap<usize, (&str, &ImageState)> {
    images
        .iter()
        .map(|image| (image.position, (image.image_id.as_str(), &image.state)))
        .collect()
}
