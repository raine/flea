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
const MAX_UPLOAD_SESSION_BYTES: usize = 1024;
const MAX_SESSION_DISCOVERIES: usize = 2;

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub photo_action: Option<&'static str>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assigned_photo_ids: Vec<u64>,
}

pub trait VintedPublicationApi: Send + Sync {
    fn configuration<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;

    fn fetch_draft<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        draft_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;

    fn upload_photo<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        upload_session_id: &'a str,
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
                photo_action: None,
                assigned_photo_ids: Vec::new(),
            });
        }

        let mut prepared_images = Vec::with_capacity(image_paths.len());
        for path in image_paths {
            prepared_images.push(prepare_image(path).await.map_err(|error| {
                publication_error(error, &operation, &[], MutationStatus::NotAttempted)
            })?);
        }
        let input = input.expect("validated publication input");
        let temp_uuid = Uuid::new_v4().to_string();
        let remote_before = if let PublicationOperation::CompleteDraft { draft_id } = &operation {
            let photos = self
                .fetch_assigned_photos(&credentials, draft_id, &operation)
                .await?;
            if photos.is_empty() && prepared_images.is_empty() {
                return Err(publication_error(
                    empty_draft_photos(draft_id),
                    &operation,
                    &[],
                    MutationStatus::NotAttempted,
                ));
            }
            Some(photos)
        } else {
            None
        };

        for discovery in 0..MAX_SESSION_DISCOVERIES {
            let configuration = self
                .api
                .configuration(&credentials)
                .await
                .map_err(|error| {
                    publication_error(error, &operation, &[], MutationStatus::NotAttempted)
                })?;
            let upload_session_id = upload_session_id(&configuration).map_err(|error| {
                publication_error(error, &operation, &[], MutationStatus::NotAttempted)
            })?;
            let mut photos = Vec::with_capacity(prepared_images.len());
            let mut rejected_session = false;
            for prepared in &prepared_images {
                match self
                    .api
                    .upload_photo(&credentials, upload_session_id, prepared.clone())
                    .await
                {
                    Ok(photo) => photos.push(photo),
                    Err(error)
                        if discovery + 1 < MAX_SESSION_DISCOVERIES
                            && is_upload_session_rejection(&error) =>
                    {
                        rejected_session = true;
                        break;
                    }
                    Err(error) => {
                        return Err(publication_error(
                            error,
                            &operation,
                            &photos,
                            MutationStatus::NotAttempted,
                        ));
                    }
                }
            }
            if rejected_session {
                continue;
            }

            let (assigned_photos, uploaded_images, photo_action) =
                if let PublicationOperation::CompleteDraft { draft_id } = &operation {
                    if prepared_images.is_empty() {
                        (
                            remote_before.clone().expect("completion photos fetched"),
                            0,
                            "reused",
                        )
                    } else {
                        let replacement = PublicationOperation::UpdateDraft {
                            draft_id: draft_id.clone(),
                        };
                        let body = publication_body(
                            &replacement,
                            input.clone(),
                            &photos,
                            upload_session_id,
                            &temp_uuid,
                        );
                        match self
                            .api
                            .mutate(&credentials, &replacement, Some(body))
                            .await
                        {
                            Ok(_) => {}
                            Err(error)
                                if discovery + 1 < MAX_SESSION_DISCOVERIES
                                    && is_upload_session_rejection(&error) =>
                            {
                                continue;
                            }
                            Err(error) => {
                                return Err(classify_replacement_error(
                                    error,
                                    &replacement,
                                    remote_before.as_deref().unwrap_or_default(),
                                    &photos,
                                ));
                            }
                        }
                        let verified = self
                            .fetch_assigned_photos(&credentials, draft_id, &operation)
                            .await?;
                        verify_replacement(draft_id, &photos, &verified)?;
                        (verified, photos.len(), "replaced")
                    }
                } else {
                    (photos, prepared_images.len(), "uploaded")
                };

            let body = publication_body(
                &operation,
                input.clone(),
                &assigned_photos,
                upload_session_id,
                &temp_uuid,
            );
            match self.api.mutate(&credentials, &operation, Some(body)).await {
                Ok(response) => {
                    return normalize_result(
                        &operation,
                        uploaded_images,
                        Some(photo_action),
                        &assigned_photos,
                        &response,
                    );
                }
                Err(error)
                    if discovery + 1 < MAX_SESSION_DISCOVERIES
                        && is_upload_session_rejection(&error) =>
                {
                    continue;
                }
                Err(error) if is_upload_session_rejection(&error) => {
                    let error = publication_error(
                        error,
                        &operation,
                        &assigned_photos,
                        MutationStatus::ConfirmedRejected,
                    );
                    return Err(enrich_completion_error(
                        error,
                        &operation,
                        photo_action,
                        uploaded_images,
                    ));
                }
                Err(error) => {
                    let error = classify_mutation_error(error, &operation, &assigned_photos);
                    return Err(enrich_completion_error(
                        error,
                        &operation,
                        photo_action,
                        uploaded_images,
                    ));
                }
            }
        }
        Err(publication_error(
            upload_session_rejected(),
            &operation,
            &[],
            MutationStatus::ConfirmedRejected,
        ))
    }

    async fn fetch_assigned_photos(
        &self,
        credentials: &VintedCredentialRecord,
        draft_id: &str,
        operation: &PublicationOperation,
    ) -> Result<Vec<UploadedPhoto>, AppError> {
        let response = self
            .api
            .fetch_draft(credentials, draft_id)
            .await
            .map_err(|error| {
                publication_error(error, operation, &[], MutationStatus::NotAttempted)
            })?;
        decode_draft_photos(draft_id, &response)
            .map_err(|error| publication_error(error, operation, &[], MutationStatus::NotAttempted))
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

    async fn configuration_request(
        &self,
        credentials: &VintedCredentialRecord,
    ) -> Result<Value, AppError> {
        let url = self.endpoint("items/configuration")?;
        let request = self.auth.authenticated_request(
            Method::GET,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        let response = self
            .auth
            .executor()
            .execute(request)
            .await
            .map_err(execution_error)?;
        let status = response.status;
        let value = bounded_json(response)?;
        if status.is_success() {
            Ok(value)
        } else {
            Err(upstream_error(status, &value, "configuration"))
        }
    }

    async fn fetch_draft_request(
        &self,
        credentials: &VintedCredentialRecord,
        draft_id: &str,
    ) -> Result<Value, AppError> {
        let url = self.endpoint(&format!("item_upload/items/{draft_id}"))?;
        let request = self.auth.authenticated_request(
            Method::GET,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        let response = self
            .auth
            .executor()
            .execute(request)
            .await
            .map_err(execution_error)?;
        decode_draft_response(response)
    }

    async fn upload_photo_request(
        &self,
        credentials: &VintedCredentialRecord,
        upload_session_id: &str,
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
            MultipartPart::bytes("upload_session_id", upload_session_id.as_bytes().to_vec()),
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
    fn configuration<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.configuration_request(credentials))
    }

    fn fetch_draft<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        draft_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.fetch_draft_request(credentials, draft_id))
    }

    fn upload_photo<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        upload_session_id: &'a str,
        image: PreparedImage,
    ) -> Pin<Box<dyn Future<Output = Result<UploadedPhoto, AppError>> + Send + 'a>> {
        Box::pin(self.upload_photo_request(credentials, upload_session_id, image))
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

#[derive(Clone)]
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
    if image_paths.is_empty() && !matches!(operation, PublicationOperation::CompleteDraft { .. }) {
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

fn upload_session_id(configuration: &Value) -> Result<&str, AppError> {
    let Some(value) = configuration.get("upload_session_id") else {
        return Err(AppError::upstream(
            "vinted.upload_session_missing",
            "Vinted configuration did not provide an upload session",
        ));
    };
    let Some(session) = value.as_str() else {
        return Err(AppError::upstream(
            "vinted.upload_session_malformed",
            "Vinted configuration provided an invalid upload session",
        ));
    };
    if session.is_empty()
        || session.len() > MAX_UPLOAD_SESSION_BYTES
        || session.chars().any(char::is_control)
    {
        return Err(AppError::upstream(
            "vinted.upload_session_malformed",
            "Vinted configuration provided an invalid upload session",
        ));
    }
    Ok(session)
}

fn is_upload_session_rejection(error: &AppError) -> bool {
    error.code == "vinted.upload_session_rejected"
}

fn upload_session_rejected() -> AppError {
    let mut error = AppError::conflict(
        "vinted.upload_session_rejected",
        "Vinted rejected the upload session after one refresh",
    );
    error.safe_to_retry = true;
    error
}

fn publication_body(
    operation: &PublicationOperation,
    input: ListingInput,
    photos: &[UploadedPhoto],
    upload_session_id: &str,
    temp_uuid: &str,
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
        "temp_uuid": temp_uuid,
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

fn decode_draft_response(response: TransportResponse) -> Result<Value, AppError> {
    let status = response.status;
    let value = bounded_json(response)?;
    if status.is_success() {
        Ok(value)
    } else {
        Err(upstream_error(status, &value, "draft inspection"))
    }
}

fn decode_draft_photos(draft_id: &str, response: &Value) -> Result<Vec<UploadedPhoto>, AppError> {
    let item = response
        .get("item")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_response("draft inspection"))?;
    let returned_id = item.get("id").and_then(value_as_id);
    if returned_id.as_deref() != Some(draft_id) {
        return Err(AppError::conflict(
            "vinted.draft_identity_mismatch",
            format!("Vinted returned a different item while inspecting draft {draft_id}"),
        ));
    }
    if item.get("is_draft").and_then(Value::as_bool) == Some(false) {
        return Err(AppError::conflict(
            "vinted.not_a_draft",
            format!("Vinted item {draft_id} is not an editable draft"),
        ));
    }
    let values = item
        .get("photos")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("draft photo assignment"))?;
    let mut photos = Vec::with_capacity(values.len());
    let mut main_index = None;
    for (index, value) in values.iter().enumerate() {
        let id = value
            .get("id")
            .and_then(value_as_id)
            .and_then(|id| id.parse::<u64>().ok())
            .filter(|id| *id != 0)
            .ok_or_else(|| invalid_response("draft photo assignment"))?;
        if photos.iter().any(|photo: &UploadedPhoto| photo.id == id) {
            return Err(invalid_response("draft photo assignment"));
        }
        if value.get("is_main").and_then(Value::as_bool) == Some(true)
            && main_index.replace(index).is_some()
        {
            return Err(invalid_response("draft photo order"));
        }
        let width = value
            .get("width")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        let height = value
            .get("height")
            .and_then(Value::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(0);
        photos.push(UploadedPhoto {
            id,
            orientation: 0,
            width,
            height,
        });
    }
    if main_index.is_some_and(|index| index != 0) {
        return Err(invalid_response("draft photo order"));
    }
    Ok(photos)
}

fn verify_replacement(
    draft_id: &str,
    intended: &[UploadedPhoto],
    remote: &[UploadedPhoto],
) -> Result<(), AppError> {
    let intended_ids = intended.iter().map(|photo| photo.id).collect::<Vec<_>>();
    let remote_ids = remote.iter().map(|photo| photo.id).collect::<Vec<_>>();
    if intended_ids == remote_ids {
        return Ok(());
    }
    Err(AppError::partial(
        "vinted.photo_replacement_unverified",
        format!(
            "Vinted draft {draft_id} did not retain the complete replacement photo set in the requested order; inspect the draft before publishing"
        ),
        json!({
            "draft_id": draft_id,
            "intended_photo_ids": intended_ids,
            "remote_photo_ids": remote_ids,
            "final_mutation": "unattempted"
        }),
    ))
}

fn empty_draft_photos(draft_id: &str) -> AppError {
    AppError::validation(
        "vinted.draft_photos_required",
        format!(
            "Vinted draft {draft_id} has no assigned photos; pass one or more --image values to replace the complete photo set"
        ),
    )
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
    photo_action: Option<&'static str>,
    photos: &[UploadedPhoto],
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
    let operation_name = operation_name(operation);
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
        photo_action,
        assigned_photo_ids: photos.iter().map(|photo| photo.id).collect(),
    })
}

const fn operation_name(operation: &PublicationOperation) -> &'static str {
    match operation {
        PublicationOperation::CreateDraft => "create_draft",
        PublicationOperation::UpdateDraft { .. } => "update_draft",
        PublicationOperation::CompleteDraft { .. } => "complete_draft",
        PublicationOperation::Publish => "publish",
        PublicationOperation::DeleteDraft { .. } => "delete_draft",
    }
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
    let upstream_message = value.get("message").and_then(Value::as_str);
    if upload_session_rejection(status, message_code, upstream_message) {
        return upload_session_rejected();
    }
    let message = upstream_message.unwrap_or("Vinted rejected the request");
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

fn classify_replacement_error(
    error: AppError,
    operation: &PublicationOperation,
    previous_photos: &[UploadedPhoto],
    intended_photos: &[UploadedPhoto],
) -> AppError {
    let mut error = classify_mutation_error(error, operation, intended_photos);
    if let Some(partial) = error.partial.as_deref_mut().and_then(Value::as_object_mut) {
        partial.insert("draft_id".to_owned(), json!(draft_id(operation)));
        partial.insert(
            "previous_photo_ids".to_owned(),
            json!(
                previous_photos
                    .iter()
                    .map(|photo| photo.id)
                    .collect::<Vec<_>>()
            ),
        );
        partial.insert(
            "intended_photo_ids".to_owned(),
            json!(
                intended_photos
                    .iter()
                    .map(|photo| photo.id)
                    .collect::<Vec<_>>()
            ),
        );
        partial.insert("mutation".to_owned(), json!(operation_name(operation)));
    }
    error
}

fn enrich_completion_error(
    mut error: AppError,
    operation: &PublicationOperation,
    photo_action: &'static str,
    uploaded_images: usize,
) -> AppError {
    if matches!(operation, PublicationOperation::CompleteDraft { .. })
        && let Some(partial) = error.partial.as_deref_mut().and_then(Value::as_object_mut)
    {
        partial.insert("photo_action".to_owned(), json!(photo_action));
        partial.insert(
            "photo_assignment_status".to_owned(),
            json!("verified_remote"),
        );
        partial.insert("uploaded_images".to_owned(), json!(uploaded_images));
    }
    error
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

fn upload_session_rejection(
    status: StatusCode,
    message_code: Option<&str>,
    message: Option<&str>,
) -> bool {
    if !matches!(
        status,
        StatusCode::BAD_REQUEST | StatusCode::CONFLICT | StatusCode::UNPROCESSABLE_ENTITY
    ) {
        return false;
    }
    [message_code, message].into_iter().flatten().any(|value| {
        let normalized = value.to_ascii_lowercase().replace(['-', ' '], "_");
        normalized.contains("upload_session")
            && (normalized.contains("expired")
                || normalized.contains("invalid")
                || normalized.contains("reject"))
    })
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
    use std::{
        collections::VecDeque,
        sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        },
    };

    struct FixturePublicationApi {
        configurations: Mutex<VecDeque<Value>>,
        uploaded_sessions: Mutex<Vec<String>>,
        mutation_sessions: Mutex<Vec<String>>,
        rejected_mutations: AtomicUsize,
    }

    impl FixturePublicationApi {
        fn new(configurations: impl IntoIterator<Item = Value>, rejected_mutations: usize) -> Self {
            Self {
                configurations: Mutex::new(configurations.into_iter().collect()),
                uploaded_sessions: Mutex::new(Vec::new()),
                mutation_sessions: Mutex::new(Vec::new()),
                rejected_mutations: AtomicUsize::new(rejected_mutations),
            }
        }
    }

    impl VintedPublicationApi for FixturePublicationApi {
        fn configuration<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(self
                    .configurations
                    .lock()
                    .unwrap()
                    .pop_front()
                    .expect("fixture configuration"))
            })
        }

        fn fetch_draft<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            draft_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async move {
                Ok(json!({
                    "item": {
                        "id": draft_id,
                        "is_draft": true,
                        "photos": [{ "id": "9", "is_main": true }]
                    }
                }))
            })
        }

        fn upload_photo<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            upload_session_id: &'a str,
            _image: PreparedImage,
        ) -> Pin<Box<dyn Future<Output = Result<UploadedPhoto, AppError>> + Send + 'a>> {
            Box::pin(async move {
                self.uploaded_sessions
                    .lock()
                    .unwrap()
                    .push(upload_session_id.to_owned());
                Ok(UploadedPhoto {
                    id: 9,
                    orientation: 0,
                    width: 4,
                    height: 6,
                })
            })
        }

        fn mutate<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _operation: &'a PublicationOperation,
            body: Option<Value>,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async move {
                let session = body
                    .as_ref()
                    .and_then(|value| value.get("upload_session_id"))
                    .and_then(Value::as_str)
                    .expect("mutation session");
                self.mutation_sessions
                    .lock()
                    .unwrap()
                    .push(session.to_owned());
                if self
                    .rejected_mutations
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |remaining| {
                        remaining.checked_sub(1)
                    })
                    .is_ok()
                {
                    Err(upload_session_rejected())
                } else {
                    Ok(json!({"item": {"id": 71}}))
                }
            })
        }
    }

    struct CompletionApi {
        photos: Mutex<Vec<UploadedPhoto>>,
        uploads: AtomicUsize,
        updates: AtomicUsize,
        completions: AtomicUsize,
        rejected_completions: usize,
        reject_update: bool,
    }

    impl CompletionApi {
        fn new(photos: Vec<UploadedPhoto>, rejected_completions: usize) -> Self {
            Self {
                photos: Mutex::new(photos),
                uploads: AtomicUsize::new(0),
                updates: AtomicUsize::new(0),
                completions: AtomicUsize::new(0),
                rejected_completions,
                reject_update: false,
            }
        }

        fn rejecting_update(mut self) -> Self {
            self.reject_update = true;
            self
        }
    }

    impl VintedPublicationApi for CompletionApi {
        fn configuration<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async { Ok(json!({"upload_session_id": "server-session"})) })
        }

        fn fetch_draft<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            draft_id: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async move {
                let photos = self.photos.lock().unwrap();
                Ok(json!({
                    "item": {
                        "id": draft_id,
                        "is_draft": true,
                        "photos": photos.iter().enumerate().map(|(index, photo)| json!({
                            "id": photo.id.to_string(),
                            "width": photo.width,
                            "height": photo.height,
                            "is_main": index == 0
                        })).collect::<Vec<_>>()
                    }
                }))
            })
        }

        fn upload_photo<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            _upload_session_id: &'a str,
            _image: PreparedImage,
        ) -> Pin<Box<dyn Future<Output = Result<UploadedPhoto, AppError>> + Send + 'a>> {
            Box::pin(async move {
                let index = self.uploads.fetch_add(1, Ordering::SeqCst);
                Ok(photo(100 + index as u64))
            })
        }

        fn mutate<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            operation: &'a PublicationOperation,
            body: Option<Value>,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(async move {
                match operation {
                    PublicationOperation::UpdateDraft { draft_id } => {
                        self.updates.fetch_add(1, Ordering::SeqCst);
                        if self.reject_update {
                            return Err(AppError::upstream(
                                "vinted.transport_failed",
                                "replacement outcome is uncertain",
                            ));
                        }
                        let photos = body
                            .as_ref()
                            .unwrap()
                            .pointer("/draft/assigned_photos")
                            .unwrap()
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|value| photo(value["id"].as_u64().unwrap()))
                            .collect();
                        *self.photos.lock().unwrap() = photos;
                        Ok(json!({ "draft": { "id": draft_id } }))
                    }
                    PublicationOperation::CompleteDraft { .. } => {
                        let attempt = self.completions.fetch_add(1, Ordering::SeqCst);
                        if attempt < self.rejected_completions {
                            Err(AppError::validation(
                                "vinted.publication_validation_failed",
                                "brand and color require correction",
                            )
                            .with_details(json!({"http_status": 422})))
                        } else {
                            Ok(json!({ "item": { "id": "900" } }))
                        }
                    }
                    _ => Ok(json!({ "item": { "id": "900" } })),
                }
            })
        }
    }

    fn photo(id: u64) -> UploadedPhoto {
        UploadedPhoto {
            id,
            orientation: 0,
            width: 10,
            height: 20,
        }
    }

    fn completion() -> PublicationOperation {
        PublicationOperation::CompleteDraft {
            draft_id: "42".to_owned(),
        }
    }

    fn credentials() -> VintedCredentialRecord {
        VintedCredentialRecord::new_for_adapter(
            PortalId::Fi,
            "user".to_owned(),
            None,
            "access".to_owned(),
            "refresh".to_owned(),
            u64::MAX,
            "device".to_owned(),
            "anonymous".to_owned(),
            None,
        )
    }

    fn image_path() -> (tempfile::TempDir, PathBuf) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("fixture.png");
        image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
        (directory, path)
    }

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

    #[tokio::test]
    async fn workflow_uses_server_session_for_photos_and_mutation() {
        let (_directory, path) = image_path();
        let api = FixturePublicationApi::new([json!({"upload_session_id": "server-session"})], 0);
        let session = |_: PortalId| Ok(credentials());

        VintedPublication::new(&session, &api)
            .execute(
                PortalId::Fi,
                PublicationOperation::Publish,
                Some(input()),
                vec![path],
            )
            .await
            .unwrap();

        assert_eq!(*api.uploaded_sessions.lock().unwrap(), ["server-session"]);
        assert_eq!(*api.mutation_sessions.lock().unwrap(), ["server-session"]);
    }

    #[tokio::test]
    async fn rejected_session_is_rediscovered_once_for_the_whole_workflow() {
        let (_directory, path) = image_path();
        let api = FixturePublicationApi::new(
            [
                json!({"upload_session_id": "expired-session"}),
                json!({"upload_session_id": "refreshed-session"}),
            ],
            1,
        );
        let session = |_: PortalId| Ok(credentials());

        VintedPublication::new(&session, &api)
            .execute(
                PortalId::Fi,
                PublicationOperation::Publish,
                Some(input()),
                vec![path],
            )
            .await
            .unwrap();

        assert_eq!(
            *api.uploaded_sessions.lock().unwrap(),
            ["expired-session", "refreshed-session"]
        );
        assert_eq!(
            *api.mutation_sessions.lock().unwrap(),
            ["expired-session", "refreshed-session"]
        );
    }

    #[tokio::test]
    async fn repeated_session_rejection_stops_after_one_rediscovery() {
        let (_directory, path) = image_path();
        let api = FixturePublicationApi::new(
            [
                json!({"upload_session_id": "expired-session"}),
                json!({"upload_session_id": "rejected-session"}),
            ],
            2,
        );
        let session = |_: PortalId| Ok(credentials());

        let error = VintedPublication::new(&session, &api)
            .execute(
                PortalId::Fi,
                PublicationOperation::Publish,
                Some(input()),
                vec![path],
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted.upload_session_rejected");
        assert_eq!(api.mutation_sessions.lock().unwrap().len(), 2);
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
        let body = publication_body(
            &PublicationOperation::Publish,
            input(),
            &photos,
            "server-session/value",
            "temporary-item-id",
        );
        assert_eq!(body.pointer("/item/assigned_photos/0/id"), Some(&json!(9)));
        assert_eq!(body.pointer("/item/assigned_photos/1/id"), Some(&json!(7)));
        assert_eq!(
            body.pointer("/item/item_attributes/0/code"),
            Some(&json!("condition"))
        );
        assert_eq!(
            body.pointer("/upload_session_id"),
            Some(&json!("server-session/value"))
        );
        assert_eq!(
            body.pointer("/item/temp_uuid"),
            Some(&json!("temporary-item-id"))
        );
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
            "temporary-item-id",
        );
        assert_eq!(body.pointer("/draft/id"), Some(&json!("42")));
        assert!(body.get("item").is_none());
    }

    #[test]
    fn configuration_session_is_opaque_and_unchanged() {
        let configuration = json!({"upload_session_id": "server/value+with=padding"});
        assert_eq!(
            upload_session_id(&configuration).unwrap(),
            "server/value+with=padding"
        );
    }

    #[test]
    fn configuration_session_must_be_present() {
        let error = upload_session_id(&json!({})).unwrap_err();
        assert_eq!(error.code, "vinted.upload_session_missing");
    }

    #[test]
    fn configuration_session_must_be_a_bounded_non_control_string() {
        for configuration in [
            json!({"upload_session_id": null}),
            json!({"upload_session_id": 42}),
            json!({"upload_session_id": ""}),
            json!({"upload_session_id": "bad\nsession"}),
            json!({"upload_session_id": "x".repeat(MAX_UPLOAD_SESSION_BYTES + 1)}),
        ] {
            let error = upload_session_id(&configuration).unwrap_err();
            assert_eq!(error.code, "vinted.upload_session_malformed");
        }
    }

    #[test]
    fn identifies_expired_and_rejected_upload_sessions_without_echoing_them() {
        for (message_code, message) in [
            (Some("upload_session_expired"), None),
            (None, Some("Upload session is invalid: private-value")),
        ] {
            let error = upstream_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                &json!({"message_code": message_code, "message": message}),
                "publication",
            );
            assert_eq!(error.code, "vinted.upload_session_rejected");
            assert!(!error.message.contains("private-value"));
        }
    }

    #[test]
    fn rejects_direct_publication_without_images_but_allows_draft_reuse() {
        let error =
            validate_operation(&PublicationOperation::Publish, Some(&input()), &[]).unwrap_err();
        assert_eq!(error.code, "vinted.images_required");
        validate_operation(&completion(), Some(&input()), &[]).unwrap();
    }

    #[test]
    fn draft_photo_decoder_preserves_verified_remote_order() {
        let photos = decode_draft_photos(
            "42",
            &json!({
                "item": {
                    "id": "42",
                    "is_draft": true,
                    "photos": [
                        { "id": "19", "is_main": true },
                        { "id": 7, "is_main": false }
                    ]
                }
            }),
        )
        .unwrap();
        assert_eq!(
            photos.iter().map(|photo| photo.id).collect::<Vec<_>>(),
            [19, 7]
        );
    }

    #[tokio::test]
    async fn corrected_validation_attempt_reuses_persisted_replacement_without_uploading_again() {
        let api = CompletionApi::new(vec![photo(9)], 1);
        let session = |_| Ok(credentials());
        let workflow = VintedPublication::new(&session, &api);
        let (_directory, path) = image_path();

        let first = workflow
            .execute(PortalId::Fi, completion(), Some(input()), vec![path])
            .await
            .unwrap_err();
        assert_eq!(first.code, "vinted.publication_validation_failed");
        assert_eq!(first.partial.as_ref().unwrap()["photo_action"], "replaced");
        assert_eq!(
            first.partial.as_ref().unwrap()["photo_assignment_status"],
            "verified_remote"
        );
        assert_eq!(first.partial.as_ref().unwrap()["uploaded_images"], 1);
        assert_eq!(api.uploads.load(Ordering::SeqCst), 1);
        assert_eq!(api.updates.load(Ordering::SeqCst), 1);

        let result = workflow
            .execute(PortalId::Fi, completion(), Some(input()), Vec::new())
            .await
            .unwrap();
        assert_eq!(result.photo_action, Some("reused"));
        assert_eq!(result.uploaded_images, 0);
        assert_eq!(result.assigned_photo_ids, [100]);
        assert_eq!(api.uploads.load(Ordering::SeqCst), 1);
        assert_eq!(api.completions.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn explicit_images_are_reported_as_verified_replace_all() {
        let api = CompletionApi::new(vec![photo(9), photo(8)], 0);
        let session = |_| Ok(credentials());
        let (_directory, path) = image_path();
        let result = VintedPublication::new(&session, &api)
            .execute(PortalId::Fi, completion(), Some(input()), vec![path])
            .await
            .unwrap();

        assert_eq!(result.photo_action, Some("replaced"));
        assert_eq!(result.uploaded_images, 1);
        assert_eq!(result.assigned_photo_ids, [100]);
    }

    #[tokio::test]
    async fn empty_remote_draft_stops_before_final_mutation() {
        let api = CompletionApi::new(Vec::new(), 0);
        let session = |_| Ok(credentials());
        let error = VintedPublication::new(&session, &api)
            .execute(PortalId::Fi, completion(), Some(input()), Vec::new())
            .await
            .unwrap_err();

        assert_eq!(error.code, "vinted.draft_photos_required");
        assert!(error.message.contains("--image"));
        assert_eq!(api.completions.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn uncertain_replacement_preserves_old_and_intended_assignments() {
        let api = CompletionApi::new(vec![photo(9)], 0).rejecting_update();
        let session = |_| Ok(credentials());
        let (_directory, path) = image_path();
        let error = VintedPublication::new(&session, &api)
            .execute(PortalId::Fi, completion(), Some(input()), vec![path])
            .await
            .unwrap_err();
        let partial = error.partial.unwrap();

        assert_eq!(partial["previous_photo_ids"], json!([9]));
        assert_eq!(partial["intended_photo_ids"], json!([100]));
        assert_eq!(partial["mutation"], "update_draft");
        assert_eq!(partial["mutation_status"], "unknown");
        assert_eq!(api.completions.load(Ordering::SeqCst), 0);
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
