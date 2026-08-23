use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    domain::{
        envelope::NextAction,
        search::{
            SearchCollection, SearchExplainFailure, SearchExplainSummary, SearchMatchExplanation,
        },
    },
    error::AppError,
    marketplace::tori::{
        item::{PublicItemApi, PublicItems},
        search::{
            PublicSearch, PublicSearchApi, SEARCH_AREA_LOCATION_MAX, SEARCH_FACET_OPTION_LIMIT,
            SEARCH_FACET_OPTION_LIMIT_MAX, SEARCH_LIMIT_DEFAULT, SEARCH_LIMIT_MAX, SEARCH_PAGE_MAX,
            SEARCH_RADIUS_MAX_KM, UpstreamSearchRequest,
        },
    },
};

pub const SEARCH_EXPLAIN_LIMIT_MAX: usize = 20;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SearchSort {
    Relevance,
    Newest,
    PriceAsc,
    PriceDesc,
}

impl SearchSort {
    fn upstream(self) -> &'static str {
        match self {
            Self::Relevance => "RELEVANCE",
            Self::Newest => "PUBLISHED_DESC",
            Self::PriceAsc => "PRICE_ASC",
            Self::PriceDesc => "PRICE_DESC",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TradeType {
    Sell,
    GiveAway,
    Wanted,
}

impl TradeType {
    fn upstream(self) -> &'static str {
        match self {
            Self::Sell => "1",
            Self::GiveAway => "2",
            Self::Wanted => "3",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Seller {
    Private,
    Business,
}

impl Seller {
    fn upstream(self) -> &'static str {
        match self {
            Self::Private => "1",
            Self::Business => "3",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchRequest {
    pub query: String,
    pub category: Option<String>,
    pub location: Option<String>,
    pub area: Vec<String>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub radius_km: Option<f64>,
    pub price_from: Option<u64>,
    pub price_to: Option<u64>,
    pub trade_type: Option<TradeType>,
    pub condition: Vec<String>,
    pub seller: Option<Seller>,
    pub shipping: bool,
    pub facets: BTreeMap<String, Vec<String>>,
    pub sort: Option<SearchSort>,
    pub page: Option<usize>,
    pub limit: Option<usize>,
    pub explain: Option<usize>,
    pub include_facets: bool,
    pub facet_option_limit: Option<usize>,
    pub raw: bool,
}

#[derive(Debug, PartialEq)]
pub enum SearchResult {
    Search {
        collection: SearchCollection,
        next_actions: Vec<NextAction>,
    },
    Raw(Value),
}

pub struct ToriDiscovery<'a> {
    search_api: &'a dyn PublicSearchApi,
    item_api: Option<&'a dyn PublicItemApi>,
}

impl<'a> ToriDiscovery<'a> {
    pub fn new(
        search_api: &'a dyn PublicSearchApi,
        item_api: Option<&'a dyn PublicItemApi>,
    ) -> Self {
        Self {
            search_api,
            item_api,
        }
    }

    pub async fn execute(&self, input: SearchRequest) -> Result<SearchResult, AppError> {
        let prepared = prepare(input, self.search_api).await?;
        let input = prepared.input;
        let request = prepared.request;
        let search = PublicSearch::new(self.search_api);
        let (mut result, raw) = search
            .execute_with_area(&request, prepared.resolved_location, prepared.resolved_area)
            .await?;
        if input.raw {
            return Ok(SearchResult::Raw(raw));
        }
        if let Some(request_limit) = input.explain {
            let item_api = self.item_api.ok_or_else(|| {
                AppError::unexpected("search explanation requires the public item service")
            })?;
            explain_matches(&mut result, item_api, request_limit).await;
        }
        let mut actions = Vec::new();
        if let Some(next_page) = result.pagination.next_page {
            actions.push(NextAction {
                command: next_page_command(
                    &request,
                    next_page,
                    result.resolved_area.as_ref(),
                    input.explain,
                ),
            });
        } else if result.pagination.capped {
            let mut refinement = request.clone();
            refinement.include_filters = true;
            actions.push(NextAction {
                command: next_page_command(
                    &refinement,
                    1,
                    result.resolved_area.as_ref(),
                    input.explain,
                ),
            });
        }
        if let Some(option_count) = result
            .facets
            .iter()
            .filter(|facet| facet.truncated)
            .map(|facet| facet.option_count)
            .max()
        {
            let current_limit = request
                .facet_option_limit
                .unwrap_or(SEARCH_FACET_OPTION_LIMIT);
            if current_limit < SEARCH_FACET_OPTION_LIMIT_MAX {
                let mut broader = request.clone();
                broader.facet_option_limit = Some(
                    option_count
                        .min(SEARCH_FACET_OPTION_LIMIT_MAX)
                        .max(current_limit + 1),
                );
                actions.push(NextAction {
                    command: next_page_command(
                        &broader,
                        request.page,
                        result.resolved_area.as_ref(),
                        input.explain,
                    ),
                });
            } else {
                actions.push(NextAction {
                    command: format!(
                        "{} --raw",
                        next_page_command(
                            &request,
                            request.page,
                            result.resolved_area.as_ref(),
                            None,
                        )
                    ),
                });
            }
        }
        Ok(SearchResult::Search {
            collection: result,
            next_actions: actions,
        })
    }

    pub async fn saved_search_parameters(
        &self,
        input: SearchRequest,
    ) -> Result<BTreeMap<String, Vec<String>>, AppError> {
        prepare_saved_search(input, self.search_api).await
    }
}

struct PreparedSearch {
    input: SearchRequest,
    request: UpstreamSearchRequest,
    resolved_location: Option<crate::domain::search::SearchLocation>,
    resolved_area: Option<crate::domain::search::SearchArea>,
}

async fn prepare(
    input: SearchRequest,
    api: &dyn PublicSearchApi,
) -> Result<PreparedSearch, AppError> {
    validate(&input)?;
    let search = PublicSearch::new(api);
    let mut parameters = input.facets.clone();
    if let Some(category) = input.category.as_deref() {
        let name = category_parameter(category)?;
        insert_parameter(&mut parameters, name, vec![category.to_owned()])?;
    }
    insert_parameter(
        &mut parameters,
        "price_from",
        input
            .price_from
            .map(|value| value.to_string())
            .into_iter()
            .collect(),
    )?;
    insert_parameter(
        &mut parameters,
        "price_to",
        input
            .price_to
            .map(|value| value.to_string())
            .into_iter()
            .collect(),
    )?;
    insert_parameter(
        &mut parameters,
        "trade_type",
        input
            .trade_type
            .map(|value| value.upstream().to_owned())
            .into_iter()
            .collect(),
    )?;
    insert_parameter(&mut parameters, "condition", input.condition.clone())?;
    insert_parameter(
        &mut parameters,
        "dealer_segment",
        input
            .seller
            .map(|value| value.upstream().to_owned())
            .into_iter()
            .collect(),
    )?;
    if input.shipping {
        insert_parameter(&mut parameters, "shipping_exists", vec!["true".to_owned()])?;
    }
    insert_parameter(
        &mut parameters,
        "sort",
        input
            .sort
            .map(|value| value.upstream().to_owned())
            .into_iter()
            .collect(),
    )?;
    let resolved_location = if let Some(location) = input.location.as_deref() {
        let resolved = search.resolve_location(location).await?;
        insert_parameter(&mut parameters, "location", vec![resolved.id.clone()])?;
        Some(resolved)
    } else {
        None
    };
    let resolved_area = if input.area.is_empty() {
        None
    } else {
        let resolved = search.resolve_area(&input.area).await?;
        insert_parameter(
            &mut parameters,
            "location",
            resolved
                .locations
                .iter()
                .map(|location| location.id.clone())
                .collect(),
        )?;
        Some(resolved)
    };
    if let (Some(latitude), Some(longitude), Some(radius_km)) =
        (input.latitude, input.longitude, input.radius_km)
    {
        parameters.insert("lat".to_owned(), vec![latitude.to_string()]);
        parameters.insert("lon".to_owned(), vec![longitude.to_string()]);
        parameters.insert(
            "radius".to_owned(),
            vec![((radius_km * 1000.0).ceil() as u64).to_string()],
        );
    }
    let request = UpstreamSearchRequest {
        query: input.query.clone(),
        page: input.page.unwrap_or(1),
        limit: input.limit.unwrap_or(SEARCH_LIMIT_DEFAULT),
        include_filters: input.include_facets,
        facet_option_limit: input.facet_option_limit,
        parameters,
    };
    Ok(PreparedSearch {
        input,
        request,
        resolved_location,
        resolved_area,
    })
}

async fn prepare_saved_search(
    input: SearchRequest,
    api: &dyn PublicSearchApi,
) -> Result<BTreeMap<String, Vec<String>>, AppError> {
    let prepared = prepare(input, api).await?;
    let input = &prepared.input;
    if input.page.is_some()
        || input.limit.is_some()
        || input.explain.is_some()
        || input.include_facets
        || input.facet_option_limit.is_some()
        || input.raw
    {
        return Err(AppError::usage(
            "saved searches do not accept pagination, explanation, facet-output, or raw-output options",
        ));
    }
    let mut parameters = prepared.request.parameters;
    if !prepared.request.query.is_empty() {
        parameters.insert("q".to_owned(), vec![prepared.request.query]);
    }
    Ok(parameters)
}

async fn explain_matches(
    result: &mut SearchCollection,
    item_api: &dyn PublicItemApi,
    request_limit: usize,
) {
    let query_terms = normalized_terms(&result.query);
    let candidates = result
        .results
        .iter()
        .enumerate()
        .filter(|(_, listing)| {
            !query_terms.is_empty() && !contains_all_terms(&listing.title, &query_terms)
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let requested = candidates.len().min(request_limit);
    let mut hydrated = 0;
    let mut explained = 0;
    let mut failures = Vec::new();
    let items = PublicItems::new(item_api);

    for index in candidates.iter().take(request_limit).copied() {
        let listing_id = result.results[index].listing_id.clone();
        match items.show(&listing_id).await {
            Ok((detail, _)) => {
                hydrated += 1;
                if let Some(explanation) = description_explanation(
                    &result.results[index].title,
                    &detail.description,
                    &query_terms,
                ) {
                    result.results[index].match_explanation = Some(explanation);
                    explained += 1;
                }
            }
            Err(error) => failures.push(SearchExplainFailure {
                listing_id,
                code: error.code.to_owned(),
                upstream_transient: error.upstream_transient,
                safe_to_retry: error.safe_to_retry,
            }),
        }
    }

    result.explain = Some(SearchExplainSummary {
        request_limit,
        requested,
        hydrated,
        explained,
        truncated: candidates.len() > request_limit,
        failures,
    });
}

fn description_explanation(
    title: &str,
    description: &str,
    query_terms: &[String],
) -> Option<SearchMatchExplanation> {
    let title_terms = normalized_terms(title);
    let description_terms = normalized_terms(description);
    let matched_terms = query_terms
        .iter()
        .filter(|term| !title_terms.contains(*term) && description_terms.contains(*term))
        .cloned()
        .collect::<Vec<_>>();
    let all_terms_covered = query_terms
        .iter()
        .all(|term| title_terms.contains(term) || description_terms.contains(term));
    if matched_terms.is_empty() || !all_terms_covered {
        return None;
    }

    Some(SearchMatchExplanation {
        source_field: "description".to_owned(),
        evidence_origin: "public_item".to_owned(),
        match_method: "cli_derived_token_match".to_owned(),
        excerpt: excerpt(description, &matched_terms[0], 160),
        matched_terms,
    })
}

fn normalized_terms(value: &str) -> Vec<String> {
    let mut terms = Vec::new();
    for term in value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
    {
        if !terms.contains(&term) {
            terms.push(term);
        }
    }
    terms
}

fn contains_all_terms(value: &str, terms: &[String]) -> bool {
    let value_terms = normalized_terms(value);
    terms.iter().all(|term| value_terms.contains(term))
}

fn excerpt(value: &str, matched_term: &str, limit: usize) -> String {
    let sanitized = value
        .chars()
        .fold((String::new(), false), |(mut output, space), character| {
            if character.is_whitespace() || character.is_control() {
                if !space && !output.is_empty() {
                    output.push(' ');
                }
                (output, true)
            } else {
                output.push(character);
                (output, false)
            }
        })
        .0
        .trim()
        .to_owned();
    let characters = sanitized.chars().collect::<Vec<_>>();
    if characters.len() <= limit {
        return sanitized;
    }

    let lowercase = sanitized.to_lowercase();
    let match_index = lowercase
        .find(matched_term)
        .map(|byte_index| lowercase[..byte_index].chars().count())
        .unwrap_or(0);
    let start = match_index.saturating_sub(limit / 3);
    let end = (start + limit).min(characters.len());
    let start = end.saturating_sub(limit);
    let mut excerpt_characters = characters[start..end].to_vec();
    if start > 0 {
        excerpt_characters.splice(..3.min(excerpt_characters.len()), ['.', '.', '.']);
    }
    if end < characters.len() {
        let suffix_start = excerpt_characters.len().saturating_sub(3);
        excerpt_characters.splice(suffix_start.., ['.', '.', '.']);
    }
    excerpt_characters
        .into_iter()
        .collect::<String>()
        .trim()
        .to_owned()
}

fn next_page_command(
    request: &UpstreamSearchRequest,
    page: usize,
    resolved_area: Option<&crate::domain::search::SearchAreaContext>,
    explain: Option<usize>,
) -> String {
    let mut parts = vec!["flea tori search".to_owned()];
    if !request.query.is_empty() {
        parts.push(shell_quote(&request.query));
    }
    for (name, values) in &request.parameters {
        for value in values {
            match name.as_str() {
                "category" | "sub_category" | "product_category" => {
                    parts.push(format!("--category {}", shell_quote(value)));
                }
                "location" if resolved_area.is_none() => {
                    parts.push(format!("--location {}", shell_quote(value)));
                }
                "location" => {}
                "price_from" => parts.push(format!("--price-from {value}")),
                "price_to" => parts.push(format!("--price-to {value}")),
                "trade_type" => {
                    let semantic = match value.as_str() {
                        "1" => "sell",
                        "2" => "give-away",
                        "3" => "wanted",
                        _ => value,
                    };
                    parts.push(format!("--trade-type {}", shell_quote(semantic)));
                }
                "condition" => parts.push(format!("--condition {}", shell_quote(value))),
                "dealer_segment" => {
                    let semantic = if value == "1" { "private" } else { "business" };
                    parts.push(format!("--seller {semantic}"));
                }
                "shipping_exists" => parts.push("--shipping".to_owned()),
                "sort" => {
                    let semantic = match value.as_str() {
                        "RELEVANCE" => "relevance",
                        "PUBLISHED_DESC" => "newest",
                        "PRICE_ASC" => "price-asc",
                        "PRICE_DESC" => "price-desc",
                        _ => value,
                    };
                    parts.push(format!("--sort {}", shell_quote(semantic)));
                }
                "lat" => parts.push(format!("--latitude {value}")),
                "lon" => parts.push(format!("--longitude {value}")),
                "radius" => {
                    let meters = value.parse::<f64>().unwrap_or_default();
                    parts.push(format!("--radius-km {}", meters / 1000.0));
                }
                _ => parts.push(format!(
                    "--facet {}",
                    shell_quote(&format!("{name}={value}"))
                )),
            }
        }
    }
    if let Some(area) = resolved_area {
        let ids = area
            .locations
            .iter()
            .map(|location| location.id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        parts.push(format!("--area {}", shell_quote(&ids)));
    }
    if request.include_filters {
        parts.push("--include-facets".to_owned());
    }
    if let Some(limit) = request.facet_option_limit {
        parts.push(format!("--facet-option-limit {limit}"));
    }
    if let Some(limit) = explain {
        parts.push(format!("--explain {limit}"));
    }
    parts.push(format!("--page {page}"));
    parts.push(format!("--limit {}", request.limit));
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn validate(input: &SearchRequest) -> Result<(), AppError> {
    if input.query.chars().count() > 500 {
        return Err(AppError::usage(
            "search query must be at most 500 characters",
        ));
    }
    if input
        .location
        .as_deref()
        .is_some_and(|location| location.chars().count() > 256)
    {
        return Err(AppError::usage("--location must be at most 256 characters"));
    }
    if !input.area.is_empty() {
        if !(2..=SEARCH_AREA_LOCATION_MAX).contains(&input.area.len()) {
            return Err(AppError::usage(
                "--area must contain between 2 and 20 locations",
            ));
        }
        if input.area.iter().any(|location| {
            let length = location.chars().count();
            length == 0 || length > 256
        }) {
            return Err(AppError::usage(
                "--area locations must contain 1 through 256 characters",
            ));
        }
    }
    if input.condition.iter().any(|value| {
        let length = value.chars().count();
        length == 0 || length > 256
    }) {
        return Err(AppError::usage(
            "--condition values must contain 1 through 256 characters",
        ));
    }
    if let Some(page) = input.page
        && !(1..=SEARCH_PAGE_MAX).contains(&page)
    {
        return Err(AppError::usage("--page must be between 1 and 50"));
    }
    if let Some(limit) = input.limit
        && !(1..=SEARCH_LIMIT_MAX).contains(&limit)
    {
        return Err(AppError::usage("--limit must be between 1 and 300"));
    }
    if let Some(limit) = input.explain
        && !(1..=SEARCH_EXPLAIN_LIMIT_MAX).contains(&limit)
    {
        return Err(AppError::usage("--explain must be between 1 and 20"));
    }
    if let Some(limit) = input.facet_option_limit
        && !(1..=SEARCH_FACET_OPTION_LIMIT_MAX).contains(&limit)
    {
        return Err(AppError::usage(
            "--facet-option-limit must be between 1 and 5000",
        ));
    }
    if input.facet_option_limit.is_some() && !input.include_facets {
        return Err(AppError::usage(
            "--facet-option-limit requires --include-facets",
        ));
    }
    if input.explain.is_some() && input.query.trim().is_empty() {
        return Err(AppError::usage(
            "--explain requires a non-empty search query",
        ));
    }
    if input.explain.is_some() && input.raw {
        return Err(AppError::usage("--explain cannot be combined with --raw"));
    }
    if let (Some(from), Some(to)) = (input.price_from, input.price_to)
        && from > to
    {
        return Err(AppError::usage("--price-from must not exceed --price-to"));
    }
    for (name, value, minimum, maximum) in [
        ("--latitude", input.latitude, -90.0, 90.0),
        ("--longitude", input.longitude, -180.0, 180.0),
    ] {
        if value.is_some_and(|value| !value.is_finite() || value < minimum || value > maximum) {
            return Err(AppError::usage(format!(
                "{name} is outside its valid range"
            )));
        }
    }
    if input
        .radius_km
        .is_some_and(|radius| !radius.is_finite() || radius <= 0.0 || radius > SEARCH_RADIUS_MAX_KM)
    {
        return Err(AppError::usage(
            "--radius-km must be positive and at most 1000",
        ));
    }
    let coordinate_count = [
        input.latitude.is_some(),
        input.longitude.is_some(),
        input.radius_km.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if !input.area.is_empty() && input.location.is_some() {
        return Err(AppError::usage("--area cannot be combined with --location"));
    }
    if coordinate_count > 0 && (!input.area.is_empty() || input.location.is_some()) {
        return Err(AppError::usage(
            "--area and --location cannot be combined with coordinate radius arguments",
        ));
    }
    if coordinate_count != 0 && coordinate_count != 3 {
        return Err(AppError::usage(
            "--latitude, --longitude, and --radius-km must be provided together",
        ));
    }
    for (name, values) in &input.facets {
        validate_facet_name(name)?;
        if values.is_empty()
            || values.iter().any(|value| {
                let length = value.chars().count();
                length == 0 || length > 256
            })
        {
            return Err(AppError::usage(
                "facet values must contain 1 through 256 characters",
            ));
        }
    }
    Ok(())
}

fn category_parameter(category: &str) -> Result<&'static str, AppError> {
    if category.len() > 64 {
        return Err(AppError::usage("--category must be at most 64 characters"));
    }
    let parts: Vec<&str> = category.split('.').collect();
    if parts
        .iter()
        .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(AppError::usage(
            "--category must be a numeric Tori taxonomy value",
        ));
    }
    match (parts.first().copied(), parts.len()) {
        (Some("0"), 2) => Ok("category"),
        (Some("1"), 3) => Ok("sub_category"),
        (Some("2"), 4) => Ok("product_category"),
        _ => Err(AppError::usage(
            "--category must use a supported Tori taxonomy depth",
        )),
    }
}

fn parse_facets(values: &[String]) -> Result<BTreeMap<String, Vec<String>>, AppError> {
    let mut facets = BTreeMap::new();
    for value in values {
        let (name, value) = value
            .split_once('=')
            .ok_or_else(|| AppError::usage("--facet must use NAME=VALUE"))?;
        validate_facet_name(name)?;
        let length = value.chars().count();
        if length == 0 || length > 256 {
            return Err(AppError::usage(
                "facet values must contain 1 through 256 characters",
            ));
        }
        facets
            .entry(name.to_owned())
            .or_insert_with(Vec::new)
            .push(value.to_owned());
    }
    Ok(facets)
}

fn validate_facet_name(name: &str) -> Result<(), AppError> {
    if name.is_empty()
        || name.len() > 64
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    {
        return Err(AppError::usage(
            "facet names must be lowercase machine names",
        ));
    }
    if matches!(
        name,
        "client"
            | "page"
            | "rows"
            | "include_results"
            | "include_filters"
            | "q"
            | "lat"
            | "lon"
            | "radius"
            | "category"
            | "sub_category"
            | "product_category"
            | "location"
            | "price_from"
            | "price_to"
            | "trade_type"
            | "condition"
            | "dealer_segment"
            | "shipping_exists"
            | "sort"
    ) {
        return Err(AppError::usage(
            "facet name is reserved; use its dedicated flag",
        ));
    }
    Ok(())
}

fn insert_parameter(
    parameters: &mut BTreeMap<String, Vec<String>>,
    name: &str,
    values: Vec<String>,
) -> Result<(), AppError> {
    if values.is_empty() {
        return Ok(());
    }
    if parameters.contains_key(name) {
        return Err(AppError::usage(format!(
            "search parameter `{name}` was provided more than once"
        )));
    }
    parameters.insert(name.to_owned(), values);
    Ok(())
}
