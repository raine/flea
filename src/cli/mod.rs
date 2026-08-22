pub mod auth;
pub mod category;
pub mod draft;
pub mod listing;
pub mod runtime;

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
    Draft(Box<draft::DraftArgs>),
    Listing(listing::ListingArgs),
}

pub trait CommandRuntime {
    fn execute(&self, command: Command) -> Result<Value, AppError>;
}

pub fn dispatch(command: Command) -> Result<Value, AppError> {
    runtime::ProductionRuntime.execute(command)
}

pub fn dispatch_with_runtime(
    command: Command,
    runtime: &dyn CommandRuntime,
) -> Result<Value, AppError> {
    runtime.execute(command)
}
