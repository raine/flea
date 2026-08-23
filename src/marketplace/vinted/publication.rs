use std::{future::Future, path::PathBuf, pin::Pin};

use reqwest::{
    Method, StatusCode,
    header::{HeaderName, HeaderValue},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;
use uuid::Uuid;

use crate::{
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    image_processing,
    marketplace::{
        PortalId,
        vinted::{
            auth::{VintedAuthentication, VintedCredentialRecord},
            binding::VINTED_FI_BINDING,
            search::VintedSearchSession,
        },
    },
    transport::{
        MultipartPart, RequestBody, Transport, TransportError, TransportErrorKind,
        TransportResponse,
    },
};

const API_V2_PATH: &str = "/api/v2/";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_IMAGES: usize = 20;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ItemAttributeInput {
    pub code: String,
    #[serde(default)]
    pub ids: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ShipmentPricesInput {
    pub domestic: Option<String>,
    pub international: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ParcelInput {
    pub width: f64,
    pub height: f64,
    pub length: f64,
    pub weight: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ListingInput {
    pub title: String,
    pub description: String,
    pub catalog_id: u64,
    pub price: String,
    pub currency: String,
    pub package_size_id: u64,
    #[serde(default)]
    pub brand_id: Option<u64>,
    #[serde(default)]
    pub brand: Option<String>,
    #[serde(default)]
    pub isbn: Option<String>,
    #[serde(default)]
    pub is_unisex: bool,
    #[serde(default)]
    pub ai_photo: bool,
    #[serde(default)]
    pub color_ids: Vec<u64>,
    #[serde(default)]
    pub measurement_length: Option<u64>,
    #[serde(default)]
    pub measurement_width: Option<u64>,
    #[serde(default)]
    pub item_attributes: Vec<ItemAttributeInput>,
    #[serde(default)]
    pub manufacturer: Option<String>,
    #[serde(default)]
    pub manufacturer_labelling: Option<String>,
    #[serde(default)]
    pub shipment_prices: Option<ShipmentPricesInput>,
    #[serde(default)]
    pub parcel: Option<ParcelInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct UploadedPhoto {
    pub id: u64,
    pub orientation: u16,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PublicationOperation {
    CreateDraft,
    UpdateDraft { draft_id: String },
    CompleteDraft { draft_id: String },
    Publish,
    DeleteDraft { draft_id: String },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PublicationResult {
    pub operation: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub after_upload_actions: Vec<String>,
    pub uploaded_images: usize,
}

pub trait VintedPublicationApi: Send + Sync {
    fn upload_photo<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        image: PreparedImage,
    ) -> Pin<Box<dyn Future<Output = Result<UploadedPhoto, AppError>> + Send + 'a>>;

    fn mutate<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        operation: &'a PublicationOperation,
        body: Option<Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;
}

pub struct VintedPublication<'a> {
    session: &'a dyn VintedSearchSession,
    api: &'a dyn VintedPublicationApi,
}

impl<'a> VintedPublication<'a> {
    pub fn new(session: &'a dyn VintedSearchSession, api: &'a dyn VintedPublicationApi) -> Self {
        Self { session, api }
    }

    pub async fn execute(
        &self,
        portal: PortalId,
        operation: PublicationOperation,
        input: Option<ListingInput>,
        image_paths: Vec<PathBuf>,
    ) -> Result<PublicationResult, AppError> {
        validate_operation(&operation, input.as_ref(), &image_paths).map_err(|error| {
            publication_error(error, &operation, &[], MutationStatus::NotAttempted)
        })?;
        let credentials = self.session.credentials(portal).map_err(|error| {
            publication_error(error, &operation, &[], MutationStatus::NotAttempted)
        })?;

        if matches!(operation, PublicationOperation::DeleteDraft { .. }) {
            self.api
                .mutate(&credentials, &operation, None)
                .await
                .map_err(|error| classify_mutation_error(error, &operation, &[]))?;
            return Ok(PublicationResult {
                operation: "delete_draft",
                draft_id: draft_id(&operation).map(ToOwned::to_owned),
                item_id: None,
                after_upload_actions: Vec::new(),
                uploaded_images: 0,
            });
        }

        let mut photos = Vec::with_capacity(image_paths.len());
        for path in image_paths {
            let prepared = prepare_image(path).await.map_err(|error| {
                publication_error(error, &operation, &photos, MutationStatus::NotAttempted)
            })?;
            photos.push(
                self.api
                    .upload_photo(&credentials, prepared)
                    .await
                    .map_err(|error| {
                        publication_error(error, &operation, &photos, MutationStatus::NotAttempted)
                    })?,
            );
        }
        let input = input.expect("validated publication input");
        let upload_session_id = Uuid::new_v4().to_string();
        let body = publication_body(&operation, input, &photos, &upload_session_id);
        let response = self
            .api
            .mutate(&credentials, &operation, Some(body))
            .await
            .map_err(|error| classify_mutation_error(error, &operation, &photos))?;
        normalize_result(&operation, photos.len(), &response)
    }
}

pub struct HttpVintedPublicationApi {
    auth: VintedAuthentication,
    api_base_url: String,
}

impl HttpVintedPublicationApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            api_base_url: VINTED_FI_BINDING.api_host.to_owned(),
        }
    }

    #[cfg(test)]
    fn with_api_base_url(mut self, api_base_url: String) -> Self {
        self.api_base_url = api_base_url;
        self
    }

    fn endpoint(&self, path: &str) -> Result<Url, AppError> {
        let mut url = Url::parse(&self.api_base_url).map_err(|error| {
            AppError::unexpected("Vinted API binding is invalid").with_source(error)
        })?;
        url.set_path(&format!("{API_V2_PATH}{path}"));
        Ok(url)
    }

    async fn upload_photo_request(
        &self,
        credentials: &VintedCredentialRecord,
        image: PreparedImage,
    ) -> Result<UploadedPhoto, AppError> {
        let url = self.endpoint("photos")?;
        let mut request = self.auth.authenticated_request(
            Method::POST,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        request.body = RequestBody::Multipart(vec![
            MultipartPart::bytes("photo[type]", b"item".to_vec()),
            MultipartPart::bytes("photo[file]", image.bytes)
                .file_name(image.file_name)
                .mime_type(image.media_type),
        ]);
        let response = self
            .auth
            .executor()
            .execute(request)
            .await
            .map_err(execution_error)?;
        decode_photo_response(response)
    }

    async fn mutation_request(
        &self,
        credentials: &VintedCredentialRecord,
        operation: &PublicationOperation,
        body: Option<Value>,
    ) -> Result<Value, AppError> {
        let (method, path) = operation_endpoint(operation);
        let url = self.endpoint(&path)?;
        let mut request = self.auth.authenticated_request(
            method,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        for (name, value) in [
            ("x-upload-form", "true"),
            ("x-enable-dynamic-attribute-condition", "true"),
            ("x-enable-dynamic-attribute-size", "true"),
            ("x-enable-dynamic-attribute-video-game-rating", "true"),
        ] {
            insert_header(&mut request.headers, name, value);
        }
        if let Some(body) = body {
            insert_header(&mut request.headers, "content-type", "application/json");
            request.body = RequestBody::Bytes(serde_json::to_vec(&body).map_err(|error| {
                AppError::unexpected("Failed to serialize Vinted publication").with_source(error)
            })?);
        }
        let response = self
            .auth
            .executor()
            .execute(request)
            .await
            .map_err(mutation_execution_error)?;
        decode_mutation_response(response)
    }
}

impl Default for HttpVintedPublicationApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedPublicationApi for HttpVintedPublicationApi {
    fn upload_photo<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        image: PreparedImage,
    ) -> Pin<Box<dyn Future<Output = Result<UploadedPhoto, AppError>> + Send + 'a>> {
        Box::pin(self.upload_photo_request(credentials, image))
    }

    fn mutate<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        operation: &'a PublicationOperation,
        body: Option<Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.mutation_request(credentials, operation, body))
    }
}

pub struct PreparedImage {
    bytes: Vec<u8>,
    file_name: &'static str,
    media_type: &'static str,
}

async fn prepare_image(path: PathBuf) -> Result<PreparedImage, AppError> {
    tokio::task::spawn_blocking(move || image_processing::preprocess_path(&path))
        .await
        .map_err(|error| {
            AppError::unexpected("Image preprocessing task failed").with_source(error)
        })?
        .map(|image| {
            let media_type = match image.report.uploaded_format.as_str() {
                "jpeg" => "image/jpeg",
                "png" => "image/png",
                _ => "application/octet-stream",
            };
            PreparedImage {
                bytes: image.bytes,
                file_name: image.file_name,
                media_type,
            }
        })
        .map_err(|error| {
            let mut app_error = AppError::validation(error.code, error.message);
            app_error.details = error.details.map(Box::new);
            app_error
        })
}

fn validate_operation(
    operation: &PublicationOperation,
    input: Option<&ListingInput>,
    image_paths: &[PathBuf],
) -> Result<(), AppError> {
    if matches!(operation, PublicationOperation::DeleteDraft { .. }) {
        if input.is_some() || !image_paths.is_empty() {
            return Err(AppError::usage(
                "Draft deletion does not accept listing input or images",
            ));
        }
        return validate_draft_id(draft_id(operation).expect("delete draft ID"));
    }
    let input = input.ok_or_else(|| AppError::usage("Publication input is required"))?;
    validate_input(input)?;
    if let Some(id) = draft_id(operation) {
        validate_draft_id(id)?;
    }
    if image_paths.is_empty() {
        return Err(AppError::validation(
            "vinted.images_required",
            "At least one image is required",
        ));
    }
    if image_paths.len() > MAX_IMAGES {
        return Err(AppError::validation(
            "vinted.too_many_images",
            format!("At most {MAX_IMAGES} images are allowed"),
        ));
    }
    Ok(())
}

fn validate_input(input: &ListingInput) -> Result<(), AppError> {
    for (name, value) in [("title", &input.title), ("description", &input.description)] {
        if value.trim().is_empty() {
            return Err(AppError::validation(
                "vinted.required_field",
                format!("{name} must not be empty"),
            ));
        }
    }
    let parsed_price = input.price.parse::<f64>().ok();
    if input.currency.trim().is_empty() || parsed_price.is_none_or(|price| !price.is_finite()) {
        return Err(AppError::validation(
            "vinted.invalid_price",
            "Price must be a decimal string and currency must not be empty",
        ));
    }
    if parsed_price.is_some_and(|price| price <= 0.0) {
        return Err(AppError::validation(
            "vinted.invalid_price",
            "Price must be greater than zero",
        ));
    }
    if input
        .item_attributes
        .iter()
        .any(|attribute| attribute.code.trim().is_empty())
    {
        return Err(AppError::validation(
            "vinted.invalid_attribute",
            "Dynamic attribute codes must not be empty",
        ));
    }
    Ok(())
}

fn validate_draft_id(id: &str) -> Result<(), AppError> {
    if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
        Err(AppError::usage("Vinted draft ID must contain only digits"))
    } else {
        Ok(())
    }
}

fn publication_body(
    operation: &PublicationOperation,
    input: ListingInput,
    photos: &[UploadedPhoto],
    upload_session_id: &str,
) -> Value {
    let wrapper = if matches!(operation, PublicationOperation::Publish) {
        "item"
    } else {
        "draft"
    };
    let id = draft_id(operation).map(ToOwned::to_owned);
    let ListingInput {
        title,
        description,
        catalog_id,
        price,
        currency,
        package_size_id,
        brand_id,
        brand,
        isbn,
        is_unisex,
        ai_photo,
        color_ids,
        measurement_length,
        measurement_width,
        item_attributes,
        manufacturer,
        manufacturer_labelling,
        shipment_prices,
        parcel,
    } = input;
    let item = json!({
        "id": id,
        "currency": currency,
        "temp_uuid": Uuid::new_v4().to_string(),
        "title": title,
        "description": description,
        "brand_id": brand_id,
        "brand": brand,
        "catalog_id": catalog_id,
        "isbn": isbn,
        "is_unisex": is_unisex,
        "ai_photo": ai_photo,
        "price": price,
        "package_size_id": package_size_id,
        "shipment_prices": shipment_prices.unwrap_or(ShipmentPricesInput { domestic: None, international: None }),
        "color_ids": color_ids,
        "assigned_photos": photos.iter().map(|photo| json!({"id": photo.id, "orientation": photo.orientation})).collect::<Vec<_>>(),
        "measurement_length": measurement_length,
        "measurement_width": measurement_width,
        "item_attributes": item_attributes,
        "manufacturer": manufacturer,
        "manufacturer_labelling": manufacturer_labelling
    });
    let mut body = serde_json::Map::new();
    body.insert(wrapper.to_owned(), item);
    body.insert("push_up".to_owned(), Value::Null);
    body.insert(
        "parcel".to_owned(),
        serde_json::to_value(parcel).expect("parcel serializes"),
    );
    body.insert(
        "upload_session_id".to_owned(),
        Value::String(upload_session_id.to_owned()),
    );
    Value::Object(body)
}

fn operation_endpoint(operation: &PublicationOperation) -> (Method, String) {
    match operation {
        PublicationOperation::CreateDraft => (Method::POST, "item_upload/drafts".to_owned()),
        PublicationOperation::UpdateDraft { draft_id } => {
            (Method::PUT, format!("item_upload/drafts/{draft_id}"))
        }
        PublicationOperation::CompleteDraft { draft_id } => (
            Method::POST,
            format!("item_upload/drafts/{draft_id}/completion"),
        ),
        PublicationOperation::Publish => (Method::POST, "item_upload/items".to_owned()),
        PublicationOperation::DeleteDraft { draft_id } => {
            (Method::DELETE, format!("item_upload/drafts/{draft_id}"))
        }
    }
}

fn draft_id(operation: &PublicationOperation) -> Option<&str> {
    match operation {
        PublicationOperation::UpdateDraft { draft_id }
        | PublicationOperation::CompleteDraft { draft_id }
        | PublicationOperation::DeleteDraft { draft_id } => Some(draft_id),
        PublicationOperation::CreateDraft | PublicationOperation::Publish => None,
    }
}

fn decode_photo_response(response: TransportResponse) -> Result<UploadedPhoto, AppError> {
    let status = response.status;
    let value = bounded_json(response)?;
    if !status.is_success() {
        return Err(upstream_error(status, &value, "image upload"));
    }
    let id = value
        .get("id")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_response("image upload"))?;
    let width = value
        .get("width")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_response("image upload"))?;
    let height = value
        .get("height")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| invalid_response("image upload"))?;
    let orientation = value
        .get("orientation")
        .and_then(Value::as_u64)
        .and_then(|value| u16::try_from(value).ok())
        .unwrap_or(0);
    Ok(UploadedPhoto {
        id,
        orientation,
        width,
        height,
    })
}

fn decode_mutation_response(response: TransportResponse) -> Result<Value, AppError> {
    let status = response.status;
    let value = serde_json::from_slice(&response.body).map_err(|error| {
        invalid_response("publication")
            .with_details(json!({
                "mutation_response": "malformed",
                "http_status": status.as_u16()
            }))
            .with_source(error)
    })?;
    if status.is_success() {
        Ok(value)
    } else {
        Err(upstream_error(status, &value, "publication"))
    }
}

fn bounded_json(response: TransportResponse) -> Result<Value, AppError> {
    serde_json::from_slice(&response.body)
        .map_err(|error| invalid_response("publication").with_source(error))
}

fn normalize_result(
    operation: &PublicationOperation,
    uploaded_images: usize,
    response: &Value,
) -> Result<PublicationResult, AppError> {
    let draft_id = response
        .pointer("/draft/id")
        .and_then(value_as_id)
        .or_else(|| {
            matches!(operation, PublicationOperation::UpdateDraft { .. })
                .then(|| draft_id(operation).unwrap().to_owned())
        });
    let item_id = response.pointer("/item/id").and_then(value_as_id);
    let after_upload_actions = response
        .get("after_upload_actions")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let operation_name = match operation {
        PublicationOperation::CreateDraft => "create_draft",
        PublicationOperation::UpdateDraft { .. } => "update_draft",
        PublicationOperation::CompleteDraft { .. } => "complete_draft",
        PublicationOperation::Publish => "publish",
        PublicationOperation::DeleteDraft { .. } => "delete_draft",
    };
    if matches!(operation, PublicationOperation::CreateDraft) && draft_id.is_none() {
        return Err(invalid_response("draft creation"));
    }
    if matches!(
        operation,
        PublicationOperation::Publish | PublicationOperation::CompleteDraft { .. }
    ) && item_id.is_none()
    {
        return Err(invalid_response("publication"));
    }
    Ok(PublicationResult {
        operation: operation_name,
        draft_id,
        item_id,
        after_upload_actions,
        uploaded_images,
    })
}

fn value_as_id(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .or_else(|| value.as_u64().map(|value| value.to_string()))
}

fn upstream_error(status: StatusCode, value: &Value, stage: &str) -> AppError {
    let response_code = value.get("code").cloned().unwrap_or(Value::Null);
    let message_code = value.get("message_code").and_then(Value::as_str);
    let message = value
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("Vinted rejected the request");
    let gate = message_code.unwrap_or_default().to_ascii_lowercase();
    let (error_code, exit_class) = if gate.contains("verification") || gate.contains("verify") {
        (
            "vinted.publication_verification_required",
            ExitClass::Validation,
        )
    } else if gate.contains("confirmation") || gate.contains("confirm") {
        (
            "vinted.publication_confirmation_required",
            ExitClass::Validation,
        )
    } else if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        (
            "vinted.publication_authentication_required",
            ExitClass::Authentication,
        )
    } else if status == StatusCode::BAD_REQUEST || status == StatusCode::UNPROCESSABLE_ENTITY {
        (
            "vinted.publication_validation_failed",
            ExitClass::Validation,
        )
    } else {
        ("vinted.publication_failed", ExitClass::Upstream)
    };
    let mut error = AppError::new(
        if stage == "publication" {
            error_code.to_owned()
        } else {
            format!("vinted.{}_failed", stage.replace(' ', "_"))
        },
        message,
        exit_class,
    );
    error.details = Some(Box::new(json!({
        "http_status": status.as_u16(),
        "response_code": response_code,
        "message_code": message_code,
        "field_errors": value
            .get("errors")
            .or_else(|| value.get("field_errors"))
            .cloned()
            .unwrap_or(Value::Null)
    })));
    if stage == "publication"
        && (error.exit_class == ExitClass::Authentication
            || error.code == "vinted.publication_verification_required")
    {
        error.next_actions.push(NextAction {
            command: crate::invocation::vinted_fi("auth login"),
        });
    }
    error.safe_to_retry = stage == "image upload" && status.is_server_error();
    error.upstream_transient = status.is_server_error();
    error
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
enum MutationStatus {
    NotAttempted,
    ConfirmedRejected,
    Unknown,
}

fn classify_mutation_error(
    error: AppError,
    operation: &PublicationOperation,
    photos: &[UploadedPhoto],
) -> AppError {
    let status = error
        .details
        .as_deref()
        .and_then(|details| details.get("http_status"))
        .and_then(Value::as_u64);
    let malformed = error
        .details
        .as_deref()
        .and_then(|details| details.get("mutation_response"))
        .is_some();
    let phase = error
        .details
        .as_deref()
        .and_then(|details| details.get("transport_phase"))
        .and_then(Value::as_str);
    let kind = error
        .details
        .as_deref()
        .and_then(|details| details.get("transport_kind"))
        .and_then(Value::as_str);
    let mutation_status = if status.is_some_and(|status| (400..500).contains(&status)) && !malformed
    {
        MutationStatus::ConfirmedRejected
    } else if phase == Some("request") && kind == Some("connection") {
        MutationStatus::NotAttempted
    } else {
        MutationStatus::Unknown
    };
    publication_error(error, operation, photos, mutation_status)
}

fn publication_error(
    mut error: AppError,
    operation: &PublicationOperation,
    photos: &[UploadedPhoto],
    mutation_status: MutationStatus,
) -> AppError {
    let assignments_reusable = !photos.is_empty();
    error.safe_to_retry = matches!(
        mutation_status,
        MutationStatus::NotAttempted | MutationStatus::ConfirmedRejected
    ) && (photos.is_empty() || assignments_reusable);
    let (resource, state_explanation) = mutation_state(operation, mutation_status);
    error.partial = Some(Box::new(json!({
        "uploaded_photos": photos,
        "uploaded_photo_assignments_reusable": assignments_reusable,
        "mutation_status": mutation_status,
        "remote_state": {
            "resource": resource,
            "may_have_changed": mutation_status == MutationStatus::Unknown,
            "explanation": state_explanation
        }
    })));
    error
}

fn mutation_state(
    operation: &PublicationOperation,
    status: MutationStatus,
) -> (&'static str, &'static str) {
    let resource = match operation {
        PublicationOperation::CreateDraft
        | PublicationOperation::UpdateDraft { .. }
        | PublicationOperation::DeleteDraft { .. } => "draft",
        PublicationOperation::Publish => "listing",
        PublicationOperation::CompleteDraft { .. } => "draft_or_listing",
    };
    let explanation = match status {
        MutationStatus::NotAttempted => {
            "The publication mutation was not attempted, so the draft or listing did not change through this operation."
        }
        MutationStatus::ConfirmedRejected => {
            "Vinted confirmed that it rejected the publication mutation, so the draft or listing did not change through this operation."
        }
        MutationStatus::Unknown => {
            "The publication mutation outcome is unknown, so the draft or listing state may have changed. Inspect remote state before retrying."
        }
    };
    (resource, explanation)
}

fn invalid_response(stage: &str) -> AppError {
    AppError::upstream(
        "vinted.invalid_response",
        format!("Vinted returned an invalid {stage} response"),
    )
}

fn insert_header(
    headers: &mut reqwest::header::HeaderMap,
    name: &'static str,
    value: &'static str,
) {
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    );
}

fn transport_error(error: TransportError) -> AppError {
    let phase = match error.phase {
        crate::transport::TransportErrorPhase::Request => "request",
        crate::transport::TransportErrorPhase::Response => "response",
    };
    let kind = match error.kind {
        TransportErrorKind::Timeout => "timeout",
        TransportErrorKind::Connection => "connection",
        TransportErrorKind::ResponseTooLarge => "response_too_large",
        TransportErrorKind::Other => "other",
    };
    let mut app_error = AppError::upstream("vinted.transport_failed", "Vinted request failed")
        .with_details(json!({ "transport_phase": phase, "transport_kind": kind }))
        .with_source(error);
    app_error.upstream_transient = true;
    app_error.safe_to_retry = false;
    app_error
}

fn mutation_execution_error(error: TransportError) -> AppError {
    if error.kind == TransportErrorKind::ResponseTooLarge {
        let status = error.status.map(|status| status.as_u16());
        invalid_response("publication")
            .with_details(json!({
                "mutation_response": "malformed",
                "http_status": status,
                "transport_phase": "response",
                "transport_kind": "response_too_large"
            }))
            .with_source(error)
    } else {
        transport_error(error)
    }
}

fn execution_error(error: TransportError) -> AppError {
    if error.kind == TransportErrorKind::ResponseTooLarge {
        invalid_response("publication")
    } else {
        transport_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> ListingInput {
        ListingInput {
            title: "Known item".to_owned(),
            description: "Truthful description".to_owned(),
            catalog_id: 12,
            price: "5.00".to_owned(),
            currency: "EUR".to_owned(),
            package_size_id: 3,
            brand_id: None,
            brand: None,
            isbn: None,
            is_unisex: false,
            ai_photo: false,
            color_ids: vec![4],
            measurement_length: None,
            measurement_width: None,
            item_attributes: vec![ItemAttributeInput {
                code: "condition".to_owned(),
                ids: vec![6],
            }],
            manufacturer: None,
            manufacturer_labelling: None,
            shipment_prices: None,
            parcel: None,
        }
    }

    #[test]
    fn publication_body_preserves_photo_order_and_dynamic_attributes() {
        let photos = vec![
            UploadedPhoto {
                id: 9,
                orientation: 0,
                width: 100,
                height: 200,
            },
            UploadedPhoto {
                id: 7,
                orientation: 90,
                width: 200,
                height: 100,
            },
        ];
        let body = publication_body(&PublicationOperation::Publish, input(), &photos, "session");
        assert_eq!(body.pointer("/item/assigned_photos/0/id"), Some(&json!(9)));
        assert_eq!(body.pointer("/item/assigned_photos/1/id"), Some(&json!(7)));
        assert_eq!(
            body.pointer("/item/item_attributes/0/code"),
            Some(&json!("condition"))
        );
        assert_eq!(body.pointer("/upload_session_id"), Some(&json!("session")));
    }

    #[test]
    fn draft_completion_uses_draft_wrapper_and_identifier() {
        let body = publication_body(
            &PublicationOperation::CompleteDraft {
                draft_id: "42".to_owned(),
            },
            input(),
            &[UploadedPhoto {
                id: 9,
                orientation: 0,
                width: 100,
                height: 200,
            }],
            "session",
        );
        assert_eq!(body.pointer("/draft/id"), Some(&json!("42")));
        assert!(body.get("item").is_none());
    }

    #[test]
    fn rejects_publication_without_images() {
        let error =
            validate_operation(&PublicationOperation::Publish, Some(&input()), &[]).unwrap_err();
        assert_eq!(error.code, "vinted.images_required");
    }

    #[test]
    fn endpoint_paths_match_publication_contract() {
        assert_eq!(
            operation_endpoint(&PublicationOperation::CreateDraft),
            (Method::POST, "item_upload/drafts".to_owned())
        );
        assert_eq!(
            operation_endpoint(&PublicationOperation::CompleteDraft {
                draft_id: "8".to_owned()
            }),
            (Method::POST, "item_upload/drafts/8/completion".to_owned())
        );
    }

    fn response(status: StatusCode, body: Value) -> TransportResponse {
        TransportResponse {
            status,
            headers: reqwest::header::HeaderMap::new(),
            body: serde_json::to_vec(&body).unwrap(),
        }
    }

    fn classify(error: AppError) -> AppError {
        classify_mutation_error(error, &PublicationOperation::Publish, &[])
    }

    #[test]
    fn validation_response_is_a_confirmed_rejection_with_upstream_fields() {
        let value = json!({
            "code": 100,
            "message_code": "validation_error",
            "message": "Tarkista kentät",
            "errors": {
                "brand": ["Merkki ei voi olla tyhjä"],
                "color_ids": ["Väri ei voi olla tyhjä"]
            }
        });
        let error = classify(
            decode_mutation_response(response(StatusCode::BAD_REQUEST, value)).unwrap_err(),
        );

        assert_eq!(error.code, "vinted.publication_validation_failed");
        assert_eq!(error.exit_class, ExitClass::Validation);
        assert!(error.safe_to_retry);
        let details = error.details.unwrap();
        assert_eq!(details["http_status"], 400);
        assert_eq!(details["response_code"], 100);
        assert_eq!(details["message_code"], "validation_error");
        assert_eq!(
            details["field_errors"]["brand"][0],
            "Merkki ei voi olla tyhjä"
        );
        let partial = error.partial.unwrap();
        assert_eq!(partial["mutation_status"], "confirmed_rejected");
        assert_eq!(partial["remote_state"]["may_have_changed"], false);
    }

    #[test]
    fn authentication_response_is_a_confirmed_rejection_with_login_action() {
        let error = classify(
            decode_mutation_response(response(
                StatusCode::UNAUTHORIZED,
                json!({"message": "Kirjaudu sisään"}),
            ))
            .unwrap_err(),
        );

        assert_eq!(error.code, "vinted.publication_authentication_required");
        assert_eq!(error.exit_class, ExitClass::Authentication);
        assert_eq!(
            error.partial.unwrap()["mutation_status"],
            "confirmed_rejected"
        );
        assert_eq!(
            error.next_actions[0].command,
            crate::invocation::vinted_fi("auth login")
        );
    }

    #[test]
    fn verification_gate_has_a_dedicated_error_and_action() {
        let error = classify(
            decode_mutation_response(response(
                StatusCode::FORBIDDEN,
                json!({
                    "code": 55,
                    "message_code": "phone_verification_required",
                    "message": "Vahvista puhelinnumerosi"
                }),
            ))
            .unwrap_err(),
        );

        assert_eq!(error.code, "vinted.publication_verification_required");
        assert_eq!(
            error.partial.unwrap()["mutation_status"],
            "confirmed_rejected"
        );
    }

    #[test]
    fn verification_gate_on_validation_status_has_a_dedicated_error() {
        let error = classify(
            decode_mutation_response(response(
                StatusCode::BAD_REQUEST,
                json!({
                    "message_code": "phone_verification_required",
                    "message": "Vahvista puhelinnumerosi"
                }),
            ))
            .unwrap_err(),
        );

        assert_eq!(error.code, "vinted.publication_verification_required");
        assert_eq!(
            error.next_actions[0].command,
            crate::invocation::vinted_fi("auth login")
        );
    }

    #[test]
    fn confirmation_gate_has_a_dedicated_error() {
        let error = classify(
            decode_mutation_response(response(
                StatusCode::BAD_REQUEST,
                json!({
                    "message_code": "email_confirmation_required",
                    "message": "Vahvista sähköpostiosoitteesi"
                }),
            ))
            .unwrap_err(),
        );

        assert_eq!(error.code, "vinted.publication_confirmation_required");
        assert_eq!(
            error.partial.unwrap()["mutation_status"],
            "confirmed_rejected"
        );
    }

    #[test]
    fn timeout_after_mutation_start_has_an_unknown_outcome() {
        let error = classify(transport_error(TransportError::request(
            TransportErrorKind::Timeout,
        )));

        assert_eq!(error.partial.unwrap()["mutation_status"], "unknown");
        assert!(!error.safe_to_retry);
    }

    #[test]
    fn connection_failure_before_send_is_not_attempted() {
        let error = classify(transport_error(TransportError::request(
            TransportErrorKind::Connection,
        )));

        assert_eq!(error.partial.unwrap()["mutation_status"], "not_attempted");
        assert!(error.safe_to_retry);
    }

    #[test]
    fn response_transport_failure_has_an_unknown_outcome() {
        let error = classify(mutation_execution_error(TransportError::response(
            TransportErrorKind::ResponseTooLarge,
            StatusCode::OK,
        )));

        assert_eq!(error.partial.unwrap()["mutation_status"], "unknown");
        assert!(!error.safe_to_retry);
    }

    #[test]
    fn malformed_response_has_an_unknown_outcome() {
        let error = classify(
            decode_mutation_response(TransportResponse {
                status: StatusCode::OK,
                headers: reqwest::header::HeaderMap::new(),
                body: b"not json".to_vec(),
            })
            .unwrap_err(),
        );

        assert_eq!(error.code, "vinted.invalid_response");
        assert_eq!(error.partial.unwrap()["mutation_status"], "unknown");
        assert!(!error.safe_to_retry);
    }

    #[test]
    fn successful_response_is_returned_for_normalization() {
        let value = json!({"item": {"id": 42}});
        assert_eq!(
            decode_mutation_response(response(StatusCode::OK, value.clone())).unwrap(),
            value
        );
    }

    #[test]
    fn safe_retry_requires_a_known_mutation_outcome_even_with_reusable_photos() {
        let photo = UploadedPhoto {
            id: 9,
            orientation: 0,
            width: 100,
            height: 200,
        };
        let rejected = publication_error(
            AppError::validation("rejected", "rejected"),
            &PublicationOperation::Publish,
            std::slice::from_ref(&photo),
            MutationStatus::ConfirmedRejected,
        );
        let unknown = publication_error(
            AppError::upstream("unknown", "unknown"),
            &PublicationOperation::Publish,
            &[photo],
            MutationStatus::Unknown,
        );

        assert!(rejected.safe_to_retry);
        assert_eq!(
            rejected.partial.unwrap()["uploaded_photo_assignments_reusable"],
            true
        );
        assert!(!unknown.safe_to_retry);
        assert_eq!(
            unknown.partial.unwrap()["uploaded_photo_assignments_reusable"],
            true
        );
    }

    #[test]
    fn test_api_base_url_can_be_rebound() {
        let api =
            HttpVintedPublicationApi::new().with_api_base_url("http://127.0.0.1:9".to_owned());
        assert_eq!(
            api.endpoint("photos").unwrap().as_str(),
            "http://127.0.0.1:9/api/v2/photos"
        );
    }
}
