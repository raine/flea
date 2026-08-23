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
    cli::outcome::{CommandData, CommandOutcome},
    error::AppError,
    marketplace::tori::{
        discovery::{
            SearchRequest, SearchResult, SearchSort as ToriSearchSort, Seller as ToriSeller,
            ToriDiscovery, TradeType as ToriTradeType,
        },
        item::PublicItemApi,
        search::PublicSearchApi,
    },
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum SearchSort {
    Relevance,
    Newest,
    PriceAsc,
    PriceDesc,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum TradeType {
    Sell,
    GiveAway,
    Wanted,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum Seller {
    Private,
    Business,
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

pub async fn dispatch(
    args: SearchArgs,
    api: &dyn PublicSearchApi,
    item_api: Option<&dyn PublicItemApi>,
) -> Result<CommandOutcome, AppError> {
    let request = collect_input(args)?.into();
    match ToriDiscovery::new(api, item_api).execute(request).await? {
        SearchResult::Search {
            collection,
            next_actions,
        } => {
            Ok(CommandOutcome::new(CommandData::Search(*collection))
                .with_next_actions(next_actions))
        }
        SearchResult::Raw(raw) => Ok(CommandOutcome::new(CommandData::Raw(raw))),
    }
}

#[cfg(test)]
pub async fn saved_search_parameters(
    args: SearchArgs,
    api: &dyn PublicSearchApi,
) -> Result<BTreeMap<String, Vec<String>>, AppError> {
    ToriDiscovery::new(api, None)
        .saved_search_parameters(request_from_args(args)?)
        .await
}

pub(crate) fn request_from_args(args: SearchArgs) -> Result<SearchRequest, AppError> {
    Ok(collect_input(args)?.into())
}

impl From<SearchInput> for SearchRequest {
    fn from(input: SearchInput) -> Self {
        Self {
            query: input.query,
            category: input.category,
            location: input.location,
            area: input.area,
            latitude: input.latitude,
            longitude: input.longitude,
            radius_km: input.radius_km,
            price_from: input.price_from,
            price_to: input.price_to,
            trade_type: input.trade_type.map(Into::into),
            condition: input.condition,
            seller: input.seller.map(Into::into),
            shipping: input.shipping,
            facets: input.facets,
            sort: input.sort.map(Into::into),
            page: input.page,
            limit: input.limit,
            explain: input.explain,
            include_facets: input.include_facets,
            facet_option_limit: input.facet_option_limit,
            raw: input.raw,
        }
    }
}

impl From<SearchSort> for ToriSearchSort {
    fn from(value: SearchSort) -> Self {
        match value {
            SearchSort::Relevance => Self::Relevance,
            SearchSort::Newest => Self::Newest,
            SearchSort::PriceAsc => Self::PriceAsc,
            SearchSort::PriceDesc => Self::PriceDesc,
        }
    }
}

impl From<TradeType> for ToriTradeType {
    fn from(value: TradeType) -> Self {
        match value {
            TradeType::Sell => Self::Sell,
            TradeType::GiveAway => Self::GiveAway,
            TradeType::Wanted => Self::Wanted,
        }
    }
}

impl From<Seller> for ToriSeller {
    fn from(value: Seller) -> Self {
        match value {
            Seller::Private => Self::Private,
            Seller::Business => Self::Business,
        }
    }
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

fn parse_facets(values: &[String]) -> Result<BTreeMap<String, Vec<String>>, AppError> {
    let mut facets = BTreeMap::new();
    for value in values {
        let (name, value) = value
            .split_once('=')
            .ok_or_else(|| AppError::usage("--facet must use NAME=VALUE"))?;
        facets
            .entry(name.to_owned())
            .or_insert_with(Vec::new)
            .push(value.to_owned());
    }
    Ok(facets)
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
