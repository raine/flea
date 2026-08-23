pub mod auth;
mod auth_callback;
pub mod category;
pub mod draft;
mod draft_input;
pub mod favorite;
pub mod item;
pub mod listing;
pub mod location;
pub mod outcome;
pub mod runtime;
pub mod saved_search;
pub mod search;
pub mod skill;
pub mod vinted_search;

use std::{ffi::OsString, future::Future, pin::Pin};

use clap::{Args, Parser, Subcommand};

use crate::{
    error::AppError,
    marketplace::{MarketplaceContext, PortalId},
    output::OutputFormat,
};

#[derive(Debug, Parser)]
#[command(
    name = "flea",
    about = "Manage marketplace workflows with Flea",
    long_about = "Flea manages marketplace authentication, discovery, drafts, and listings through explicit Tori and Vinted command trees."
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
        about = "Show the marketplace capability matrix",
        long_about = "Show the offline capability matrix for every configured marketplace and portal."
    )]
    Capabilities,
    #[command(
        about = "List configured marketplaces and portals",
        long_about = "List marketplace identifiers and the portal bindings available for each marketplace."
    )]
    Marketplaces,
    #[command(
        about = "Manage Tori.fi",
        long_about = "Authenticate with Tori.fi and manage its discovery, draft, listing, favorite, and saved-search workflows."
    )]
    Tori(ToriArgs),
    #[command(
        about = "Manage Vinted",
        long_about = "Authenticate with Vinted through an explicit portal binding and inspect its available capabilities."
    )]
    Vinted(VintedArgs),
    #[command(
        about = "Print or install the coding-agent skill",
        long_about = "Print the bundled flea coding-agent skill or install it for supported coding agents."
    )]
    Skill(skill::SkillArgs),
    #[command(external_subcommand)]
    Unsupported(Vec<OsString>),
}

impl Command {
    pub fn context(&self) -> Option<MarketplaceContext> {
        match self {
            Self::Tori(_) => Some(MarketplaceContext::TORI_FI),
            Self::Vinted(_) => Some(MarketplaceContext::VINTED_FI),
            Self::Capabilities | Self::Marketplaces | Self::Skill(_) | Self::Unsupported(_) => None,
        }
    }
}

#[derive(Debug, Args)]
pub struct ToriArgs {
    #[command(subcommand)]
    pub command: ToriCommand,
}

#[derive(Debug, Subcommand)]
pub enum ToriCommand {
    #[command(
        about = "Manage browser authentication",
        long_about = "Start, complete, inspect, or clear the Tori browser OAuth authentication session."
    )]
    Auth(auth::AuthArgs),
    #[command(
        about = "Show Tori capabilities",
        long_about = "Show Tori operations, authentication requirements, and implementation maturity without making a network request."
    )]
    Capabilities,
    #[command(
        about = "Discover Tori categories (authentication required)",
        long_about = "Search or browse Tori categories and return machine values suitable for listing input. Authentication is required. Run `flea tori auth login` first."
    )]
    Category(category::CategoryArgs),
    #[command(
        about = "Preview input and manage remote drafts",
        long_about = "Preview draft input locally, or create, inspect, update, publish, delete, and manage images for remote drafts."
    )]
    Draft(Box<draft::DraftArgs>),
    #[command(
        about = "Manage saved Tori listings",
        long_about = "List favorites folders and add or remove Tori listings for the authenticated account."
    )]
    Favorite(favorite::FavoriteArgs),
    #[command(
        about = "Inspect public Tori listings",
        long_about = "Inspect normalized public Tori listing details by search result ID without account authentication."
    )]
    Item(item::ItemArgs),
    #[command(
        about = "Manage published Tori listings",
        long_about = "List, inspect, update, dispose of, or delete published listings for the authenticated Tori account."
    )]
    Listing(listing::ListingArgs),
    #[command(
        about = "Search public Tori listings",
        long_about = "Search public Tori listings with taxonomy, location, price, pagination, and detail-explanation filters.",
        after_long_help = "Helsinki-area example:\n  flea tori search 'tuoli' --area Helsinki,Espoo,Vantaa"
    )]
    Search(Box<search::SearchArgs>),
    #[command(
        about = "Manage Tori saved searches and alerts",
        long_about = "List, inspect, create, update, or delete authenticated Tori search alerts."
    )]
    SavedSearch(Box<saved_search::SavedSearchArgs>),
    #[command(
        about = "Discover public Tori location identifiers",
        long_about = "Search public Tori location metadata and return identifiers suitable for search filters."
    )]
    Location(location::LocationArgs),
}

#[derive(Debug, Args)]
pub struct VintedArgs {
    /// Select a validated Vinted portal binding.
    #[arg(long, value_enum, default_value_t)]
    pub portal: PortalId,

    #[command(subcommand)]
    pub command: VintedCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedCommand {
    #[command(
        about = "Manage Vinted browser authentication",
        long_about = "Sign in, inspect the locally stored session, or clear Vinted credentials."
    )]
    Auth(auth::AuthArgs),
    #[command(
        about = "Show capabilities for this Vinted portal",
        long_about = "Show Vinted operations, authentication requirements, and implementation maturity without making a network request."
    )]
    Capabilities,
    #[command(
        about = "Search Vinted listings (authentication required)",
        long_about = "Search Vinted listings by text, price, ordering, and page. Authentication is required. Run `flea vinted auth login` first."
    )]
    Search(vinted_search::VintedSearchArgs),
    #[command(external_subcommand)]
    Unsupported(Vec<OsString>),
}

pub type CommandFuture<'a> =
    Pin<Box<dyn Future<Output = Result<outcome::CommandOutcome, AppError>> + 'a>>;

pub trait CommandRuntime {
    fn execute(&self, command: Command) -> CommandFuture<'_>;
}

pub async fn dispatch(command: Command) -> Result<outcome::CommandOutcome, AppError> {
    runtime::ProductionRuntime.execute(command).await
}

pub async fn dispatch_with_runtime(
    command: Command,
    runtime: &dyn CommandRuntime,
) -> Result<outcome::CommandOutcome, AppError> {
    runtime.execute(command).await
}
