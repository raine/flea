use std::{future::Future, pin::Pin};

use reqwest::{Method, StatusCode, header::ETAG};
use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use crate::{
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{
        PortalId,
        vinted::{
            auth::{VintedAuthentication, VintedCredentialRecord},
            binding::VINTED_FI_BINDING,
            search::VintedSearchSession,
        },
    },
    transport::{Transport, TransportError, TransportErrorKind, TransportResponse},
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
pub const DEFAULT_PAGE_SIZE: u16 = 20;
pub const MAX_PAGE_SIZE: u16 = 100;
const MAX_DRAFT_ID_BYTES: usize = 20;
const MAX_BLOCKERS: usize = 100;
const MAX_PHOTOS: usize = 20;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DraftListRequest {
    pub page: u32,
    pub per_page: u16,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DraftDocument {
    pub body: Value,
    pub revision: Option<String>,
}

pub trait VintedDraftApi: Send + Sync {
    fn list<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a DraftListRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DraftDocument, AppError>> + Send + 'a>>;

    fn show<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        draft_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<DraftDocument, AppError>> + Send + 'a>>;
}

pub struct HttpVintedDraftApi {
    auth: VintedAuthentication,
    api_base_url: String,
}

impl HttpVintedDraftApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            api_base_url: VINTED_FI_BINDING.api_host.to_owned(),
        }
    }

    async fn request(
        &self,
        credentials: &VintedCredentialRecord,
        path: &str,
        query: &[(&str, String)],
        draft_id: Option<&str>,
    ) -> Result<DraftDocument, AppError> {
        let mut url = Url::parse(&self.api_base_url).map_err(|error| {
            AppError::unexpected("Vinted API binding is invalid").with_source(error)
        })?;
        url.set_path(path);
        if !query.is_empty() {
            url.query_pairs_mut()
                .extend_pairs(query.iter().map(|(key, value)| (*key, value.as_str())));
        }
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
            .map_err(|error| execution_error(error, draft_id))?;
        decode_response(response, draft_id)
    }
}

impl Default for HttpVintedDraftApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedDraftApi for HttpVintedDraftApi {
    fn list<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a DraftListRequest,
    ) -> Pin<Box<dyn Future<Output = Result<DraftDocument, AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.request(
                credentials,
                &format!("/api/v2/wardrobe/{}/items", credentials.user_id),
                &[
                    ("cond", "draft".to_owned()),
                    ("page", request.page.to_string()),
                    ("per_page", request.per_page.to_string()),
                    ("order", "newest_first".to_owned()),
                ],
                None,
            )
            .await
        })
    }

    fn show<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        draft_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<DraftDocument, AppError>> + Send + 'a>> {
        Box::pin(async move {
            self.request(
                credentials,
                &format!("/api/v2/item_upload/items/{draft_id}"),
                &[],
                Some(draft_id),
            )
            .await
        })
    }
}

pub struct VintedDrafts<'a> {
    session: &'a dyn VintedSearchSession,
    api: &'a dyn VintedDraftApi,
}

impl<'a> VintedDrafts<'a> {
    pub fn new(session: &'a dyn VintedSearchSession, api: &'a dyn VintedDraftApi) -> Self {
        Self { session, api }
    }

    pub async fn list(
        &self,
        portal: PortalId,
        request: DraftListRequest,
    ) -> Result<VintedDraftCollection, AppError> {
        validate_page(&request)?;
        let credentials = self.session.credentials(portal)?;
        let document = self.api.list(&credentials, &request).await?;
        normalize_collection(&document.body, &request)
    }

    pub async fn show(
        &self,
        portal: PortalId,
        draft_id: &str,
    ) -> Result<VintedDraftState, AppError> {
        validate_draft_id(draft_id)?;
        let credentials = self.session.credentials(portal)?;
        let document = self.api.show(&credentials, draft_id).await?;
        normalize_draft(document, draft_id)
    }

    pub async fn validate(
        &self,
        portal: PortalId,
        draft_id: &str,
    ) -> Result<VintedDraftValidation, AppError> {
        let draft = self.show(portal, draft_id).await?;
        Ok(validate_remote_draft(&draft))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VintedDraftCollection {
    pub drafts: Vec<VintedDraftSummary>,
    pub pagination: VintedDraftPagination,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VintedDraftSummary {
    pub draft_id: String,
    pub title: Option<String>,
    pub price: Option<Value>,
    pub brand: Option<Value>,
    pub photo_count: usize,
    pub first_photo_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VintedDraftPagination {
    pub page: u32,
    pub per_page: u16,
    pub total_pages: Option<u32>,
    pub total_entries: Option<u64>,
    pub has_more: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct VintedDraftState {
    pub draft_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub category_id: Option<String>,
    pub attributes: Value,
    pub brand: Option<Value>,
    pub colors: Vec<Value>,
    pub price: Option<Value>,
    pub package_size_id: Option<String>,
    pub assigned_photos: Vec<VintedAssignedPhoto>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parcel: Option<Value>,
    pub publication_blockers: Vec<VintedDraftBlocker>,
    pub editable_state: Value,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VintedAssignedPhoto {
    pub photo_id: String,
    pub display_order: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub orientation: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VintedDraftBlockerClass {
    LocalSchema,
    UpstreamValidation,
    AccountPrerequisite,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VintedDraftBlocker {
    pub class: VintedDraftBlockerClass,
    pub field: String,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VintedDraftValidation {
    pub draft_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    pub ready: bool,
    pub blockers: Vec<VintedDraftBlocker>,
}

fn validate_page(request: &DraftListRequest) -> Result<(), AppError> {
    if request.page == 0 || request.per_page == 0 || request.per_page > MAX_PAGE_SIZE {
        return Err(AppError::usage(format!(
            "Draft pagination requires a positive page and a limit from 1 to {MAX_PAGE_SIZE}"
        )));
    }
    Ok(())
}

pub(crate) fn validate_draft_id(draft_id: &str) -> Result<(), AppError> {
    if draft_id.is_empty()
        || draft_id.len() > MAX_DRAFT_ID_BYTES
        || !draft_id.bytes().all(|byte| byte.is_ascii_digit())
        || draft_id.parse::<u64>().ok().filter(|id| *id > 0).is_none()
    {
        return Err(AppError::validation(
            "vinted_draft.invalid_id",
            "Vinted draft ID must be a positive numeric ID",
        )
        .with_details(serde_json::json!({ "draft_id": draft_id })));
    }
    Ok(())
}

pub(crate) fn normalize_collection(
    body: &Value,
    request: &DraftListRequest,
) -> Result<VintedDraftCollection, AppError> {
    let root = body.get("data").unwrap_or(body);
    let items = root
        .get("items")
        .or_else(|| root.get("drafts"))
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected_response("draft collection did not contain an item array"))?;
    if items.len() > usize::from(request.per_page) {
        return Err(unexpected_response(
            "draft collection exceeded the requested page size",
        ));
    }
    let drafts = items
        .iter()
        .filter(|item| item.get("is_draft").and_then(Value::as_bool) != Some(false))
        .map(|item| {
            let draft_id = id(item.get("id"))
                .ok_or_else(|| unexpected_response("draft summary ID was unavailable"))?;
            validate_draft_id(&draft_id)
                .map_err(|_| unexpected_response("draft summary ID was invalid"))?;
            let photos = photo_values(item);
            Ok(VintedDraftSummary {
                draft_id,
                title: text(item.get("title")),
                price: item.get("price").filter(|value| !value.is_null()).cloned(),
                brand: item.get("brand").filter(|value| !value.is_null()).cloned(),
                photo_count: photos.len(),
                first_photo_id: photos.first().and_then(|photo| photo_id(photo)),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let pagination = root.get("pagination").and_then(Value::as_object);
    let page = pagination
        .and_then(|value| number_u32(value, &["current_page", "page"]))
        .unwrap_or(request.page);
    let per_page = pagination
        .and_then(|value| number_u16(value, &["per_page"]))
        .unwrap_or(request.per_page);
    if page != request.page || per_page > MAX_PAGE_SIZE {
        return Err(unexpected_response(
            "draft collection pagination did not match the request",
        ));
    }
    let total_pages = pagination.and_then(|value| number_u32(value, &["total_pages"]));
    let total_entries = pagination.and_then(|value| number_u64(value, &["total_entries", "total"]));
    let has_more = total_pages
        .map(|total| page < total)
        .unwrap_or(drafts.len() == usize::from(request.per_page));
    Ok(VintedDraftCollection {
        drafts,
        pagination: VintedDraftPagination {
            page,
            per_page,
            total_pages,
            total_entries,
            has_more,
        },
    })
}

pub(crate) fn normalize_draft(
    document: DraftDocument,
    expected_id: &str,
) -> Result<VintedDraftState, AppError> {
    let root = document.body.get("data").unwrap_or(&document.body);
    let item = root
        .get("item")
        .or_else(|| root.get("draft"))
        .unwrap_or(root)
        .as_object()
        .ok_or_else(|| unexpected_response("draft detail did not contain an editable object"))?;
    let returned_id =
        id(item.get("id")).ok_or_else(|| unexpected_response("draft detail ID was unavailable"))?;
    if returned_id != expected_id || item.get("is_draft").and_then(Value::as_bool) == Some(false) {
        return Err(not_found(expected_id));
    }

    let assigned_photos = normalize_photos(item)?;
    let revision = document.revision.or_else(|| {
        ["revision", "version", "updated_at"]
            .into_iter()
            .find_map(|key| scalar_text(item.get(key)))
    });
    let attributes = item
        .get("item_attributes")
        .or_else(|| item.get("attributes"))
        .cloned()
        .unwrap_or_else(|| Value::Array(Vec::new()));
    let colors = item
        .get("color_ids")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            [item.get("color1_id"), item.get("color2_id")]
                .into_iter()
                .flatten()
                .filter(|value| !value.is_null())
                .cloned()
                .collect()
        });
    let validation_keys = ["validation_errors", "upstream_validation_errors", "errors"];
    let account_keys = ["account_prerequisites", "account_blockers", "prerequisites"];
    let mut upstream_blockers = collect_blockers(
        root,
        &validation_keys,
        VintedDraftBlockerClass::UpstreamValidation,
    );
    let mut account_blockers = collect_blockers(
        root,
        &account_keys,
        VintedDraftBlockerClass::AccountPrerequisite,
    );
    if document.body.get("data").is_some() {
        upstream_blockers.extend(collect_blockers(
            &document.body,
            &validation_keys,
            VintedDraftBlockerClass::UpstreamValidation,
        ));
        account_blockers.extend(collect_blockers(
            &document.body,
            &account_keys,
            VintedDraftBlockerClass::AccountPrerequisite,
        ));
    }
    if let Some(alert) = item.get("item_alert").filter(|value| !value.is_null())
        && alert_is_account_prerequisite(alert)
    {
        account_blockers.push(blocker_from_value(
            alert,
            VintedDraftBlockerClass::AccountPrerequisite,
            "account",
        ));
    }
    if root.get("verification_required").and_then(Value::as_bool) == Some(true)
        || document
            .body
            .get("verification_required")
            .and_then(Value::as_bool)
            == Some(true)
    {
        account_blockers.push(VintedDraftBlocker {
            class: VintedDraftBlockerClass::AccountPrerequisite,
            field: "account.verification".to_owned(),
            code: "verification_required".to_owned(),
            message: "Account verification is required before publication".to_owned(),
        });
    }

    let mut draft = VintedDraftState {
        draft_id: returned_id,
        revision,
        title: text(item.get("title")),
        description: text(item.get("description")),
        category_id: id(item.get("catalog_id").or_else(|| item.get("category_id"))),
        attributes,
        brand: item
            .get("brand_dto")
            .or_else(|| item.get("brand"))
            .or_else(|| item.get("brand_id"))
            .filter(|value| !value.is_null())
            .cloned(),
        colors,
        price: item.get("price").filter(|value| !value.is_null()).cloned(),
        package_size_id: id(item.get("package_size_id")),
        assigned_photos,
        parcel: root
            .get("parcel")
            .or_else(|| document.body.get("parcel"))
            .filter(|value| !value.is_null())
            .cloned(),
        publication_blockers: Vec::new(),
        editable_state: Value::Object(item.clone()),
    };
    draft.publication_blockers = schema_blockers(&draft);
    draft.publication_blockers.extend(upstream_blockers);
    draft.publication_blockers.extend(account_blockers);
    draft.publication_blockers.truncate(MAX_BLOCKERS);
    Ok(draft)
}

pub(crate) fn validate_remote_draft(draft: &VintedDraftState) -> VintedDraftValidation {
    VintedDraftValidation {
        draft_id: draft.draft_id.clone(),
        revision: draft.revision.clone(),
        ready: draft.publication_blockers.is_empty(),
        blockers: draft.publication_blockers.clone(),
    }
}

fn schema_blockers(draft: &VintedDraftState) -> Vec<VintedDraftBlocker> {
    let mut blockers = Vec::new();
    required(&mut blockers, "title", draft.title.is_some());
    required(&mut blockers, "description", draft.description.is_some());
    required(&mut blockers, "catalog_id", draft.category_id.is_some());
    required(&mut blockers, "price", valid_price(draft.price.as_ref()));
    required(
        &mut blockers,
        "package_size_id",
        draft.package_size_id.is_some() || draft.parcel.is_some(),
    );
    required(
        &mut blockers,
        "assigned_photos",
        !draft.assigned_photos.is_empty(),
    );
    blockers
}

fn required(blockers: &mut Vec<VintedDraftBlocker>, field: &str, present: bool) {
    if !present {
        blockers.push(VintedDraftBlocker {
            class: VintedDraftBlockerClass::LocalSchema,
            field: field.to_owned(),
            code: "required".to_owned(),
            message: format!("{field} is required for publication"),
        });
    }
}

fn valid_price(price: Option<&Value>) -> bool {
    let Some(price) = price else { return false };
    let amount = price
        .as_str()
        .or_else(|| price.get("amount").and_then(Value::as_str))
        .and_then(|value| value.parse::<f64>().ok())
        .or_else(|| price.as_f64())
        .or_else(|| price.get("amount").and_then(Value::as_f64));
    amount.is_some_and(|amount| amount.is_finite() && amount > 0.0)
}

fn normalize_photos(item: &Map<String, Value>) -> Result<Vec<VintedAssignedPhoto>, AppError> {
    let photos = photo_values_map(item);
    if photos.len() > MAX_PHOTOS {
        return Err(unexpected_response(
            "draft contained too many assigned photos",
        ));
    }
    photos
        .into_iter()
        .enumerate()
        .map(|(display_order, photo)| {
            let photo_id = photo_id(photo)
                .ok_or_else(|| unexpected_response("assigned photo ID was unavailable"))?;
            Ok(VintedAssignedPhoto {
                photo_id,
                display_order,
                orientation: photo.get("orientation").and_then(Value::as_u64),
            })
        })
        .collect()
}

fn photo_values(item: &Value) -> Vec<&Value> {
    item.as_object().map(photo_values_map).unwrap_or_default()
}

fn photo_values_map(item: &Map<String, Value>) -> Vec<&Value> {
    item.get("assigned_photos")
        .or_else(|| item.get("photos"))
        .and_then(Value::as_array)
        .map(|values| values.iter().collect())
        .unwrap_or_default()
}

fn photo_id(photo: &Value) -> Option<String> {
    id(photo.get("id").or_else(|| photo.get("photo_id")))
}

fn collect_blockers(
    root: &Value,
    keys: &[&str],
    class: VintedDraftBlockerClass,
) -> Vec<VintedDraftBlocker> {
    keys.iter()
        .filter_map(|key| root.get(key))
        .flat_map(|value| match value {
            Value::Array(values) => values.iter().collect::<Vec<_>>(),
            Value::Object(values) => values.values().collect(),
            _ => vec![value],
        })
        .take(MAX_BLOCKERS)
        .map(|value| blocker_from_value(value, class, "draft"))
        .collect()
}

fn blocker_from_value(
    value: &Value,
    class: VintedDraftBlockerClass,
    default_field: &str,
) -> VintedDraftBlocker {
    let field = ["field", "field_name", "path"]
        .into_iter()
        .find_map(|key| text(value.get(key)))
        .unwrap_or_else(|| default_field.to_owned());
    let code = ["code", "message_code", "type"]
        .into_iter()
        .find_map(|key| scalar_text(value.get(key)))
        .unwrap_or_else(|| match class {
            VintedDraftBlockerClass::UpstreamValidation => "upstream_rejected".to_owned(),
            VintedDraftBlockerClass::AccountPrerequisite => "account_required".to_owned(),
            VintedDraftBlockerClass::LocalSchema => "invalid".to_owned(),
        });
    let message = ["message", "reason", "title"]
        .into_iter()
        .find_map(|key| text(value.get(key)))
        .or_else(|| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| code.clone());
    VintedDraftBlocker {
        class,
        field,
        code,
        message,
    }
}

fn alert_is_account_prerequisite(value: &Value) -> bool {
    ["type", "code", "message_code"]
        .into_iter()
        .filter_map(|key| scalar_text(value.get(key)))
        .any(|value| {
            let value = value.to_ascii_lowercase();
            ["verification", "account", "kyc", "email", "phone", "tax"]
                .iter()
                .any(|needle| value.contains(needle))
        })
}

fn decode_response(
    response: TransportResponse,
    draft_id: Option<&str>,
) -> Result<DraftDocument, AppError> {
    if !response.status.is_success() {
        return Err(status_error(response.status, draft_id));
    }
    let revision = response
        .headers
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .map(str::to_owned);
    let body = serde_json::from_slice(&response.body)
        .map_err(|_| unexpected_response("response was not valid JSON"))?;
    Ok(DraftDocument { body, revision })
}

fn execution_error(error: TransportError, draft_id: Option<&str>) -> AppError {
    if let Some(status) = error.status
        && !status.is_success()
    {
        return status_error(status, draft_id);
    }
    if error.kind == TransportErrorKind::ResponseTooLarge {
        unexpected_response("response exceeded the size limit")
    } else {
        transport_error(error)
    }
}

fn transport_error(error: TransportError) -> AppError {
    let mut result = AppError::upstream(
        "vinted_draft.transport_failed",
        "Vinted draft state could not be reached",
    )
    .with_source(error);
    result.upstream_transient = true;
    result.safe_to_retry = true;
    result
}

fn status_error(status: StatusCode, draft_id: Option<&str>) -> AppError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        let mut error = AppError::authentication(
            "vinted_draft.authentication_required",
            "Vinted draft inspection requires a valid authenticated session",
        );
        error.next_actions.push(NextAction {
            command: crate::invocation::vinted_fi("auth login"),
        });
        return error;
    }
    if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
        return not_found(draft_id.unwrap_or("unknown"));
    }
    let mut error = AppError::new(
        "vinted_draft.upstream_failed",
        format!("Vinted draft inspection returned HTTP {}", status.as_u16()),
        ExitClass::Upstream,
    );
    error.upstream_transient = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
    error.safe_to_retry = error.upstream_transient;
    error
}

fn not_found(draft_id: &str) -> AppError {
    AppError::validation(
        "vinted_draft.not_found",
        "Vinted draft was not found or has been deleted",
    )
    .with_details(serde_json::json!({ "draft_id": draft_id }))
}

fn unexpected_response(reason: &str) -> AppError {
    AppError::upstream(
        "vinted_draft.unexpected_response",
        "Vinted returned an unsupported draft response",
    )
    .with_details(serde_json::json!({ "reason": reason }))
}

fn id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn text(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

fn scalar_text(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| match value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}

fn number_u64(object: &Map<String, Value>, keys: &[&str]) -> Option<u64> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_u64))
}

fn number_u32(object: &Map<String, Value>, keys: &[&str]) -> Option<u32> {
    number_u64(object, keys).and_then(|value| u32::try_from(value).ok())
}

fn number_u16(object: &Map<String, Value>, keys: &[&str]) -> Option<u16> {
    number_u64(object, keys).and_then(|value| u16::try_from(value).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(item: Value) -> DraftDocument {
        DraftDocument {
            body: json!({ "item": item, "parcel": null }),
            revision: Some("revision-7".to_owned()),
        }
    }

    fn ready_item() -> Value {
        json!({
            "id": 42,
            "is_draft": true,
            "title": "Bicycle lock",
            "description": "Steel lock",
            "catalog_id": "123",
            "item_attributes": [{"code": "condition", "ids": [1]}],
            "brand_dto": {"id": 9, "title": "Brand"},
            "color1_id": "2",
            "price": {"amount": "15.00", "currency_code": "EUR"},
            "package_size_id": "1",
            "photos": [
                {"id": 700, "orientation": 0},
                {"id": 701, "orientation": 90}
            ]
        })
    }

    #[test]
    fn ready_draft_preserves_editable_state_and_photo_order() {
        let draft = normalize_draft(document(ready_item()), "42").unwrap();
        let report = validate_remote_draft(&draft);
        assert!(report.ready);
        assert!(report.blockers.is_empty());
        assert_eq!(draft.assigned_photos[0].photo_id, "700");
        assert_eq!(draft.assigned_photos[0].display_order, 0);
        assert_eq!(draft.assigned_photos[1].display_order, 1);
        assert_eq!(draft.revision.as_deref(), Some("revision-7"));
        assert_eq!(draft.editable_state["catalog_id"], "123");
    }

    #[test]
    fn incomplete_draft_reports_field_level_local_schema_blockers() {
        let draft = normalize_draft(
            document(json!({"id": 42, "is_draft": true, "photos": []})),
            "42",
        )
        .unwrap();
        let report = validate_remote_draft(&draft);
        assert!(!report.ready);
        assert!(
            report
                .blockers
                .iter()
                .all(|blocker| { blocker.class == VintedDraftBlockerClass::LocalSchema })
        );
        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.field == "title")
        );
        assert!(
            report
                .blockers
                .iter()
                .any(|blocker| blocker.field == "assigned_photos")
        );
    }

    #[test]
    fn missing_and_deleted_drafts_share_a_deterministic_error() {
        for status in [StatusCode::NOT_FOUND, StatusCode::GONE] {
            let error = status_error(status, Some("42"));
            assert_eq!(error.code, "vinted_draft.not_found");
            assert_eq!(error.exit_class, ExitClass::Validation);
            assert_eq!(error.details.as_deref(), Some(&json!({"draft_id": "42"})));
        }
        let error =
            normalize_draft(document(json!({"id": 42, "is_draft": false})), "42").unwrap_err();
        assert_eq!(error.code, "vinted_draft.not_found");
    }

    #[test]
    fn verification_blocker_is_an_account_prerequisite() {
        let mut item = ready_item();
        item["item_alert"] = json!({
            "type": "identity_verification_required",
            "message": "Verify the seller account"
        });
        let draft = normalize_draft(document(item), "42").unwrap();
        let report = validate_remote_draft(&draft);
        assert!(!report.ready);
        assert_eq!(report.blockers.len(), 1);
        assert_eq!(
            report.blockers[0].class,
            VintedDraftBlockerClass::AccountPrerequisite
        );
    }

    #[test]
    fn collection_obeys_requested_page_bounds() {
        let request = DraftListRequest {
            page: 2,
            per_page: 2,
        };
        let collection = normalize_collection(
            &json!({
                "items": [
                    {"id": 42, "is_draft": true, "title": "One", "photos": [{"id": 1}]},
                    {"id": "43", "is_draft": true, "title": "Two", "photos": []}
                ],
                "pagination": {
                    "current_page": 2,
                    "total_pages": 3,
                    "total_entries": 5,
                    "per_page": 2
                }
            }),
            &request,
        )
        .unwrap();
        assert_eq!(collection.drafts[0].draft_id, "42");
        assert_eq!(collection.drafts[0].first_photo_id.as_deref(), Some("1"));
        assert!(collection.pagination.has_more);

        let oversized = json!({"items": [{"id": 1}, {"id": 2}, {"id": 3}]});
        assert_eq!(
            normalize_collection(&oversized, &request).unwrap_err().code,
            "vinted_draft.unexpected_response"
        );
    }

    #[test]
    fn upstream_validation_errors_remain_distinct() {
        let draft = normalize_draft(
            DraftDocument {
                body: json!({
                    "item": ready_item(),
                    "validation_errors": [{
                        "field": "price",
                        "code": "price_too_low",
                        "message": "Price is below the category minimum"
                    }]
                }),
                revision: None,
            },
            "42",
        )
        .unwrap();
        let report = validate_remote_draft(&draft);
        assert_eq!(
            report.blockers[0].class,
            VintedDraftBlockerClass::UpstreamValidation
        );
    }
}
