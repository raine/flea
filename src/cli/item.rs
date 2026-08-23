use clap::{Args, Subcommand};

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    error::AppError,
    marketplace::tori::item::{PublicItemApi, PublicItems, ShowItemRequest, ShowItemResult},
};

#[derive(Debug, Args)]
pub struct ItemArgs {
    #[command(subcommand)]
    pub command: ItemCommand,
}

#[derive(Debug, Subcommand)]
pub enum ItemCommand {
    #[command(
        about = "Show a public marketplace listing",
        long_about = "Fetch and normalize the public details of any marketplace listing ID returned by search without account authentication."
    )]
    Show {
        /// Numeric marketplace listing ID returned by `flea tori search`.
        listing_id: String,

        /// Return the upstream JSON body inside the standard output envelope.
        #[arg(long)]
        raw: bool,
    },
}

impl ItemCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Show { .. } => "item show",
        }
    }
}

pub async fn dispatch(args: ItemArgs, api: &dyn PublicItemApi) -> Result<CommandOutcome, AppError> {
    let request = match args.command {
        ItemCommand::Show { listing_id, raw } => ShowItemRequest { listing_id, raw },
    };
    match PublicItems::new(api).execute(request).await? {
        ShowItemResult::Detail { item, observation } => {
            Ok(CommandOutcome::new(CommandData::Item(*item)).with_observation(observation))
        }
        ShowItemResult::Raw(raw) => Ok(CommandOutcome::new(CommandData::Raw(raw))),
    }
}
