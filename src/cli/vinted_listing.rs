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
        long_about = "Inspect authoritative Vinted account state and complete editable listing fields directly by item ID without relying on search indexing."
    )]
    Show {
        /// Numeric item ID returned by Vinted publication.
        item_id: String,
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
            Self::List => "listing list",
        }
    }
}
