use super::delivery::{
    invalid_delivery_api, normalize_delivery_composer, normalize_delivery_composer_with_limit,
    shipping_products, shipping_unavailable,
};
use super::http::{
    AdInputProtocol, ApiError, HttpRequest, HttpResponse, Method, RequestBody, RetryPolicy,
};
use super::normalization::{
    normalize_authoritative_draft_state, normalize_draft_state, normalize_publication_categories,
    normalize_publication_draft, normalize_publication_draft_with_limit,
    normalize_source_draft_state, validate_resource_id,
};
use super::types::{
    CategoryPrediction, ComposerModelStatus, Confirmation, DeliveryComposer, DraftState,
    ImageState, ListingDraftSeed, ProductContext, Publication, PublicationCategory,
    PublicationDraftState, UploadedImage, model_error,
};
use crate::domain::commerce::normalized_select_to_machine;
use crate::domain::observation::Observation;
use crate::domain::observation::ObservationOperation;
use crate::domain::observation::ObservationSource;
use crate::domain::observation::ObservationState;
use crate::domain::observation::SourceStateEvidence;
use crate::domain::observation::StatusEvidence;
use crate::marketplace::tori::client::compatibility;
use crate::marketplace::tori::listings::observation::ListingObservations;
use crate::retry::FailureKind;
use crate::retry::OperationMethod;
use crate::retry::RetryContext;
use crate::retry::classify;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;

mod delivery;
mod draft_mutation;
mod draft_read;
mod images;
mod listing_observation;
mod publication;

#[allow(async_fn_in_trait)]
pub trait DraftRead: Send + Sync {
    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError>;
    async fn publication_draft(&self, draft_id: &str) -> Result<PublicationDraftState, ApiError> {
        self.get_draft(draft_id)
            .await
            .map(|draft| PublicationDraftState {
                draft,
                composer_model: ComposerModelStatus::Available,
            })
    }
    async fn publication_draft_for_inspection(
        &self,
        draft_id: &str,
        _include_all_options: bool,
    ) -> Result<PublicationDraftState, ApiError> {
        self.publication_draft(draft_id).await
    }
    async fn category_predictions(
        &self,
        draft_id: &str,
    ) -> Result<Vec<CategoryPrediction>, ApiError>;
    async fn publication_categories(&self) -> Result<Vec<PublicationCategory>, ApiError>;
}

#[allow(async_fn_in_trait)]
pub trait DraftMutation: Send + Sync {
    async fn create_draft(&self) -> Result<DraftState, ApiError>;
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
    async fn patch_item_fields(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<String, ApiError>;
    async fn update_recommerce(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<DraftState, ApiError>;
    async fn delete_draft(&self, draft_id: &str) -> Result<(), ApiError>;
}

#[allow(async_fn_in_trait)]
pub trait DraftImages: Send + Sync {
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
}

#[allow(async_fn_in_trait)]
pub trait DraftDeliveryApi: Send + Sync {
    async fn delivery_composer(&self, draft_id: &str) -> Result<DeliveryComposer, ApiError>;
    async fn delivery_composer_for_inspection(
        &self,
        draft_id: &str,
        _include_all_options: bool,
    ) -> Result<DeliveryComposer, ApiError> {
        self.delivery_composer(draft_id).await
    }
    async fn apply_delivery(
        &self,
        draft_id: &str,
        composer: &DeliveryComposer,
        delivery: &str,
    ) -> Result<(), ApiError>;
}

#[allow(async_fn_in_trait)]
pub trait DraftPublication: Send + Sync {
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
    async fn confirmation(&self, publication: &Publication) -> Result<Confirmation, ApiError>;
    async fn track_confirmation(&self, confirmation: &Confirmation) -> Result<(), ApiError>;
}

#[allow(async_fn_in_trait)]
pub trait DraftListingObservation: Send + Sync {
    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError>;
    async fn active_listing(&self, listing_id: &str) -> Result<Option<Value>, ApiError>;
    async fn observed_listing(&self, listing_id: &str) -> Result<Value, ApiError>;
}

pub trait AdInputApi:
    DraftRead
    + DraftMutation
    + DraftImages
    + DraftDeliveryApi
    + DraftPublication
    + DraftListingObservation
{
}

impl<T> AdInputApi for T where
    T: DraftRead
        + DraftMutation
        + DraftImages
        + DraftDeliveryApi
        + DraftPublication
        + DraftListingObservation
{
}

pub struct HttpAdInputApi<T> {
    transport: T,
}

impl<T> HttpAdInputApi<T> {
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }
}

fn unrecognized_read(mut error: ApiError, source: ObservationSource, status: u16) -> ApiError {
    error = error.with_observation(
        Observation::unrecognized_response(source, Some(status)),
        ObservationOperation::Read,
    );
    error
}

impl<T: AdInputProtocol> HttpAdInputApi<T> {
    async fn json(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        debug_assert!(!request.method.is_mutation() || request.retry == RetryPolicy::Never);
        let retry_context = request.retry_context();
        let source = request.observation_source;
        let response = self.transport.execute(request).await?;
        if (200..300).contains(&response.status) {
            Ok(response)
        } else {
            Err(ApiError::response(&response, retry_context, source))
        }
    }

    async fn draft_request(
        &self,
        request: HttpRequest,
        require_authoritative_model: bool,
    ) -> Result<DraftState, ApiError> {
        let is_mutation = request.method.is_mutation();
        let source = request.observation_source;
        let response = self.json(request).await?;
        if response.body_is_unparseable {
            let operation = if is_mutation {
                ObservationOperation::Mutation
            } else {
                ObservationOperation::Read
            };
            let mut error = unexpected_representation("receive_draft_state", &response)
                .with_observation(
                    Observation::unrecognized_response(source, Some(response.status)),
                    operation,
                );
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
            let operation = if is_mutation {
                ObservationOperation::Mutation
            } else {
                ObservationOperation::Read
            };
            let observation = Observation::unrecognized_response(source, Some(response.status));
            let classification = observation.retry_classification(operation);
            error.upstream_transient = classification.upstream_transient;
            error.safe_to_retry = classification.safe_to_retry;
            error.observation = Some(Box::new(observation));
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

    async fn reconcile_missing_draft(&self, draft_id: &str, detail_error: ApiError) -> ApiError {
        let detail_observation =
            detail_error
                .observation
                .as_deref()
                .cloned()
                .unwrap_or_else(|| {
                    Observation::confirmed_absent(ObservationSource::DraftDetail, Some(404))
                });
        match ListingObservations::new(&self.transport)
            .find_summary(draft_id)
            .await
        {
            Ok(Some(summary)) => {
                let collection = Observation::confirmed_present(
                    ObservationSource::AuthenticatedListingCollection,
                    Some(200),
                );
                let observation = Observation::reconcile(&[detail_observation, collection])
                    .expect("draft lifecycle observations are non-empty");
                let state = summary
                    .pointer("/state/type")
                    .or_else(|| summary.get("state"))
                    .and_then(Value::as_str);
                let mut error = ApiError::new(
                    "draft.observation_conflict",
                    "Draft lifecycle sources disagree about whether the draft is present",
                )
                .with_observation(observation, ObservationOperation::Read);
                error.status = Some(404);
                error.details = Some(Box::new(json!({
                    "draft_id": draft_id,
                    "detail_status": "not_found",
                    "collection_status": "present",
                    "collection_state": state,
                })));
                error
            }
            Ok(None) => {
                let collection = Observation::confirmed_absent(
                    ObservationSource::AuthenticatedListingCollection,
                    Some(200),
                );
                let source_states = [&detail_observation, &collection]
                    .into_iter()
                    .map(|observation| SourceStateEvidence {
                        source: observation.source.clone(),
                        state: observation.state,
                    })
                    .collect();
                let observation = Observation::new(
                    ObservationState::ConfirmedAbsent,
                    "draft_lifecycle_reconciliation",
                    StatusEvidence {
                        http_status: Some(404),
                        response_received: true,
                        model_parsed: false,
                        source_states,
                    },
                );
                let mut error = detail_error;
                error.observation = Some(Box::new(observation));
                error
            }
            Err(collection_error) => {
                let collection_observation = collection_error
                    .observation
                    .map(|value| *value)
                    .unwrap_or_else(|| {
                        Observation::unrecognized_response(
                            ObservationSource::AuthenticatedListingCollection,
                            None,
                        )
                    });
                let mut error = ApiError::new(
                    "draft.observation_incomplete",
                    "Draft absence could not be confirmed against the authenticated collection",
                )
                .with_observation(collection_observation, ObservationOperation::Read);
                error.status = Some(404);
                error.details = Some(Box::new(json!({
                    "draft_id": draft_id,
                    "detail_status": "not_found",
                    "collection_status": "unavailable",
                    "detail_observation": detail_observation,
                })));
                error
            }
        }
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
                    "Inspect the draft with `flea tori draft show {draft_id}`; do not repeat creation"
                )
            })));
            error
        })
    }
}

fn observed_detail_matches(value: &Value, listing_id: &str) -> bool {
    match value.get("id").or_else(|| value.get("listing_id")) {
        Some(Value::String(value)) => value == listing_id,
        Some(Value::Number(value)) => value.to_string() == listing_id,
        _ => false,
    }
}

fn listing_is_active(value: &Value) -> bool {
    let state = value.get("state");
    let candidates = [
        state.and_then(Value::as_str),
        state
            .and_then(|state| state.get("type"))
            .and_then(Value::as_str),
        state
            .and_then(|state| state.get("display"))
            .and_then(Value::as_str),
        state
            .and_then(|state| state.get("label"))
            .and_then(Value::as_str),
    ];
    candidates.into_iter().flatten().any(|state| {
        matches!(
            state.trim().to_ascii_lowercase().as_str(),
            "active" | "published"
        )
    })
}

fn public_listing_url(listing_id: &str) -> String {
    format!("https://www.tori.fi/recommerce/forsale/item/{listing_id}")
}

fn listing_not_copyable(listing_id: &str, reason: &str, status: Option<u16>) -> ApiError {
    let eligibility =
        Observation::confirmed_absent(ObservationSource::ListingCopyEligibility, status);
    let listing_presence = status.map(|_| {
        Observation::confirmed_present(ObservationSource::AuthenticatedListingCollection, Some(200))
    });
    let mut error = ApiError::new(
        "listing.not_copyable",
        "Only listings in the authenticated seller's listing collection can be copied",
    )
    .with_observation(eligibility.clone(), ObservationOperation::Read);
    error.status = status;
    error.details = Some(Box::new(json!({
        "listing_id": listing_id,
        "source_scope": "authenticated_seller_listings",
        "reason": reason,
        "listing_presence": listing_presence,
        "copy_eligibility": eligibility,
        "remote_draft_allocated": false,
    })));
    error
}

fn copy_source_error(mut error: ApiError, listing_id: &str) -> ApiError {
    let copy_eligibility = error.observation.clone();
    let upstream = error.details.take().map(|details| *details);
    error.details = Some(Box::new(json!({
        "listing_id": listing_id,
        "source_scope": "authenticated_seller_listings",
        "listing_presence": Observation::confirmed_present(
            ObservationSource::AuthenticatedListingCollection,
            Some(200),
        ),
        "copy_eligibility": copy_eligibility,
        "remote_draft_allocated": false,
        "upstream_error": upstream,
    })));
    error
}

fn malformed_copy_source(listing_id: &str, status: u16) -> ApiError {
    let eligibility =
        Observation::unrecognized_response(ObservationSource::ListingCopyEligibility, Some(status));
    let mut error = malformed_read_response("source_listing", ObservationSource::ListingDetail)
        .with_observation(eligibility.clone(), ObservationOperation::Read);
    error.status = Some(status);
    error.details = Some(Box::new(json!({
        "listing_id": listing_id,
        "source_scope": "authenticated_seller_listings",
        "listing_presence": Observation::confirmed_present(
            ObservationSource::AuthenticatedListingCollection,
            Some(200),
        ),
        "copy_eligibility": eligibility,
        "remote_draft_allocated": false,
    })));
    error
}

pub(super) fn validate_price(price: &Value) -> Result<(), ApiError> {
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

fn item_creation_fields(values: &Map<String, Value>) -> Result<Map<String, Value>, ApiError> {
    let mut data = Map::new();
    for key in ["title", "description"] {
        if let Some(value) = values.get(key).filter(|value| !value.is_null()) {
            data.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(condition) = values.get("condition").filter(|value| !value.is_null()) {
        data.insert("condition".to_owned(), numeric_string(condition));
    }
    if let Some(price) = values.get("price") {
        validate_price(price)?;
        if price.as_f64().is_some_and(|amount| amount > 0.0) {
            data.insert("price".to_owned(), json!({ "price_amount": price }));
        }
    }
    let excluded = [
        "category",
        "delivery",
        "image",
        "location",
        "multi_image",
        "price",
        "postal_code",
        "postal-code",
        "postalCode",
        "title",
        "description",
        "condition",
        "trade_type",
        "revision",
    ];
    for (key, value) in values {
        if excluded.contains(&key.as_str())
            || !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
        {
            continue;
        }
        data.insert(key.clone(), numeric_string(value));
    }
    Ok(data)
}

fn numeric_string(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|value| value.parse::<i64>().ok())
        .map_or_else(|| value.clone(), |value| json!(value))
}

fn composer_values(values: &Map<String, Value>) -> Result<Map<String, Value>, ApiError> {
    let mut encoded = values.clone();
    if let Some(postal_code) = encoded.remove("postal_code") {
        let locations = encoded
            .entry("location".to_owned())
            .or_insert_with(|| Value::Array(vec![Value::Object(Map::new())]))
            .as_array_mut()
            .ok_or_else(|| {
                ApiError::new(
                    "upstream.unrecognized_model",
                    "Tori returned an unsupported location representation",
                )
            })?;
        if locations.is_empty() {
            locations.push(Value::Object(Map::new()));
        }
        let location = locations[0].as_object_mut().ok_or_else(|| {
            ApiError::new(
                "upstream.unrecognized_model",
                "Tori returned an unsupported location representation",
            )
        })?;
        location.insert("postal-code".to_owned(), postal_code);
    }
    if let Some(trade_type) = encoded.get_mut("trade_type") {
        *trade_type = normalized_select_to_machine("trade_type", trade_type).ok_or_else(|| {
            ApiError::new(
                "draft.invalid_trade_type",
                "Trade type must be sell, give_away, or wanted",
            )
        })?;
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

pub(super) fn error_at_stage(mut error: ApiError, stage: &str) -> ApiError {
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

fn uncertain_item_update(
    response: &HttpResponse,
    stage: &str,
    path: &str,
    reason: &str,
) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The item patch may have succeeded, but its resulting revision is unknown",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": stage,
        "path": path,
        "status": response.status,
        "content_type": response.content_type,
        "reason": reason,
    })));
    error
}

fn normalize_item_update(
    response: HttpResponse,
    draft_id: &str,
    stage: &str,
) -> Result<String, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_item_update(
            &response,
            stage,
            "$",
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
            stage,
            "$.id",
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
                stage,
                "$.etag",
                "successful response did not contain an authoritative ETag",
            )
        })
}

fn normalize_recommerce_update(
    response: HttpResponse,
    draft_id: &str,
) -> Result<DraftState, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_mutation_model(
            &response,
            "update_recommerce",
            "$",
            "successful response was not valid JSON",
        ));
    }
    let body = if response.body.get("ad").is_some() {
        response.body.clone()
    } else {
        json!({ "ad": response.body.clone(), "model": { "sections": [] } })
    };
    normalize_source_draft_state(body, response.etag.as_deref())
        .and_then(|state| {
            (state.draft_id == draft_id)
                .then_some(state)
                .ok_or_else(|| {
                    model_error(
                        "update_recommerce",
                        "$.id",
                        "successful response identified a different draft",
                    )
                })
        })
        .map_err(|error| {
            let path = error
                .details
                .as_deref()
                .and_then(|details| details.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("$");
            uncertain_mutation_model(&response, "update_recommerce", path, &error.message)
        })
}

fn uncertain_mutation_model(
    response: &HttpResponse,
    stage: &str,
    path: &str,
    reason: &str,
) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The mutation may have succeeded, but its resulting state is unknown",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": stage,
        "path": path,
        "reason": reason,
        "status": response.status,
    })));
    error
}

fn normalize_product_context(
    body: Value,
    draft_id: &str,
    revision: &str,
) -> Result<ProductContext, ApiError> {
    let id = body.get("id").and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    });
    if id.as_deref() != Some(draft_id) {
        return Err(read_model_error(
            "product_context",
            "$.id",
            "product context identified a different draft",
        ));
    }
    let basic_package_urn = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| {
            choices.iter().find_map(|choice| {
                (choice.get("package-identifier").and_then(Value::as_i64) == Some(10))
                    .then(|| choice.get("specification-urn").and_then(Value::as_str))
                    .flatten()
            })
        })
        .filter(|urn| *urn == "urn:product:package-specification:10")
        .ok_or_else(|| {
            read_model_error(
                "product_context",
                "$.choices[*].specification-urn",
                "free Basic package is unavailable",
            )
        })?;
    Ok(ProductContext {
        revision: revision.to_owned(),
        basic_package_urn: basic_package_urn.to_owned(),
    })
}

fn normalize_publication(
    response: HttpResponse,
    draft_id: &str,
    revision: &str,
) -> Result<Publication, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_mutation_model(
            &response,
            "package_choice",
            "$",
            "successful response was not valid JSON",
        ));
    }
    let order_id = response
        .body
        .get("order-id")
        .and_then(|value| match value {
            Value::String(value) if !value.is_empty() => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .ok_or_else(|| {
            uncertain_mutation_model(
                &response,
                "package_choice",
                "$.order-id",
                "successful response omitted the order identity",
            )
        })?;
    if response.body.get("is-completed").and_then(Value::as_bool) != Some(true) {
        return Err(uncertain_mutation_model(
            &response,
            "package_choice",
            "$.is-completed",
            "package order completion is unavailable",
        ));
    }
    Ok(Publication {
        listing_id: draft_id.to_owned(),
        revision: revision.to_owned(),
        state: "pending".to_owned(),
        order_id,
    })
}

fn read_model_error(stage: &str, path: &str, reason: &str) -> ApiError {
    let mut error = model_error(stage, path, reason).retry_classification(classify(
        FailureKind::MalformedSuccess,
        RetryContext::read(OperationMethod::Get),
    ));
    error.code = "upstream.unexpected_response".to_owned();
    error
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

pub(super) fn malformed_read_response(stage: &str, source: ObservationSource) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an invalid success response",
    )
    .with_observation(
        Observation::unrecognized_response(source, Some(200)),
        ObservationOperation::Read,
    );
    error.details = Some(Box::new(json!({ "stage": stage })));
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

pub(super) fn draft_id_from_body(body: &Value) -> Option<String> {
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

pub(super) fn valid_image_location(location: &str) -> Option<String> {
    let parsed = url::Url::parse(location).ok()?;
    (parsed.scheme() == "https"
        && parsed.host_str() == Some("img.tori.net")
        && parsed.path().starts_with("/dynamic/default/")
        && parsed.query().is_none()
        && parsed.fragment().is_none())
    .then(|| location.to_owned())
}
