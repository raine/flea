use clap::{Args, Subcommand, ValueEnum};

use crate::{
    cli::outcome::{CommandData, CommandOutcome, SavedSearchListOutput},
    error::AppError,
    marketplace::tori::{
        saved_searches::{
            NotificationSelection, NotificationState as ToriNotificationState, SavedSearchRequest,
            SavedSearchResult, SavedSearches, SavedSearchesApi,
        },
        search::PublicSearchApi,
    },
};

use super::search::{SearchArgs, request_from_args};

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
        search: Box<SearchArgs>,
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

impl SavedSearchCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::List { .. } => "saved-search list",
            Self::Show { .. } => "saved-search show",
            Self::Create { .. } => "saved-search create",
            Self::Update { .. } => "saved-search update",
            Self::Delete { .. } => "saved-search delete",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum NotificationState {
    On,
    Off,
}

pub async fn dispatch(
    args: SavedSearchArgs,
    api: &dyn SavedSearchesApi,
    search_api: &dyn PublicSearchApi,
) -> Result<CommandOutcome, AppError> {
    let request = match args.command {
        SavedSearchCommand::List { limit } => SavedSearchRequest::List { limit },
        SavedSearchCommand::Show { id } => SavedSearchRequest::Show { id },
        SavedSearchCommand::Create {
            name,
            email,
            push,
            notification_center,
            no_notifications,
            search,
        } => {
            let notifications =
                NotificationSelection::new(email, push, notification_center, no_notifications)?;
            SavedSearchRequest::Create {
                name,
                notifications,
                search: Box::new(request_from_args(*search)?),
            }
        }
        SavedSearchCommand::Update {
            id,
            name,
            email,
            push,
            notification_center,
        } => SavedSearchRequest::Update {
            id,
            name,
            email: email.map(Into::into),
            push: push.map(Into::into),
            notification_center: notification_center.map(Into::into),
        },
        SavedSearchCommand::Delete { id } => SavedSearchRequest::Delete { id },
    };
    let (data, observation) = match SavedSearches::new(api).execute(request, search_api).await? {
        SavedSearchResult::List {
            saved_searches,
            count,
            observation,
        } => (
            CommandData::SavedSearchList(SavedSearchListOutput {
                saved_searches,
                count,
            }),
            observation,
        ),
        SavedSearchResult::Search {
            saved_search,
            observation,
        } => (CommandData::SavedSearch(*saved_search), observation),
        SavedSearchResult::Deleted {
            deleted,
            observation,
        } => (CommandData::DeletedSavedSearch(deleted), observation),
    };
    Ok(CommandOutcome::new(data).with_observation(observation))
}

impl From<NotificationState> for ToriNotificationState {
    fn from(value: NotificationState) -> Self {
        match value {
            NotificationState::On => Self::On,
            NotificationState::Off => Self::Off,
        }
    }
}
