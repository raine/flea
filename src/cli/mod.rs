pub(crate) mod auth;
pub(crate) mod category;
pub(crate) mod draft;
pub(crate) mod favorite;
pub(crate) mod item;
pub(crate) mod listing;
pub(crate) mod location;
pub(crate) mod outcome;
pub(crate) mod runtime;
pub(crate) mod saved_search;
pub(crate) mod search;
pub(crate) mod skill;
pub(crate) mod vinted_category;
pub(crate) mod vinted_item;
pub(crate) mod vinted_listing;
pub(crate) mod vinted_listing_input;
pub(crate) mod vinted_publish;
pub(crate) mod vinted_search;
pub(crate) mod vinted_sell;

use std::ffi::OsString;

use clap::{Args, Parser, Subcommand};

#[cfg(test)]
use crate::marketplace::CapabilityId;
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

    /// Use an existing Chrome debugging HTTP URL; reuse its session on macOS/Linux.
    #[arg(long, global = true, value_parser = crate::browser::parse_browser_url)]
    pub browser_url: Option<url::Url>,

    #[arg(skip)]
    pub format_explicit: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Open the dedicated Flea browser without enabling remote debugging.
    #[command(
        long_about = "Open the dedicated Flea Chrome profile without enabling remote debugging. Close any debugging-enabled Flea Chrome instance first. Use browser disconnect --browser-url URL to release a persistent external-browser connection."
    )]
    Browser(BrowserArgs),

    #[command(name = "__browser-session", hide = true)]
    BrowserSession,

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
            Self::Browser(_)
            | Self::BrowserSession
            | Self::Capabilities
            | Self::Marketplaces
            | Self::Skill(_)
            | Self::Unsupported(_) => None,
        }
    }

    #[cfg(test)]
    pub fn capability_id(&self) -> Option<CapabilityId> {
        match self {
            Self::Tori(args) => args.command.capability_id(),
            Self::Vinted(args) => args.command.capability_id(),
            Self::Browser(_)
            | Self::BrowserSession
            | Self::Capabilities
            | Self::Marketplaces
            | Self::Skill(_)
            | Self::Unsupported(_) => None,
        }
    }

    pub fn telemetry_name(&self) -> String {
        match self {
            Self::Capabilities => "capabilities".to_owned(),
            Self::Marketplaces => "marketplaces".to_owned(),
            Self::Tori(args) => args.command.telemetry_name(),
            Self::Vinted(args) => args.command.telemetry_name(),
            Self::Skill(_) => "skill".to_owned(),
            Self::Browser(args) => match args.command {
                None => "browser".to_owned(),
                Some(BrowserCommand::Disconnect) => "browser disconnect".to_owned(),
            },
            Self::BrowserSession => "browser session".to_owned(),
            Self::Unsupported(_) => "unknown".to_owned(),
        }
    }
}

#[derive(Debug, Args)]
pub struct BrowserArgs {
    #[command(subcommand)]
    pub command: Option<BrowserCommand>,
}

#[derive(Debug, Subcommand)]
pub enum BrowserCommand {
    #[command(
        about = "Release the persistent connection to an external browser",
        long_about = "Disconnect the background Flea session selected by --browser-url without closing Chrome or clearing browser data. Waits for any active browser operation to finish."
    )]
    Disconnect,
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
    Auth(auth::ToriAuthArgs),
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
    #[cfg(test)]
    pub fn capability_id(&self) -> Option<CapabilityId> {
        Some(match self {
            Self::Auth(args) => args.command.capability_id(),
            Self::Capabilities => return None,
            Self::Category(_) => CapabilityId::Category,
            Self::Draft(_) => CapabilityId::Draft,
            Self::Favorite(_) => CapabilityId::Favorite,
            Self::Item(_) => CapabilityId::ItemShow,
            Self::Listing(_) => CapabilityId::Listing,
            Self::Search(_) => CapabilityId::Search,
            Self::SavedSearch(_) => CapabilityId::SavedSearch,
            Self::Location(_) => CapabilityId::LocationSearch,
        })
    }

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
        about = "Manage Vinted authentication",
        long_about = "Set up, inspect, or clear the account credentials and persistent browser session used by Vinted catalog and publication commands."
    )]
    Auth(auth::VintedAuthArgs),
    #[command(
        about = "Show capabilities for this Vinted portal",
        long_about = "Show Vinted operations, authentication requirements, and implementation maturity without making a network request."
    )]
    Capabilities,
    #[command(
        about = "Search Vinted listings (authentication required)",
        long_about = "Search or browse Vinted listings with catalog, dynamic attributes, decimal prices, ordering, pagination, and optional contextual facets. Authentication is required. Run `flea vinted auth login` first.",
        after_long_help = "Examples:\n  flea vinted search takki --price-from 10.50 --sort newest\n  flea vinted search --catalog 123 --brand 53,88 --status 1 --include-facets\n  flea vinted search mekko --attribute fixture_code=10,20"
    )]
    Search(Box<vinted_search::VintedSearchArgs>),
    #[command(
        about = "Discover and search contextual Vinted filters",
        long_about = "Discover active filter codes and option IDs, retrieve lazy facets, and search large option lists with the same context used for catalog search."
    )]
    Filter(Box<vinted_search::VintedFilterArgs>),
    #[command(
        about = "Inspect Vinted listings (authentication required)",
        long_about = "Inspect a Vinted listing by search result ID. Seller-disclosed location is profile information, not a catalog location filter or guaranteed item location. Authentication is required."
    )]
    Item(vinted_item::VintedItemArgs),
    #[command(
        about = "Manage Vinted account listings (authentication required)",
        long_about = "Inspect authoritative account listing state by publication item ID, update supported fields on owned public listings, or enumerate active and draft-associated account items without search indexing."
    )]
    Listing(vinted_listing::VintedListingArgs),
    #[command(
        about = "Discover Vinted publication categories and fields",
        long_about = "Discover runtime category, dynamic attribute, brand, color, configuration, and package values for Vinted publication."
    )]
    Category(vinted_category::VintedCategoryArgs),
    #[command(
        about = "Prepare a Vinted listing from semantic seller facts",
        long_about = "Resolve semantic seller facts through scoped runtime category, brand, color, package, configuration, and attribute discovery. Return a compact proposed publication mutation when every value is exact, or structured resumable choices without uploading images or changing remote state.",
        after_long_help = "Example:\n  flea vinted sell --input facts.json --image front.jpg\n  flea vinted sell --input facts.json --select category=123"
    )]
    Sell(vinted_sell::VintedSellArgs),
    #[command(
        about = "Check Vinted publication readiness",
        long_about = "Validate the authenticated session and report selling prerequisites that Vinted exposes without uploading images or mutating a listing. Verification remains a manual user action in Vinted."
    )]
    Readiness,
    #[command(
        about = "Manage Vinted drafts",
        long_about = "List, inspect, validate, create, replace, publish, or delete authoritative Vinted drafts."
    )]
    Draft(vinted_publish::VintedDraftArgs),
    #[command(
        about = "Publish a Vinted listing",
        long_about = "Upload locally sanitized images and publish a complete runtime-discovered Vinted listing payload. Confirmed review-pending publications receive bounded read-only account verification."
    )]
    Publish(vinted_publish::PublicationInputArgs),
    #[command(external_subcommand)]
    Unsupported(Vec<OsString>),
}

impl VintedCommand {
    #[cfg(test)]
    pub fn capability_id(&self) -> Option<CapabilityId> {
        Some(match self {
            Self::Auth(args) => args.command.capability_id(),
            Self::Capabilities | Self::Unsupported(_) => return None,
            Self::Search(_) | Self::Filter(_) => CapabilityId::Search,
            Self::Item(_) => CapabilityId::ItemShow,
            Self::Listing(_) => CapabilityId::Listing,
            Self::Category(_) => CapabilityId::Category,
            Self::Sell(_) | Self::Readiness | Self::Draft(_) | Self::Publish(_) => {
                CapabilityId::Draft
            }
        })
    }

    pub fn telemetry_name(&self) -> String {
        match self {
            Self::Auth(args) => format!("vinted {}", args.command.telemetry_name()),
            Self::Capabilities => "vinted capabilities".to_owned(),
            Self::Search(_) => "vinted search".to_owned(),
            Self::Filter(args) => format!("vinted {}", args.command.telemetry_name()),
            Self::Item(args) => format!("vinted {}", args.command.telemetry_name()),
            Self::Listing(args) => format!("vinted {}", args.command.telemetry_name()),
            Self::Category(args) => format!("vinted {}", args.command.telemetry_name()),
            Self::Sell(_) => "vinted sell".to_owned(),
            Self::Readiness => "vinted readiness".to_owned(),
            Self::Draft(args) => format!("vinted {}", args.command.telemetry_name()),
            Self::Publish(_) => "vinted publish".to_owned(),
            Self::Unsupported(_) => "unknown".to_owned(),
        }
    }
}

pub async fn dispatch(
    command: Command,
    dependencies: &runtime::ApplicationDependencies,
) -> Result<outcome::CommandOutcome, AppError> {
    runtime::dispatch(command, dependencies).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_url_is_global_and_validated() {
        for args in [
            vec![
                "flea",
                "--browser-url",
                "http://localhost:9222",
                "vinted",
                "auth",
                "status",
                "--browser",
            ],
            vec![
                "flea",
                "vinted",
                "auth",
                "status",
                "--browser",
                "--browser-url",
                "http://localhost:9222",
            ],
        ] {
            assert_eq!(
                Cli::try_parse_from(args)
                    .unwrap()
                    .browser_url
                    .unwrap()
                    .port(),
                Some(9222)
            );
        }
        for value in [
            "not-a-url",
            "file:///tmp/chrome",
            "http://user:secret@localhost:9222",
        ] {
            assert!(Cli::try_parse_from(["flea", "--browser-url", value, "capabilities"]).is_err());
        }
        assert!(matches!(
            Cli::try_parse_from(["flea", "browser"]).unwrap().command,
            Command::Browser(_)
        ));
    }

    #[test]
    fn parsed_command_variants_have_stable_telemetry_names() {
        let cases: &[(&[&str], &str)] = &[
            (&["browser"], "browser"),
            (&["browser", "disconnect"], "browser disconnect"),
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
                &["vinted", "auth", "login", "--browser"],
                "vinted auth login",
            ),
            (&["vinted", "auth", "status"], "vinted auth status"),
            (&["vinted", "auth", "status", "--api"], "vinted auth status"),
            (&["vinted", "auth", "logout"], "vinted auth logout"),
            (
                &["vinted", "--portal", "fi", "capabilities"],
                "vinted capabilities",
            ),
            (&["vinted", "search", "private query"], "vinted search"),
            (&["vinted", "filter", "list"], "vinted filter list"),
            (
                &["vinted", "filter", "facets", "brand"],
                "vinted filter facets",
            ),
            (
                &["vinted", "filter", "search", "brand", "Mar"],
                "vinted filter search",
            ),
            (&["vinted", "item", "show", "123"], "vinted item show"),
            (&["vinted", "sell", "--input", "facts.json"], "vinted sell"),
            (&["vinted", "readiness"], "vinted readiness"),
            (&["vinted", "draft", "list"], "vinted draft list"),
            (&["vinted", "draft", "show", "123"], "vinted draft show"),
            (
                &["vinted", "draft", "validate", "123"],
                "vinted draft validate",
            ),
            (&["vinted", "listing", "show", "123"], "vinted listing show"),
            (
                &[
                    "vinted",
                    "listing",
                    "update",
                    "123",
                    "--input",
                    "changes.json",
                ],
                "vinted listing update",
            ),
            (&["vinted", "listing", "list"], "vinted listing list"),
            (&["unsupported"], "unknown"),
            (&["vinted", "unsupported"], "unknown"),
        ];

        for (args, expected) in cases {
            let cli = Cli::try_parse_from(std::iter::once("flea").chain(args.iter().copied()))
                .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
            assert_eq!(cli.command.telemetry_name(), *expected, "args: {args:?}");
        }
    }

    fn assert_manifest_matches_reachable_commands(
        marketplace_id: crate::marketplace::MarketplaceId,
        cases: &[&[&str]],
    ) {
        use std::collections::HashSet;

        use crate::marketplace::{AuthRequirement, CapabilityMaturity, marketplace};

        let reachable = cases
            .iter()
            .map(|args| {
                let cli = Cli::try_parse_from(std::iter::once("flea").chain(args.iter().copied()))
                    .unwrap_or_else(|error| panic!("failed to parse {args:?}: {error}"));
                assert_eq!(
                    cli.command.context().map(|context| context.marketplace),
                    Some(marketplace_id),
                    "args: {args:?}"
                );
                cli.command
                    .capability_id()
                    .unwrap_or_else(|| panic!("command has no capability: {args:?}"))
            })
            .collect::<HashSet<_>>();
        let declared = marketplace(marketplace_id)
            .capabilities
            .iter()
            .filter(|capability| {
                capability.auth != AuthRequirement::Internal
                    && capability.maturity != CapabilityMaturity::Unavailable
            })
            .map(|capability| capability.id)
            .collect::<HashSet<_>>();

        assert_eq!(reachable, declared);
    }

    #[test]
    fn tori_manifest_matches_reachable_command_capabilities() {
        use crate::marketplace::MarketplaceId;

        assert_manifest_matches_reachable_commands(
            MarketplaceId::Tori,
            &[
                &["tori", "auth", "login"],
                &[
                    "tori",
                    "auth",
                    "callback",
                    "--state-root",
                    "/tmp/flea",
                    "https://example.com/callback",
                ],
                &["tori", "auth", "status"],
                &["tori", "auth", "logout"],
                &["tori", "category", "list"],
                &["tori", "draft", "create"],
                &["tori", "favorite", "folders"],
                &["tori", "item", "show", "123"],
                &["tori", "listing", "list"],
                &["tori", "search", "chair"],
                &["tori", "saved-search", "list"],
                &["tori", "location", "search", "Helsinki"],
            ],
        );
    }

    #[test]
    fn vinted_manifest_matches_reachable_command_capabilities() {
        use crate::marketplace::MarketplaceId;

        assert_manifest_matches_reachable_commands(
            MarketplaceId::Vinted,
            &[
                &["vinted", "auth", "login"],
                &["vinted", "auth", "status"],
                &["vinted", "auth", "logout"],
                &["vinted", "search", "chair"],
                &["vinted", "item", "show", "123"],
                &["vinted", "listing", "list"],
                &["vinted", "category", "list"],
                &["vinted", "draft", "delete", "123"],
            ],
        );
    }

    #[test]
    fn vinted_draft_publish_accepts_remote_photo_reuse_without_images() {
        let result = Cli::try_parse_from([
            "flea",
            "vinted",
            "draft",
            "publish",
            "123",
            "--input",
            "listing.json",
        ]);

        assert!(result.is_ok());
    }

    #[test]
    fn vinted_publication_uses_the_browser_transport_without_selection() {
        assert!(
            Cli::try_parse_from([
                "flea",
                "vinted",
                "publish",
                "--input",
                "listing.json",
                "--image",
                "photo.jpg",
            ])
            .is_ok()
        );
        assert!(Cli::try_parse_from(["flea", "vinted", "draft", "delete", "123"]).is_ok());
        assert!(
            Cli::try_parse_from([
                "flea",
                "vinted",
                "draft",
                "delete",
                "123",
                "--transport",
                "native",
            ])
            .is_err()
        );
    }

    #[test]
    fn vinted_auth_rejects_the_tori_callback_command() {
        let result = Cli::try_parse_from([
            "flea",
            "vinted",
            "auth",
            "callback",
            "--state-root",
            "/tmp/flea",
            "vintedfr://auth?code=secret&state=secret",
        ]);

        assert!(result.is_err());
    }
}
