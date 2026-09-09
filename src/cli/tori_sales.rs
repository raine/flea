use clap::{Args, Subcommand};

use crate::marketplace::tori::sales::ToriSalesRequest;

#[derive(Debug, Args)]
pub struct ToriSalesArgs {
    #[command(subcommand)]
    pub command: ToriSalesCommand,
}

#[derive(Debug, Subcommand)]
pub enum ToriSalesCommand {
    #[command(
        about = "List Tori ads marked sold (authentication required)",
        long_about = "Read one page of your Tori ads marked sold, not ToriDiili transactions. Coverage is limited to ads Tori still exposes. Subtitle is display text, not a verified sale price. Follow next_actions for more results."
    )]
    List {
        /// Zero-indexed result offset.
        #[arg(long, default_value_t = 0)]
        offset: usize,
        /// Results requested per page (positive integer; default 50).
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
}

impl From<ToriSalesCommand> for ToriSalesRequest {
    fn from(command: ToriSalesCommand) -> Self {
        let ToriSalesCommand::List { offset, limit } = command;
        Self { offset, limit }
    }
}
