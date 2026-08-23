use std::{
    collections::BTreeMap,
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    api::{
        item::{PublicItemApi, PublicItems},
        search::{
            PublicSearch, PublicSearchApi, SEARCH_AREA_LOCATION_MAX, SEARCH_FACET_OPTION_LIMIT,
            SEARCH_FACET_OPTION_LIMIT_MAX, SEARCH_LIMIT_DEFAULT, SEARCH_LIMIT_MAX, SEARCH_PAGE_MAX,
            SEARCH_RADIUS_MAX_KM, UpstreamSearchRequest,
        },
    },
    domain::search::{
        SearchCollection, SearchExplainFailure, SearchExplainSummary, SearchMatchExplanation,
    },
    error::AppError,
};

pub const SEARCH_EXPLAIN_LIMIT_MAX: usize = 20;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
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

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
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

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Free-text marketplace query. May be omitted to browse filtered listings.
    pub query: Option<String>,

    /// Canonical taxonomy_value from `flea tori category search`, such as 2.93.3215.8368. At most 64 characters.
    #[arg(long)]
    pub category: Option<String>,

    /// One exact Tori location ID or unambiguous exact place name.
    #[arg(long)]
    pub location: Option<String>,

    /// Explicit area as comma-separated Tori location IDs or unambiguous names.
    #[arg(
        long,
        value_name = "PLACE,PLACE,...",
        value_delimiter = ',',
        conflicts_with_all = ["location", "latitude", "longitude", "radius_km"]
    )]
    pub area: Vec<String>,

    /// Search center latitude in decimal degrees. Requires longitude and radius.
    #[arg(long, allow_hyphen_values = true)]
    pub latitude: Option<f64>,

    /// Search center longitude in decimal degrees. Requires latitude and radius.
    #[arg(long, allow_hyphen_values = true)]
    pub longitude: Option<f64>,

    /// Positive search radius in kilometers, at most 1000.
    #[arg(long, visible_alias = "distance-km")]
    pub radius_km: Option<f64>,

    /// Minimum listing price in euros.
    #[arg(long)]
    pub price_from: Option<u64>,

    /// Maximum listing price in euros.
    #[arg(long)]
    pub price_to: Option<u64>,

    /// Listing trade type.
    #[arg(long, value_enum)]
    pub trade_type: Option<TradeType>,

    /// Listing condition machine value, 1 through 256 characters. May be repeated.
    #[arg(long)]
    pub condition: Vec<String>,

    /// Seller type.
    #[arg(long, value_enum)]
    pub seller: Option<Seller>,

    /// Require ToriDiili shipping.
    #[arg(long)]
    pub shipping: bool,

    /// Dynamic Tori facet as NAME=VALUE. May be repeated.
    #[arg(long = "facet", value_name = "NAME=VALUE")]
    pub facets: Vec<String>,

    /// Result ordering.
    #[arg(long, value_enum)]
    pub sort: Option<SearchSort>,

    /// One-indexed page, bounded by Tori to 1 through 50.
    #[arg(long)]
    pub page: Option<usize>,

    /// Results per page, from 1 through 300.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Explain opaque matches with at most LIMIT public item detail requests.
    #[arg(long, value_name = "LIMIT")]
    pub explain: Option<usize>,

    /// Include normalized available facet and option metadata.
    #[arg(long)]
    pub include_facets: bool,

    /// Maximum options returned per facet. Requires --include-facets.
    #[arg(long, value_name = "LIMIT", requires = "include_facets")]
    pub facet_option_limit: Option<usize>,

    /// JSON object containing search arguments. Duplicate JSON and flag fields fail.
    #[arg(long, value_name = "PATH")]
    pub input: Option<PathBuf>,

    /// Return the upstream JSON body inside the standard output envelope.
    #[arg(long)]
    pub raw: bool,
}

#[derive(Debug, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct SearchInput {
    query: String,
    category: Option<String>,
    location: Option<String>,
    area: Vec<String>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    radius_km: Option<f64>,
    price_from: Option<u64>,
    price_to: Option<u64>,
    trade_type: Option<TradeType>,
    condition: Vec<String>,
    seller: Option<Seller>,
    shipping: bool,
    facets: BTreeMap<String, Vec<String>>,
    sort: Option<SearchSort>,
    page: Option<usize>,
    limit: Option<usize>,
    explain: Option<usize>,
    include_facets: bool,
    facet_option_limit: Option<usize>,
    raw: bool,
}

pub async fn dispatch_with_api(
    args: SearchArgs,
    api: &dyn PublicSearchApi,
) -> Result<Value, AppError> {
    dispatch_with_apis(args, api, None).await
}

pub async fn dispatch_with_apis(
    args: SearchArgs,
    api: &dyn PublicSearchApi,
    item_api: Option<&dyn PublicItemApi>,
) -> Result<Value, AppError> {
    let prepared = prepare(args, api)?;
    let input = prepared.input;
    let request = prepared.request;
    let search = PublicSearch::new(api);
    let (mut result, raw) =
        search.execute_with_area(&request, prepared.resolved_location, prepared.resolved_area)?;
    if input.raw {
        return Ok(raw);
    }
    if let Some(request_limit) = input.explain {
        let item_api = item_api.ok_or_else(|| {
            AppError::unexpected("search explanation requires the public item service")
        })?;
        explain_matches(&mut result, item_api, request_limit).await;
    }
    let mut value = serde_json::to_value(&result).map_err(|error| {
        AppError::output("failed to serialize search output").with_source(error)
    })?;
    let mut actions = Vec::new();
    if let Some(next_page) = result.pagination.next_page {
        actions.push(json!({
            "command": next_page_command(
                &request,
                next_page,
                result.resolved_area.as_ref(),
                input.explain,
            )
        }));
    } else if result.pagination.capped {
        let mut refinement = request.clone();
        refinement.include_filters = true;
        actions.push(json!({
            "command": next_page_command(
                &refinement,
                1,
                result.resolved_area.as_ref(),
                input.explain,
            )
        }));
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
            actions.push(json!({
                "command": next_page_command(
                    &broader,
                    request.page,
                    result.resolved_area.as_ref(),
                    input.explain,
                ),
                "reason": "facet_options_truncated"
            }));
        } else {
            actions.push(json!({
                "command": format!(
                    "{} --raw",
                    next_page_command(
                        &request,
                        request.page,
                        result.resolved_area.as_ref(),
                        None,
                    )
                ),
                "reason": "facet_options_truncated"
            }));
        }
    }
    if !actions.is_empty() {
        value
            .as_object_mut()
            .expect("search output is an object")
            .insert("_next_actions".to_owned(), Value::Array(actions));
    }
    Ok(value)
}

struct PreparedSearch {
    input: SearchInput,
    request: UpstreamSearchRequest,
    resolved_location: Option<crate::domain::search::SearchLocation>,
    resolved_area: Option<crate::domain::search::SearchArea>,
}

fn prepare(args: SearchArgs, api: &dyn PublicSearchApi) -> Result<PreparedSearch, AppError> {
    let input = collect_input(args)?;
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
        let resolved = search.resolve_location(location)?;
        insert_parameter(&mut parameters, "location", vec![resolved.id.clone()])?;
        Some(resolved)
    } else {
        None
    };
    let resolved_area = if input.area.is_empty() {
        None
    } else {
        let resolved = search.resolve_area(&input.area)?;
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

pub fn saved_search_parameters(
    args: SearchArgs,
    api: &dyn PublicSearchApi,
) -> Result<BTreeMap<String, Vec<String>>, AppError> {
    let prepared = prepare(args, api)?;
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

fn collect_input(args: SearchArgs) -> Result<SearchInput, AppError> {
    let mut object = match args.input.as_deref() {
        Some(path) => read_json_object(path)?,
        None => Map::new(),
    };
    insert_flag(&mut object, "query", args.query.map(Value::String))?;
    insert_flag(&mut object, "category", args.category.map(Value::String))?;
    insert_flag(&mut object, "location", args.location.map(Value::String))?;
    if !args.area.is_empty() {
        insert_flag(&mut object, "area", Some(json!(args.area)))?;
    }
    insert_flag(&mut object, "latitude", args.latitude.map(value_from_f64))?;
    insert_flag(&mut object, "longitude", args.longitude.map(value_from_f64))?;
    insert_flag(&mut object, "radius_km", args.radius_km.map(value_from_f64))?;
    insert_flag(&mut object, "price_from", args.price_from.map(Value::from))?;
    insert_flag(&mut object, "price_to", args.price_to.map(Value::from))?;
    insert_flag(&mut object, "trade_type", args.trade_type.map(enum_value))?;
    insert_flag(&mut object, "seller", args.seller.map(enum_value))?;
    insert_flag(&mut object, "sort", args.sort.map(enum_value))?;
    insert_flag(&mut object, "page", args.page.map(Value::from))?;
    insert_flag(&mut object, "limit", args.limit.map(Value::from))?;
    insert_flag(&mut object, "explain", args.explain.map(Value::from))?;
    insert_flag(
        &mut object,
        "facet_option_limit",
        args.facet_option_limit.map(Value::from),
    )?;
    if !args.condition.is_empty() {
        insert_flag(&mut object, "condition", Some(json!(args.condition)))?;
    }
    if !args.facets.is_empty() {
        let facets = parse_facets(&args.facets)?;
        insert_flag(
            &mut object,
            "facets",
            Some(serde_json::to_value(facets).expect("facet map serializes")),
        )?;
    }
    for (name, enabled) in [
        ("shipping", args.shipping),
        ("include_facets", args.include_facets),
        ("raw", args.raw),
    ] {
        if enabled {
            insert_flag(&mut object, name, Some(Value::Bool(true)))?;
        }
    }
    serde_json::from_value(Value::Object(object))
        .map_err(|error| AppError::usage(format!("invalid search input: {error}")))
}

fn validate(input: &SearchInput) -> Result<(), AppError> {
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

fn insert_flag(
    object: &mut Map<String, Value>,
    name: &str,
    value: Option<Value>,
) -> Result<(), AppError> {
    let Some(value) = value else { return Ok(()) };
    if object.contains_key(name) {
        return Err(AppError::usage(format!(
            "search field `{name}` is present in both --input and command flags"
        )));
    }
    object.insert(name.to_owned(), value);
    Ok(())
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>, AppError> {
    const MAX_INPUT_BYTES: u64 = 1024 * 1024;
    let mut source = String::new();
    if path == Path::new("-") {
        std::io::stdin()
            .take(MAX_INPUT_BYTES + 1)
            .read_to_string(&mut source)
            .map_err(|error| AppError::usage(format!("failed to read JSON from stdin: {error}")))?;
    } else {
        let file = File::open(path).map_err(|error| {
            AppError::usage(format!("failed to read {}: {error}", path.display()))
        })?;
        file.take(MAX_INPUT_BYTES + 1)
            .read_to_string(&mut source)
            .map_err(|error| {
                AppError::usage(format!("failed to read {}: {error}", path.display()))
            })?;
    }
    if source.len() as u64 > MAX_INPUT_BYTES {
        return Err(AppError::usage("search JSON input exceeds 1 MiB"));
    }
    match serde_json::from_str(&source) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(AppError::usage("--input must contain a JSON object")),
        Err(error) => Err(AppError::usage(format!("invalid JSON input: {error}"))),
    }
}

fn enum_value<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).expect("search enum serializes")
}

fn value_from_f64(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map_or_else(|| Value::String(value.to_string()), Value::Number)
}
