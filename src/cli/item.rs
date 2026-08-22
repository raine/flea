use clap::{Args, Subcommand};
use serde_json::Value;

use crate::{
    api::item::{PublicItemApi, PublicItems},
    error::AppError,
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
        /// Numeric marketplace listing ID returned by `flea search`.
        listing_id: String,

        /// Return the upstream JSON body inside the standard output envelope.
        #[arg(long)]
        raw: bool,
    },
}

pub fn dispatch_with_api(args: ItemArgs, api: &dyn PublicItemApi) -> Result<Value, AppError> {
    match args.command {
        ItemCommand::Show { listing_id, raw } => {
            let (detail, upstream) = PublicItems::new(api).show(&listing_id)?;
            if raw {
                Ok(upstream)
            } else {
                serde_json::to_value(detail).map_err(|error| {
                    AppError::output("failed to serialize public item output").with_source(error)
                })
            }
        }
    }
}
