use std::{collections::BTreeMap, fmt, sync::Arc};

use reqwest::{Method, StatusCode};
use serde_json::{Map, Value};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};
use url::form_urlencoded;

use crate::{
    api::client::{RequestSpec, ToriClient, compatibility},
    domain::search::{
        AppliedFilter, LocationCollection, SearchArea, SearchAreaContext, SearchCollection,
        SearchFacet, SearchFacetOption, SearchFacetRange, SearchListing, SearchLocation,
        SearchLocationContext, SearchPagination, SearchPrice,
    },
    error::{AppError, ExitClass},
};

pub const SEARCH_PAGE_MAX: usize = 50;
pub const SEARCH_LIMIT_DEFAULT: usize = 20;
pub const SEARCH_LIMIT_MAX: usize = 300;
pub const SEARCH_RADIUS_MAX_KM: f64 = 1000.0;
pub const SEARCH_FACET_OPTION_LIMIT: usize = 500;
pub const LOCATION_RESULT_LIMIT: usize = 100;
pub const SEARCH_AREA_LOCATION_MAX: usize = 20;

const SEARCH_PATH: &str = "/search/SEARCH_ID_BAP_COMMON";
const SEARCH_CLIENT: &str = "android";

pub trait PublicSearchApi: Send + Sync {
    fn search(&self, request: &UpstreamSearchRequest) -> Result<Value, SearchApiError>;
    fn location_metadata(&self) -> Result<Value, SearchApiError>;
}

pub struct HttpPublicSearchApi {
    client: Arc<dyn ToriClient>,
}

impl HttpPublicSearchApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    fn get(&self, path: String) -> Result<Value, SearchApiError> {
        let request = RequestSpec::new(Method::GET, path, compatibility::SERVICE_SEARCH);
        let client = Arc::clone(&self.client);
        let response = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|error| SearchApiError::Transport(error.to_string()))?
                        .block_on(client.execute(request))
                        .map_err(|error| SearchApiError::Transport(error.to_string()))
                })
                .join()
                .map_err(|_| SearchApiError::Transport("HTTP worker panicked".to_owned()))?
        })?;
        if !response.status.is_success() {
            return Err(match response.status {
                StatusCode::BAD_REQUEST => SearchApiError::Rejected,
                status => SearchApiError::Upstream(status.as_u16()),
            });
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| SearchApiError::Unexpected("invalid JSON response".to_owned()))
    }
}

impl PublicSearchApi for HttpPublicSearchApi {
    fn search(&self, request: &UpstreamSearchRequest) -> Result<Value, SearchApiError> {
        self.get(request.path_and_query())
    }

    fn location_metadata(&self) -> Result<Value, SearchApiError> {
        self.get(
            UpstreamSearchRequest {
                page: 1,
                limit: 0,
                include_filters: true,
                ..UpstreamSearchRequest::default()
            }
            .path_and_query(),
        )
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpstreamSearchRequest {
    pub query: String,
    pub page: usize,
    pub limit: usize,
    pub include_filters: bool,
    pub parameters: BTreeMap<String, Vec<String>>,
}

impl UpstreamSearchRequest {
    pub fn path_and_query(&self) -> String {
        let mut parameters = self.parameters.clone();
        parameters.insert("client".to_owned(), vec![SEARCH_CLIENT.to_owned()]);
        parameters.insert("page".to_owned(), vec![self.page.to_string()]);
        parameters.insert("rows".to_owned(), vec![self.limit.to_string()]);
        if self.include_filters {
            parameters.insert("include_filters".to_owned(), vec!["true".to_owned()]);
        }
        if !self.query.is_empty() {
            parameters.insert("q".to_owned(), vec![self.query.clone()]);
        }
        let mut encoder = form_urlencoded::Serializer::new(String::new());
        for (name, values) in parameters {
            for value in values {
                encoder.append_pair(&name, &value);
            }
        }
        format!("{SEARCH_PATH}?{}", encoder.finish())
    }
}

#[derive(Clone, thiserror::Error, PartialEq, Eq)]
pub enum SearchApiError {
    #[error("search request was rejected")]
    Rejected,
    #[error("search transport failed")]
    Transport(String),
    #[error("Tori search returned HTTP {0}")]
    Upstream(u16),
    #[error("unexpected search response: {0}")]
    Unexpected(String),
}

impl fmt::Debug for SearchApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected => formatter.write_str("Rejected"),
            Self::Transport(_) => formatter.write_str("Transport([REDACTED])"),
            Self::Upstream(status) => formatter.debug_tuple("Upstream").field(status).finish(),
            Self::Unexpected(_) => formatter.write_str("Unexpected([REDACTED])"),
        }
    }
}

pub struct PublicSearch<'a> {
    api: &'a dyn PublicSearchApi,
}

impl<'a> PublicSearch<'a> {
    pub fn new(api: &'a dyn PublicSearchApi) -> Self {
        Self { api }
    }

    pub fn execute(
        &self,
        request: &UpstreamSearchRequest,
        resolved_location: Option<SearchLocation>,
    ) -> Result<(SearchCollection, Value), AppError> {
        self.execute_with_area(request, resolved_location, None)
    }

    pub fn execute_with_area(
        &self,
        request: &UpstreamSearchRequest,
        resolved_location: Option<SearchLocation>,
        resolved_area: Option<SearchArea>,
    ) -> Result<(SearchCollection, Value), AppError> {
        let raw = self.api.search(request).map_err(search_error)?;
        let normalized = normalize_search(&raw, request, resolved_location, resolved_area)?;
        Ok((normalized, raw))
    }

    pub fn locations(&self, query: &str) -> Result<LocationCollection, AppError> {
        let raw = self.api.location_metadata().map_err(search_error)?;
        let mut locations = extract_locations(&raw)?;
        let needle = normalize_name(query);
        if !needle.is_empty() {
            locations.retain(|location| normalize_name(&location.name).contains(&needle));
        }
        locations.sort_by(|left, right| {
            normalize_name(&left.name)
                .cmp(&normalize_name(&right.name))
                .then_with(|| left.depth.cmp(&right.depth))
                .then_with(|| left.id.cmp(&right.id))
        });
        let total = locations.len();
        let truncated = total > LOCATION_RESULT_LIMIT;
        locations.truncate(LOCATION_RESULT_LIMIT);
        let returned = locations.len();
        Ok(LocationCollection {
            locations,
            returned,
            total,
            truncated,
        })
    }

    pub fn resolve_location(&self, value: &str) -> Result<SearchLocation, AppError> {
        let raw = self.api.location_metadata().map_err(search_error)?;
        resolve_location(&extract_locations(&raw)?, value)
    }

    pub fn resolve_area(&self, values: &[String]) -> Result<SearchArea, AppError> {
        let raw = self.api.location_metadata().map_err(search_error)?;
        let locations = extract_locations(&raw)?;
        let mut resolved = Vec::with_capacity(values.len());
        for value in values {
            let location = resolve_location(&locations, value)?;
            if resolved
                .iter()
                .any(|existing: &SearchLocation| existing.id == location.id)
            {
                return Err(AppError::validation(
                    "search.area_duplicate_location",
                    "area contains the same location more than once",
                )
                .with_details(serde_json::json!({
                    "location": value,
                    "location_id": location.id
                })));
            }
            resolved.push(location);
        }
        Ok(SearchArea {
            locations: resolved,
        })
    }
}

fn resolve_location(locations: &[SearchLocation], value: &str) -> Result<SearchLocation, AppError> {
    let value = value.trim();
    if value.is_empty() {
        return Err(AppError::usage("location must not be empty"));
    }
    if let Some(location) = locations.iter().find(|location| location.id == value) {
        return Ok(location.clone());
    }
    let needle = normalize_name(value);
    let mut matches = locations
        .iter()
        .filter(|location| normalize_name(&location.name) == needle)
        .cloned()
        .collect::<Vec<_>>();
    matches.sort_by(|left, right| {
        left.depth
            .cmp(&right.depth)
            .then_with(|| left.id.cmp(&right.id))
    });
    match matches.len() {
        0 => Err(
            AppError::validation("search.location_not_found", "location was not found")
                .with_details(serde_json::json!({
                    "location": value,
                    "suggestion": "Run `tori location search` with this name to find Tori location names and IDs"
                })),
        ),
        1 => Ok(matches.remove(0)),
        _ => Err(AppError::validation(
            "search.location_ambiguous",
            "location name matches more than one Tori location",
        )
        .with_details(serde_json::json!({
            "location": value,
            "matches": matches,
            "suggestion": "Use one of the exact location IDs shown in matches"
        }))),
    }
}

fn normalize_search(
    raw: &Value,
    request: &UpstreamSearchRequest,
    resolved_location: Option<SearchLocation>,
    resolved_area: Option<SearchArea>,
) -> Result<SearchCollection, AppError> {
    let object = raw
        .as_object()
        .ok_or_else(|| unexpected("search response must be an object"))?;
    let docs = object
        .get("docs")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected("search response omitted docs"))?;
    let results = docs
        .iter()
        .map(normalize_listing)
        .collect::<Result<Vec<_>, _>>()?;
    let metadata = object.get("metadata").and_then(Value::as_object);
    let total = metadata
        .and_then(|value| value.get("result_size"))
        .and_then(|value| value.get("match_count"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(results.len());
    let upstream_last = metadata
        .and_then(|value| value.get("paging"))
        .and_then(|value| value.get("last"))
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1)
        .min(SEARCH_PAGE_MAX);
    let total_pages = if request.limit == 0 {
        0
    } else {
        total.div_ceil(request.limit)
    };
    let accessible_pages = upstream_last.min(total_pages.max(1));
    let next_page = (request.page < accessible_pages).then_some(request.page + 1);
    let facets = request
        .include_filters
        .then(|| object.get("filters").and_then(Value::as_array))
        .flatten()
        .map(|filters| filters.iter().map(normalize_facet).collect())
        .unwrap_or_default();
    let has_location_context = resolved_location.is_some() || resolved_area.is_some();
    let applied_filters = metadata
        .and_then(|value| value.get("params"))
        .and_then(Value::as_object)
        .map(|params| normalize_applied_filters(params, has_location_context))
        .unwrap_or_else(|| {
            request
                .parameters
                .iter()
                .filter(|(name, values)| {
                    !values.is_empty() && (!has_location_context || name.as_str() != "location")
                })
                .map(|(name, values)| AppliedFilter {
                    name: name.clone(),
                    values: values.clone(),
                })
                .collect()
        });

    Ok(SearchCollection {
        query: request.query.clone(),
        location: resolved_location.map(|location| SearchLocationContext {
            id: location.id,
            name: location.name,
            parent: location.parent,
        }),
        pagination: SearchPagination {
            page: request.page,
            limit: request.limit,
            returned: results.len(),
            total,
            has_next: next_page.is_some(),
            next_page,
            capped: total_pages > SEARCH_PAGE_MAX,
        },
        results,
        applied_filters,
        facets,
        resolved_area: resolved_area.map(|area| SearchAreaContext {
            locations: area
                .locations
                .into_iter()
                .map(|location| SearchLocationContext {
                    id: location.id,
                    name: location.name,
                    parent: location.parent,
                })
                .collect(),
        }),
    })
}

fn normalize_listing(doc: &Value) -> Result<SearchListing, AppError> {
    let object = doc
        .as_object()
        .ok_or_else(|| unexpected("search document must be an object"))?;
    let listing_id = string_value(object.get("ad_id").or_else(|| object.get("id")))
        .ok_or_else(|| unexpected("search document has an invalid ID"))?;
    let title = object
        .get("heading")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let price = object
        .get("price")
        .and_then(Value::as_object)
        .and_then(|price| {
            price.get("amount").map(|amount| SearchPrice {
                amount: amount.clone(),
                currency: price
                    .get("currency_code")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })
        });
    let image_count = if object.contains_key("image_urls") || object.contains_key("image") {
        let mut image_urls = string_array(object.get("image_urls"));
        if let Some(url) = object
            .get("image")
            .and_then(Value::as_object)
            .and_then(|image| image.get("url"))
            .and_then(Value::as_str)
            && !image_urls.iter().any(|existing| existing == url)
        {
            image_urls.push(url.to_owned());
        }
        Some(image_urls.len())
    } else {
        None
    };
    let annotations: Vec<Value> = ["labels", "extras"]
        .into_iter()
        .filter_map(|key| object.get(key).and_then(Value::as_array))
        .flatten()
        .cloned()
        .collect();
    let flags = string_array(object.get("flags"));
    let seller = semantic_value(object.get("seller"))
        .or_else(|| semantic_value(object.get("seller_type")))
        .or_else(|| seller_from_segment(object.get("dealer_segment")))
        .or_else(|| {
            flags
                .iter()
                .find(|flag| matches!(flag.as_str(), "private" | "business" | "dealer"))
                .cloned()
        })
        .or_else(|| label_value(&annotations, &["seller", "private", "business", "dealer"]));
    let shipping = ["shipping", "shipping_available", "shipping_exists"]
        .iter()
        .find_map(|key| {
            let value = object.get(*key)?;
            value.as_bool().or_else(|| {
                value
                    .as_object()
                    .and_then(|value| value.get("available"))
                    .and_then(Value::as_bool)
            })
        })
        .or_else(|| {
            labels_or_flags_contain(
                &annotations,
                &flags,
                &["shipping", "tori_diili", "fiksudiili"],
            )
            .then_some(true)
        });
    let condition = semantic_value(object.get("condition"))
        .or_else(|| label_value(&annotations, &["condition"]));
    let published_at = object
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(format_timestamp);
    let url = object
        .get("canonical_url")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("https://www.tori.fi/recommerce/forsale/item/{listing_id}"));

    Ok(SearchListing {
        listing_id,
        title,
        price,
        location: object
            .get("location")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
        url,
        published_at,
        image_count,
        distance: object
            .get("distance")
            .and_then(Value::as_f64)
            .filter(|distance| distance.is_finite() && *distance > 0.0),
        condition,
        shipping,
        seller,
    })
}

fn normalize_facet(value: &Value) -> SearchFacet {
    let object = value.as_object();
    let name = object
        .and_then(|value| value.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let label = object
        .and_then(|value| value.get("display_name"))
        .and_then(Value::as_str)
        .unwrap_or(&name)
        .to_owned();
    let facet_type = object
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("UNKNOWN")
        .to_owned();
    let mut options = Vec::new();
    if let Some(items) = object
        .and_then(|value| value.get("filter_items"))
        .and_then(Value::as_array)
    {
        flatten_options(items, None, 0, &mut options);
    }
    let option_count = options.len();
    options.truncate(SEARCH_FACET_OPTION_LIMIT);
    let range = object.and_then(|value| {
        let has_range = ["min_value", "max_value", "step", "name_from", "name_to"]
            .iter()
            .any(|key| value.contains_key(*key));
        has_range.then(|| SearchFacetRange {
            minimum: value.get("min_value").cloned(),
            maximum: value.get("max_value").cloned(),
            step: value.get("step").cloned(),
            unit: value.get("unit").and_then(Value::as_str).map(str::to_owned),
            from_name: value
                .get("name_from")
                .and_then(Value::as_str)
                .map(str::to_owned),
            to_name: value
                .get("name_to")
                .and_then(Value::as_str)
                .map(str::to_owned),
        })
    });
    SearchFacet {
        name,
        label,
        facet_type,
        options,
        option_count,
        truncated: option_count > SEARCH_FACET_OPTION_LIMIT,
        range,
    }
}

fn flatten_options(
    items: &[Value],
    parent: Option<&str>,
    depth: usize,
    output: &mut Vec<SearchFacetOption>,
) {
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let value = string_value(object.get("value")).unwrap_or_default();
        output.push(SearchFacetOption {
            value: value.clone(),
            label: object
                .get("display_name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            name: object
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned(),
            parent_value: parent.map(str::to_owned),
            depth,
            hits: object.get("hits").and_then(Value::as_i64),
            selected: object
                .get("selected")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
        if let Some(children) = object.get("filter_items").and_then(Value::as_array) {
            flatten_options(children, Some(&value), depth + 1, output);
        }
    }
}

fn extract_locations(raw: &Value) -> Result<Vec<SearchLocation>, AppError> {
    let filters = raw
        .get("filters")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected("location metadata omitted filters"))?;
    let location = filters
        .iter()
        .find(|filter| filter.get("name").and_then(Value::as_str) == Some("location"))
        .ok_or_else(|| unexpected("location metadata omitted the location facet"))?;
    let items = location
        .get("filter_items")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected("location facet has an invalid shape"))?;
    let mut output = Vec::new();
    flatten_locations(items, None, 0, &mut output);
    Ok(output)
}

fn flatten_locations(
    items: &[Value],
    parent: Option<&str>,
    depth: usize,
    output: &mut Vec<SearchLocation>,
) {
    for item in items {
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(id) = string_value(object.get("value")) else {
            continue;
        };
        let name = object
            .get("display_name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        output.push(SearchLocation {
            id,
            name: name.clone(),
            parent: parent.map(str::to_owned),
            depth,
        });
        if let Some(children) = object.get("filter_items").and_then(Value::as_array) {
            flatten_locations(children, Some(&name), depth + 1, output);
        }
    }
}

fn normalize_applied_filters(
    params: &Map<String, Value>,
    has_resolved_location: bool,
) -> Vec<AppliedFilter> {
    params
        .iter()
        .filter(|(name, _)| name.as_str() != "q")
        .filter(|(name, _)| !has_resolved_location || name.as_str() != "location")
        .filter_map(|(name, value)| {
            let values: Vec<String> = match value {
                Value::Array(values) => values
                    .iter()
                    .filter_map(|value| string_value(Some(value)))
                    .collect(),
                other => string_value(Some(other)).into_iter().collect(),
            };
            (!values.is_empty()).then(|| AppliedFilter {
                name: name.clone(),
                values,
            })
        })
        .collect()
}

fn format_timestamp(timestamp: i64) -> Option<String> {
    let nanoseconds = if timestamp.unsigned_abs() >= 100_000_000_000 {
        i128::from(timestamp) * 1_000_000
    } else {
        i128::from(timestamp) * 1_000_000_000
    };
    OffsetDateTime::from_unix_timestamp_nanos(nanoseconds)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

fn semantic_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        Value::Object(value) => value
            .get("label")
            .or_else(|| value.get("text"))
            .or_else(|| value.get("display_name"))
            .or_else(|| value.get("value"))
            .and_then(|value| semantic_value(Some(value))),
        _ => None,
    }
}

fn seller_from_segment(value: Option<&Value>) -> Option<String> {
    match string_value(value).as_deref() {
        Some("1") => Some("private".to_owned()),
        Some("3") => Some("business".to_owned()),
        _ => None,
    }
}

fn label_value(labels: &[Value], identifiers: &[&str]) -> Option<String> {
    labels.iter().find_map(|label| {
        let object = label.as_object()?;
        let id = object
            .get("id")
            .or_else(|| object.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        identifiers
            .iter()
            .any(|identifier| id.contains(identifier))
            .then(|| {
                object
                    .get("text")
                    .or_else(|| object.get("display_name"))
                    .or_else(|| object.get("label"))
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .to_owned()
            })
    })
}

fn labels_or_flags_contain(labels: &[Value], flags: &[String], identifiers: &[&str]) -> bool {
    flags.iter().any(|flag| {
        let flag = flag.to_ascii_lowercase();
        identifiers
            .iter()
            .any(|identifier| flag.contains(identifier))
    }) || labels.iter().any(|label| {
        let Some(object) = label.as_object() else {
            return false;
        };
        let id = object
            .get("id")
            .or_else(|| object.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        identifiers.iter().any(|identifier| id.contains(identifier))
    })
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn string_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn normalize_name(value: &str) -> String {
    value
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn search_error(error: SearchApiError) -> AppError {
    match error {
        SearchApiError::Rejected => {
            AppError::validation("search.rejected", "Tori rejected the search parameters")
        }
        SearchApiError::Unexpected(_) => unexpected("Tori returned an unexpected search response"),
        SearchApiError::Transport(_) => AppError::new(
            "upstream.request_failed",
            "the Tori search request failed",
            ExitClass::Upstream,
        )
        .retryable(true),
        SearchApiError::Upstream(status) => AppError::new(
            "upstream.request_failed",
            "the Tori search request failed",
            ExitClass::Upstream,
        )
        .retryable(status == 429 || status >= 500),
    }
}

fn unexpected(message: &str) -> AppError {
    AppError::new("upstream.unexpected_response", message, ExitClass::Upstream)
}
