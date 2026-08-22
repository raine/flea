use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::{
    api::favorites::{Favorites, FavoritesApi},
    domain::observation::Observation,
    error::AppError,
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
        /// Numeric marketplace listing ID returned by `flea search`.
        listing_id: String,
    },
    #[command(
        about = "Save a marketplace listing",
        long_about = "Add a marketplace listing to an explicit favorites folder or the account's default folder."
    )]
    Add {
        /// Numeric marketplace listing ID returned by `flea search`.
        listing_id: String,

        /// Favorites folder ID returned by `flea favorite folders`.
        #[arg(long)]
        folder: Option<u64>,
    },
    #[command(
        about = "Remove a saved marketplace listing",
        long_about = "Remove a marketplace listing from an explicit favorites folder or the account's default folder."
    )]
    Remove {
        /// Numeric marketplace listing ID returned by `flea search`.
        listing_id: String,

        /// Favorites folder ID returned by `flea favorite folders`.
        #[arg(long)]
        folder: Option<u64>,
    },
}

pub fn dispatch_with_api(args: FavoriteArgs, api: &dyn FavoritesApi) -> Result<Value, AppError> {
    let favorites = Favorites::new(api);
    match args.command {
        FavoriteCommand::Folders => {
            let folders = favorites.folders()?;
            Ok(json!({
                "folders": folders,
                "_observation": Observation::confirmed_present("favorites_folders", None),
            }))
        }
        FavoriteCommand::Status { listing_id } => {
            let status = favorites.status(&listing_id)?;
            let observation = if status.favorite {
                Observation::confirmed_present("favorites_minimal", None)
            } else {
                Observation::confirmed_absent("favorites_minimal", None)
            };
            let mut value = serde_json::to_value(status).map_err(|error| {
                AppError::output("failed to serialize favorite status").with_source(error)
            })?;
            value
                .as_object_mut()
                .expect("favorite status serializes as an object")
                .insert(
                    "_observation".to_owned(),
                    serde_json::to_value(observation).expect("observation is serializable"),
                );
            Ok(value)
        }
        FavoriteCommand::Add { listing_id, folder } => {
            let mutation = favorites.add(&listing_id, folder)?;
            Ok(mutation_value(mutation)?)
        }
        FavoriteCommand::Remove { listing_id, folder } => {
            let mutation = favorites.remove(&listing_id, folder)?;
            Ok(mutation_value(mutation)?)
        }
    }
}

fn mutation_value(mutation: crate::api::favorites::FavoriteMutation) -> Result<Value, AppError> {
    let mut value = serde_json::to_value(&mutation).map_err(|error| {
        AppError::output("failed to serialize favorite output").with_source(error)
    })?;
    let observation = if mutation.favorite {
        Observation::confirmed_present("favorite_mutation_response", None)
    } else {
        Observation::confirmed_absent("favorite_mutation_response", None)
    };
    value
        .as_object_mut()
        .expect("favorite mutation serializes as an object")
        .insert(
            "_observation".to_owned(),
            serde_json::to_value(observation).expect("observation is serializable"),
        );
    Ok(value)
}
