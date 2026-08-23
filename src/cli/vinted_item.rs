use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct VintedItemArgs {
    #[command(subcommand)]
    pub command: VintedItemCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedItemCommand {
    #[command(
        about = "Show an authenticated Vinted listing",
        long_about = "Fetch and normalize a Vinted listing by search result ID. A returned location is seller-disclosed profile information, not a catalog filter value or a guarantee of the item's physical location. Authentication is required."
    )]
    Show {
        /// Numeric Vinted item ID returned by `flea vinted search`.
        item_id: String,

        /// Return the exact upstream JSON body inside the standard output envelope.
        #[arg(long)]
        raw: bool,
    },
}

impl VintedItemCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Show { .. } => "item show",
        }
    }
}
