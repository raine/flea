use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use crate::error::AppError;

#[derive(Debug, Args)]
pub struct DraftArgs {
    #[command(subcommand)]
    pub command: DraftCommand,
}

#[derive(Clone, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum TradeType {
    Sell,
    GiveAway,
    Wanted,
}

#[derive(Debug, Args, Serialize)]
pub struct ListingInputArgs {
    #[arg(long)]
    pub category: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long, conflicts_with = "description_file")]
    pub description: Option<String>,
    #[arg(long, value_name = "PATH", conflicts_with = "description")]
    pub description_file: Option<PathBuf>,
    #[arg(long)]
    pub price: Option<String>,
    #[arg(long, value_enum)]
    pub trade_type: Option<TradeType>,
    #[arg(long)]
    pub postal_code: Option<String>,
    #[arg(long, value_name = "VALUE")]
    pub delivery: Vec<String>,
    #[arg(long, value_name = "PATH")]
    pub image: Vec<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub input: Option<PathBuf>,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum DraftCommand {
    Create {
        #[arg(long, conflicts_with_all = ["category", "title", "description", "description_file", "price", "trade_type", "postal_code", "delivery", "image", "input"])]
        from_listing: Option<String>,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    Show {
        draft_id: String,
    },
    Update {
        draft_id: String,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    Image(ImageArgs),
    Publish {
        draft_id: String,
    },
    Delete {
        draft_id: String,
    },
}

#[derive(Debug, Args, Serialize)]
pub struct ImageArgs {
    #[command(subcommand)]
    pub command: ImageCommand,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ImageCommand {
    Add {
        draft_id: String,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    Remove {
        draft_id: String,
        #[arg(required = true)]
        image_ids: Vec<String>,
    },
}

pub fn dispatch(command: DraftArgs) -> Result<Value, AppError> {
    let details = serde_json::to_value(command.command)
        .map_err(|error| AppError::output(error.to_string()))?;
    Err(AppError::protocol_unavailable("draft", details))
}
