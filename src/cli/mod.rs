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

    pub fn telemetry_name(&self) -> String {
        match self {
            Self::Capabilities => "capabilities".to_owned(),
            Self::Marketplaces => "marketplaces".to_owned(),
            Self::Tori(args) => args.command.telemetry_name(),
            Self::Vinted(args) => args.command.telemetry_name(),
            Self::Skill(_) => "skill".to_owned(),
            Self::Unsupported(_) => "unknown".to_owned(),
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

impl ToriCommand {
    pub fn telemetry_name(&self) -> String {
        let command = match self {
            Self::Auth(args) => args.command.telemetry_name(),
            Self::Capabilities => return "tori capabilities".to_owned(),
            Self::Category(args) => args.command.telemetry_name(),
            Self::Draft(args) => args.command.telemetry_name(),
            Self::Favorite(args) => args.command.telemetry_name(),
            Self::Item(args) => args.command.telemetry_name(),
            Self::Listing(args) => args.command.telemetry_name(),
            Self::Search(_) => return "tori search".to_owned(),
            Self::SavedSearch(args) => args.command.telemetry_name(),
            Self::Location(args) => args.command.telemetry_name(),
        };
        format!("tori {command}")
    }
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

impl VintedCommand {
    pub fn telemetry_name(&self) -> String {
        match self {
            Self::Auth(args) => format!("vinted {}", args.command.telemetry_name()),
            Self::Capabilities => "vinted capabilities".to_owned(),
            Self::Search(_) => "vinted search".to_owned(),
            Self::Unsupported(_) => "unknown".to_owned(),
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsed_command_variants_have_stable_telemetry_names() {
        let cases: &[(&[&str], &str)] = &[
            (&["capabilities"], "capabilities"),
            (&["marketplaces"], "marketplaces"),
            (&["skill"], "skill"),
            (&["skill", "install"], "skill"),
            (&["tori", "auth", "login"], "tori auth login"),
            (
                &[
                    "tori",
                    "auth",
                    "callback",
                    "--state-root",
                    "/tmp/flea",
                    "https://example.com/callback",
                ],
                "tori auth callback",
            ),
            (&["tori", "auth", "status"], "tori auth status"),
            (&["tori", "auth", "logout"], "tori auth logout"),
            (&["tori", "capabilities"], "tori capabilities"),
            (
                &["tori", "category", "search", "chairs"],
                "tori category search",
            ),
            (&["tori", "category", "list"], "tori category list"),
            (&["tori", "draft", "create"], "tori draft create"),
            (&["tori", "draft", "preview"], "tori draft preview"),
            (&["tori", "draft", "show", "draft-1"], "tori draft show"),
            (&["tori", "draft", "update", "draft-1"], "tori draft update"),
            (
                &["tori", "draft", "image", "add", "draft-1", "photo.jpg"],
                "tori draft image add",
            ),
            (
                &["tori", "draft", "image", "remove", "draft-1", "image-1"],
                "tori draft image remove",
            ),
            (
                &["tori", "draft", "validate", "draft-1"],
                "tori draft validate",
            ),
            (
                &[
                    "tori",
                    "draft",
                    "publish",
                    "draft-1",
                    "--if-revision",
                    "revision-1",
                ],
                "tori draft publish",
            ),
            (&["tori", "draft", "delete", "draft-1"], "tori draft delete"),
            (&["tori", "favorite", "folders"], "tori favorite folders"),
            (
                &["tori", "favorite", "status", "123"],
                "tori favorite status",
            ),
            (&["tori", "favorite", "add", "123"], "tori favorite add"),
            (
                &["tori", "favorite", "remove", "123"],
                "tori favorite remove",
            ),
            (&["tori", "item", "show", "123"], "tori item show"),
            (&["tori", "listing", "list"], "tori listing list"),
            (
                &["tori", "listing", "show", "listing-1"],
                "tori listing show",
            ),
            (
                &["tori", "listing", "update", "listing-1"],
                "tori listing update",
            ),
            (
                &["tori", "listing", "dispose", "listing-1"],
                "tori listing dispose",
            ),
            (
                &["tori", "listing", "delete", "listing-1"],
                "tori listing delete",
            ),
            (&["tori", "search", "private query"], "tori search"),
            (&["tori", "saved-search", "list"], "tori saved-search list"),
            (
                &["tori", "saved-search", "show", "search-1"],
                "tori saved-search show",
            ),
            (
                &[
                    "tori",
                    "saved-search",
                    "create",
                    "--name",
                    "chairs",
                    "--no-notifications",
                ],
                "tori saved-search create",
            ),
            (
                &["tori", "saved-search", "update", "search-1"],
                "tori saved-search update",
            ),
            (
                &["tori", "saved-search", "delete", "search-1"],
                "tori saved-search delete",
            ),
            (
                &["tori", "location", "search", "Helsinki"],
                "tori location search",
            ),
            (&["vinted", "auth", "login"], "vinted auth login"),
            (
                &[
                    "vinted",
                    "auth",
                    "callback",
                    "--state-root",
                    "/tmp/flea",
                    "https://example.com/callback",
                ],
                "vinted auth callback",
            ),
            (&["vinted", "auth", "status"], "vinted auth status"),
            (&["vinted", "auth", "logout"], "vinted auth logout"),
            (
                &["vinted", "--portal", "fi", "capabilities"],
                "vinted capabilities",
            ),
            (&["vinted", "search", "private query"], "vinted search"),
            (&["unsupported"], "unknown"),
            (&["vinted", "unsupported"], "unknown"),
        ];

        for (args, expected) in cases {
            let cli = Cli::try_parse_from(std::iter::once("flea").chain(args.iter().copied()))
                .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
            assert_eq!(cli.command.telemetry_name(), *expected, "args: {args:?}");
        }
    }
}
