use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    future::Future,
    pin::Pin,
    str::FromStr,
};

use reqwest::{Method, StatusCode};
use serde_json::{Map, Number, Value};
use url::Url;

use crate::{
    domain::{
        envelope::NextAction,
        search::{
            AppliedFilter, FilterCollection, SearchCollection, SearchFacet, SearchFacetOption,
            SearchFacetRange, SearchListing, SearchPagination, SearchPrice,
        },
    },
    error::{AppError, ExitClass},
    marketplace::{
        PortalId,
        vinted::{
            auth::{VintedAuthentication, VintedCredentialRecord},
            binding::VINTED_FI_BINDING,
        },
    },
    transport::{Transport, TransportError, TransportErrorKind, TransportResponse},
};

pub const SEARCH_LIMIT_DEFAULT: usize = 20;
pub const SEARCH_LIMIT_MAX: usize = 96;
pub const SEARCH_PAGE_MAX: usize = 100;
pub const FILTER_OPTION_LIMIT_DEFAULT: usize = 500;
pub const FILTER_OPTION_LIMIT_MAX: usize = 5_000;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const ITEMS_PATH: &str = "/svc-catalogue/items";
const FILTERS_PATH: &str = "/svc-filters/filters";
const FACETS_PATH: &str = "/svc-filters/filters/facets";
const OPTION_SEARCH_PATH: &str = "/svc-filters/filters/search";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SearchSort {
    Relevance,
    Newest,
    PriceAsc,
    PriceDesc,
}

impl SearchSort {
    pub const fn upstream(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Newest => "newest_first",
            Self::PriceAsc => "price_low_to_high",
            Self::PriceDesc => "price_high_to_low",
        }
    }
}

#[derive(Clone, Debug)]
pub struct DecimalAmount {
    text: String,
    cents: u64,
}

impl DecimalAmount {
    pub fn as_str(&self) -> &str {
        &self.text
    }
}

impl fmt::Display for DecimalAmount {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.text)
    }
}

impl PartialEq for DecimalAmount {
    fn eq(&self, other: &Self) -> bool {
        self.cents == other.cents
    }
}

impl Eq for DecimalAmount {}

impl PartialOrd for DecimalAmount {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for DecimalAmount {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.cents.cmp(&other.cents)
    }
}

impl FromStr for DecimalAmount {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (whole, fraction) = match value.split_once('.') {
            Some((whole, fraction)) => (whole, Some(fraction)),
            None => (value, None),
        };
        if whole.is_empty() || whole.len() > 9 || !whole.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(
                "price must be a non-negative decimal with at most 9 whole digits".to_owned(),
            );
        }
        let fraction = fraction.unwrap_or("");
        if value.contains('.')
            && (fraction.is_empty()
                || fraction.len() > 2
                || !fraction.bytes().all(|byte| byte.is_ascii_digit()))
        {
            return Err("price must have one or two decimal places".to_owned());
        }
        let whole_value = whole
            .parse::<u64>()
            .map_err(|_| "price is outside the supported range".to_owned())?;
        let fractional_value = match fraction.len() {
            0 => 0,
            1 => fraction.parse::<u64>().unwrap() * 10,
            2 => fraction.parse::<u64>().unwrap(),
            _ => unreachable!(),
        };
        let cents = whole_value
            .checked_mul(100)
            .and_then(|value| value.checked_add(fractional_value))
            .ok_or_else(|| "price is outside the supported range".to_owned())?;
        Ok(Self {
            text: value.to_owned(),
            cents,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttributeSelection {
    pub code: String,
    pub ids: Vec<String>,
}

impl FromStr for AttributeSelection {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let (code, ids) = value
            .split_once('=')
            .ok_or_else(|| "attribute must use CODE=ID[,ID...]".to_owned())?;
        validate_filter_code(code).map_err(|error| error.to_string())?;
        let ids = ids
            .split(',')
            .map(|id| {
                validate_option_id(id).map_err(|error| error.to_string())?;
                Ok(id.to_owned())
            })
            .collect::<Result<Vec<_>, String>>()?;
        if ids.is_empty() {
            return Err("attribute must contain at least one option ID".to_owned());
        }
        Ok(Self {
            code: code.to_owned(),
            ids,
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchRequest {
    pub query: Option<String>,
    pub price_from: Option<DecimalAmount>,
    pub price_to: Option<DecimalAmount>,
    pub sort: Option<SearchSort>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub catalog: Vec<String>,
    pub brand: Vec<String>,
    pub size: Vec<String>,
    pub status: Vec<String>,
    pub color: Vec<String>,
    pub material: Vec<String>,
    pub attributes: Vec<AttributeSelection>,
    pub include_facets: bool,
    pub include_hidden: bool,
    pub option_limit: Option<usize>,
    pub raw: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct FilterContextInput {
    pub query: Option<String>,
    pub price_from: Option<DecimalAmount>,
    pub price_to: Option<DecimalAmount>,
    pub sort: Option<SearchSort>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub catalog: Vec<String>,
    pub brand: Vec<String>,
    pub size: Vec<String>,
    pub status: Vec<String>,
    pub color: Vec<String>,
    pub material: Vec<String>,
    pub attributes: Vec<AttributeSelection>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum FilterRequest {
    List {
        context: FilterContextInput,
        include_hidden: bool,
        option_limit: Option<usize>,
        raw: bool,
    },
    Facets {
        code: String,
        context: FilterContextInput,
        option_limit: Option<usize>,
        raw: bool,
    },
    Search {
        code: String,
        text: String,
        context: FilterContextInput,
        option_limit: Option<usize>,
        raw: bool,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogueContext {
    pub query: String,
    pub price_from: Option<DecimalAmount>,
    pub price_to: Option<DecimalAmount>,
    pub sort: SearchSort,
    pub page: usize,
    pub limit: usize,
    pub currency: String,
    pub attributes: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CatalogueOperation {
    Items,
    Filters,
    Facets {
        filter_code: String,
    },
    OptionSearch {
        filter_code: String,
        search_text: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CatalogueRequest {
    pub operation: CatalogueOperation,
    pub context: CatalogueContext,
}

#[derive(Debug, PartialEq)]
pub enum SearchResult {
    Search(Box<SearchCollection>),
    Filters(FilterCollection),
    Raw(Value),
}

pub trait VintedSearchSession: Send + Sync {
    fn credentials<'a>(
        &'a self,
        portal: PortalId,
    ) -> Pin<Box<dyn Future<Output = Result<VintedCredentialRecord, AppError>> + Send + 'a>>;
}

impl<F> VintedSearchSession for F
where
    F: Fn(PortalId) -> Result<VintedCredentialRecord, AppError> + Send + Sync,
{
    fn credentials<'a>(
        &'a self,
        portal: PortalId,
    ) -> Pin<Box<dyn Future<Output = Result<VintedCredentialRecord, AppError>> + Send + 'a>> {
        Box::pin(std::future::ready(self(portal)))
    }
}

impl VintedSearchSession for super::session::VintedCredentialResolver {
    fn credentials<'a>(
        &'a self,
        portal: PortalId,
    ) -> Pin<Box<dyn Future<Output = Result<VintedCredentialRecord, AppError>> + Send + 'a>> {
        Box::pin(self.credentials(portal))
    }
}

pub trait VintedSearchApi: Send + Sync {
    fn execute<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a CatalogueRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;
}

pub struct VintedSearch<'a> {
    session: &'a dyn VintedSearchSession,
    api: &'a dyn VintedSearchApi,
}

impl<'a> VintedSearch<'a> {
    pub fn new(session: &'a dyn VintedSearchSession, api: &'a dyn VintedSearchApi) -> Self {
        Self { session, api }
    }

    pub async fn execute(
        &self,
        portal: PortalId,
        input: SearchRequest,
    ) -> Result<SearchResult, AppError> {
        if input.raw && input.include_facets {
            return Err(AppError::usage(
                "--raw cannot be combined with --include-facets; use `vinted filter list --raw`",
            ));
        }
        let include_facets = input.include_facets;
        let include_hidden = input.include_hidden;
        let option_limit = validate_option_limit(input.option_limit)?;
        let raw_output = input.raw;
        let context = prepare_search_context(input)?;
        let credentials = self.session.credentials(portal).await?;
        let items_request = CatalogueRequest {
            operation: CatalogueOperation::Items,
            context: context.clone(),
        };
        let raw = self.api.execute(&credentials, &items_request).await?;
        if raw_output {
            return Ok(SearchResult::Raw(raw));
        }
        let mut normalized = normalize_search(&raw, &context)?;
        if include_facets {
            let filters_request = CatalogueRequest {
                operation: CatalogueOperation::Filters,
                context: context.clone(),
            };
            let filters_raw = self.api.execute(&credentials, &filters_request).await?;
            let filter_collection =
                normalize_filter_list(&filters_raw, &context, include_hidden, option_limit)?;
            normalized.applied_filters = filter_collection.applied_filters;
            normalized.facets = filter_collection.filters;
        }
        Ok(SearchResult::Search(Box::new(normalized)))
    }

    pub async fn execute_filter(
        &self,
        portal: PortalId,
        input: FilterRequest,
    ) -> Result<SearchResult, AppError> {
        let (operation, context_input, include_hidden, option_limit, raw_output) = match input {
            FilterRequest::List {
                context,
                include_hidden,
                option_limit,
                raw,
            } => (
                CatalogueOperation::Filters,
                context,
                include_hidden,
                option_limit,
                raw,
            ),
            FilterRequest::Facets {
                code,
                context,
                option_limit,
                raw,
            } => {
                validate_filter_code(&code)?;
                (
                    CatalogueOperation::Facets { filter_code: code },
                    context,
                    true,
                    option_limit,
                    raw,
                )
            }
            FilterRequest::Search {
                code,
                text,
                context,
                option_limit,
                raw,
            } => {
                validate_filter_code(&code)?;
                validate_option_query(&text)?;
                (
                    CatalogueOperation::OptionSearch {
                        filter_code: code,
                        search_text: text,
                    },
                    context,
                    true,
                    option_limit,
                    raw,
                )
            }
        };
        let option_limit = validate_option_limit(option_limit)?;
        let context = prepare_filter_context(context_input)?;
        let request = CatalogueRequest { operation, context };
        let credentials = self.session.credentials(portal).await?;
        let raw = self.api.execute(&credentials, &request).await?;
        if raw_output {
            return Ok(SearchResult::Raw(raw));
        }
        let collection = match &request.operation {
            CatalogueOperation::Filters => {
                normalize_filter_list(&raw, &request.context, include_hidden, option_limit)?
            }
            CatalogueOperation::Facets { filter_code } => normalize_option_collection(
                &raw,
                &request.context,
                filter_code,
                None,
                option_limit,
            )?,
            CatalogueOperation::OptionSearch {
                filter_code,
                search_text,
            } => normalize_option_collection(
                &raw,
                &request.context,
                filter_code,
                Some(search_text),
                option_limit,
            )?,
            CatalogueOperation::Items => {
                unreachable!("filter requests never use the items endpoint")
            }
        };
        Ok(SearchResult::Filters(collection))
    }
}

pub struct HttpVintedSearchApi {
    auth: VintedAuthentication,
    native_api_base_url: String,
}

impl HttpVintedSearchApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            native_api_base_url: VINTED_FI_BINDING.native_api_host.to_owned(),
        }
    }

    async fn execute_request(
        &self,
        credentials: &VintedCredentialRecord,
        request: &CatalogueRequest,
    ) -> Result<Value, AppError> {
        let url = request_url(&self.native_api_base_url, request)?;
        let transport_request = self.auth.authenticated_request(
            Method::GET,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        let response = self
            .auth
            .executor()
            .execute(transport_request)
            .await
            .map_err(execution_error)?;
        if !response.status.is_success() {
            return Err(status_error(response.status));
        }
        bounded_json(response)
    }

    #[cfg(test)]
    fn with_native_api_base_url(mut self, native_api_base_url: String) -> Self {
        self.native_api_base_url = native_api_base_url;
        self
    }
}

impl VintedSearchApi for HttpVintedSearchApi {
    fn execute<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a CatalogueRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.execute_request(credentials, request))
    }
}

impl Default for HttpVintedSearchApi {
    fn default() -> Self {
        Self::new()
    }
}

fn prepare_search_context(input: SearchRequest) -> Result<CatalogueContext, AppError> {
    prepare_context(FilterContextInput {
        query: input.query,
        price_from: input.price_from,
        price_to: input.price_to,
        sort: input.sort,
        page: input.page,
        limit: input.limit,
        catalog: input.catalog,
        brand: input.brand,
        size: input.size,
        status: input.status,
        color: input.color,
        material: input.material,
        attributes: input.attributes,
    })
}

fn prepare_filter_context(input: FilterContextInput) -> Result<CatalogueContext, AppError> {
    prepare_context(input)
}

fn prepare_context(input: FilterContextInput) -> Result<CatalogueContext, AppError> {
    let mut attributes = BTreeMap::new();
    insert_convenience(&mut attributes, "catalog", input.catalog)?;
    insert_convenience(&mut attributes, "brand", input.brand)?;
    insert_convenience(&mut attributes, "size", input.size)?;
    insert_convenience(&mut attributes, "status", input.status)?;
    insert_convenience(&mut attributes, "color", input.color)?;
    insert_convenience(&mut attributes, "material", input.material)?;
    for selection in input.attributes {
        if attributes.contains_key(&selection.code) {
            return Err(AppError::usage(format!(
                "filter code `{}` was provided by more than one option",
                selection.code
            )));
        }
        insert_attribute(&mut attributes, selection.code, selection.ids)?;
    }
    let context = CatalogueContext {
        query: input.query.unwrap_or_default(),
        price_from: input.price_from,
        price_to: input.price_to,
        sort: input.sort.unwrap_or(SearchSort::Relevance),
        page: input.page.unwrap_or(1),
        limit: input.limit.unwrap_or(SEARCH_LIMIT_DEFAULT),
        currency: "EUR".to_owned(),
        attributes,
    };
    validate_context(&context)?;
    Ok(context)
}

fn insert_convenience(
    attributes: &mut BTreeMap<String, Vec<String>>,
    code: &str,
    ids: Vec<String>,
) -> Result<(), AppError> {
    if ids.is_empty() {
        return Ok(());
    }
    insert_attribute(attributes, code.to_owned(), ids)
}

fn insert_attribute(
    attributes: &mut BTreeMap<String, Vec<String>>,
    code: String,
    ids: Vec<String>,
) -> Result<(), AppError> {
    validate_filter_code(&code)?;
    let mut seen = BTreeSet::new();
    for id in &ids {
        validate_option_id(id)?;
        if !seen.insert(id.clone()) {
            return Err(AppError::usage(format!(
                "filter `{code}` contains duplicate option ID `{id}`"
            )));
        }
    }
    if ids.is_empty() {
        return Err(AppError::usage(format!(
            "filter `{code}` must contain at least one option ID"
        )));
    }
    attributes.insert(code, ids);
    Ok(())
}

fn validate_filter_code(code: &str) -> Result<(), AppError> {
    if code.is_empty()
        || code.len() > 128
        || code.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, '[' | ']' | '&' | '=' | ',')
        })
    {
        return Err(AppError::usage(
            "filter code must contain 1 to 128 non-whitespace characters and must not contain brackets, ampersands, equals signs, or commas",
        ));
    }
    Ok(())
}

fn validate_option_id(id: &str) -> Result<(), AppError> {
    if id.is_empty()
        || id.len() > 256
        || id.chars().any(|character| {
            character.is_control() || character.is_whitespace() || character == ','
        })
    {
        return Err(AppError::usage(
            "filter option ID must contain 1 to 256 non-whitespace characters and must not contain commas",
        ));
    }
    Ok(())
}

fn validate_option_query(text: &str) -> Result<(), AppError> {
    if text.trim().is_empty() {
        return Err(AppError::usage("filter option query must not be empty"));
    }
    if text.len() > 256 {
        return Err(AppError::usage(
            "filter option query must be at most 256 bytes",
        ));
    }
    Ok(())
}

fn validate_option_limit(limit: Option<usize>) -> Result<usize, AppError> {
    let limit = limit.unwrap_or(FILTER_OPTION_LIMIT_DEFAULT);
    if !(1..=FILTER_OPTION_LIMIT_MAX).contains(&limit) {
        return Err(AppError::usage(format!(
            "filter option limit must be between 1 and {FILTER_OPTION_LIMIT_MAX}"
        )));
    }
    Ok(limit)
}

fn validate_context(context: &CatalogueContext) -> Result<(), AppError> {
    if context.query.len() > 256 {
        return Err(AppError::usage("search query must be at most 256 bytes"));
    }
    if !(1..=SEARCH_PAGE_MAX).contains(&context.page) {
        return Err(AppError::usage(format!(
            "search page must be between 1 and {SEARCH_PAGE_MAX}"
        )));
    }
    if !(1..=SEARCH_LIMIT_MAX).contains(&context.limit) {
        return Err(AppError::usage(format!(
            "search limit must be between 1 and {SEARCH_LIMIT_MAX}"
        )));
    }
    if context
        .price_from
        .as_ref()
        .zip(context.price_to.as_ref())
        .is_some_and(|(from, to)| from > to)
    {
        return Err(AppError::usage(
            "minimum price must not exceed maximum price",
        ));
    }
    Ok(())
}

fn request_url(base_url: &str, request: &CatalogueRequest) -> Result<Url, AppError> {
    let mut url = Url::parse(base_url).map_err(|error| {
        AppError::unexpected("Vinted API binding is invalid").with_source(error)
    })?;
    let path = match request.operation {
        CatalogueOperation::Items => ITEMS_PATH,
        CatalogueOperation::Filters => FILTERS_PATH,
        CatalogueOperation::Facets { .. } => FACETS_PATH,
        CatalogueOperation::OptionSearch { .. } => OPTION_SEARCH_PATH,
    };
    url.set_path(path);
    {
        let mut query = url.query_pairs_mut();
        match &request.operation {
            CatalogueOperation::Facets { filter_code } => {
                query.append_pair("filter_code", filter_code);
            }
            CatalogueOperation::OptionSearch {
                filter_code,
                search_text,
            } => {
                query.append_pair("filter_search_code", filter_code);
                query.append_pair("filter_search_text", search_text);
            }
            CatalogueOperation::Items | CatalogueOperation::Filters => {}
        }
        query.append_pair("page", &request.context.page.to_string());
        query.append_pair("per_page", &request.context.limit.to_string());
        query.append_pair("order", request.context.sort.upstream());
        if !request.context.query.is_empty() {
            query.append_pair("search_text", &request.context.query);
        }
        if let Some(price) = &request.context.price_from {
            query.append_pair("price_from", price.as_str());
        }
        if let Some(price) = &request.context.price_to {
            query.append_pair("price_to", price.as_str());
        }
        query.append_pair("currency", &request.context.currency);
        for (code, ids) in &request.context.attributes {
            query.append_pair(&format!("attribute_ids[{code}]"), &ids.join(","));
        }
    }
    Ok(url)
}

fn bounded_json(response: TransportResponse) -> Result<Value, AppError> {
    serde_json::from_slice(&response.body)
        .map_err(|_| unexpected_response("response was not valid JSON"))
}

fn payload<'a>(raw: &'a Value, expected_key: &str) -> &'a Value {
    if raw.get(expected_key).is_some() {
        return raw;
    }
    raw.get("data")
        .filter(|data| data.get(expected_key).is_some())
        .unwrap_or(raw)
}

fn normalize_search(raw: &Value, context: &CatalogueContext) -> Result<SearchCollection, AppError> {
    let body = payload(raw, "items");
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected_response("items are unavailable"))?;
    let pagination = body.get("pagination").and_then(Value::as_object);
    let results = items
        .iter()
        .map(normalize_item)
        .collect::<Result<Vec<_>, _>>()?;
    let page = pagination
        .and_then(|value| usize_value(value, &["current_page", "currentPage"]))
        .unwrap_or(context.page);
    let limit = pagination
        .and_then(|value| usize_value(value, &["per_page", "perPage"]))
        .unwrap_or(context.limit);
    let total = pagination
        .and_then(|value| usize_value(value, &["total_entries", "totalEntries"]))
        .unwrap_or(results.len());
    let total_pages = pagination
        .and_then(|value| usize_value(value, &["total_pages", "totalPages"]))
        .unwrap_or_else(|| total.div_ceil(limit));
    let has_next = page < total_pages;
    Ok(SearchCollection {
        query: context.query.clone(),
        location: None,
        results,
        pagination: SearchPagination {
            page,
            limit,
            returned: items.len(),
            total,
            has_next,
            next_page: has_next.then_some(page + 1),
            capped: false,
        },
        applied_filters: applied_filters(context),
        facets: Vec::new(),
        resolved_area: None,
        explain: None,
    })
}

fn normalize_item(item: &Value) -> Result<SearchListing, AppError> {
    let object = item
        .as_object()
        .ok_or_else(|| unexpected_response("an item was not an object"))?;
    let listing_id = identifier(object.get("id"))
        .ok_or_else(|| unexpected_response("an item ID was unavailable"))?;
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| unexpected_response("an item title was unavailable"))?
        .to_owned();
    let price = object.get("price").and_then(normalize_price);
    let url = object
        .get("url")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(|value| absolute_item_url(value, &listing_id))
        .unwrap_or_else(|| format!("{}/items/{listing_id}", VINTED_FI_BINDING.host));
    let image_count = object
        .get("photos")
        .and_then(Value::as_array)
        .map(Vec::len)
        .or_else(|| {
            object
                .get("photo")
                .filter(|value| !value.is_null())
                .map(|_| 1)
        });
    let seller = object
        .get("user")
        .and_then(Value::as_object)
        .and_then(|user| user.get("business"))
        .and_then(Value::as_bool)
        .map(|business| if business { "business" } else { "private" }.to_owned());
    Ok(SearchListing {
        listing_id,
        title,
        price,
        location: None,
        category_id: None,
        category_path: None,
        url,
        published_at: None,
        image_count,
        distance: None,
        condition: None,
        shipping: None,
        seller,
        match_explanation: None,
    })
}

fn normalize_price(value: &Value) -> Option<SearchPrice> {
    let object = value.as_object()?;
    let amount = object.get("amount").and_then(|amount| match amount {
        Value::Number(amount) => Some(Value::Number(amount.clone())),
        Value::String(amount) => Number::from_str(amount).ok().map(Value::Number),
        _ => None,
    })?;
    let currency = ["currency_code", "currencyCode", "currency"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .filter(|currency| !currency.is_empty())
        .map(str::to_owned);
    Some(SearchPrice { amount, currency })
}

fn normalize_filter_list(
    raw: &Value,
    context: &CatalogueContext,
    include_hidden: bool,
    option_limit: usize,
) -> Result<FilterCollection, AppError> {
    let body = payload(raw, "filters");
    let filters = body
        .get("filters")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected_response("filters are unavailable"))?;
    let selected = selected_index(body.get("selected_filters"), &context.attributes)?;
    let mut normalized = Vec::new();
    for filter in filters {
        let facet = normalize_filter(filter, &selected, option_limit)?;
        if !facet.hidden || include_hidden || selected.contains_key(&facet.name) {
            normalized.push(facet);
        }
    }
    let total = filters.len();
    let returned = normalized.len();
    Ok(FilterCollection {
        filters: normalized,
        filter_code: None,
        query: (!context.query.is_empty()).then(|| context.query.clone()),
        option_query: None,
        returned,
        total,
        truncated: returned < total,
        applied_filters: applied_filters_with_selected(context, &selected),
    })
}

fn normalize_filter(
    value: &Value,
    selected: &BTreeMap<String, BTreeSet<String>>,
    option_limit: usize,
) -> Result<SearchFacet, AppError> {
    let object = value
        .as_object()
        .ok_or_else(|| unexpected_response("a filter was not an object"))?;
    let code = required_string(object, "code", "a filter code was unavailable")?;
    let label = required_string(object, "title", "a filter title was unavailable")?;
    let selection_type = required_string(
        object,
        "selection_type",
        "a filter selection type was unavailable",
    )?;
    let options_value = object
        .get("options")
        .ok_or_else(|| unexpected_response("filter options were unavailable"))?;
    let options = options_value.as_array().map(Vec::as_slice).unwrap_or(&[]);
    let mut flattened = Vec::new();
    flatten_options(options, &code, selected, None, 0, &mut flattened)?;
    let flattened_count = flattened.len();
    flattened.truncate(option_limit);
    let total = object
        .get("total_count_max")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(flattened_count)
        .max(flattened_count);
    let lazy = object
        .get("is_lazy")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let display_type = object
        .get("display_type")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let range = object.get("minimum_validation").and_then(|minimum| {
        let money = minimum.as_object()?;
        Some(SearchFacetRange {
            minimum: money.get("amount").cloned(),
            maximum: None,
            step: None,
            unit: ["currency_code", "currencyCode", "currency"]
                .into_iter()
                .find_map(|key| money.get(key).and_then(Value::as_str))
                .map(str::to_owned),
            from_name: Some("price_from".to_owned()),
            to_name: Some("price_to".to_owned()),
        })
    });
    Ok(SearchFacet {
        name: code,
        label,
        facet_type: display_type
            .clone()
            .unwrap_or_else(|| selection_type.clone()),
        returned_option_count: flattened.len(),
        option_count: total,
        truncated: lazy || total > flattened.len() || flattened_count > flattened.len(),
        options: flattened,
        range,
        selection_type: Some(selection_type),
        display_type,
        hidden: object
            .get("is_hidden")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        lazy,
        selection_highlighted: object
            .get("is_selection_highlighted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        position: object
            .get("position")
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok()),
    })
}

fn normalize_option_collection(
    raw: &Value,
    context: &CatalogueContext,
    filter_code: &str,
    option_query: Option<&str>,
    option_limit: usize,
) -> Result<FilterCollection, AppError> {
    let body = payload(raw, "options");
    let options = body
        .get("options")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected_response("filter options are unavailable"))?;
    let selected = selected_index(body.get("selected_filters"), &context.attributes)?;
    let mut flattened = Vec::new();
    flatten_options(options, filter_code, &selected, None, 0, &mut flattened)?;
    let flattened_count = flattened.len();
    flattened.truncate(option_limit);
    let total = ["total_count", "total_count_max"]
        .into_iter()
        .find_map(|key| body.get(key).and_then(Value::as_u64))
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(flattened_count)
        .max(flattened_count);
    let returned = flattened.len();
    let facet = SearchFacet {
        name: filter_code.to_owned(),
        label: filter_code.to_owned(),
        facet_type: "list".to_owned(),
        options: flattened,
        option_count: total,
        returned_option_count: returned,
        truncated: total > returned || flattened_count > returned,
        range: None,
        selection_type: None,
        display_type: None,
        hidden: false,
        lazy: false,
        selection_highlighted: body
            .get("is_selection_highlighted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        position: None,
    };
    Ok(FilterCollection {
        filters: vec![facet],
        filter_code: Some(filter_code.to_owned()),
        query: (!context.query.is_empty()).then(|| context.query.clone()),
        option_query: option_query.map(str::to_owned),
        returned,
        total,
        truncated: total > returned || flattened_count > returned,
        applied_filters: applied_filters_with_selected(context, &selected),
    })
}

fn flatten_options(
    options: &[Value],
    code: &str,
    selected: &BTreeMap<String, BTreeSet<String>>,
    parent: Option<&str>,
    depth: usize,
    output: &mut Vec<SearchFacetOption>,
) -> Result<(), AppError> {
    for option in options {
        let object = option
            .as_object()
            .ok_or_else(|| unexpected_response("a filter option was not an object"))?;
        let id = required_string(object, "id", "a filter option ID was unavailable")?;
        let title = required_string(object, "title", "a filter option title was unavailable")?;
        output.push(SearchFacetOption {
            value: id.clone(),
            label: title.clone(),
            name: title,
            parent_value: parent.map(str::to_owned),
            depth,
            hits: object.get("items_count").and_then(Value::as_i64),
            selected: selected.get(code).is_some_and(|ids| ids.contains(&id)),
        });
        if let Some(children) = object.get("options") {
            let children = children
                .as_array()
                .ok_or_else(|| unexpected_response("nested filter options were not an array"))?;
            flatten_options(children, code, selected, Some(&id), depth + 1, output)?;
        }
    }
    Ok(())
}

fn selected_index(
    value: Option<&Value>,
    requested: &BTreeMap<String, Vec<String>>,
) -> Result<BTreeMap<String, BTreeSet<String>>, AppError> {
    let mut selected = requested
        .iter()
        .map(|(code, ids)| {
            (
                code.clone(),
                ids.iter().cloned().collect::<BTreeSet<String>>(),
            )
        })
        .collect::<BTreeMap<_, _>>();
    let entries: Vec<&Value> = match value {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(values)) => values.iter().collect(),
        Some(Value::Object(_)) => vec![value.unwrap()],
        Some(_) => {
            return Err(unexpected_response(
                "selected filters had an unsupported shape",
            ));
        }
    };
    for entry in entries {
        let object = entry
            .as_object()
            .ok_or_else(|| unexpected_response("a selected filter was not an object"))?;
        let code = required_string(object, "code", "a selected filter code was unavailable")?;
        let ids = object
            .get("ids")
            .and_then(Value::as_array)
            .ok_or_else(|| unexpected_response("selected filter IDs were unavailable"))?;
        let target = selected.entry(code).or_default();
        for id in ids {
            let id = identifier(Some(id))
                .ok_or_else(|| unexpected_response("a selected filter ID was unavailable"))?;
            target.insert(id);
        }
    }
    Ok(selected)
}

fn required_string(
    object: &Map<String, Value>,
    key: &str,
    reason: &str,
) -> Result<String, AppError> {
    identifier(object.get(key)).ok_or_else(|| unexpected_response(reason))
}

fn absolute_item_url(value: &str, listing_id: &str) -> String {
    if value.starts_with("https://") {
        value.to_owned()
    } else if value.starts_with('/') {
        format!("{}{}", VINTED_FI_BINDING.host, value)
    } else {
        format!("{}/items/{listing_id}", VINTED_FI_BINDING.host)
    }
}

fn applied_filters_with_selected(
    context: &CatalogueContext,
    selected: &BTreeMap<String, BTreeSet<String>>,
) -> Vec<AppliedFilter> {
    let mut selected_context = context.clone();
    selected_context.attributes = selected
        .iter()
        .filter(|(_, ids)| !ids.is_empty())
        .map(|(code, ids)| (code.clone(), ids.iter().cloned().collect()))
        .collect();
    applied_filters(&selected_context)
}

fn applied_filters(context: &CatalogueContext) -> Vec<AppliedFilter> {
    let mut filters = context
        .attributes
        .iter()
        .map(|(name, values)| AppliedFilter {
            name: name.clone(),
            values: values.clone(),
        })
        .collect::<Vec<_>>();
    if let Some(value) = &context.price_from {
        filters.push(AppliedFilter {
            name: "price_from".to_owned(),
            values: vec![value.to_string()],
        });
    }
    if let Some(value) = &context.price_to {
        filters.push(AppliedFilter {
            name: "price_to".to_owned(),
            values: vec![value.to_string()],
        });
    }
    if context.sort != SearchSort::Relevance {
        filters.push(AppliedFilter {
            name: "sort".to_owned(),
            values: vec![context.sort.upstream().to_owned()],
        });
    }
    filters
}

fn usize_value(object: &Map<String, Value>, keys: &[&str]) -> Option<usize> {
    keys.iter().find_map(|key| {
        object
            .get(*key)
            .and_then(Value::as_u64)
            .and_then(|value| usize::try_from(value).ok())
    })
}

fn identifier(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(value)) if !value.is_empty() => Some(value.clone()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    }
}

fn transport_error(error: TransportError) -> AppError {
    let mut app_error = AppError::upstream(
        "vinted_search.transport_failed",
        "Vinted search could not be reached",
    )
    .with_source(error);
    app_error.upstream_transient = true;
    app_error.safe_to_retry = true;
    app_error
}

fn execution_error(error: TransportError) -> AppError {
    if let Some(status) = error.status
        && !status.is_success()
    {
        return status_error(status);
    }
    if error.kind == TransportErrorKind::ResponseTooLarge {
        unexpected_response("response exceeded the size limit")
    } else {
        transport_error(error)
    }
}

fn status_error(status: StatusCode) -> AppError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        let mut error = AppError::authentication(
            "vinted_search.authentication_required",
            "Vinted search requires a valid authenticated session",
        );
        error.next_actions.push(NextAction {
            command: "flea vinted --portal fi auth login".to_owned(),
        });
        return error;
    }
    let mut error = AppError::new(
        "vinted_search.upstream_failed",
        format!("Vinted search returned HTTP {}", status.as_u16()),
        ExitClass::Upstream,
    );
    error.upstream_transient = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
    error.safe_to_retry = error.upstream_transient;
    error
}

fn unexpected_response(reason: &str) -> AppError {
    AppError::upstream(
        "vinted_search.unexpected_response",
        "Vinted returned an unsupported search response",
    )
    .with_details(serde_json::json!({ "reason": reason }))
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    struct FixtureApi {
        response: Value,
        requests: Mutex<Vec<CatalogueRequest>>,
    }

    impl FixtureApi {
        fn new(response: Value) -> Self {
            Self {
                response,
                requests: Mutex::new(Vec::new()),
            }
        }
    }

    impl VintedSearchApi for FixtureApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            request: &'a CatalogueRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.requests.lock().unwrap().push(request.clone());
            Box::pin(async { Ok(self.response.clone()) })
        }
    }

    struct RoutingFixtureApi {
        requests: Mutex<Vec<CatalogueRequest>>,
    }

    impl VintedSearchApi for RoutingFixtureApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            request: &'a CatalogueRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.requests.lock().unwrap().push(request.clone());
            Box::pin(async move {
                let fixture = match &request.operation {
                    CatalogueOperation::Items => include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/vinted/items.json"
                    )),
                    CatalogueOperation::Filters => include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/vinted/filters.json"
                    )),
                    CatalogueOperation::Facets { .. } => include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/vinted/facets.json"
                    )),
                    CatalogueOperation::OptionSearch { .. } => include_str!(concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/vinted/option-search.json"
                    )),
                };
                Ok(serde_json::from_str(fixture).unwrap())
            })
        }
    }

    fn credentials() -> VintedCredentialRecord {
        VintedCredentialRecord {
            portal: PortalId::Fi,
            user_id: "user-1".to_owned(),
            login: Some("fixture".to_owned()),
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            access_expires_at_unix: u64::MAX,
            device_uuid: "device".to_owned(),
            anonymous_id: "anonymous".to_owned(),
            user_device_token: None,
        }
    }

    fn items_response() -> Value {
        serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vinted/items.json"
        )))
        .unwrap()
    }

    fn context() -> CatalogueContext {
        CatalogueContext {
            query: "takki".to_owned(),
            price_from: Some("10.50".parse().unwrap()),
            price_to: Some("50".parse().unwrap()),
            sort: SearchSort::Newest,
            page: 2,
            limit: 20,
            currency: "EUR".to_owned(),
            attributes: BTreeMap::from([
                ("brand".to_owned(), vec!["53".to_owned(), "88".to_owned()]),
                ("catalog".to_owned(), vec!["123".to_owned()]),
            ]),
        }
    }

    #[tokio::test]
    async fn basic_search_preserves_defaults_and_normalized_output() {
        let api = FixtureApi::new(items_response());
        let session = |_| Ok(credentials());
        let output = VintedSearch::new(&session, &api)
            .execute(
                PortalId::Fi,
                SearchRequest {
                    query: Some("takki".to_owned()),
                    ..SearchRequest::default()
                },
            )
            .await
            .unwrap();
        let SearchResult::Search(collection) = output else {
            panic!("expected normalized search output");
        };
        assert_eq!(collection.query, "takki");
        assert_eq!(collection.results[0].listing_id, "123");
        let requests = api.requests.lock().unwrap();
        assert_eq!(requests[0].operation, CatalogueOperation::Items);
        assert_eq!(requests[0].context.page, 1);
        assert_eq!(requests[0].context.limit, SEARCH_LIMIT_DEFAULT);
    }

    #[tokio::test]
    async fn search_facets_use_the_exact_items_context() {
        let api = RoutingFixtureApi {
            requests: Mutex::new(Vec::new()),
        };
        let session = |_| Ok(credentials());
        let output = VintedSearch::new(&session, &api)
            .execute(
                PortalId::Fi,
                SearchRequest {
                    query: Some("takki".to_owned()),
                    brand: vec!["53".to_owned()],
                    include_facets: true,
                    ..SearchRequest::default()
                },
            )
            .await
            .unwrap();
        let SearchResult::Search(collection) = output else {
            panic!("expected normalized search output");
        };
        assert!(!collection.facets.is_empty());
        let requests = api.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].operation, CatalogueOperation::Items);
        assert_eq!(requests[1].operation, CatalogueOperation::Filters);
        assert_eq!(requests[0].context, requests[1].context);
    }

    #[tokio::test]
    async fn raw_output_bypasses_response_normalization() {
        let malformed = serde_json::json!({"future": "shape"});
        let api = FixtureApi::new(malformed.clone());
        let session = |_| Ok(credentials());
        let output = VintedSearch::new(&session, &api)
            .execute(
                PortalId::Fi,
                SearchRequest {
                    raw: true,
                    ..SearchRequest::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(output, SearchResult::Raw(malformed));
    }

    #[tokio::test]
    async fn every_filter_raw_flow_returns_the_exact_upstream_document() {
        let malformed = serde_json::json!({"future": "filter shape"});
        for request in [
            FilterRequest::List {
                context: FilterContextInput::default(),
                include_hidden: false,
                option_limit: None,
                raw: true,
            },
            FilterRequest::Facets {
                code: "brand".to_owned(),
                context: FilterContextInput::default(),
                option_limit: None,
                raw: true,
            },
            FilterRequest::Search {
                code: "brand".to_owned(),
                text: "Mar".to_owned(),
                context: FilterContextInput::default(),
                option_limit: None,
                raw: true,
            },
        ] {
            let api = FixtureApi::new(malformed.clone());
            let session = |_| Ok(credentials());
            assert_eq!(
                VintedSearch::new(&session, &api)
                    .execute_filter(PortalId::Fi, request)
                    .await
                    .unwrap(),
                SearchResult::Raw(malformed.clone())
            );
        }
    }

    #[test]
    fn central_parameters_encode_dynamic_selections_and_decimals() {
        let request = CatalogueRequest {
            operation: CatalogueOperation::Items,
            context: context(),
        };
        let url = request_url("https://api.vinted.com", &request).unwrap();
        assert_eq!(url.path(), ITEMS_PATH);
        let parameters = url.query_pairs().collect::<BTreeMap<_, _>>();
        assert_eq!(parameters["attribute_ids[brand]"], "53,88");
        assert_eq!(parameters["attribute_ids[catalog]"], "123");
        assert_eq!(parameters["price_from"], "10.50");
        assert!(!parameters.contains_key("brand_ids"));
        assert!(!parameters.contains_key("catalog_ids"));
    }

    #[test]
    fn every_documented_sort_order_uses_the_central_wire_value() {
        for (sort, expected) in [
            (SearchSort::Relevance, "relevance"),
            (SearchSort::Newest, "newest_first"),
            (SearchSort::PriceAsc, "price_low_to_high"),
            (SearchSort::PriceDesc, "price_high_to_low"),
        ] {
            let mut request_context = context();
            request_context.sort = sort;
            let url = request_url(
                "https://api.vinted.com",
                &CatalogueRequest {
                    operation: CatalogueOperation::Items,
                    context: request_context,
                },
            )
            .unwrap();
            assert_eq!(
                url.query_pairs()
                    .find(|(name, _)| name == "order")
                    .unwrap()
                    .1,
                expected
            );
        }
    }

    #[test]
    fn common_and_generic_attributes_encode_multiple_ids() {
        let prepared = prepare_context(FilterContextInput {
            catalog: vec!["123".to_owned(), "124".to_owned()],
            brand: vec!["53".to_owned(), "88".to_owned()],
            size: vec!["4".to_owned(), "5".to_owned()],
            status: vec!["1".to_owned(), "2".to_owned()],
            color: vec!["7".to_owned()],
            material: vec!["12".to_owned(), "14".to_owned()],
            attributes: vec!["contextual_code=90,91".parse().unwrap()],
            ..FilterContextInput::default()
        })
        .unwrap();
        let url = request_url(
            "https://api.vinted.com",
            &CatalogueRequest {
                operation: CatalogueOperation::Items,
                context: prepared,
            },
        )
        .unwrap();
        let parameters = url.query_pairs().collect::<BTreeMap<_, _>>();
        for (code, value) in [
            ("catalog", "123,124"),
            ("brand", "53,88"),
            ("size", "4,5"),
            ("status", "1,2"),
            ("color", "7"),
            ("material", "12,14"),
            ("contextual_code", "90,91"),
        ] {
            let key = format!("attribute_ids[{code}]");
            assert_eq!(parameters[key.as_str()], value);
        }
    }

    #[test]
    fn every_filter_endpoint_uses_the_shared_context() {
        for (operation, path) in [
            (CatalogueOperation::Filters, FILTERS_PATH),
            (
                CatalogueOperation::Facets {
                    filter_code: "brand".to_owned(),
                },
                FACETS_PATH,
            ),
            (
                CatalogueOperation::OptionSearch {
                    filter_code: "brand".to_owned(),
                    search_text: "Mar".to_owned(),
                },
                OPTION_SEARCH_PATH,
            ),
        ] {
            let url = request_url(
                "https://api.vinted.com",
                &CatalogueRequest {
                    operation,
                    context: context(),
                },
            )
            .unwrap();
            assert_eq!(url.path(), path);
            let parameters = url.query_pairs().collect::<BTreeMap<_, _>>();
            assert_eq!(parameters["search_text"], "takki");
            assert_eq!(parameters["attribute_ids[brand]"], "53,88");
            assert_eq!(parameters["currency"], "EUR");
        }
    }

    #[test]
    fn decimal_prices_reject_malformed_values_and_compare_numerically() {
        for value in ["", "-1", ".5", "10.", "1e3", "10,5", "1.234"] {
            assert!(value.parse::<DecimalAmount>().is_err(), "{value}");
        }
        assert_eq!(
            "10.5".parse::<DecimalAmount>().unwrap(),
            "10.50".parse::<DecimalAmount>().unwrap()
        );
        let mut invalid = context();
        invalid.price_from = Some("50.01".parse().unwrap());
        assert_eq!(
            validate_context(&invalid).unwrap_err().exit_class,
            ExitClass::Usage
        );
    }

    #[test]
    fn bounded_error_responses_preserve_protocol_status_classification() {
        let error = execution_error(TransportError::response(
            TransportErrorKind::ResponseTooLarge,
            StatusCode::UNAUTHORIZED,
        ));

        assert_eq!(error.code, "vinted_search.authentication_required");
    }

    #[test]
    fn dynamic_selection_validation_rejects_conflicts_and_duplicates() {
        let conflict = FilterContextInput {
            brand: vec!["53".to_owned()],
            attributes: vec!["brand=88".parse().unwrap()],
            ..FilterContextInput::default()
        };
        assert_eq!(
            prepare_context(conflict).unwrap_err().exit_class,
            ExitClass::Usage
        );
        let duplicate = FilterContextInput {
            brand: vec!["53".to_owned(), "53".to_owned()],
            ..FilterContextInput::default()
        };
        assert_eq!(
            prepare_context(duplicate).unwrap_err().exit_class,
            ExitClass::Usage
        );
    }

    #[test]
    fn direct_and_wrapped_items_payloads_normalize_equally() {
        let raw = items_response();
        let wrapped = serde_json::json!({"data": raw.clone()});
        assert_eq!(
            normalize_search(&raw, &context()).unwrap(),
            normalize_search(&wrapped, &context()).unwrap()
        );
    }

    #[test]
    fn recursive_filters_preserve_metadata_counts_and_selected_state() {
        let raw: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vinted/filters.json"
        )))
        .unwrap();
        let collection = normalize_filter_list(&raw, &context(), true, 100).unwrap();
        let catalog = collection
            .filters
            .iter()
            .find(|filter| filter.name == "catalog")
            .unwrap();
        assert_eq!(catalog.selection_type.as_deref(), Some("single"));
        assert_eq!(catalog.display_type.as_deref(), Some("list"));
        assert_eq!(catalog.options[1].parent_value.as_deref(), Some("100"));
        assert_eq!(catalog.options[1].depth, 1);
        assert_eq!(catalog.options[1].hits, Some(12));
        assert!(catalog.options[2].selected);
        let brand = collection
            .filters
            .iter()
            .find(|filter| filter.name == "brand")
            .unwrap();
        assert!(brand.lazy);
        assert!(brand.truncated);
        assert!(collection.filters.iter().any(|filter| filter.hidden));
    }

    #[test]
    fn hidden_filters_are_omitted_unless_requested() {
        let raw: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vinted/filters.json"
        )))
        .unwrap();
        let visible = normalize_filter_list(&raw, &context(), false, 100).unwrap();
        assert!(!visible.filters.iter().any(|filter| filter.hidden));
        assert!(visible.truncated);
    }

    #[test]
    fn lazy_facets_accept_wrapped_payload_and_report_truncation() {
        let raw: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vinted/facets.json"
        )))
        .unwrap();
        let collection = normalize_option_collection(
            &serde_json::json!({"data": raw}),
            &context(),
            "brand",
            None,
            2,
        )
        .unwrap();
        assert_eq!(collection.returned, 2);
        assert_eq!(collection.total, 8);
        assert!(collection.truncated);
        assert!(collection.filters[0].options[0].selected);
    }

    #[test]
    fn option_search_accepts_object_selected_filters() {
        let raw: Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/vinted/option-search.json"
        )))
        .unwrap();
        let collection =
            normalize_option_collection(&raw, &context(), "brand", Some("Mar"), 100).unwrap();
        assert_eq!(collection.option_query.as_deref(), Some("Mar"));
        assert!(collection.filters[0].options[0].selected);
    }

    #[test]
    fn malformed_filter_and_item_responses_fail_safely() {
        assert!(normalize_search(&serde_json::json!({"items": [{}]}), &context()).is_err());
        assert!(
            normalize_filter_list(
                &serde_json::json!({"filters": [{"code": "brand"}]}),
                &context(),
                true,
                100
            )
            .is_err()
        );
    }

    #[test]
    fn item_normalization_does_not_infer_unverified_fields_from_display_text() {
        let mut raw = items_response();
        raw["items"][0]["item_box"] = serde_json::json!({
            "first_line": "Coats · New",
            "second_line": "Helsinki · one minute ago"
        });
        let item = normalize_search(&raw, &context())
            .unwrap()
            .results
            .remove(0);
        assert!(item.category_id.is_none());
        assert!(item.condition.is_none());
        assert!(item.published_at.is_none());
        assert!(item.location.is_none());
    }

    #[test]
    fn vinted_fixtures_contain_only_synthetic_non_secret_data() {
        for fixture in [
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/vinted/items.json"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/vinted/filters.json"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/vinted/facets.json"
            )),
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/vinted/option-search.json"
            )),
        ] {
            let lowercase = fixture.to_ascii_lowercase();
            for forbidden in [
                "bearer ",
                "access_token",
                "refresh_token",
                "device_uuid",
                "anonymous_id",
                "request_id",
                "cookie",
            ] {
                assert!(!lowercase.contains(forbidden), "found {forbidden}");
            }
        }
    }

    #[test]
    fn test_client_can_override_the_central_api_host() {
        let api =
            HttpVintedSearchApi::new().with_native_api_base_url("http://127.0.0.1:1".to_owned());
        assert_eq!(api.native_api_base_url, "http://127.0.0.1:1");
    }
}
