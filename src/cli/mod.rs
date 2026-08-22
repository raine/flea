pub mod auth;
pub mod category;
pub mod draft;
pub mod listing;

use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::{error::AppError, output::OutputFormat};

#[derive(Debug, Parser)]
#[command(name = "tori", about = "Agent CLI for Tori.fi listing workflows")]
#[command(version, propagate_version = true)]
pub struct Cli {
    #[arg(long, global = true, value_enum, default_value_t)]
    pub format: OutputFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Auth(auth::AuthArgs),
    Category(category::CategoryArgs),
    Draft(draft::DraftArgs),
    Listing(listing::ListingArgs),
}

pub fn dispatch(command: Command) -> Result<Value, AppError> {
    match command {
        Command::Auth(args) => auth::dispatch(args),
        Command::Category(args) => category::dispatch(args),
        Command::Draft(args) => draft::dispatch(args),
        Command::Listing(args) => listing::dispatch(args),
    }
}
