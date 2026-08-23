use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::{
    api::{
        saved_searches::{CreateSavedSearch, SavedSearches, SavedSearchesApi},
        search::PublicSearchApi,
    },
    domain::observation::Observation,
    error::AppError,
};

use super::search::{SearchArgs, saved_search_parameters};

#[derive(Debug, Args)]
pub struct SavedSearchArgs {
    #[command(subcommand)]
    pub command: SavedSearchCommand,
}

#[derive(Debug, Subcommand)]
pub enum SavedSearchCommand {
    #[command(
        about = "List saved searches",
        long_about = "List search alerts for the authenticated Tori account. Upstream identifiers and notification values are returned as opaque strings."
    )]
    List {
        /// Maximum number requested from Tori.
        #[arg(long)]
        limit: Option<usize>,
    },
    #[command(
        about = "Show a saved search",
        long_about = "Show one authenticated search alert by the opaque ID returned by `flea tori saved-search list`."
    )]
    Show {
        /// Opaque saved search ID returned by list.
        id: String,
    },
    #[command(
        about = "Create a search alert",
        long_about = "Create an authenticated Tori search alert using the same query and filter arguments as public search. Choose notification channels explicitly; Flea does not infer seller or notification intent.",
        after_long_help = "Alert definitions accept query, category, location or area, coordinate radius, price, trade type, condition, seller, shipping, dynamic facets, and sorting. Pagination, explanation, facet-output, and raw-output options belong to public result retrieval and are rejected for alerts."
    )]
    Create {
        /// Alert name shown by Tori.
        #[arg(long)]
        name: String,

        /// Send daily email notifications.
        #[arg(long)]
        email: bool,

        /// Send mobile push notifications.
        #[arg(long)]
        push: bool,

        /// Show matches in Tori's notification center.
        #[arg(long = "notification-center")]
        notification_center: bool,

        /// Create the saved search with every notification channel disabled.
        #[arg(long, conflicts_with_all = ["email", "push", "notification_center"])]
        no_notifications: bool,

        #[command(flatten)]
        search: SearchArgs,
    },
    #[command(
        about = "Update a search alert",
        long_about = "Rename an alert or enable and disable source-backed email, push, and notification-center channels. Omitted channels retain their remote state."
    )]
    Update {
        /// Opaque saved search ID returned by list.
        id: String,

        /// Replacement alert name.
        #[arg(long)]
        name: Option<String>,

        /// Enable or disable daily email notifications.
        #[arg(long, value_enum)]
        email: Option<NotificationState>,

        /// Enable or disable mobile push notifications.
        #[arg(long, value_enum)]
        push: Option<NotificationState>,

        /// Enable or disable Tori notification-center matches.
        #[arg(long = "notification-center", value_enum)]
        notification_center: Option<NotificationState>,
    },
    #[command(
        about = "Delete a search alert",
        long_about = "Permanently delete an authenticated Tori search alert. On uncertain failure Flea performs read-only recovery before reporting retry safety."
    )]
    Delete {
        /// Opaque saved search ID returned by list.
        id: String,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum NotificationState {
    On,
    Off,
}

pub async fn dispatch_with_apis(
    args: SavedSearchArgs,
    api: &dyn SavedSearchesApi,
    search_api: &dyn PublicSearchApi,
) -> Result<Value, AppError> {
    let saved = SavedSearches::new(api);
    match args.command {
        SavedSearchCommand::List { limit } => {
            if limit.is_some_and(|limit| !(1..=1000).contains(&limit)) {
                return Err(AppError::usage("--limit must be between 1 and 1000"));
            }
            let searches = saved.list(limit).await?;
            let count = searches.len();
            Ok(json!({
                "saved_searches": searches,
                "count": count,
                "_observation": Observation::confirmed_present("saved_search_list", Some(200)),
            }))
        }
        SavedSearchCommand::Show { id } => {
            let search = saved.show(&id).await?;
            let mut value = serde_json::to_value(search).map_err(|error| {
                AppError::output("failed to serialize saved search").with_source(error)
            })?;
            value.as_object_mut().unwrap().insert(
                "_observation".to_owned(),
                json!(Observation::confirmed_present(
                    "saved_search_show",
                    Some(200)
                )),
            );
            Ok(value)
        }
        SavedSearchCommand::Create {
            name,
            email,
            push,
            notification_center,
            no_notifications,
            search,
        } => {
            if !no_notifications && !email && !push && !notification_center {
                return Err(AppError::usage(
                    "choose --email, --push, --notification-center, or --no-notifications",
                ));
            }
            let parameters = saved_search_parameters(search, search_api).await?;
            let notifications = notification_list(email, push, notification_center);
            mutation_value(
                saved
                    .create(CreateSavedSearch {
                        name,
                        notifications,
                        parameters,
                    })
                    .await?,
                true,
            )
        }
        SavedSearchCommand::Update {
            id,
            name,
            email,
            push,
            notification_center,
        } => {
            if name.is_none() && email.is_none() && push.is_none() && notification_center.is_none()
            {
                return Err(AppError::usage(
                    "provide --name or a notification channel state",
                ));
            }
            let current = saved.show(&id).await?;
            let mut notifications = current.notifications;
            apply_notification(&mut notifications, "EMAIL", email);
            apply_notification(&mut notifications, "PUSH", push);
            apply_notification(&mut notifications, "NC", notification_center);
            mutation_value(saved.update(&id, name, Some(notifications)).await?, true)
        }
        SavedSearchCommand::Delete { id } => {
            let deleted = saved.delete(&id).await?;
            let mut value = serde_json::to_value(deleted).map_err(|error| {
                AppError::output("failed to serialize saved search deletion").with_source(error)
            })?;
            value.as_object_mut().unwrap().insert(
                "_observation".to_owned(),
                json!(Observation::confirmed_absent(
                    "saved_search_show",
                    Some(200)
                )),
            );
            Ok(value)
        }
    }
}

fn notification_list(email: bool, push: bool, notification_center: bool) -> Vec<String> {
    [
        (email, "EMAIL"),
        (push, "PUSH"),
        (notification_center, "NC"),
    ]
    .into_iter()
    .filter(|(enabled, _)| *enabled)
    .map(|(_, value)| value.to_owned())
    .collect()
}

fn apply_notification(
    notifications: &mut Vec<String>,
    value: &str,
    state: Option<NotificationState>,
) {
    match state {
        Some(NotificationState::On) if !notifications.iter().any(|existing| existing == value) => {
            notifications.push(value.to_owned())
        }
        Some(NotificationState::Off) => notifications.retain(|existing| existing != value),
        _ => {}
    }
}

fn mutation_value(
    search: crate::api::saved_searches::SavedSearch,
    present: bool,
) -> Result<Value, AppError> {
    let mut value = serde_json::to_value(search).map_err(|error| {
        AppError::output("failed to serialize saved search mutation").with_source(error)
    })?;
    let observation = if present {
        Observation::confirmed_present("saved_search_show", Some(200))
    } else {
        Observation::confirmed_absent("saved_search_show", Some(200))
    };
    value
        .as_object_mut()
        .unwrap()
        .insert("_observation".to_owned(), json!(observation));
    Ok(value)
}
