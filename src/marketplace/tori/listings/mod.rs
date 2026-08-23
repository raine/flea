pub(crate) mod observation;
mod scan;
mod taxonomy;

pub use taxonomy::{
    CATEGORY_SEARCH_LIMIT_DEFAULT, CATEGORY_SEARCH_LIMIT_MAX, CategorySearchOptions, Taxonomy,
    TaxonomyApi, UpstreamCategory, UpstreamCategoryTaxonomy,
};

use std::{
    collections::{BTreeMap, HashSet},
    fmt,
    future::Future,
    ops::ControlFlow,
    pin::Pin,
    sync::Arc,
};

use scan::{
    COLLECTION_PAGE_SIZE, CollectionPage, CollectionPageSource, CollectionScan,
    CollectionScanError, scan_collection,
};

use reqwest::{Method, StatusCode, header::HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};

use crate::{
    domain::{
        commerce::{Price, PriceKind, TradeType, normalize_commerce_fields},
        listing::{
            ListingAction, ListingActionName, ListingCollection, ListingCopySource, ListingDetail,
            ListingFacet, ListingMutation, ListingRef, ListingSnapshot, ListingState,
            ListingStatistics, ListingSummary,
        },
        observation::{Observation, ObservationOperation},
    },
    error::{AppError, ExitClass},
    marketplace::tori::client::{
        HttpFailure, RequestSpec, ToriClient, compatibility, map_http_error,
    },
    retry::{FailureKind, OperationMethod, RetryContext, classify},
};

pub const LISTING_PAGE_SIZE: usize = COLLECTION_PAGE_SIZE;

pub trait ListingsApi: Send + Sync {
    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListingPage, ListingsApiError>> + Send + '_>>;
    fn listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>>;
    fn update_listing<'a>(
        &'a self,
        listing_id: &'a str,
        etag: &'a str,
        fields: &'a BTreeMap<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>>;
    fn dispose_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>>;
    fn delete_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>>;
}

struct ListingCollectionSource<'a> {
    api: &'a dyn ListingsApi,
}

impl CollectionPageSource for ListingCollectionSource<'_> {
    type Item = UpstreamListingSummary;
    type Metadata = Vec<UpstreamFacet>;
    type Error = ListingsApiError;

    async fn page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<CollectionPage<Self::Item, Self::Metadata>, Self::Error> {
        self.api
            .listing_page(offset, limit)
            .await
            .map(|page| CollectionPage {
                items: page.summaries,
                total: page.total,
                metadata: page.facets,
                status: None,
            })
    }
}

pub struct HttpListingsApi {
    client: Arc<dyn ToriClient>,
}

impl HttpListingsApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    async fn request<T: DeserializeOwned>(
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
        let response = self
            .client
            .execute(request)
            .await
            .map_err(map_http_error::<ListingsApiError>)?;
        decode_response(response.status, &response.body)
    }

    async fn request_with_service<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        service: &str,
    ) -> Result<T, ListingsApiError> {
        let request = RequestSpec::new(method, path, service);
        let response = self
            .client
            .execute(request)
            .await
            .map_err(map_http_error::<ListingsApiError>)?;
        decode_response(response.status, &response.body)
    }

    async fn empty(&self, method: Method, path: String) -> Result<(), ListingsApiError> {
        self.request::<Value>(method, path, None, None)
            .await
            .map(|_| ())
    }
}

impl From<HttpFailure> for ListingsApiError {
    fn from(failure: HttpFailure) -> Self {
        match failure {
            HttpFailure::Transport(_) => Self::Transport,
            HttpFailure::Local(_) => Self::UnexpectedResponse("HTTP adapter failed".to_owned()),
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

impl TaxonomyApi for HttpListingsApi {
    fn categories(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<UpstreamCategory>, ListingsApiError>> + Send + '_>>
    {
        Box::pin(async move {
            self.request_with_service::<UpstreamCategoryTaxonomy>(
                Method::GET,
                "/categories/taxonomy".to_owned(),
                compatibility::SERVICE_ITEM_CREATION,
            )
            .await
            .map(|taxonomy| taxonomy.categories)
        })
    }
}

impl ListingsApi for HttpListingsApi {
    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListingPage, ListingsApiError>> + Send + '_>>
    {
        Box::pin(self.request(
            Method::GET,
            format!("/search?limit={limit}&offset={offset}"),
            None,
            None,
        ))
    }

    fn listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>> {
        Box::pin(self.request(Method::GET, format!("/{listing_id}"), None, None))
    }

    fn update_listing<'a>(
        &'a self,
        listing_id: &'a str,
        etag: &'a str,
        fields: &'a BTreeMap<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>> {
        Box::pin(self.request(
            Method::PUT,
            format!("/my/listings/{listing_id}"),
            Some(Value::Object(fields.clone().into_iter().collect())),
            Some(etag),
        ))
    }

    fn dispose_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>> {
        Box::pin(self.empty(Method::POST, format!("/my/listings/{listing_id}/dispose")))
    }

    fn delete_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>> {
        Box::pin(self.empty(Method::DELETE, format!("/my/listings/{listing_id}")))
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

pub struct Listings<'a> {
    api: &'a dyn ListingsApi,
}

impl<'a> Listings<'a> {
    pub fn new(api: &'a dyn ListingsApi) -> Self {
        Self { api }
    }

    pub async fn list(&self) -> Result<ListingCollection, AppError> {
        let source = ListingCollectionSource { api: self.api };
        let mut listings = Vec::new();
        let mut facets = Vec::new();
        let mut seen = HashSet::new();
        let scan = scan_collection(&source, async |summaries, page_facets| {
            if facets.is_empty() {
                facets = page_facets.into_iter().map(normalize_facet).collect();
            }
            for summary in summaries {
                let listing_id = match value_id(&summary.id) {
                    Ok(listing_id) => listing_id,
                    Err(error) => return ControlFlow::Break(Err(error)),
                };
                let detail = match self.api.listing(&listing_id).await {
                    Ok(detail) => detail,
                    Err(error) => {
                        return ControlFlow::Break(Err(listing_error(
                            error,
                            Some(&listing_id),
                            RetryContext::read(OperationMethod::Get),
                        )));
                    }
                };
                match value_id(&detail.id) {
                    Ok(detail_id) if detail_id == listing_id => {}
                    Ok(_) => {
                        return ControlFlow::Break(Err(unexpected(
                            "listing detail returned a different ID",
                        )));
                    }
                    Err(error) => return ControlFlow::Break(Err(error)),
                }
                let (trade_type, price) = commerce_from_fields(&detail.fields);
                let normalized = match normalize_summary(summary, trade_type, price) {
                    Ok(normalized) => normalized,
                    Err(error) => return ControlFlow::Break(Err(error)),
                };
                if !seen.insert(normalized.listing_id.clone()) {
                    return ControlFlow::Break(Err(unexpected(
                        "listing pagination returned a duplicate item",
                    )));
                }
                listings.push(normalized);
            }
            ControlFlow::Continue(())
        })
        .await
        .map_err(list_scan_error)?;
        let total = match scan {
            CollectionScan::Complete { total } => total,
            CollectionScan::Match(Err(error)) => return Err(error),
            CollectionScan::Match(Ok(())) => unreachable!("listing scans stop only on errors"),
        };

        Ok(ListingCollection {
            listings,
            total: total as u64,
            facets,
        })
    }

    pub async fn show(&self, listing_id: &str) -> Result<ListingDetail, AppError> {
        validate_id(listing_id)?;
        match self.api.listing(listing_id).await {
            Ok(listing) => match normalize_listing_detail_for_id(listing, listing_id) {
                Ok(detail) => Ok(detail),
                Err(detail_error) => match self.find_summary(listing_id).await {
                    Ok(Some(summary)) => Ok(summary_detail(summary)),
                    Ok(None) | Err(_) => Err(detail_error),
                },
            },
            Err(detail_error) => {
                let summary = self.find_summary(listing_id).await;
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

    async fn find_summary(
        &self,
        listing_id: &str,
    ) -> Result<Option<ListingSummary>, ListingsApiError> {
        let source = ListingCollectionSource { api: self.api };
        let scan = scan_collection(&source, async |summaries, _| {
            for summary in summaries {
                match summary_id(&summary.id) {
                    Ok(id) if id == listing_id => return ControlFlow::Break(Ok(summary)),
                    Ok(_) => {}
                    Err(error) => return ControlFlow::Break(Err(error)),
                }
            }
            ControlFlow::Continue(())
        })
        .await
        .map_err(reconciliation_scan_error)?;

        match scan {
            CollectionScan::Complete { .. } => Ok(None),
            CollectionScan::Match(Err(error)) => Err(error),
            CollectionScan::Match(Ok(summary)) => {
                normalize_summary(summary, TradeType::Unknown, Price::unavailable(None))
                    .map(Some)
                    .map_err(|_| {
                        ListingsApiError::UnexpectedResponse(
                            "matching listing summary was malformed".to_owned(),
                        )
                    })
            }
        }
    }

    pub async fn snapshot(&self, listing_id: &str) -> Result<ListingSnapshot, AppError> {
        validate_id(listing_id)?;
        self.api
            .listing(listing_id)
            .await
            .map_err(|error| {
                listing_error(
                    error,
                    Some(listing_id),
                    RetryContext::read(OperationMethod::Get),
                )
            })
            .and_then(|listing| normalize_listing_for_id(listing, listing_id))
    }

    pub async fn update(
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

        let snapshot = self.snapshot(listing_id).await?;
        let mut complete_fields = snapshot.source_fields;
        complete_fields.extend(changes);
        match self
            .api
            .update_listing(listing_id, &snapshot.etag, &complete_fields)
            .await
        {
            Ok(listing) => {
                normalize_listing_for_id(listing, listing_id).map(|snapshot| snapshot.detail)
            }
            Err(ListingsApiError::Conflict) => {
                let fresh = self
                    .snapshot(listing_id)
                    .await
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

    pub async fn dispose(&self, listing_id: &str) -> Result<ListingMutation, AppError> {
        validate_id(listing_id)?;
        self.api
            .dispose_listing(listing_id)
            .await
            .map_err(|error| listing_mutation_error(error, listing_id, "dispose"))?;
        Ok(ListingMutation {
            listing_id: listing_id.to_owned(),
            state: ListingState::Disposed,
        })
    }

    pub async fn delete(&self, listing_id: &str) -> Result<ListingRef, AppError> {
        validate_id(listing_id)?;
        self.api
            .delete_listing(listing_id)
            .await
            .map_err(|error| listing_mutation_error(error, listing_id, "delete"))?;
        Ok(ListingRef {
            listing_id: listing_id.to_owned(),
        })
    }

    pub async fn copy_source(&self, listing_id: &str) -> Result<ListingCopySource, AppError> {
        let snapshot = self.snapshot(listing_id).await?;
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

fn normalize_summary(
    raw: UpstreamListingSummary,
    mut trade_type: TradeType,
    mut price: Price,
) -> Result<ListingSummary, AppError> {
    let listing_id = value_id(&raw.id)?;
    enrich_commerce_from_tori_subtitle(&raw.data.subtitle, &mut trade_type, &mut price);
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
    let (mut trade_type, mut price) = commerce_from_fields(&raw.fields);
    enrich_commerce_from_tori_subtitle(&raw.data.subtitle, &mut trade_type, &mut price);
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

fn enrich_commerce_from_tori_subtitle(
    subtitle: &str,
    trade_type: &mut TradeType,
    price: &mut Price,
) {
    let mut parts = subtitle.split_whitespace();
    let Some(marketplace) = parts.next() else {
        return;
    };
    let Some(trade_label) = parts.next() else {
        return;
    };
    if !marketplace.eq_ignore_ascii_case("Tori") {
        return;
    }
    let inferred_trade =
        crate::domain::commerce::normalize_trade_type(Some(&Value::String(trade_label.to_owned())));
    if inferred_trade == TradeType::Unknown {
        return;
    }
    let amount_text = parts.collect::<Vec<_>>().join(" ");
    let amount = euro_amount(&amount_text);
    let mut fields = serde_json::Map::new();
    fields.insert(
        "trade_type".to_owned(),
        Value::String(
            inferred_trade
                .normalized_value()
                .expect("recognized trade type has a normalized value")
                .to_owned(),
        ),
    );
    if let Some(amount) = amount {
        fields.insert("price".to_owned(), Value::Number(amount));
        fields.insert("currency".to_owned(), Value::String("EUR".to_owned()));
    }
    fields.insert(
        "price_display".to_owned(),
        Value::String(subtitle.to_owned()),
    );
    let (inferred_trade, inferred_price) = normalize_commerce_fields(&fields);
    if *trade_type == TradeType::Unknown {
        *trade_type = inferred_trade;
    }
    if price.kind == PriceKind::Unavailable {
        *price = inferred_price;
    }
}

fn euro_amount(value: &str) -> Option<serde_json::Number> {
    let value = value.trim().strip_suffix('€')?.trim();
    if value.is_empty() {
        return None;
    }
    let normalized = value
        .chars()
        .filter(|character| !matches!(character, ' ' | '\u{a0}' | '\u{202f}'))
        .map(|character| if character == ',' { '.' } else { character })
        .collect::<String>();
    if !normalized
        .chars()
        .all(|character| character.is_ascii_digit() || character == '.')
        || normalized.matches('.').count() > 1
    {
        return None;
    }
    normalized.parse().ok()
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

pub(super) fn resource_not_found(code: &str, resource: &str, id: &str) -> AppError {
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

pub(super) fn upstream_error(error: ListingsApiError, context: RetryContext) -> AppError {
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

fn list_scan_error(error: CollectionScanError<ListingsApiError>) -> AppError {
    match error {
        CollectionScanError::Source(error) => {
            listing_error(error, None, RetryContext::read(OperationMethod::Get))
        }
        CollectionScanError::TotalChanged { .. } => {
            unexpected("listing total changed during pagination")
        }
        CollectionScanError::PrematureEmptyPage { .. } => {
            unexpected("listing pagination ended before the reported total")
        }
        CollectionScanError::PageBoundExceeded => {
            unexpected("listing pagination exceeded its safety bound")
        }
    }
}

fn reconciliation_scan_error(error: CollectionScanError<ListingsApiError>) -> ListingsApiError {
    match error {
        CollectionScanError::Source(error) => error,
        CollectionScanError::TotalChanged { .. } => ListingsApiError::UnexpectedResponse(
            "listing total changed during identity reconciliation".to_owned(),
        ),
        CollectionScanError::PrematureEmptyPage { .. } => ListingsApiError::UnexpectedResponse(
            "listing pagination ended before the reported total".to_owned(),
        ),
        CollectionScanError::PageBoundExceeded => ListingsApiError::UnexpectedResponse(
            "listing identity reconciliation exceeded its safety bound".to_owned(),
        ),
    }
}

fn unexpected(message: &str) -> AppError {
    upstream_error(
        ListingsApiError::UnexpectedResponse(message.to_owned()),
        RetryContext::read(OperationMethod::Get),
    )
}
