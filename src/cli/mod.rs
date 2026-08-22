pub mod auth;
mod auth_callback;
pub mod category;
pub mod draft;
mod draft_input;
pub mod item;
pub mod listing;
pub mod location;
pub mod runtime;
pub mod search;
pub mod skill;

use clap::{Parser, Subcommand};
use serde_json::Value;

use crate::{error::AppError, output::OutputFormat};

#[derive(Debug, Parser)]
#[command(
    name = "flea",
    about = "Manage Tori.fi listing workflows with Flea",
    long_about = "Flea manages Tori.fi authentication, categories, drafts, and published listings through deterministic agent workflows."
)]
#[command(version, propagate_version = true)]
pub struct Cli {
    /// Select the structured output format.
    #[arg(long, global = true, value_enum, default_value_t)]
    pub format: OutputFormat,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    #[command(
        about = "Manage browser authentication",
        long_about = "Start, complete, inspect, or clear the browser OAuth authentication session."
    )]
    Auth(auth::AuthArgs),
    #[command(
        about = "Discover Tori category machine values",
        long_about = "Search or browse Tori categories and return machine values suitable for listing input."
    )]
    Category(category::CategoryArgs),
    #[command(
        about = "Preview input and manage remote drafts",
        long_about = "Preview draft input locally, or create, inspect, update, publish, delete, and manage images for remote drafts."
    )]
    Draft(Box<draft::DraftArgs>),
    #[command(
        about = "Inspect public marketplace listings",
        long_about = "Inspect normalized public marketplace listing details by search result ID without account authentication."
    )]
    Item(item::ItemArgs),
    #[command(
        about = "Manage published listings",
        long_about = "List, inspect, update, mark sold, or permanently delete published listings."
    )]
    Listing(listing::ListingArgs),
    #[command(
        about = "Search public marketplace listings",
        long_about = "Search public Tori marketplace listings with normalized filters, facets, locations, sorting, and bounded pagination."
    )]
    Search(Box<search::SearchArgs>),
    #[command(
        about = "Discover public marketplace location identifiers",
        long_about = "Discover deterministic Tori location identifiers for subsequent public marketplace searches."
    )]
    Location(location::LocationArgs),
    #[command(
        about = "Print or install the coding-agent skill",
        long_about = "Print the bundled flea coding-agent skill or install it for supported coding agents."
    )]
    Skill(skill::SkillArgs),
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
