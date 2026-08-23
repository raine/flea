use std::{future::Future, pin::Pin, str::FromStr};

use reqwest::{Method, StatusCode};
use serde_json::{Map, Number, Value};
use url::Url;

use crate::{
    domain::{
        envelope::NextAction,
        search::{SearchCollection, SearchListing, SearchPagination, SearchPrice},
    },
    error::{AppError, ExitClass},
    marketplace::vinted::{
        auth::{VintedAuthentication, VintedCredentialRecord},
        binding::VINTED_FI_BINDING,
    },
};

pub const SEARCH_LIMIT_DEFAULT: usize = 20;
pub const SEARCH_LIMIT_MAX: usize = 96;
pub const SEARCH_PAGE_MAX: usize = 100;
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const SEARCH_PATH: &str = "/svc-catalogue/items";

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

#[derive(Clone, Debug, PartialEq)]
pub struct SearchRequest {
    pub query: String,
    pub price_from: Option<u64>,
    pub price_to: Option<u64>,
    pub sort: SearchSort,
    pub page: usize,
    pub limit: usize,
}

pub trait VintedSearchApi: Send + Sync {
    fn execute<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a SearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(SearchCollection, Value), AppError>> + Send + 'a>>;
}

pub struct VintedSearch {
    auth: VintedAuthentication,
    api_base_url: String,
}

impl VintedSearch {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            api_base_url: VINTED_FI_BINDING.api_host.to_owned(),
        }
    }

    async fn execute_request(
        &self,
        credentials: &VintedCredentialRecord,
        request: &SearchRequest,
    ) -> Result<(SearchCollection, Value), AppError> {
        validate_request(request)?;
        let url = request_url(&self.api_base_url, request)?;
        let response = self
            .auth
            .authenticated_request(Method::GET, url.to_string(), credentials)?
            .send()
            .await
            .map_err(transport_error)?;
        let status = response.status();
        if !status.is_success() {
            return Err(status_error(status));
        }
        let raw = bounded_json(response).await?;
        let normalized = normalize_search(&raw, request)?;
        Ok((normalized, raw))
    }

    #[cfg(test)]
    fn with_api_base_url(mut self, api_base_url: String) -> Self {
        self.api_base_url = api_base_url;
        self
    }
}

impl VintedSearchApi for VintedSearch {
    fn execute<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a SearchRequest,
    ) -> Pin<Box<dyn Future<Output = Result<(SearchCollection, Value), AppError>> + Send + 'a>>
    {
        Box::pin(self.execute_request(credentials, request))
    }
}

impl Default for VintedSearch {
    fn default() -> Self {
        Self::new()
    }
}

fn validate_request(request: &SearchRequest) -> Result<(), AppError> {
    if request.query.len() > 256 {
        return Err(AppError::usage("search query must be at most 256 bytes"));
    }
    if !(1..=SEARCH_PAGE_MAX).contains(&request.page) {
        return Err(AppError::usage(format!(
            "search page must be between 1 and {SEARCH_PAGE_MAX}"
        )));
    }
    if !(1..=SEARCH_LIMIT_MAX).contains(&request.limit) {
        return Err(AppError::usage(format!(
            "search limit must be between 1 and {SEARCH_LIMIT_MAX}"
        )));
    }
    if request
        .price_from
        .zip(request.price_to)
        .is_some_and(|(from, to)| from > to)
    {
        return Err(AppError::usage(
            "minimum price must not exceed maximum price",
        ));
    }
    Ok(())
}

fn request_url(base_url: &str, request: &SearchRequest) -> Result<Url, AppError> {
    let mut url = Url::parse(base_url).map_err(|error| {
        AppError::unexpected("Vinted API binding is invalid").with_source(error)
    })?;
    url.set_path(SEARCH_PATH);
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("page", &request.page.to_string());
        query.append_pair("per_page", &request.limit.to_string());
        query.append_pair("order", request.sort.upstream());
        if !request.query.is_empty() {
            query.append_pair("search_text", &request.query);
        }
        if let Some(price) = request.price_from {
            query.append_pair("price_from", &price.to_string());
        }
        if let Some(price) = request.price_to {
            query.append_pair("price_to", &price.to_string());
        }
        query.append_pair("currency", "EUR");
    }
    Ok(url)
}

async fn bounded_json(mut response: reqwest::Response) -> Result<Value, AppError> {
    if response
        .content_length()
        .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
    {
        return Err(unexpected_response("response exceeded the size limit"));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| transport_error(error))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
            return Err(unexpected_response("response exceeded the size limit"));
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|_| unexpected_response("response was not valid JSON"))
}

fn normalize_search(raw: &Value, request: &SearchRequest) -> Result<SearchCollection, AppError> {
    let body = raw.get("data").unwrap_or(raw);
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected_response("items are unavailable"))?;
    let pagination = body
        .get("pagination")
        .and_then(Value::as_object)
        .ok_or_else(|| unexpected_response("pagination is unavailable"))?;
    let results = items
        .iter()
        .map(normalize_item)
        .collect::<Result<Vec<_>, _>>()?;
    let page = usize_value(pagination, &["current_page", "currentPage"]).unwrap_or(request.page);
    let limit = usize_value(pagination, &["per_page", "perPage"]).unwrap_or(request.limit);
    let total =
        usize_value(pagination, &["total_entries", "totalEntries"]).unwrap_or(results.len());
    let total_pages = usize_value(pagination, &["total_pages", "totalPages"])
        .unwrap_or_else(|| total.div_ceil(limit));
    let has_next = page < total_pages;
    Ok(SearchCollection {
        query: request.query.clone(),
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
        applied_filters: applied_filters(request),
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

fn absolute_item_url(value: &str, listing_id: &str) -> String {
    if value.starts_with("https://") {
        value.to_owned()
    } else if value.starts_with('/') {
        format!("{}{}", VINTED_FI_BINDING.host, value)
    } else {
        format!("{}/items/{listing_id}", VINTED_FI_BINDING.host)
    }
}

fn applied_filters(request: &SearchRequest) -> Vec<crate::domain::search::AppliedFilter> {
    let mut filters = Vec::new();
    if let Some(value) = request.price_from {
        filters.push(crate::domain::search::AppliedFilter {
            name: "price_from".to_owned(),
            values: vec![value.to_string()],
        });
    }
    if let Some(value) = request.price_to {
        filters.push(crate::domain::search::AppliedFilter {
            name: "price_to".to_owned(),
            values: vec![value.to_string()],
        });
    }
    if request.sort != SearchSort::Relevance {
        filters.push(crate::domain::search::AppliedFilter {
            name: "sort".to_owned(),
            values: vec![request.sort.upstream().to_owned()],
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

fn transport_error(error: reqwest::Error) -> AppError {
    let mut app_error = AppError::upstream(
        "vinted_search.transport_failed",
        "Vinted search could not be reached",
    )
    .with_source(error);
    app_error.upstream_transient = true;
    app_error.safe_to_retry = true;
    app_error
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
    use super::*;

    fn request() -> SearchRequest {
        SearchRequest {
            query: "takki".to_owned(),
            price_from: Some(10),
            price_to: Some(50),
            sort: SearchSort::Newest,
            page: 2,
            limit: 20,
        }
    }

    #[test]
    fn source_parameters_are_encoded_for_the_central_catalogue_endpoint() {
        let url = request_url("https://api.vinted.com", &request()).unwrap();
        assert_eq!(url.path(), "/svc-catalogue/items");
        let parameters = url
            .query_pairs()
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(parameters["search_text"], "takki");
        assert_eq!(parameters["order"], "newest_first");
        assert_eq!(parameters["price_from"], "10");
        assert_eq!(parameters["price_to"], "50");
        assert_eq!(parameters["currency"], "EUR");
    }

    #[test]
    fn source_item_model_normalizes_into_shared_search_output() {
        let raw = serde_json::json!({
            "items": [{
                "id": 123,
                "title": "Villakangastakki",
                "price": { "amount": "25.50", "currency_code": "EUR" },
                "url": "/items/123-villakangastakki",
                "user": { "id": 9, "login": "seller", "business": false },
                "photos": [{ "id": 1 }, { "id": 2 }]
            }],
            "pagination": {
                "current_page": 2,
                "per_page": 20,
                "total_entries": 55,
                "total_pages": 3
            }
        });

        let output = normalize_search(&raw, &request()).unwrap();

        assert_eq!(output.results[0].listing_id, "123");
        assert_eq!(
            output.results[0].price.as_ref().unwrap().amount,
            serde_json::json!(25.50)
        );
        assert_eq!(output.results[0].image_count, Some(2));
        assert_eq!(output.results[0].seller.as_deref(), Some("private"));
        assert_eq!(output.pagination.next_page, Some(3));
        assert_eq!(output.pagination.total, 55);
    }

    #[test]
    fn request_validation_rejects_inverted_prices() {
        let mut request = request();
        request.price_from = Some(51);
        assert_eq!(
            validate_request(&request).unwrap_err().exit_class,
            ExitClass::Usage
        );
    }

    #[test]
    fn test_client_can_override_the_central_api_host() {
        let search = VintedSearch::new().with_api_base_url("http://127.0.0.1:1".to_owned());
        assert_eq!(search.api_base_url, "http://127.0.0.1:1");
    }
}
