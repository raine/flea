use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read},
    path::Path,
};

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    api::listings::{Listings, ListingsApi},
    cli::{
        draft::{ListingInputArgs, TradeType, parse_price},
        outcome::CommandOutcome,
    },
    domain::observation::Observation,
    error::AppError,
};

#[derive(Debug, Args)]
pub struct ListingArgs {
    #[command(subcommand)]
    pub command: ListingCommand,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ListingCommand {
    #[command(
        about = "List published listings",
        long_about = "Fetch all published listings across Tori result pages and return normalized summaries and facet totals."
    )]
    List,
    #[command(
        about = "Show a published listing",
        long_about = "Fetch a published listing with normalized values, state, statistics, and available actions."
    )]
    Show {
        /// Tori listing identifier.
        listing_id: String,
    },
    #[command(
        about = "Update a published listing",
        long_about = "Merge explicit fields into the latest published listing while preserving unspecified values."
    )]
    Update {
        /// Tori listing identifier.
        listing_id: String,
        #[command(flatten)]
        values: Box<ListingInputArgs>,
    },
    #[command(
        about = "Mark a listing as sold",
        long_about = "Mark a published listing as sold immediately without prompting for confirmation."
    )]
    Dispose {
        /// Tori listing identifier.
        listing_id: String,
    },
    #[command(
        about = "Delete a published listing",
        long_about = "Permanently delete a published listing immediately without prompting for confirmation."
    )]
    Delete {
        /// Tori listing identifier.
        listing_id: String,
    },
}

pub async fn dispatch_with_api(
    command: ListingArgs,
    api: &dyn ListingsApi,
) -> Result<CommandOutcome, AppError> {
    let listings = Listings::new(api);
    let (value, source) = match command.command {
        ListingCommand::List => (
            serde_json::to_value(listings.list().await?),
            "listing_collection",
        ),
        ListingCommand::Show { listing_id } => (
            serde_json::to_value(listings.show(&listing_id).await?),
            "listing_detail",
        ),
        ListingCommand::Update { listing_id, values } => {
            let changes = listing_changes(*values)?;
            (
                serde_json::to_value(listings.update(&listing_id, changes).await?),
                "listing_update_response",
            )
        }
        ListingCommand::Dispose { listing_id } => (
            serde_json::to_value(listings.dispose(&listing_id).await?),
            "listing_dispose_response",
        ),
        ListingCommand::Delete { listing_id } => {
            let deleted = listings.delete(&listing_id).await?;
            (
                Ok(json!({ "listing_id": deleted.listing_id, "deleted": true })),
                "listing_delete_response",
            )
        }
    };
    value
        .map(|value| {
            CommandOutcome::new(value)
                .with_observation(Observation::confirmed_present(source, None))
        })
        .map_err(|error| AppError::output(error.to_string()))
}

pub fn listing_changes(values: ListingInputArgs) -> Result<BTreeMap<String, Value>, AppError> {
    let mut changes = match values.input.as_deref() {
        Some(path) => read_input(path)?,
        None => BTreeMap::new(),
    };

    insert_flag(&mut changes, "category", values.category.map(Value::String))?;
    insert_flag(&mut changes, "title", values.title.map(Value::String))?;

    let description = match (values.description, values.description_file) {
        (Some(description), None) => Some(description),
        (None, Some(path)) => Some(fs::read_to_string(&path).map_err(|error| {
            AppError::usage(format!(
                "failed to read description file {}: {error}",
                path.display()
            ))
        })?),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(AppError::usage(
                "--description and --description-file cannot be combined",
            ));
        }
    };
    insert_flag(&mut changes, "description", description.map(Value::String))?;

    let price = values.price.map(|price| parse_price(&price)).transpose()?;
    insert_flag(&mut changes, "price", price)?;
    let trade_type = values.trade_type.map(|trade_type| {
        Value::String(
            match trade_type {
                TradeType::Sell => "sell",
                TradeType::GiveAway => "give_away",
                TradeType::Wanted => "wanted",
            }
            .to_owned(),
        )
    });
    insert_flag(&mut changes, "trade_type", trade_type)?;
    insert_flag(
        &mut changes,
        "postal_code",
        values.postal_code.map(Value::String),
    )?;
    insert_flag(
        &mut changes,
        "delivery",
        (!values.delivery.is_empty())
            .then(|| Value::Array(values.delivery.into_iter().map(Value::String).collect())),
    )?;
    insert_flag(
        &mut changes,
        "image",
        (!values.image.is_empty()).then(|| {
            Value::Array(
                values
                    .image
                    .into_iter()
                    .map(|path| Value::String(path.to_string_lossy().into_owned()))
                    .collect(),
            )
        }),
    )?;

    Ok(changes)
}

fn read_input(path: &Path) -> Result<BTreeMap<String, Value>, AppError> {
    let mut document = String::new();
    if path == Path::new("-") {
        io::stdin()
            .read_to_string(&mut document)
            .map_err(|error| AppError::usage(format!("failed to read JSON from stdin: {error}")))?;
    } else {
        document = fs::read_to_string(path).map_err(|error| {
            AppError::usage(format!(
                "failed to read input file {}: {error}",
                path.display()
            ))
        })?;
    }

    let value: Value = serde_json::from_str(&document)
        .map_err(|error| AppError::usage(format!("input must contain valid JSON: {error}")))?;
    let Value::Object(object) = value else {
        return Err(AppError::usage("input JSON must be an object"));
    };
    Ok(object.into_iter().collect())
}

fn insert_flag(
    changes: &mut BTreeMap<String, Value>,
    key: &str,
    value: Option<Value>,
) -> Result<(), AppError> {
    let Some(value) = value else {
        return Ok(());
    };
    if changes.contains_key(key) {
        return Err(AppError::usage(format!(
            "field `{key}` appears in both flags and JSON input"
        )));
    }
    changes.insert(key.to_owned(), value);
    Ok(())
}
