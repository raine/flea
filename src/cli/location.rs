use clap::{Args, Subcommand};
use serde_json::Value;

use crate::{
    api::search::{PublicSearch, PublicSearchApi},
    error::AppError,
};

#[derive(Debug, Args)]
pub struct LocationArgs {
    #[command(subcommand)]
    pub command: LocationCommand,
}

#[derive(Debug, Subcommand)]
pub enum LocationCommand {
    /// Discover Tori location identifiers by a case-insensitive name fragment.
    Search {
        /// Name fragment, such as Helsinki. Omit to list the first bounded page.
        query: Option<String>,
    },
}

pub fn dispatch_with_api(args: LocationArgs, api: &dyn PublicSearchApi) -> Result<Value, AppError> {
    let search = PublicSearch::new(api);
    let result = match args.command {
        LocationCommand::Search { query } => search.locations(query.as_deref().unwrap_or(""))?,
    };
    serde_json::to_value(result)
        .map_err(|error| AppError::output("failed to serialize location output").with_source(error))
}
