use std::{
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::{Map, Value, json};

use crate::{
    api::adinput::{AdInputApi, DraftWorkflow, WorkflowConfig, WorkflowError},
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
};

#[derive(Debug, Args)]
pub struct DraftArgs {
    #[command(subcommand)]
    pub command: DraftCommand,
}

#[derive(Clone, Debug, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum TradeType {
    Sell,
    GiveAway,
    Wanted,
}

impl TradeType {
    const fn machine_value(&self) -> &'static str {
        match self {
            Self::Sell => "sell",
            Self::GiveAway => "give_away",
            Self::Wanted => "wanted",
        }
    }
}

#[derive(Debug, Args, Serialize)]
pub struct ListingInputArgs {
    #[arg(long)]
    pub category: Option<String>,
    #[arg(long)]
    pub title: Option<String>,
    #[arg(long, conflicts_with = "description_file")]
    pub description: Option<String>,
    #[arg(long, value_name = "PATH", conflicts_with = "description")]
    pub description_file: Option<PathBuf>,
    #[arg(long)]
    pub price: Option<String>,
    #[arg(long, value_enum)]
    pub trade_type: Option<TradeType>,
    #[arg(long)]
    pub postal_code: Option<String>,
    #[arg(long, value_name = "VALUE")]
    pub delivery: Vec<String>,
    #[arg(long, value_name = "PATH")]
    pub image: Vec<PathBuf>,
    #[arg(long, value_name = "PATH")]
    pub input: Option<PathBuf>,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum DraftCommand {
    Create {
        #[arg(long, conflicts_with_all = ["category", "title", "description", "description_file", "price", "trade_type", "postal_code", "delivery", "image", "input"])]
        from_listing: Option<String>,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    Show {
        draft_id: String,
    },
    Update {
        draft_id: String,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    Image(ImageArgs),
    Publish {
        draft_id: String,
    },
    Delete {
        draft_id: String,
    },
}

#[derive(Debug, Args, Serialize)]
pub struct ImageArgs {
    #[command(subcommand)]
    pub command: ImageCommand,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum ImageCommand {
    Add {
        draft_id: String,
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    Remove {
        draft_id: String,
        #[arg(required = true)]
        image_ids: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct CollectedInput {
    pub values: Map<String, Value>,
    pub image_paths: Vec<PathBuf>,
}

pub fn collect_input(args: ListingInputArgs) -> Result<CollectedInput, AppError> {
    let mut stdin = io::stdin().lock();
    collect_input_with_reader(args, &mut stdin)
}

pub fn collect_input_with_reader(
    args: ListingInputArgs,
    stdin: &mut impl Read,
) -> Result<CollectedInput, AppError> {
    let mut values = match args.input.as_deref() {
        Some(path) => read_json_object(path, stdin)?,
        None => Map::new(),
    };
    let mut image_paths = take_json_images(&mut values)?;

    insert_flag(&mut values, "category", args.category.map(Value::String))?;
    insert_flag(&mut values, "title", args.title.map(Value::String))?;

    let description = match (args.description, args.description_file) {
        (Some(value), None) => Some(Value::String(value)),
        (None, Some(path)) => Some(Value::String(std::fs::read_to_string(&path).map_err(
            |error| input_error(format!("failed to read {}: {error}", path.display())),
        )?)),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(input_error(
                "--description and --description-file cannot be used together",
            ));
        }
    };
    insert_flag(&mut values, "description", description)?;
    insert_flag(&mut values, "price", args.price.map(Value::String))?;
    insert_flag(
        &mut values,
        "trade_type",
        args.trade_type
            .map(|value| Value::String(value.machine_value().to_owned())),
    )?;
    insert_flag(
        &mut values,
        "postal_code",
        args.postal_code.map(Value::String),
    )?;
    insert_flag(
        &mut values,
        "delivery",
        (!args.delivery.is_empty())
            .then(|| Value::Array(args.delivery.into_iter().map(Value::String).collect())),
    )?;

    if !args.image.is_empty() && !image_paths.is_empty() {
        return Err(duplicate_field("image"));
    }
    if !args.image.is_empty() {
        image_paths = args.image;
    }

    Ok(CollectedInput {
        values,
        image_paths,
    })
}

fn read_json_object(path: &Path, stdin: &mut impl Read) -> Result<Map<String, Value>, AppError> {
    let mut source = String::new();
    if path == Path::new("-") {
        stdin
            .read_to_string(&mut source)
            .map_err(|error| input_error(format!("failed to read JSON from stdin: {error}")))?;
    } else {
        File::open(path)
            .and_then(|mut file| file.read_to_string(&mut source))
            .map_err(|error| input_error(format!("failed to read {}: {error}", path.display())))?;
    }
    match serde_json::from_str(&source) {
        Ok(Value::Object(values)) => Ok(values),
        Ok(_) => Err(input_error("--input must contain a JSON object")),
        Err(error) => Err(input_error(format!("invalid JSON input: {error}"))),
    }
}

fn take_json_images(values: &mut Map<String, Value>) -> Result<Vec<PathBuf>, AppError> {
    let Some(images) = values.remove("image") else {
        return Ok(Vec::new());
    };
    let strings = match images {
        Value::String(path) => vec![path],
        Value::Array(paths) => paths
            .into_iter()
            .map(|path| {
                path.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| input_error("input field `image` must contain path strings"))
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => {
            return Err(input_error(
                "input field `image` must be a path string or array of path strings",
            ));
        }
    };
    Ok(strings.into_iter().map(PathBuf::from).collect())
}

fn insert_flag(
    values: &mut Map<String, Value>,
    field: &'static str,
    value: Option<Value>,
) -> Result<(), AppError> {
    if let Some(value) = value {
        if values.contains_key(field) {
            return Err(duplicate_field(field));
        }
        values.insert(field.to_owned(), value);
    }
    Ok(())
}

fn duplicate_field(field: &str) -> AppError {
    let mut error = input_error(format!(
        "field `{field}` appears in both command flags and JSON input"
    ));
    error.details = Some(json!({ "duplicate_field": field }));
    error
}

fn input_error(message: impl Into<String>) -> AppError {
    let mut error = AppError::usage(message);
    error.code = "cli.invalid_input".to_owned();
    error
}

pub async fn execute<A: AdInputApi>(
    command: DraftCommand,
    api: A,
    config: WorkflowConfig,
) -> Result<Value, AppError> {
    let workflow = DraftWorkflow::new(api, config);
    match command {
        DraftCommand::Create {
            from_listing: Some(listing_id),
            ..
        } => serde_json::to_value(
            workflow
                .create_from_listing(&listing_id)
                .await
                .map_err(workflow_error)?,
        )
        .map_err(|error| AppError::output(error.to_string())),
        DraftCommand::Create {
            from_listing: None,
            values,
        } => {
            let input = collect_input(values)?;
            serde_json::to_value(
                workflow
                    .create(input.values, &input.image_paths)
                    .await
                    .map_err(workflow_error)?,
            )
            .map_err(|error| AppError::output(error.to_string()))
        }
        DraftCommand::Show { draft_id } => {
            serde_json::to_value(workflow.show(&draft_id).await.map_err(workflow_error)?)
                .map_err(|error| AppError::output(error.to_string()))
        }
        DraftCommand::Update { draft_id, values } => {
            let input = collect_input(values)?;
            if !input.image_paths.is_empty() {
                return Err(input_error(
                    "draft update does not accept images; use `draft image add`",
                ));
            }
            serde_json::to_value(
                workflow
                    .update(&draft_id, &input.values)
                    .await
                    .map_err(workflow_error)?,
            )
            .map_err(|error| AppError::output(error.to_string()))
        }
        DraftCommand::Image(ImageArgs {
            command: ImageCommand::Add { draft_id, paths },
        }) => serde_json::to_value(
            workflow
                .add_images(&draft_id, &paths)
                .await
                .map_err(workflow_error)?,
        )
        .map_err(|error| AppError::output(error.to_string())),
        DraftCommand::Image(ImageArgs {
            command:
                ImageCommand::Remove {
                    draft_id,
                    image_ids,
                },
        }) => serde_json::to_value(
            workflow
                .remove_images(&draft_id, &image_ids)
                .await
                .map_err(workflow_error)?,
        )
        .map_err(|error| AppError::output(error.to_string())),
        DraftCommand::Publish { draft_id } => {
            serde_json::to_value(workflow.publish(&draft_id).await.map_err(workflow_error)?)
                .map_err(|error| AppError::output(error.to_string()))
        }
        DraftCommand::Delete { draft_id } => {
            workflow.delete(&draft_id).await.map_err(workflow_error)?;
            Ok(json!({ "draft_id": draft_id, "deleted": true }))
        }
    }
}

fn workflow_error(error: WorkflowError) -> AppError {
    let exit_class = match error.code.as_str() {
        "draft.conflict" => ExitClass::Conflict,
        "draft.validation_failed" => ExitClass::Validation,
        _ if error.recovery.is_some() => ExitClass::Partial,
        _ => ExitClass::Upstream,
    };
    let retryable = error
        .recovery
        .as_ref()
        .map(|recovery| recovery.retryable)
        .or_else(|| error.source.as_ref().map(|source| source.retryable))
        .unwrap_or(false);
    let next_actions = error
        .recovery
        .as_ref()
        .map(|recovery| {
            recovery
                .next_safe_actions
                .iter()
                .cloned()
                .map(|command| NextAction { command })
                .collect()
        })
        .unwrap_or_default();
    let partial = error
        .recovery
        .as_ref()
        .and_then(|recovery| serde_json::to_value(recovery).ok());
    let mut app = AppError::new(error.code, error.message, exit_class);
    app.retryable = retryable;
    app.details = error
        .details
        .or_else(|| error.source.and_then(|source| source.details));
    app.partial = partial;
    app.next_actions = next_actions;
    app
}

pub fn dispatch(command: DraftArgs) -> Result<Value, AppError> {
    let details = serde_json::to_value(command.command)
        .map_err(|error| AppError::output(error.to_string()))?;
    Err(AppError::protocol_unavailable("draft", details))
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn empty_args() -> ListingInputArgs {
        ListingInputArgs {
            category: None,
            title: None,
            description: None,
            description_file: None,
            price: None,
            trade_type: None,
            postal_code: None,
            delivery: Vec::new(),
            image: Vec::new(),
            input: Some(PathBuf::from("-")),
        }
    }

    #[test]
    fn collects_json_and_common_flags_without_precedence() {
        let mut args = empty_args();
        args.title = Some("Chair".to_owned());
        args.delivery = vec!["pickup".to_owned(), "shipping".to_owned()];
        let mut input = Cursor::new(br#"{"attributes":{"material":"10"}}"#);

        let collected = collect_input_with_reader(args, &mut input).unwrap();

        assert_eq!(collected.values["title"], "Chair");
        assert_eq!(collected.values["delivery"], json!(["pickup", "shipping"]));
        assert_eq!(collected.values["attributes"]["material"], "10");
    }

    #[test]
    fn duplicate_flag_and_json_field_is_a_usage_error() {
        let mut args = empty_args();
        args.title = Some("flag title".to_owned());
        let mut input = Cursor::new(br#"{"title":"JSON title"}"#);

        let error = collect_input_with_reader(args, &mut input).unwrap_err();

        assert_eq!(error.exit_class, ExitClass::Usage);
        assert_eq!(error.code, "cli.invalid_input");
        assert_eq!(error.details, Some(json!({ "duplicate_field": "title" })));
    }

    #[test]
    fn duplicate_image_sources_are_rejected() {
        let mut args = empty_args();
        args.image.push(PathBuf::from("flag.jpg"));
        let mut input = Cursor::new(br#"{"image":["json.jpg"]}"#);

        let error = collect_input_with_reader(args, &mut input).unwrap_err();

        assert_eq!(error.details, Some(json!({ "duplicate_field": "image" })));
    }
}
