use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use crate::{cli::draft::ListingInputArgs, error::AppError};

#[derive(Debug, Args)]
pub struct ListingArgs {
    #[command(subcommand)]
    pub command: ListingCommand,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ListingCommand {
    List,
    Show {
        listing_id: String,
    },
    Update {
        listing_id: String,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    Dispose {
        listing_id: String,
    },
    Delete {
        listing_id: String,
    },
}

pub fn dispatch(command: ListingArgs) -> Result<Value, AppError> {
    let details = serde_json::to_value(command.command)
        .map_err(|error| AppError::output(error.to_string()))?;
    Err(AppError::protocol_unavailable("listing", details))
}
