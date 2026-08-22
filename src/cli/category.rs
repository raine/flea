use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use crate::error::AppError;

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

pub fn dispatch(command: CategoryArgs) -> Result<Value, AppError> {
    let details = serde_json::to_value(command.command)
        .map_err(|error| AppError::output(error.to_string()))?;
    Err(AppError::protocol_unavailable("category", details))
}
