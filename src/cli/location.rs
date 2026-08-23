use clap::{Args, Subcommand};
use serde_json::Value;

use crate::{
    error::AppError,
    marketplace::tori::search::{PublicSearch, PublicSearchApi},
};

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

pub async fn dispatch_with_api(
    args: LocationArgs,
    api: &dyn PublicSearchApi,
) -> Result<Value, AppError> {
    let search = PublicSearch::new(api);
    let result = match args.command {
        LocationCommand::Search { query } => {
            search.locations(query.as_deref().unwrap_or("")).await?
        }
    };
    serde_json::to_value(result)
        .map_err(|error| AppError::output("failed to serialize location output").with_source(error))
}
