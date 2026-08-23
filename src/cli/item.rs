use clap::{Args, Subcommand};

use crate::{
    api::item::{PublicItemApi, PublicItems},
    cli::outcome::CommandOutcome,
    domain::observation::Observation,
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
        /// Numeric marketplace listing ID returned by `flea tori search`.
        listing_id: String,

        /// Return the upstream JSON body inside the standard output envelope.
        #[arg(long)]
        raw: bool,
    },
}

pub async fn dispatch_with_api(
    args: ItemArgs,
    api: &dyn PublicItemApi,
) -> Result<CommandOutcome, AppError> {
    match args.command {
        ItemCommand::Show { listing_id, raw } => {
            let (detail, upstream) = PublicItems::new(api).show(&listing_id).await?;
            if raw {
                return Ok(upstream.into());
            }
            let value = serde_json::to_value(detail).map_err(|error| {
                AppError::output("failed to serialize public item output").with_source(error)
            })?;
            Ok(
                CommandOutcome::new(value).with_observation(Observation::confirmed_present(
                    "public_listing_detail",
                    None,
                )),
            )
        }
    }
}
