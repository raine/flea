use crate::{
    domain::search::LocationCollection,
    error::AppError,
    marketplace::tori::search::{PublicSearch, PublicSearchApi},
};
use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct LocationArgs {
    #[command(subcommand)]
    pub command: LocationCommand,
}

#[derive(Debug, Subcommand)]
pub enum LocationCommand {
    #[command(
        about = "Discover Tori location identifiers by name",
        long_about = "Discover Tori location identifiers by a case-insensitive name fragment and return bounded normalized matches."
    )]
    Search {
        /// Name fragment, such as Helsinki. Omit to list the first bounded page.
        query: Option<String>,
    },
}

impl LocationCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Search { .. } => "location search",
        }
    }
}

pub async fn dispatch(
    args: LocationArgs,
    api: &dyn PublicSearchApi,
) -> Result<LocationCollection, AppError> {
    let search = PublicSearch::new(api);
    let result = match args.command {
        LocationCommand::Search { query } => {
            search.locations(query.as_deref().unwrap_or("")).await?
        }
    };
    Ok(result)
}
