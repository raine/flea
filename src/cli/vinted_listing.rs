use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct VintedListingArgs {
    #[command(subcommand)]
    pub command: VintedListingCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedListingCommand {
    #[command(
        about = "Show an account listing",
        long_about = "Inspect authoritative Vinted account state directly by item ID without relying on search indexing. Condition output distinguishes upstream listing IDs from selection-scoped composer option IDs resolved through runtime discovery. Listings under review return moderated state and available summary fields until Vinted makes editable detail available."
    )]
    Show {
        /// Numeric item ID returned by Vinted publication.
        item_id: String,
    },
    #[command(
        about = "Update an owned public listing",
        long_about = "Update the title, description, or price of an owned public Vinted listing from a partial JSON object. Omitted fields and the complete existing photo order are preserved. Category, condition, attribute, brand, color, package, and photo changes are not supported. Vinted provides no remote revision precondition, so a concurrent edit can be overwritten."
    )]
    Update {
        /// Numeric item ID returned by Vinted publication.
        item_id: String,
        /// Partial JSON object containing title, description, or price strings.
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
    },
    #[command(
        about = "List active and draft-associated account items",
        long_about = "List the authenticated account's active and draft-associated Vinted items from the bounded wardrobe API."
    )]
    List,
}

impl VintedListingCommand {
    pub const fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Show { .. } => "listing show",
            Self::Update { .. } => "listing update",
            Self::List => "listing list",
        }
    }
}
