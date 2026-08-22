use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    sync::Arc,
};

use reqwest::{Method, StatusCode, header::HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    api::client::{RequestSpec, ToriClient, compatibility},
    domain::listing::{
        Category, CategoryList, ListingAction, ListingActionName, ListingCollection,
        ListingCopySource, ListingDetail, ListingFacet, ListingMutation, ListingRef,
        ListingSnapshot, ListingState, ListingStatistics, ListingSummary,
    },
    error::{AppError, ExitClass},
};

pub const LISTING_PAGE_SIZE: usize = 50;
const MAX_LISTING_PAGES: usize = 10_000;

pub trait ListingsApi: Send + Sync {
    fn categories(&self) -> Result<Vec<UpstreamCategory>, ListingsApiError>;
    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<UpstreamListingPage, ListingsApiError>;
    fn listing(&self, listing_id: &str) -> Result<UpstreamListing, ListingsApiError>;
    fn update_listing(
        &self,
        listing_id: &str,
        etag: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<UpstreamListing, ListingsApiError>;
    fn dispose_listing(&self, listing_id: &str) -> Result<(), ListingsApiError>;
    fn delete_listing(&self, listing_id: &str) -> Result<(), ListingsApiError>;
}

pub struct HttpListingsApi {
    client: Arc<dyn ToriClient>,
}

impl HttpListingsApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        body: Option<Value>,
        etag: Option<&str>,
    ) -> Result<T, ListingsApiError> {
        let mut request = RequestSpec::new(method, path, compatibility::SERVICE_AD_SUMMARIES);
        if let Some(body) = body {
            request = request.body(
                serde_json::to_vec(&body)
                    .map_err(|error| ListingsApiError::UnexpectedResponse(error.to_string()))?,
                HeaderValue::from_static("application/json"),
            );
        }
        if let Some(etag) = etag {
            request = request
                .if_match(HeaderValue::from_str(etag).map_err(|_| {
                    ListingsApiError::UnexpectedResponse("invalid ETag".to_owned())
                })?);
        }
        let client = Arc::clone(&self.client);
        let response = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| ListingsApiError::Upstream(error.to_string()))?
                        .block_on(client.execute(request))
                        .map_err(|error| ListingsApiError::Upstream(error.to_string()))
                })
                .join()
                .map_err(|_| ListingsApiError::Upstream("HTTP worker panicked".to_owned()))?
        })?;
        match response.status {
            StatusCode::NOT_FOUND => return Err(ListingsApiError::NotFound),
            StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => {
                return Err(ListingsApiError::Conflict);
            }
            status if !status.is_success() => {
                let message = serde_json::from_slice::<Value>(&response.body)
                    .ok()
                    .and_then(|value| {
                        value
                            .get("message")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .unwrap_or_else(|| format!("Tori returned HTTP {}", status.as_u16()));
                return Err(ListingsApiError::Upstream(message));
            }
            _ => {}
        }
        if response.body.is_empty() {
            serde_json::from_value(Value::Null)
        } else {
            serde_json::from_slice(&response.body)
        }
        .map_err(|error| ListingsApiError::UnexpectedResponse(error.to_string()))
    }

    fn empty(&self, method: Method, path: String) -> Result<(), ListingsApiError> {
        self.request::<Value>(method, path, None, None).map(|_| ())
    }
}

impl ListingsApi for HttpListingsApi {
    fn categories(&self) -> Result<Vec<UpstreamCategory>, ListingsApiError> {
        self.request(
            Method::GET,
            "/my/listings/categories".to_owned(),
            None,
            None,
        )
    }

    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<UpstreamListingPage, ListingsApiError> {
        self.request(
            Method::GET,
            format!("/search?limit={limit}&offset={offset}"),
            None,
            None,
        )
    }

    fn listing(&self, listing_id: &str) -> Result<UpstreamListing, ListingsApiError> {
        self.request(
            Method::GET,
            format!("/my/listings/{listing_id}"),
            None,
            None,
        )
    }

    fn update_listing(
        &self,
        listing_id: &str,
        etag: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<UpstreamListing, ListingsApiError> {
        self.request(
            Method::PUT,
            format!("/my/listings/{listing_id}"),
            Some(Value::Object(fields.clone().into_iter().collect())),
            Some(etag),
        )
    }

    fn dispose_listing(&self, listing_id: &str) -> Result<(), ListingsApiError> {
        self.empty(Method::POST, format!("/my/listings/{listing_id}/dispose"))
    }

    fn delete_listing(&self, listing_id: &str) -> Result<(), ListingsApiError> {
        self.empty(Method::DELETE, format!("/my/listings/{listing_id}"))
    }
}

#[derive(Clone, thiserror::Error, PartialEq)]
pub enum ListingsApiError {
    #[error("resource was not found")]
    NotFound,
    #[error("the resource changed remotely")]
    Conflict,
    #[error("listing validation failed: {message}")]
    Validation {
        message: String,
        fields: BTreeMap<String, String>,
    },
    #[error("{0}")]
    Upstream(String),
    #[error("unexpected upstream response: {0}")]
    UnexpectedResponse(String),
}

impl fmt::Debug for ListingsApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Conflict => formatter.write_str("Conflict"),
            Self::Validation { fields, .. } => formatter
                .debug_struct("Validation")
                .field("message", &"[REDACTED]")
                .field("field_names", &fields.keys().collect::<Vec<_>>())
                .finish(),
            Self::Upstream(_) => formatter.write_str("Upstream([REDACTED])"),
            Self::UnexpectedResponse(_) => formatter.write_str("UnexpectedResponse([REDACTED])"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamCategory {
    #[serde(alias = "category_id")]
    pub id: String,
    pub label: String,
    #[serde(default, alias = "parent_id")]
    pub parent_id: Option<String>,
    #[serde(default)]
    pub selectable: Option<bool>,
    #[serde(default)]
    pub children: Vec<UpstreamCategory>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UpstreamListingPage {
    #[serde(default, alias = "listings")]
    pub summaries: Vec<UpstreamListingSummary>,
    pub total: usize,
    #[serde(default)]
    pub facets: Vec<UpstreamFacet>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UpstreamListingSummary {
    pub id: Value,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default, rename = "daysUntilExpires", alias = "days_until_expires")]
    pub days_until_expires: Option<u64>,
    #[serde(default)]
    pub state: UpstreamState,
    #[serde(default)]
    pub actions: Vec<UpstreamAction>,
    #[serde(default)]
    pub data: UpstreamSummaryData,
    #[serde(default, rename = "externalData", alias = "external_data")]
    pub external_data: UpstreamStatistics,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamState {
    #[serde(default)]
    pub label: String,
    #[serde(default, rename = "type")]
    pub state_type: String,
    #[serde(default)]
    pub display: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamSummaryData {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub image: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamStatistics {
    #[serde(default, alias = "views")]
    pub clicks: UpstreamStatistic,
    #[serde(default)]
    pub favorites: UpstreamStatistic,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamStatistic {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamAction {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamFacet {
    #[serde(default)]
    pub label: String,
    pub name: String,
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UpstreamListing {
    pub id: Value,
    pub etag: String,
    #[serde(default)]
    pub state: UpstreamState,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    #[serde(default)]
    pub actions: Vec<UpstreamAction>,
    #[serde(default, rename = "externalData", alias = "external_data")]
    pub external_data: UpstreamStatistics,
}

pub struct Listings<'a> {
    api: &'a dyn ListingsApi,
}

impl<'a> Listings<'a> {
    pub fn new(api: &'a dyn ListingsApi) -> Self {
        Self { api }
    }

    pub fn categories(&self, parent: Option<&str>) -> Result<CategoryList, AppError> {
        let categories = self.api.categories().map_err(category_error)?;
        let flattened = flatten_categories(&categories);

        if let Some(parent_id) = parent
            && !flattened
                .iter()
                .any(|category| category.category_id == parent_id)
        {
            return Err(resource_not_found(
                "category.not_found",
                "category",
                parent_id,
            ));
        }

        Ok(CategoryList {
            categories: flattened
                .into_iter()
                .filter(|category| category.parent_id.as_deref() == parent)
                .collect(),
        })
    }

    pub fn search_categories(&self, query: &str) -> Result<CategoryList, AppError> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Err(AppError::usage("category search query must not be empty"));
        }

        let categories = self.api.categories().map_err(category_error)?;
        let mut scored: Vec<(u8, Category)> = flatten_categories(&categories)
            .into_iter()
            .filter_map(|category| {
                let label = category.label.to_lowercase();
                let path = category.path.to_lowercase();
                let score = if label == query {
                    3
                } else if label.contains(&query) {
                    2
                } else if path.contains(&query) {
                    1
                } else {
                    0
                };
                (score > 0).then_some((score, category))
            })
            .collect();
        scored.sort_by(|(left_score, left), (right_score, right)| {
            right_score
                .cmp(left_score)
                .then_with(|| left.path.cmp(&right.path))
        });

        Ok(CategoryList {
            categories: scored.into_iter().map(|(_, category)| category).collect(),
        })
    }

    pub fn list(&self) -> Result<ListingCollection, AppError> {
        let mut listings = Vec::new();
        let mut facets = Vec::new();
        let mut total = None;
        let mut offset = 0;
        let mut seen = HashSet::new();

        for _ in 0..MAX_LISTING_PAGES {
            let page = self
                .api
                .listing_page(offset, LISTING_PAGE_SIZE)
                .map_err(|error| listing_error(error, None))?;
            let expected_total = *total.get_or_insert(page.total);
            if page.total != expected_total {
                return Err(unexpected("listing total changed during pagination"));
            }
            if facets.is_empty() {
                facets = page.facets.into_iter().map(normalize_facet).collect();
            }
            let page_len = page.summaries.len();
            for summary in page.summaries {
                let normalized = normalize_summary(summary)?;
                if !seen.insert(normalized.listing_id.clone()) {
                    return Err(unexpected("listing pagination returned a duplicate item"));
                }
                listings.push(normalized);
            }
            offset += page_len;

            if offset >= expected_total {
                return Ok(ListingCollection {
                    listings,
                    total: expected_total as u64,
                    facets,
                });
            }
            if page_len == 0 {
                return Err(unexpected(
                    "listing pagination ended before the reported total",
                ));
            }
        }

        Err(unexpected("listing pagination exceeded its safety bound"))
    }

    pub fn show(&self, listing_id: &str) -> Result<ListingDetail, AppError> {
        validate_id(listing_id)?;
        self.snapshot(listing_id).map(|snapshot| snapshot.detail)
    }

    pub fn snapshot(&self, listing_id: &str) -> Result<ListingSnapshot, AppError> {
        validate_id(listing_id)?;
        self.api
            .listing(listing_id)
            .map_err(|error| listing_error(error, Some(listing_id)))
            .and_then(|listing| normalize_listing_for_id(listing, listing_id))
    }

    pub fn update(
        &self,
        listing_id: &str,
        changes: BTreeMap<String, Value>,
    ) -> Result<ListingDetail, AppError> {
        validate_id(listing_id)?;
        if changes.is_empty() {
            return Err(AppError::usage(
                "listing update requires at least one field",
            ));
        }
        validate_changes(&changes)?;

        let snapshot = self.snapshot(listing_id)?;
        let mut complete_fields = snapshot.detail.fields;
        complete_fields.extend(changes);
        match self
            .api
            .update_listing(listing_id, &snapshot.etag, &complete_fields)
        {
            Ok(listing) => {
                normalize_listing_for_id(listing, listing_id).map(|snapshot| snapshot.detail)
            }
            Err(ListingsApiError::Conflict) => {
                let fresh = self
                    .snapshot(listing_id)
                    .ok()
                    .map(|snapshot| snapshot.detail);
                let mut error = AppError::new(
                    "listing.conflict",
                    "listing changed remotely; no fields were overwritten",
                    ExitClass::Conflict,
                );
                error.retryable = true;
                error.details = Some(Box::new(json!({
                    "listing_id": listing_id,
                    "current": fresh,
                })));
                Err(error)
            }
            Err(error) => Err(listing_mutation_error(error, listing_id, "update")),
        }
    }

    pub fn dispose(&self, listing_id: &str) -> Result<ListingMutation, AppError> {
        validate_id(listing_id)?;
        self.api
            .dispose_listing(listing_id)
            .map_err(|error| listing_mutation_error(error, listing_id, "dispose"))?;
        Ok(ListingMutation {
            listing_id: listing_id.to_owned(),
            state: ListingState::Disposed,
        })
    }

    pub fn delete(&self, listing_id: &str) -> Result<ListingRef, AppError> {
        validate_id(listing_id)?;
        self.api
            .delete_listing(listing_id)
            .map_err(|error| listing_mutation_error(error, listing_id, "delete"))?;
        Ok(ListingRef {
            listing_id: listing_id.to_owned(),
        })
    }

    pub fn copy_source(&self, listing_id: &str) -> Result<ListingCopySource, AppError> {
        let snapshot = self.snapshot(listing_id)?;
        let mut fields = snapshot.detail.fields;
        let mut image_urls = Vec::new();
        for key in ["image", "images", "multi_image"] {
            if let Some(value) = fields.remove(key) {
                collect_image_urls(&value, &mut image_urls);
            }
        }
        image_urls.dedup();

        Ok(ListingCopySource {
            listing_id: listing_id.to_owned(),
            fields,
            image_urls,
        })
    }
}

fn flatten_categories(roots: &[UpstreamCategory]) -> Vec<Category> {
    fn visit(
        nodes: &[UpstreamCategory],
        inherited_parent: Option<&str>,
        parent_path: &str,
        output: &mut Vec<Category>,
    ) {
        for node in nodes {
            let parent_id = node.parent_id.as_deref().or(inherited_parent);
            let path = if parent_path.is_empty() {
                node.label.clone()
            } else {
                format!("{parent_path} > {}", node.label)
            };
            output.push(Category {
                category_id: node.id.clone(),
                label: node.label.clone(),
                parent_id: parent_id.map(ToOwned::to_owned),
                path: path.clone(),
                selectable: node.selectable.unwrap_or(node.children.is_empty()),
            });
            visit(&node.children, Some(&node.id), &path, output);
        }
    }

    let mut output = Vec::new();
    visit(roots, None, "", &mut output);
    output
}

fn normalize_summary(raw: UpstreamListingSummary) -> Result<ListingSummary, AppError> {
    Ok(ListingSummary {
        listing_id: value_id(&raw.id)?,
        title: raw.data.title,
        price: nonempty(raw.data.subtitle),
        state: normalize_state(&raw.state),
        image_url: nonempty(raw.data.image),
        created_at: raw.created,
        updated_at: raw.updated,
        expires_at: raw.expires,
        days_until_expires: raw.days_until_expires,
        statistics: normalize_statistics(&raw.external_data),
        actions: raw.actions.into_iter().map(normalize_action).collect(),
    })
}

fn normalize_listing_for_id(
    raw: UpstreamListing,
    expected_id: &str,
) -> Result<ListingSnapshot, AppError> {
    let snapshot = normalize_listing(raw)?;
    if snapshot.detail.listing_id != expected_id {
        return Err(unexpected("listing detail returned a different ID"));
    }
    Ok(snapshot)
}

fn normalize_listing(raw: UpstreamListing) -> Result<ListingSnapshot, AppError> {
    if raw.etag.trim().is_empty() {
        return Err(unexpected("listing detail omitted its ETag"));
    }
    Ok(ListingSnapshot {
        detail: ListingDetail {
            listing_id: value_id(&raw.id)?,
            state: normalize_state(&raw.state),
            fields: raw.fields,
            statistics: normalize_statistics(&raw.external_data),
            actions: raw.actions.into_iter().map(normalize_action).collect(),
        },
        etag: raw.etag,
    })
}

fn normalize_facet(raw: UpstreamFacet) -> ListingFacet {
    ListingFacet {
        state: normalize_state_name(&raw.name),
        label: raw.label,
        total: raw.total,
    }
}

fn normalize_state(raw: &UpstreamState) -> ListingState {
    let value = [&raw.state_type, &raw.display, &raw.label]
        .into_iter()
        .find(|value| !value.trim().is_empty())
        .map_or("", String::as_str);
    normalize_state_name(value)
}

fn normalize_state_name(value: &str) -> ListingState {
    match value.trim().to_ascii_lowercase().as_str() {
        "all" => ListingState::All,
        "active" | "published" => ListingState::Active,
        "pending" | "review" => ListingState::Pending,
        "expired" => ListingState::Expired,
        "disposed" | "sold" => ListingState::Disposed,
        "draft" => ListingState::Draft,
        _ => ListingState::Unknown,
    }
}

fn normalize_action(raw: UpstreamAction) -> ListingAction {
    let name = match raw.name.trim().to_ascii_lowercase().as_str() {
        "edit" => ListingActionName::Edit,
        "dispose" => ListingActionName::Dispose,
        "delete" => ListingActionName::Delete,
        "republish" | "recreate" => ListingActionName::Republish,
        "undispose" | "activate" => ListingActionName::Undispose,
        "view" | "show" => ListingActionName::View,
        _ => ListingActionName::Unknown,
    };
    ListingAction {
        name,
        label: raw.label,
        method: raw.method.to_ascii_uppercase(),
    }
}

fn normalize_statistics(raw: &UpstreamStatistics) -> ListingStatistics {
    ListingStatistics {
        views: parse_statistic(&raw.clicks.value),
        favorites: parse_statistic(&raw.favorites.value),
    }
}

fn parse_statistic(value: &str) -> Option<u64> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    (!digits.is_empty()).then(|| digits.parse().ok()).flatten()
}

fn collect_image_urls(value: &Value, output: &mut Vec<String>) {
    match value {
        Value::String(url) if !url.is_empty() => output.push(url.clone()),
        Value::Array(values) => {
            for value in values {
                collect_image_urls(value, output);
            }
        }
        Value::Object(object) => {
            for key in ["url", "uri", "src", "image_url"] {
                if let Some(Value::String(url)) = object.get(key)
                    && !url.is_empty()
                {
                    output.push(url.clone());
                    break;
                }
            }
        }
        _ => {}
    }
}

fn validate_changes(changes: &BTreeMap<String, Value>) -> Result<(), AppError> {
    if let Some(value) = changes.get("trade_type") {
        let valid = matches!(value.as_str(), Some("sell" | "give_away" | "wanted"));
        if !valid {
            return Err(validation_error(
                "trade_type",
                "expected one of: sell, give_away, wanted",
            ));
        }
    }
    if let Some(value) = changes.get("price")
        && value
            .as_f64()
            .is_none_or(|price| !price.is_finite() || price < 0.0)
    {
        return Err(validation_error("price", "expected a non-negative number"));
    }
    for key in ["category", "title", "description", "postal_code"] {
        if let Some(value) = changes.get(key) {
            let Some(value) = value.as_str() else {
                return Err(validation_error(key, "expected a string"));
            };
            if value.trim().is_empty() {
                return Err(validation_error(key, "must not be empty"));
            }
        }
    }
    if let Some(value) = changes.get("delivery") {
        let values = match value {
            Value::String(value) => vec![value.as_str()],
            Value::Array(values) => values
                .iter()
                .map(Value::as_str)
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| validation_error("delivery", "expected semantic string values"))?,
            _ => {
                return Err(validation_error(
                    "delivery",
                    "expected semantic string values",
                ));
            }
        };
        if values.is_empty() || values.iter().any(|value| !is_semantic_value(value)) {
            return Err(validation_error(
                "delivery",
                "expected non-empty lowercase machine values",
            ));
        }
    }
    Ok(())
}

fn is_semantic_value(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

fn validate_id(listing_id: &str) -> Result<(), AppError> {
    if listing_id.is_empty()
        || listing_id.len() > 128
        || !listing_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        Err(AppError::usage("listing ID is invalid"))
    } else {
        Ok(())
    }
}

fn value_id(value: &Value) -> Result<String, AppError> {
    match value {
        Value::String(value) if !value.is_empty() => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(unexpected("listing has an invalid ID")),
    }
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn validation_error(field: &str, message: &str) -> AppError {
    let mut error = AppError::new(
        "listing.validation_failed",
        "listing fields are invalid",
        ExitClass::Validation,
    );
    error.details = Some(Box::new(json!({ "fields": { field: message } })));
    error
}

fn category_error(error: ListingsApiError) -> AppError {
    match error {
        ListingsApiError::NotFound => resource_not_found("category.not_found", "category", ""),
        other => upstream_error(other),
    }
}

fn listing_error(error: ListingsApiError, listing_id: Option<&str>) -> AppError {
    match error {
        ListingsApiError::NotFound => resource_not_found(
            "listing.not_found",
            "listing",
            listing_id.unwrap_or_default(),
        ),
        ListingsApiError::Conflict => AppError::new(
            "listing.conflict",
            "listing changed remotely",
            ExitClass::Conflict,
        ),
        ListingsApiError::Validation { fields, .. } => {
            let mut error = AppError::new(
                "listing.validation_failed",
                "listing validation failed",
                ExitClass::Validation,
            );
            error.details = Some(Box::new(json!({
                "listing_id": listing_id,
                "fields": fields
            })));
            error
        }
        other => upstream_error(other),
    }
}

fn resource_not_found(code: &str, resource: &str, id: &str) -> AppError {
    let mut error = AppError::new(
        code,
        format!("{resource} was not found"),
        ExitClass::Validation,
    );
    error.details = Some(Box::new(json!({ format!("{resource}_id"): id })));
    error
}

fn listing_mutation_error(error: ListingsApiError, listing_id: &str, operation: &str) -> AppError {
    let app_error = listing_error(error, Some(listing_id));
    if app_error.exit_class != ExitClass::Upstream {
        return app_error;
    }
    let mut app_error = app_error.with_partial(json!({
        "listing_id": listing_id,
        "operation": operation,
    }));
    app_error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: format!("tori listing show {listing_id}"),
        });
    app_error
}

fn upstream_error(error: ListingsApiError) -> AppError {
    let retryable = matches!(error, ListingsApiError::Upstream(_));
    let (code, message) = match error {
        ListingsApiError::UnexpectedResponse(_) => (
            "upstream.unexpected_response",
            "Tori returned an unexpected listing response",
        ),
        _ => ("upstream.request_failed", "the Tori listing request failed"),
    };
    let mut app_error = AppError::new(code, message, ExitClass::Upstream);
    app_error.retryable = retryable;
    app_error
}

fn unexpected(message: &str) -> AppError {
    upstream_error(ListingsApiError::UnexpectedResponse(message.to_owned()))
}
