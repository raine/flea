use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fmt,
    sync::Arc,
};

use reqwest::{Method, StatusCode, header::HeaderValue};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use crate::{
    api::client::{HttpError, RequestSpec, ToriClient, TransportErrorKind, compatibility},
    domain::{
        commerce::{Price, TradeType, normalize_commerce_fields},
        listing::{
            Category, CategoryList, CategorySearchContext, CategorySearchResult, ListingAction,
            ListingActionName, ListingCollection, ListingCopySource, ListingDetail, ListingFacet,
            ListingMutation, ListingRef, ListingSnapshot, ListingState, ListingStatistics,
            ListingSummary,
        },
        observation::{Observation, ObservationOperation},
    },
    error::{AppError, ExitClass},
    retry::{FailureKind, OperationMethod, RetryContext, classify},
};

pub const LISTING_PAGE_SIZE: usize = 50;
pub const CATEGORY_SEARCH_LIMIT_DEFAULT: usize = 20;
pub const CATEGORY_SEARCH_LIMIT_MAX: usize = 100;
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
                        .map_err(|_| {
                            ListingsApiError::UnexpectedResponse("HTTP runtime failed".to_owned())
                        })?
                        .block_on(client.execute(request))
                        .map_err(listings_http_error)
                })
                .join()
                .map_err(|_| {
                    ListingsApiError::UnexpectedResponse("HTTP worker failed".to_owned())
                })?
        })?;
        decode_response(response.status, &response.body)
    }

    fn request_with_service<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        service: &str,
    ) -> Result<T, ListingsApiError> {
        let request = RequestSpec::new(method, path, service);
        let client = Arc::clone(&self.client);
        let response = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| {
                            ListingsApiError::UnexpectedResponse("HTTP runtime failed".to_owned())
                        })?
                        .block_on(client.execute(request))
                        .map_err(listings_http_error)
                })
                .join()
                .map_err(|_| {
                    ListingsApiError::UnexpectedResponse("HTTP worker failed".to_owned())
                })?
        })?;
        decode_response(response.status, &response.body)
    }

    fn empty(&self, method: Method, path: String) -> Result<(), ListingsApiError> {
        self.request::<Value>(method, path, None, None).map(|_| ())
    }
}

fn listings_http_error(error: HttpError) -> ListingsApiError {
    match error {
        HttpError::Transport(transport)
            if matches!(
                transport.kind,
                TransportErrorKind::Timeout | TransportErrorKind::Connection
            ) =>
        {
            ListingsApiError::Transport
        }
        HttpError::InvalidRequest | HttpError::ResponseTooLarge | HttpError::Transport(_) => {
            ListingsApiError::UnexpectedResponse("HTTP adapter failed".to_owned())
        }
    }
}

fn decode_response<T: DeserializeOwned>(
    status: StatusCode,
    body: &[u8],
) -> Result<T, ListingsApiError> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(ListingsApiError::Authentication);
        }
        StatusCode::NOT_FOUND => return Err(ListingsApiError::NotFound),
        StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => {
            return Err(ListingsApiError::Conflict);
        }
        status if !status.is_success() => {
            return Err(ListingsApiError::Upstream(status.as_u16()));
        }
        _ => {}
    }
    if body.is_empty() {
        serde_json::from_value(Value::Null)
    } else {
        serde_json::from_slice(body)
    }
    .map_err(|error| ListingsApiError::UnexpectedResponse(error.to_string()))
}

impl ListingsApi for HttpListingsApi {
    fn categories(&self) -> Result<Vec<UpstreamCategory>, ListingsApiError> {
        self.request_with_service::<UpstreamCategoryTaxonomy>(
            Method::GET,
            "/categories/taxonomy".to_owned(),
            compatibility::SERVICE_ITEM_CREATION,
        )
        .map(|taxonomy| taxonomy.categories)
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
        self.request(Method::GET, format!("/{listing_id}"), None, None)
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
    #[error("authentication failed")]
    Authentication,
    #[error("resource was not found")]
    NotFound,
    #[error("the resource changed remotely")]
    Conflict,
    #[error("listing validation failed: {message}")]
    Validation {
        message: String,
        fields: BTreeMap<String, String>,
    },
    #[error("listing transport failed")]
    Transport,
    #[error("Tori listing service returned HTTP {0}")]
    Upstream(u16),
    #[error("unexpected upstream response: {0}")]
    UnexpectedResponse(String),
}

impl fmt::Debug for ListingsApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication => formatter.write_str("Authentication"),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Conflict => formatter.write_str("Conflict"),
            Self::Validation { fields, .. } => formatter
                .debug_struct("Validation")
                .field("message", &"[REDACTED]")
                .field("field_names", &fields.keys().collect::<Vec<_>>())
                .finish(),
            Self::Transport => formatter.write_str("Transport"),
            Self::Upstream(status) => formatter.debug_tuple("Upstream").field(status).finish(),
            Self::UnexpectedResponse(_) => formatter.write_str("UnexpectedResponse([REDACTED])"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamCategoryTaxonomy {
    pub categories: Vec<UpstreamCategory>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamCategory {
    #[serde(alias = "category_id", deserialize_with = "deserialize_category_id")]
    pub id: String,
    pub label: String,
    #[serde(default, alias = "parent_id")]
    pub parent_id: Option<String>,
    #[serde(default, alias = "isSelectable")]
    pub selectable: Option<bool>,
    #[serde(default)]
    pub children: Vec<UpstreamCategory>,
}

fn deserialize_category_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(id) => Ok(id),
        Value::Number(id) if id.is_u64() => Ok(id.to_string()),
        _ => Err(serde::de::Error::custom(
            "category ID must be a string or unsigned integer",
        )),
    }
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
    #[serde(default, alias = "area", alias = "place")]
    pub location: String,
    #[serde(default, alias = "url", alias = "publicUrl")]
    pub public_url: String,
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
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub state: UpstreamState,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    #[serde(default)]
    pub data: UpstreamSummaryData,
    #[serde(default)]
    pub actions: Vec<UpstreamAction>,
    #[serde(default, rename = "externalData", alias = "external_data")]
    pub external_data: UpstreamStatistics,
}

#[derive(Clone, Copy, Debug)]
pub struct CategorySearchOptions<'a> {
    pub parent: Option<&'a str>,
    pub path: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

impl Default for CategorySearchOptions<'_> {
    fn default() -> Self {
        Self {
            parent: None,
            path: None,
            offset: 0,
            limit: CATEGORY_SEARCH_LIMIT_DEFAULT,
        }
    }
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
        let flattened = flatten_categories(&categories).map_err(category_protocol_error)?;

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

    pub fn search_categories(&self, query: &str) -> Result<CategorySearchResult, AppError> {
        self.search_categories_with_options(query, CategorySearchOptions::default())
    }

    pub fn search_categories_with_options(
        &self,
        query: &str,
        options: CategorySearchOptions<'_>,
    ) -> Result<CategorySearchResult, AppError> {
        let query = query.trim();
        let normalized_query = normalize_category_text(query);
        let query_tokens = category_tokens(&normalized_query);
        if normalized_query.is_empty() || query_tokens.is_empty() {
            return Err(AppError::usage(
                "category search query must contain letters or numbers",
            ));
        }
        if !(1..=CATEGORY_SEARCH_LIMIT_MAX).contains(&options.limit) {
            return Err(AppError::usage(format!(
                "--limit must be between 1 and {CATEGORY_SEARCH_LIMIT_MAX}"
            )));
        }
        if options.parent.is_some() && options.path.is_some() {
            return Err(AppError::usage(
                "--parent and --path cannot be used together",
            ));
        }

        let categories = self.api.categories().map_err(category_error)?;
        let flattened = flatten_categories(&categories).map_err(category_protocol_error)?;
        let context = resolve_category_context(&flattened, options.parent, options.path)?;
        let parents = flattened
            .iter()
            .map(|category| (category.category_id.clone(), category.parent_id.clone()))
            .collect::<HashMap<_, _>>();
        let mut scored = flattened
            .into_iter()
            .filter(|category| {
                context.as_ref().is_none_or(|context| {
                    is_category_descendant(&category.category_id, &context.category_id, &parents)
                })
            })
            .filter_map(|category| {
                category_rank(&category, &normalized_query, &query_tokens).map(|rank| {
                    let normalized_path = normalize_category_text(&category.path);
                    (rank, normalized_path, category)
                })
            })
            .collect::<Vec<_>>();
        scored.sort_by(
            |(left_rank, left_path, left), (right_rank, right_path, right)| {
                left_rank
                    .cmp(right_rank)
                    .then_with(|| left_path.cmp(right_path))
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.category_id.cmp(&right.category_id))
            },
        );

        let total = scored.len();
        let categories = scored
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .map(|(_, _, category)| category)
            .collect::<Vec<_>>();
        let returned = categories.len();
        let truncated = options.offset.saturating_add(returned) < total;

        Ok(CategorySearchResult {
            categories,
            query: query.to_owned(),
            context,
            offset: options.offset,
            limit: options.limit,
            returned,
            total,
            truncated,
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
                .map_err(|error| {
                    listing_error(error, None, RetryContext::read(OperationMethod::Get))
                })?;
            let expected_total = *total.get_or_insert(page.total);
            if page.total != expected_total {
                return Err(unexpected("listing total changed during pagination"));
            }
            if facets.is_empty() {
                facets = page.facets.into_iter().map(normalize_facet).collect();
            }
            let page_len = page.summaries.len();
            for summary in page.summaries {
                let listing_id = value_id(&summary.id)?;
                let detail = self.api.listing(&listing_id).map_err(|error| {
                    listing_error(
                        error,
                        Some(&listing_id),
                        RetryContext::read(OperationMethod::Get),
                    )
                })?;
                if value_id(&detail.id)? != listing_id {
                    return Err(unexpected("listing detail returned a different ID"));
                }
                let (trade_type, price) = commerce_from_fields(&detail.fields);
                let normalized = normalize_summary(summary, trade_type, price)?;
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
        match self.api.listing(listing_id) {
            Ok(listing) => match normalize_listing_detail_for_id(listing, listing_id) {
                Ok(detail) => Ok(detail),
                Err(detail_error) => match self.find_summary(listing_id) {
                    Ok(Some(summary)) => Ok(summary_detail(summary)),
                    Ok(None) | Err(_) => Err(detail_error),
                },
            },
            Err(detail_error) => {
                let summary = self.find_summary(listing_id);
                match summary {
                    Ok(Some(summary)) => Ok(summary_detail(summary)),
                    Ok(None) => Err(listing_error(
                        detail_error,
                        Some(listing_id),
                        RetryContext::read(OperationMethod::Get),
                    )),
                    Err(collection_error) => {
                        let classification = listing_error(
                            collection_error,
                            Some(listing_id),
                            RetryContext::read(OperationMethod::Get),
                        );
                        let observation = if classification.upstream_transient {
                            Observation::temporarily_unavailable(
                                "listing_reconciliation",
                                None,
                                false,
                            )
                        } else {
                            Observation::unrecognized_response("listing_reconciliation", None)
                        };
                        let mut error = AppError::upstream(
                            "listing.observation_delayed",
                            "listing identity could not be reconciled between detail and collection observations",
                        )
                        .with_observation(observation, ObservationOperation::Read);
                        error.details = Some(Box::new(json!({
                            "listing_id": listing_id,
                            "detail_status": detail_observation_status(&detail_error),
                            "collection_status": "unavailable",
                            "observation_attempts": 2,
                        })));
                        error
                            .next_actions
                            .push(crate::domain::envelope::NextAction {
                                command: format!("flea tori listing show {listing_id}"),
                            });
                        Err(error)
                    }
                }
            }
        }
    }

    fn find_summary(&self, listing_id: &str) -> Result<Option<ListingSummary>, ListingsApiError> {
        let mut offset = 0;
        let mut expected_total = None;
        for _ in 0..MAX_LISTING_PAGES {
            let page = self.api.listing_page(offset, LISTING_PAGE_SIZE)?;
            let total = *expected_total.get_or_insert(page.total);
            if page.total != total {
                return Err(ListingsApiError::UnexpectedResponse(
                    "listing total changed during identity reconciliation".to_owned(),
                ));
            }
            let page_len = page.summaries.len();
            for summary in page.summaries {
                let id = summary_id(&summary.id)?;
                if id == listing_id {
                    return normalize_summary(
                        summary,
                        TradeType::Unknown,
                        Price::unavailable(None),
                    )
                    .map(Some)
                    .map_err(|_| {
                        ListingsApiError::UnexpectedResponse(
                            "matching listing summary was malformed".to_owned(),
                        )
                    });
                }
            }
            offset += page_len;
            if offset >= total {
                return Ok(None);
            }
            if page_len == 0 {
                return Err(ListingsApiError::UnexpectedResponse(
                    "listing pagination ended before the reported total".to_owned(),
                ));
            }
        }
        Err(ListingsApiError::UnexpectedResponse(
            "listing identity reconciliation exceeded its safety bound".to_owned(),
        ))
    }

    pub fn snapshot(&self, listing_id: &str) -> Result<ListingSnapshot, AppError> {
        validate_id(listing_id)?;
        self.api
            .listing(listing_id)
            .map_err(|error| {
                listing_error(
                    error,
                    Some(listing_id),
                    RetryContext::read(OperationMethod::Get),
                )
            })
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
        let mut complete_fields = snapshot.source_fields;
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
                let mut retry_context = RetryContext::mutation(OperationMethod::Put).with_etag();
                if fresh.is_some() {
                    retry_context = retry_context.with_authoritative_observation();
                }
                let classification = classify(FailureKind::PreconditionFailed, retry_context);
                let mut error = AppError::new(
                    "listing.conflict",
                    "listing changed remotely; no fields were overwritten",
                    ExitClass::Conflict,
                )
                .retry_classification(classification);
                error.details = Some(Box::new(json!({
                    "listing_id": listing_id,
                    "current": fresh,
                })));
                error
                    .next_actions
                    .push(crate::domain::envelope::NextAction {
                        command: format!("flea tori listing show {listing_id}"),
                    });
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
        let mut fields = snapshot.source_fields;
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CategoryRank {
    class: u8,
    distance: usize,
}

fn resolve_category_context(
    categories: &[Category],
    parent: Option<&str>,
    path: Option<&str>,
) -> Result<Option<CategorySearchContext>, AppError> {
    if let Some(parent_id) = parent {
        return categories
            .iter()
            .find(|category| category.category_id == parent_id)
            .map(category_search_context)
            .map(Some)
            .ok_or_else(|| resource_not_found("category.not_found", "category", parent_id));
    }

    let Some(path) = path else {
        return Ok(None);
    };
    let normalized_path = normalize_category_text(path);
    let context_segments = category_path_segments(&normalized_path);
    if context_segments.is_empty() {
        return Err(AppError::usage("--path must not be empty"));
    }
    let matches = categories
        .iter()
        .filter(|category| {
            let candidate = normalize_category_text(&category.path);
            category_path_segments(&candidate).ends_with(&context_segments)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(AppError::validation(
            "category.path_not_found",
            "category path context was not found",
        )
        .with_details(json!({ "path": path }))),
        [category] => Ok(Some(category_search_context(category))),
        _ => Err(AppError::validation(
            "category.path_ambiguous",
            "category path context is ambiguous",
        )
        .with_details(json!({
            "path": path,
            "matches": matches
                .into_iter()
                .map(category_search_context)
                .collect::<Vec<_>>()
        }))),
    }
}

fn category_search_context(category: &Category) -> CategorySearchContext {
    CategorySearchContext {
        category_id: category.category_id.clone(),
        taxonomy_value: category.taxonomy_value.clone(),
        label: category.label.clone(),
        path: category.path.clone(),
    }
}

fn is_category_descendant(
    category_id: &str,
    ancestor_id: &str,
    parents: &HashMap<String, Option<String>>,
) -> bool {
    let mut parent = parents.get(category_id).and_then(Option::as_deref);
    while let Some(parent_id) = parent {
        if parent_id == ancestor_id {
            return true;
        }
        parent = parents.get(parent_id).and_then(Option::as_deref);
    }
    false
}

fn category_rank(
    category: &Category,
    query: &str,
    query_tokens: &[String],
) -> Option<CategoryRank> {
    if category.category_id == query {
        return Some(CategoryRank {
            class: 0,
            distance: 0,
        });
    }

    let label = normalize_category_text(&category.label);
    let path = normalize_category_text(&category.path);
    let label_tokens = category_tokens(&label);
    let path_segments = category_path_segments(&path);
    if label == query {
        return Some(CategoryRank {
            class: 1,
            distance: 0,
        });
    }
    if path == query {
        return Some(CategoryRank {
            class: 2,
            distance: 0,
        });
    }
    if label.starts_with(query) {
        return Some(CategoryRank {
            class: 3,
            distance: 0,
        });
    }
    if tokens_contain_all(&label_tokens, query_tokens) {
        return Some(CategoryRank {
            class: 4,
            distance: 0,
        });
    }

    let ancestors = path_segments
        .get(..path_segments.len().saturating_sub(1))
        .unwrap_or_default();
    if let Some(distance) = closest_segment_match(ancestors, |segment| segment == query) {
        return Some(CategoryRank { class: 5, distance });
    }
    if let Some(distance) = closest_segment_match(ancestors, |segment| segment.starts_with(query)) {
        return Some(CategoryRank { class: 6, distance });
    }
    if let Some(distance) = path_token_distance(&path_segments, query_tokens) {
        return Some(CategoryRank { class: 7, distance });
    }
    if label.contains(query) {
        return Some(CategoryRank {
            class: 8,
            distance: 0,
        });
    }
    if equivalent_category_terms(query_tokens)
        .iter()
        .any(|term| label.contains(term))
    {
        return Some(CategoryRank {
            class: 9,
            distance: 0,
        });
    }
    if path.contains(query) {
        return Some(CategoryRank {
            class: 10,
            distance: 0,
        });
    }
    query_tokens
        .iter()
        .all(|token| path.contains(token))
        .then_some(CategoryRank {
            class: 11,
            distance: 0,
        })
}

fn normalize_category_text(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn category_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn category_path_segments(path: &str) -> Vec<&str> {
    path.split('>')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn tokens_contain_all(haystack: &[String], needles: &[String]) -> bool {
    !needles.is_empty() && needles.iter().all(|needle| haystack.contains(needle))
}

fn closest_segment_match(ancestors: &[&str], predicate: impl Fn(&str) -> bool) -> Option<usize> {
    ancestors
        .iter()
        .rev()
        .position(|segment| predicate(segment))
        .map(|index| index + 1)
}

fn path_token_distance(segments: &[&str], query_tokens: &[String]) -> Option<usize> {
    if query_tokens.is_empty() {
        return None;
    }
    query_tokens
        .iter()
        .map(|query_token| {
            segments
                .iter()
                .rev()
                .position(|segment| category_tokens(segment).contains(query_token))
        })
        .collect::<Option<Vec<_>>>()
        .map(|distances| distances.into_iter().max().unwrap_or_default())
}

fn equivalent_category_terms(query_tokens: &[String]) -> &'static [&'static str] {
    match query_tokens {
        [term] if term == "tarvike" => &["varuste"],
        [term] if term == "tarvikkeet" => &["varusteet"],
        _ => &[],
    }
}

fn flatten_categories(roots: &[UpstreamCategory]) -> Result<Vec<Category>, String> {
    fn visit(
        nodes: &[UpstreamCategory],
        inherited_parent: Option<&str>,
        parent_path: &str,
        ancestor_ids: &[String],
        seen: &mut HashSet<String>,
        output: &mut Vec<Category>,
    ) -> Result<(), String> {
        for node in nodes {
            if node.id.trim().is_empty() || !node.id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("category taxonomy contains an invalid ID".to_owned());
            }
            if node.label.trim().is_empty() {
                return Err("category taxonomy contains an empty label".to_owned());
            }
            if !seen.insert(node.id.clone()) {
                return Err("category taxonomy contains a duplicate ID".to_owned());
            }
            if let Some(parent_id) = node.parent_id.as_deref()
                && Some(parent_id) != inherited_parent
            {
                return Err("category taxonomy contains an inconsistent parent ID".to_owned());
            }
            let path = if parent_path.is_empty() {
                node.label.clone()
            } else {
                format!("{parent_path} > {}", node.label)
            };
            let mut taxonomy_ids = ancestor_ids.to_vec();
            taxonomy_ids.push(node.id.clone());
            let taxonomy_value = format!("{}.{}", taxonomy_ids.len() - 1, taxonomy_ids.join("."));
            output.push(Category {
                category_id: node.id.clone(),
                taxonomy_value,
                label: node.label.clone(),
                parent_id: inherited_parent.map(ToOwned::to_owned),
                path: path.clone(),
                selectable: node.selectable.unwrap_or(node.children.is_empty()),
            });
            visit(
                &node.children,
                Some(&node.id),
                &path,
                &taxonomy_ids,
                seen,
                output,
            )?;
        }
        Ok(())
    }

    if roots.is_empty() {
        return Err("category taxonomy is empty".to_owned());
    }
    let mut output = Vec::new();
    visit(roots, None, "", &[], &mut HashSet::new(), &mut output)?;
    Ok(output)
}

fn normalize_summary(
    raw: UpstreamListingSummary,
    trade_type: TradeType,
    mut price: Price,
) -> Result<ListingSummary, AppError> {
    let listing_id = value_id(&raw.id)?;
    if price.display.is_none() {
        price.display = nonempty(raw.data.subtitle.clone());
    }
    Ok(ListingSummary {
        public_url: public_listing_url(&listing_id, &raw.data.public_url),
        listing_id,
        title: raw.data.title,
        trade_type,
        price,
        state: normalize_state(&raw.state),
        location: nonempty(raw.data.location),
        image_url: nonempty(raw.data.image),
        created_at: raw.created,
        updated_at: raw.updated,
        expires_at: raw.expires,
        days_until_expires: raw.days_until_expires,
        statistics: normalize_statistics(&raw.external_data),
        actions: raw.actions.into_iter().map(normalize_action).collect(),
    })
}

fn normalize_listing_detail_for_id(
    mut raw: UpstreamListing,
    expected_id: &str,
) -> Result<ListingDetail, AppError> {
    let listing_id = value_id(&raw.id)?;
    if listing_id != expected_id {
        return Err(unexpected("listing detail returned a different ID"));
    }
    merge_summary_fields(&mut raw.fields, &raw.data, &listing_id);
    let (trade_type, price) = commerce_from_fields(&raw.fields);
    let fields = display_fields(&raw.fields);
    Ok(ListingDetail {
        listing_id,
        state: normalize_state(&raw.state),
        trade_type,
        price,
        fields,
        statistics: normalize_statistics(&raw.external_data),
        actions: raw.actions.into_iter().map(normalize_action).collect(),
    })
}

fn summary_detail(summary: ListingSummary) -> ListingDetail {
    let mut fields = BTreeMap::new();
    fields.insert("title".to_owned(), Value::String(summary.title));
    if let Some(location) = summary.location {
        fields.insert("location".to_owned(), Value::String(location));
    }
    if let Some(image) = summary.image_url {
        fields.insert("image".to_owned(), Value::String(image));
    }
    fields.insert("public_url".to_owned(), Value::String(summary.public_url));
    ListingDetail {
        listing_id: summary.listing_id,
        state: summary.state,
        trade_type: summary.trade_type,
        price: summary.price,
        fields,
        statistics: summary.statistics,
        actions: summary.actions,
    }
}

fn merge_summary_fields(
    fields: &mut BTreeMap<String, Value>,
    data: &UpstreamSummaryData,
    listing_id: &str,
) {
    for (key, value) in [
        ("title", data.title.as_str()),
        ("price_display", data.subtitle.as_str()),
        ("location", data.location.as_str()),
        ("image", data.image.as_str()),
    ] {
        if !value.is_empty() {
            fields
                .entry(key.to_owned())
                .or_insert_with(|| Value::String(value.to_owned()));
        }
    }
    fields
        .entry("public_url".to_owned())
        .or_insert_with(|| Value::String(public_listing_url(listing_id, &data.public_url)));
}

fn public_listing_url(listing_id: &str, upstream: &str) -> String {
    if upstream.starts_with("https://www.tori.fi/") {
        upstream.to_owned()
    } else {
        format!("https://www.tori.fi/recommerce/forsale/item/{listing_id}")
    }
}

fn detail_observation_status(error: &ListingsApiError) -> &'static str {
    match error {
        ListingsApiError::NotFound => "not_found",
        ListingsApiError::UnexpectedResponse(_) => "unrecognized_model",
        ListingsApiError::Transport | ListingsApiError::Upstream(_) => "unavailable",
        _ => "rejected",
    }
}

fn summary_id(value: &Value) -> Result<String, ListingsApiError> {
    match value {
        Value::String(value) if !value.is_empty() => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        _ => Err(ListingsApiError::UnexpectedResponse(
            "listing summary has an invalid ID".to_owned(),
        )),
    }
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
    let source_fields = raw.fields;
    let (trade_type, price) = commerce_from_fields(&source_fields);
    Ok(ListingSnapshot {
        detail: ListingDetail {
            listing_id: value_id(&raw.id)?,
            state: normalize_state(&raw.state),
            trade_type,
            price,
            fields: display_fields(&source_fields),
            statistics: normalize_statistics(&raw.external_data),
            actions: raw.actions.into_iter().map(normalize_action).collect(),
        },
        etag: raw.etag,
        source_fields,
    })
}

fn commerce_from_fields(fields: &BTreeMap<String, Value>) -> (TradeType, Price) {
    let object = fields
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    normalize_commerce_fields(&object)
}

fn display_fields(fields: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    fields
        .iter()
        .filter(|(key, _)| {
            !matches!(
                key.as_str(),
                "price"
                    | "price_amount"
                    | "priceAmount"
                    | "currency"
                    | "currencyCode"
                    | "currency_code"
                    | "trade_type"
                    | "tradeType"
                    | "adViewTypeLabel"
                    | "price_display"
                    | "priceText"
                    | "price_text"
                    | "subtitle"
            )
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
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
    let read = RetryContext::read(OperationMethod::Get);
    match error {
        ListingsApiError::Authentication => AppError::authentication(
            "category.authentication_failed",
            "Tori rejected authentication for category discovery",
        ),
        ListingsApiError::NotFound => AppError::upstream(
            "category.endpoint_unavailable",
            "Tori's category taxonomy endpoint is unavailable",
        )
        .with_observation(
            Observation::unrecognized_response("category_taxonomy", Some(404)),
            ObservationOperation::Read,
        ),
        ListingsApiError::UnexpectedResponse(message) => category_protocol_error(message),
        other => upstream_error(other, read),
    }
}

fn category_protocol_error(_message: String) -> AppError {
    AppError::upstream(
        "category.protocol_drift",
        "Tori returned an unexpected category taxonomy response",
    )
    .with_observation(
        Observation::unrecognized_response("category_taxonomy", Some(200)),
        ObservationOperation::Read,
    )
}

fn listing_error(
    error: ListingsApiError,
    listing_id: Option<&str>,
    context: RetryContext,
) -> AppError {
    let mut app_error = match error {
        ListingsApiError::Authentication => AppError::authentication(
            "auth.rejected",
            "Tori rejected authentication for the listing request",
        ),
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
        other => upstream_error(other, context),
    };
    if let Some(listing_id) = listing_id
        && app_error.observation.is_some()
    {
        let command = match app_error
            .observation
            .as_ref()
            .map(|observation| observation.state)
        {
            Some(crate::domain::observation::ObservationState::TemporarilyUnavailable) => {
                format!("flea tori listing show {listing_id}")
            }
            _ => "flea tori listing list".to_owned(),
        };
        app_error
            .next_actions
            .push(crate::domain::envelope::NextAction { command });
    }
    app_error
}

fn resource_not_found(code: &str, resource: &str, id: &str) -> AppError {
    let source = match resource {
        "listing" => "listing_detail",
        "category" => "category_taxonomy",
        _ => "remote_resource",
    };
    AppError::new(
        code,
        format!("{resource} was not found"),
        ExitClass::Validation,
    )
    .with_details(json!({ format!("{resource}_id"): id }))
    .with_observation(
        Observation::confirmed_absent(source, Some(404)),
        ObservationOperation::Read,
    )
}

fn listing_mutation_error(error: ListingsApiError, listing_id: &str, operation: &str) -> AppError {
    let outcome_uncertain = matches!(
        &error,
        ListingsApiError::Transport
            | ListingsApiError::UnexpectedResponse(_)
            | ListingsApiError::Upstream(408 | 425 | 429 | 500 | 502 | 503 | 504)
    );
    let method = match operation {
        "update" => OperationMethod::Put,
        "delete" => OperationMethod::Delete,
        _ => OperationMethod::Post,
    };
    let mut app_error = listing_error(error, Some(listing_id), RetryContext::mutation(method));
    if app_error.exit_class != ExitClass::Upstream {
        if app_error.exit_class == ExitClass::Conflict {
            app_error
                .next_actions
                .push(crate::domain::envelope::NextAction {
                    command: format!("flea tori listing show {listing_id}"),
                });
        }
        return app_error;
    }
    if outcome_uncertain {
        app_error.code = "mutation.uncertain".to_owned();
        app_error.message =
            "The upstream failure may be temporary, but the listing mutation outcome is unknown"
                .to_owned();
    }
    let mut app_error = app_error.with_partial(json!({
        "listing_id": listing_id,
        "operation": operation,
    }));
    app_error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: format!("flea tori listing show {listing_id}"),
        });
    app_error
}

fn upstream_error(error: ListingsApiError, context: RetryContext) -> AppError {
    let unrecognized_model = matches!(error, ListingsApiError::UnexpectedResponse(_));
    let operation = if context.method.is_read() {
        ObservationOperation::Read
    } else {
        ObservationOperation::Mutation
    };
    let (code, message, observation) = match error {
        ListingsApiError::Transport => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::temporarily_unavailable("listing_service", None, false),
        ),
        ListingsApiError::Upstream(status) if crate::retry::is_transient_status(status) => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::temporarily_unavailable("listing_service", Some(status), true),
        ),
        ListingsApiError::Upstream(status) => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::unrecognized_response("listing_service", Some(status)),
        ),
        ListingsApiError::UnexpectedResponse(_) => (
            "upstream.unexpected_response",
            "Tori returned an unexpected listing response",
            Observation::unrecognized_response("listing_service", Some(200)),
        ),
        _ => (
            "upstream.request_failed",
            "the Tori listing request failed",
            Observation::unrecognized_response("listing_service", None),
        ),
    };
    let mut error =
        AppError::new(code, message, ExitClass::Upstream).with_observation(observation, operation);
    if unrecognized_model {
        error.details = Some(Box::new(json!({
            "response_model": "unrecognized",
            "response_status_class": "success",
        })));
    }
    error
}

fn unexpected(message: &str) -> AppError {
    upstream_error(
        ListingsApiError::UnexpectedResponse(message.to_owned()),
        RetryContext::read(OperationMethod::Get),
    )
}
