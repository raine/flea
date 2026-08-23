use clap::{Args, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    error::AppError,
    marketplace::vinted::{
        auth::VintedCredentialRecord,
        search::{SEARCH_LIMIT_DEFAULT, SearchRequest, SearchSort, VintedSearch},
    },
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum VintedSearchSort {
    Relevance,
    Newest,
    PriceAsc,
    PriceDesc,
}

impl From<VintedSearchSort> for SearchSort {
    fn from(value: VintedSearchSort) -> Self {
        match value {
            VintedSearchSort::Relevance => Self::Relevance,
            VintedSearchSort::Newest => Self::Newest,
            VintedSearchSort::PriceAsc => Self::PriceAsc,
            VintedSearchSort::PriceDesc => Self::PriceDesc,
        }
    }
}

#[derive(Debug, Args)]
pub struct VintedSearchArgs {
    /// Free-text marketplace query. May be omitted to browse listings.
    pub query: Option<String>,

    /// Minimum listing price in euros.
    #[arg(long)]
    pub price_from: Option<u64>,

    /// Maximum listing price in euros.
    #[arg(long)]
    pub price_to: Option<u64>,

    /// Result ordering.
    #[arg(long, value_enum)]
    pub sort: Option<VintedSearchSort>,

    /// One-indexed result page.
    #[arg(long)]
    pub page: Option<usize>,

    /// Results per page, from 1 through 96.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Return the upstream JSON body inside the standard output envelope.
    #[arg(long)]
    pub raw: bool,
}

pub async fn dispatch(
    args: VintedSearchArgs,
    credentials: &VintedCredentialRecord,
) -> Result<Value, AppError> {
    let request = SearchRequest {
        query: args.query.unwrap_or_default(),
        price_from: args.price_from,
        price_to: args.price_to,
        sort: args.sort.unwrap_or(VintedSearchSort::Relevance).into(),
        page: args.page.unwrap_or(1),
        limit: args.limit.unwrap_or(SEARCH_LIMIT_DEFAULT),
    };
    let (normalized, raw) = VintedSearch::new().execute(credentials, &request).await?;
    if args.raw {
        return Ok(raw);
    }
    serde_json::to_value(normalized).map_err(|error| {
        AppError::output("failed to serialize Vinted search output").with_source(error)
    })
}
