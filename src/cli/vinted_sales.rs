use clap::{Args, Subcommand, ValueEnum};

use crate::{
    domain::vinted_sale::VintedSalesStatus, marketplace::vinted::sales::VintedSalesRequest,
};

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum SalesStatus {
    All,
    InProgress,
    #[default]
    Completed,
    Canceled,
}

#[derive(Debug, Args)]
pub struct VintedSalesArgs {
    #[command(subcommand)]
    pub command: VintedSalesCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedSalesCommand {
    #[command(
        about = "List past Vinted sales (authentication required)",
        long_about = "Read one page of the authenticated user's sold orders, completed by default. Orders may represent bundles, not individual listings. Coverage is limited to history Vinted exposes; all-time history and manually marked-sold items are not guaranteed. Price is not net earnings, and date is not necessarily completion time."
    )]
    List {
        /// Order status filter, not localized status text.
        #[arg(long, value_enum, default_value = "completed")]
        status: SalesStatus,
        /// One-indexed result page.
        #[arg(long, default_value_t = 1)]
        page: usize,
        /// Results per page, from 1 through 96.
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
}

impl From<VintedSalesCommand> for VintedSalesRequest {
    fn from(command: VintedSalesCommand) -> Self {
        let VintedSalesCommand::List {
            status,
            page,
            limit,
        } = command;
        Self {
            status: match status {
                SalesStatus::All => VintedSalesStatus::All,
                SalesStatus::InProgress => VintedSalesStatus::InProgress,
                SalesStatus::Completed => VintedSalesStatus::Completed,
                SalesStatus::Canceled => VintedSalesStatus::Canceled,
            },
            page,
            limit,
        }
    }
}
