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
    pub content_type: Option<String>,
    pub location: Option<String>,
    pub body: Value,
    pub body_is_unparseable: bool,
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
        let mut upstream = response.body.clone();
        diagnostics::redact_value(&mut upstream);
        let mut error = Self::new("upstream.request_failed", message);
        error.status = Some(response.status);
        error.retryable = response.status >= 500;
        error.details = Some(Box::new(json!({
            "status": response.status,
            "content_type": response.content_type,
            "upstream": upstream
        })));
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
            let mut api = ApiError::new("upstream.request_failed", error.to_string());
            api.retryable = matches!(request.retry, RetryPolicy::BoundedRead);
            api
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
    } else if path.contains("/delivery") {
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
        let is_mutation = request.method.is_mutation();
        let response = self.json(request).await?;
        if response.body_is_unparseable {
            let mut error = unexpected_representation("receive_draft_state", &response);
            if is_mutation {
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "The draft mutation may have succeeded, but its resulting state is unknown"
                        .to_owned();
            }
            return Err(error);
        }
        normalize_draft_state(response.body, response.etag.as_deref()).map_err(|mut error| {
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
            return Err(ApiError::response(&response));
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
                retryable: true,
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
                retryable: false,
                next_safe_actions: vec![format!("flea draft update {draft_id} --input PATH")],
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
        let mut copied_values = draft.values.clone();
        copied_values.extend(seed.values);
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
        let price = if patch.contains_key("price") {
            requested_sale_price(&values)
                .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?
        } else {
            None
        };
        let mut field_patch = patch.clone();
        field_patch.remove("price");
        let mut state = current;

        if !field_patch.is_empty() {
            if price.is_some() {
                values.remove("price");
            }
            state = match self.api.update_item(draft_id, &state.etag, &values).await {
                Ok(state) => state,
                Err(error) if error.status == Some(412) => {
                    return Err(self.update_conflict(draft_id, &completed).await);
                }
                Err(error) => {
                    return Err(WorkflowError::for_draft(draft_id, &completed, error, false));
                }
            };
            completed.push("apply_fields".to_owned());
        }

        if let Some(price) = price {
            match self
                .api
                .update_sale_price(draft_id, &state.etag, &price)
                .await
            {
                Ok(_) => {}
                Err(error) if error.status == Some(412) => {
                    return Err(self.update_conflict(draft_id, &completed).await);
                }
                Err(error) => {
                    return Err(WorkflowError::for_draft(draft_id, &completed, error, false));
                }
            }
            completed.push("apply_price".to_owned());
            state = self.observe_price(draft_id, &price, &completed).await?;
        }
        Ok(state)
    }

    async fn update_conflict(&self, draft_id: &str, completed: &[String]) -> WorkflowError {
        let fresh = match self.api.get_draft(draft_id).await {
            Ok(fresh) => fresh,
            Err(error) => return WorkflowError::for_draft(draft_id, completed, error, true),
        };
        let mut conflict = ApiError::new(
            "draft.conflict",
            "The draft changed while the update was being applied",
        );
        conflict.status = Some(412);
        let mut workflow = WorkflowError::for_draft(draft_id, completed, conflict, false);
        if let Some(recovery) = &mut workflow.recovery {
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
    error.retryable = true;
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
