use clap::{Args, Subcommand};

use crate::{
    cli::outcome::{CommandData, CommandOutcome, FavoriteFoldersOutput},
    error::AppError,
    marketplace::tori::favorites::{FavoriteRequest, FavoriteResult, Favorites, FavoritesApi},
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
    let request = match args.command {
        FavoriteCommand::Folders => FavoriteRequest::Folders,
        FavoriteCommand::Status { listing_id } => FavoriteRequest::Status { listing_id },
        FavoriteCommand::Add { listing_id, folder } => FavoriteRequest::Add {
            listing_id,
            folder_id: folder,
        },
        FavoriteCommand::Remove { listing_id, folder } => FavoriteRequest::Remove {
            listing_id,
            folder_id: folder,
        },
    };
    let (data, observation) = match Favorites::new(api).execute(request).await? {
        FavoriteResult::Folders {
            folders,
            observation,
        } => (
            CommandData::FavoriteFolders(FavoriteFoldersOutput { folders }),
            observation,
        ),
        FavoriteResult::Status {
            status,
            observation,
        } => (CommandData::FavoriteStatus(status), observation),
        FavoriteResult::Mutation {
            mutation,
            observation,
        } => (CommandData::FavoriteMutation(mutation), observation),
    };
    Ok(CommandOutcome::new(data).with_observation(observation))
}
