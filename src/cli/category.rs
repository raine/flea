use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use crate::{
    api::listings::{Listings, ListingsApi},
    error::AppError,
};

#[derive(Debug, Args)]
pub struct CategoryArgs {
    #[command(subcommand)]
    pub command: CategoryCommand,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum CategoryCommand {
    Search {
        query: String,
    },
    List {
        #[arg(long)]
        parent: Option<String>,
    },
}

pub fn dispatch_with_api(command: CategoryArgs, api: &dyn ListingsApi) -> Result<Value, AppError> {
    let listings = Listings::new(api);
    let result = match command.command {
        CategoryCommand::Search { query } => listings.search_categories(&query)?,
        CategoryCommand::List { parent } => listings.categories(parent.as_deref())?,
    };
    serde_json::to_value(result).map_err(|error| AppError::output(error.to_string()))
}
