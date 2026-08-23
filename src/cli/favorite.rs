use clap::{Args, Subcommand};

use crate::{
    cli::outcome::{CommandData, CommandOutcome, FavoriteFoldersOutput},
    domain::observation::Observation,
    error::AppError,
    marketplace::tori::favorites::{Favorites, FavoritesApi},
};

#[derive(Debug, Args)]
pub struct FavoriteArgs {
    #[command(subcommand)]
    pub command: FavoriteCommand,
}

#[derive(Debug, Subcommand)]
pub enum FavoriteCommand {
    #[command(
        about = "List favorites folders",
        long_about = "List the authenticated account's Tori favorites folders and their item counts."
    )]
    Folders,
    #[command(
        about = "Show whether a marketplace listing is saved",
        long_about = "Inspect whether a marketplace listing is saved and return every favorites folder containing it."
    )]
    Status {
        /// Numeric marketplace listing ID returned by `flea tori search`.
        listing_id: String,
    },
    #[command(
        about = "Save a marketplace listing",
        long_about = "Add a marketplace listing to an explicit favorites folder or the account's default folder."
    )]
    Add {
        /// Numeric marketplace listing ID returned by `flea tori search`.
        listing_id: String,

        /// Favorites folder ID returned by `flea tori favorite folders`.
        #[arg(long)]
        folder: Option<u64>,
    },
    #[command(
        about = "Remove a saved marketplace listing",
        long_about = "Remove a marketplace listing from an explicit favorites folder or the account's default folder."
    )]
    Remove {
        /// Numeric marketplace listing ID returned by `flea tori search`.
        listing_id: String,

        /// Favorites folder ID returned by `flea tori favorite folders`.
        #[arg(long)]
        folder: Option<u64>,
    },
}

impl FavoriteCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Folders => "favorite folders",
            Self::Status { .. } => "favorite status",
            Self::Add { .. } => "favorite add",
            Self::Remove { .. } => "favorite remove",
        }
    }
}

pub async fn dispatch(
    args: FavoriteArgs,
    api: &dyn FavoritesApi,
) -> Result<CommandOutcome, AppError> {
    let favorites = Favorites::new(api);
    match args.command {
        FavoriteCommand::Folders => {
            let folders = favorites.folders().await?;
            Ok(
                CommandOutcome::new(CommandData::FavoriteFolders(FavoriteFoldersOutput {
                    folders,
                }))
                .with_observation(Observation::confirmed_present("favorites_folders", None)),
            )
        }
        FavoriteCommand::Status { listing_id } => {
            let status = favorites.status(&listing_id).await?;
            let observation = if status.favorite {
                Observation::confirmed_present("favorites_minimal", None)
            } else {
                Observation::confirmed_absent("favorites_minimal", None)
            };
            Ok(CommandOutcome::new(CommandData::FavoriteStatus(status))
                .with_observation(observation))
        }
        FavoriteCommand::Add { listing_id, folder } => {
            let mutation = favorites.add(&listing_id, folder).await?;
            mutation_outcome(mutation)
        }
        FavoriteCommand::Remove { listing_id, folder } => {
            let mutation = favorites.remove(&listing_id, folder).await?;
            mutation_outcome(mutation)
        }
    }
}

fn mutation_outcome(
    mutation: crate::marketplace::tori::favorites::FavoriteMutation,
) -> Result<CommandOutcome, AppError> {
    let observation = if mutation.favorite {
        Observation::confirmed_present("favorite_mutation_response", None)
    } else {
        Observation::confirmed_absent("favorite_mutation_response", None)
    };
    Ok(CommandOutcome::new(CommandData::FavoriteMutation(mutation)).with_observation(observation))
}
