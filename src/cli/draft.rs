use std::{
    fmt,
    fs::File,
    io::{self, Read},
    path::{Path, PathBuf},
};

use clap::{Args, Subcommand, ValueEnum};
use serde::{
    Deserialize, Serialize,
    de::{MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value, json};

use crate::{
    api::{
        adinput::{AdInputApi, DraftWorkflow, WorkflowConfig, WorkflowError},
        listings::ListingsApi,
    },
    cli::draft_input,
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
    /// Category machine value returned by category discovery.
    #[arg(long)]
    pub category: Option<String>,
    /// Listing title.
    #[arg(long)]
    pub title: Option<String>,
    /// Listing description text.
    #[arg(long, conflicts_with = "description_file")]
    pub description: Option<String>,
    /// Read the listing description from a UTF-8 file.
    #[arg(long, value_name = "PATH", conflicts_with = "description")]
    pub description_file: Option<PathBuf>,
    /// Non-negative listing price.
    #[arg(long)]
    pub price: Option<String>,
    /// Seller intent for the listing.
    #[arg(long, value_enum)]
    pub trade_type: Option<TradeType>,
    /// Seller postal code.
    #[arg(long)]
    pub postal_code: Option<String>,
    /// Delivery machine value. Repeat for multiple values.
    #[arg(long, value_name = "VALUE")]
    pub delivery: Vec<String>,
    /// Image file path. Repeat to preserve image order.
    #[arg(long, value_name = "PATH")]
    pub image: Vec<PathBuf>,
    /// Read listing fields from a JSON object at this path, or `-` for stdin.
    #[arg(long, value_name = "PATH")]
    pub input: Option<PathBuf>,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum DraftCommand {
    #[command(
        about = "Create a remote draft",
        long_about = "Create a remote draft from explicit listing input or copy an existing listing into a fresh draft."
    )]
    Create {
        /// Listing ID to copy into a fresh draft for inspection.
        #[arg(long, conflicts_with_all = ["category", "title", "description", "description_file", "price", "trade_type", "postal_code", "delivery", "image", "input"])]
        from_listing: Option<String>,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    #[command(
        about = "Preview and validate draft input locally",
        long_about = "Normalize fields and preprocess images without creating a remote draft. Optionally verify category existence and selectability through the authenticated read-only taxonomy."
    )]
    Preview {
        /// Query the authenticated category taxonomy without creating a draft.
        #[arg(long)]
        verify_category: bool,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    #[command(
        about = "Show current remote draft state",
        long_about = "Fetch and normalize the latest draft values, field schema, image state, and available actions."
    )]
    Show {
        /// Tori draft identifier.
        draft_id: String,
    },
    #[command(
        about = "Update a remote draft",
        long_about = "Merge explicit fields into the latest remote draft while preserving unspecified values."
    )]
    Update {
        /// Tori draft identifier.
        draft_id: String,
        #[command(flatten)]
        values: ListingInputArgs,
    },
    #[command(
        about = "Manage draft images",
        long_about = "Add image files to a remote draft or remove attached images by their identifiers."
    )]
    Image(ImageArgs),
    #[command(
        about = "Publish a remote draft",
        long_about = "Validate the latest remote draft, wait for image processing, and publish it with the free Basic package."
    )]
    Publish {
        /// Tori draft identifier.
        draft_id: String,
    },
    #[command(
        about = "Delete a remote draft",
        long_about = "Permanently delete a remote draft immediately without prompting for confirmation."
    )]
    Delete {
        /// Tori draft identifier.
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
    #[command(
        about = "Add images to a draft",
        long_about = "Upload and attach one or more image files to a remote draft in argument order."
    )]
    Add {
        /// Tori draft identifier.
        draft_id: String,
        /// Image file paths in the desired display order.
        #[arg(required = true)]
        paths: Vec<PathBuf>,
    },
    #[command(
        about = "Remove images from a draft",
        long_about = "Remove one or more attached images from a remote draft by image identifier."
    )]
    Remove {
        /// Tori draft identifier.
        draft_id: String,
        /// Image identifiers returned by draft inspection.
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
        (None, Some(path)) => Some(Value::String(read_bounded_text(&path)?)),
        (None, None) => None,
        (Some(_), Some(_)) => {
            return Err(input_error(
                "--description and --description-file cannot be used together",
            ));
        }
    };
    insert_flag(&mut values, "description", description)?;
    insert_flag(
        &mut values,
        "price",
        args.price.map(|price| parse_price(&price)).transpose()?,
    )?;
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

fn read_bounded_text(path: &Path) -> Result<String, AppError> {
    const MAX_DESCRIPTION_FILE_BYTES: u64 = 64 * 1024;

    let mut text = String::new();
    File::open(path)
        .and_then(|file| {
            file.take(MAX_DESCRIPTION_FILE_BYTES + 1)
                .read_to_string(&mut text)
        })
        .map_err(|error| input_error(format!("failed to read {}: {error}", path.display())))?;
    if text.len() as u64 > MAX_DESCRIPTION_FILE_BYTES {
        return Err(input_error("description file exceeds 64 KiB"));
    }
    Ok(text)
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object fields")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueValue)
            .ok_or_else(|| E::custom("JSON number is not finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(serde::de::Error::custom(format!(
                    "duplicate JSON field `{key}`"
                )));
            }
            let value = object.next_value::<UniqueValue>()?;
            values.insert(key, value.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

fn read_json_object(path: &Path, stdin: &mut impl Read) -> Result<Map<String, Value>, AppError> {
    const MAX_INPUT_BYTES: u64 = 1024 * 1024;

    let mut source = String::new();
    if path == Path::new("-") {
        stdin
            .take(MAX_INPUT_BYTES + 1)
            .read_to_string(&mut source)
            .map_err(|error| input_error(format!("failed to read JSON from stdin: {error}")))?;
    } else {
        File::open(path)
            .and_then(|file| file.take(MAX_INPUT_BYTES + 1).read_to_string(&mut source))
            .map_err(|error| input_error(format!("failed to read {}: {error}", path.display())))?;
    }
    if source.len() as u64 > MAX_INPUT_BYTES {
        return Err(input_error("draft JSON input exceeds 1 MiB"));
    }
    match serde_json::from_str::<UniqueValue>(&source).map(|value| value.0) {
        Ok(Value::Object(values)) => Ok(values),
        Ok(_) => Err(input_error("--input must contain a JSON object")),
        Err(error) => Err(input_error(format!("invalid JSON input: {error}"))),
    }
}

fn take_json_images(values: &mut Map<String, Value>) -> Result<Vec<PathBuf>, AppError> {
    let image = values.remove("image");
    let images = values.remove("images");
    if image.is_some() && images.is_some() {
        return Err(duplicate_field("image"));
    }
    let Some(images) = image.or(images) else {
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

pub fn parse_price(input: &str) -> Result<Value, AppError> {
    let value: Value = serde_json::from_str(input)
        .map_err(|_| input_error("--price must be a non-negative number"))?;
    if value
        .as_f64()
        .is_none_or(|price| !price.is_finite() || price < 0.0)
    {
        return Err(input_error("--price must be a non-negative number"));
    }
    Ok(value)
}

fn duplicate_field(field: &str) -> AppError {
    let mut error = input_error(format!(
        "field `{field}` appears in both command flags and JSON input"
    ));
    error.details = Some(Box::new(json!({ "duplicate_field": field })));
    error
}

fn input_error(message: impl Into<String>) -> AppError {
    let mut error = AppError::usage(message);
    error.code = "cli.invalid_input".to_owned();
    error
}

pub fn execute_preview(
    command: DraftCommand,
    taxonomy: Option<&dyn ListingsApi>,
) -> Result<Value, AppError> {
    let DraftCommand::Preview {
        verify_category,
        values,
    } = command
    else {
        return Err(AppError::unexpected("expected a draft preview command"));
    };
    draft_input::preview(collect_input(values)?, verify_category, taxonomy)
}

pub async fn execute<A: AdInputApi>(
    command: DraftCommand,
    api: A,
    config: WorkflowConfig,
) -> Result<Value, AppError> {
    if matches!(&command, DraftCommand::Preview { .. }) {
        return execute_preview(command, None);
    }
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
            let input = draft_input::normalize(collect_input(values)?, true)?;
            serde_json::to_value(
                workflow
                    .create_prepared(input.values, input.images)
                    .await
                    .map_err(workflow_error)?,
            )
            .map_err(|error| AppError::output(error.to_string()))
        }
        DraftCommand::Preview { .. } => {
            unreachable!("preview returns before authenticated workflow construction")
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
            let input = draft_input::normalize(input, false)?;
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
    let has_remote_mutation = error.recovery.as_ref().is_some_and(|recovery| {
        recovery.completed_steps.iter().any(|step| {
            step == "create_draft"
                || step.starts_with("upload_image:")
                || matches!(
                    step.as_str(),
                    "attach_images"
                        | "patch_item_fields"
                        | "submit_adinput"
                        | "apply_delivery"
                        | "publish_basic"
                )
        })
    });
    let exit_class = match error.code.as_str() {
        "draft.conflict" => ExitClass::Conflict,
        "draft.validation_failed" | "draft.invalid_image" => ExitClass::Validation,
        "draft.image_processing" | "mutation.uncertain" => ExitClass::Partial,
        _ if has_remote_mutation => ExitClass::Partial,
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
        .or_else(|| {
            error
                .source
                .and_then(|source| source.details.map(|details| *details))
        })
        .map(Box::new);
    app.partial = partial.map(Box::new);
    app.next_actions = next_actions;
    app
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
        assert_eq!(
            error.details.as_deref(),
            Some(&json!({ "duplicate_field": "title" }))
        );
    }

    #[test]
    fn every_common_flag_rejects_the_same_json_field() {
        let cases = [
            ("category", json!("furniture/chairs")),
            ("title", json!("Chair")),
            ("description", json!("Description")),
            ("price", json!(45)),
            ("trade_type", json!("sell")),
            ("postal_code", json!("00100")),
            ("delivery", json!(["pickup"])),
        ];

        for (field, value) in cases {
            let mut args = empty_args();
            match field {
                "category" => args.category = Some("furniture/chairs".to_owned()),
                "title" => args.title = Some("Chair".to_owned()),
                "description" => args.description = Some("Description".to_owned()),
                "price" => args.price = Some("45".to_owned()),
                "trade_type" => args.trade_type = Some(TradeType::Sell),
                "postal_code" => args.postal_code = Some("00100".to_owned()),
                "delivery" => args.delivery = vec!["pickup".to_owned()],
                _ => unreachable!(),
            }
            let mut input = Cursor::new(serde_json::to_vec(&json!({ (field): value })).unwrap());

            let error = collect_input_with_reader(args, &mut input).unwrap_err();
            assert_eq!(
                error.details.as_deref(),
                Some(&json!({ "duplicate_field": field }))
            );
        }
    }

    #[test]
    fn json_input_requires_an_object_and_string_image_paths() {
        for document in [r#"["not", "an", "object"]"#, r#"{"image":[42]}"#] {
            let error =
                collect_input_with_reader(empty_args(), &mut Cursor::new(document.as_bytes()))
                    .unwrap_err();
            assert_eq!(error.code, "cli.invalid_input");
            assert_eq!(error.exit_class, ExitClass::Usage);
        }
    }

    #[test]
    fn duplicate_image_sources_are_rejected() {
        let mut args = empty_args();
        args.image.push(PathBuf::from("flag.jpg"));
        let mut input = Cursor::new(br#"{"image":["json.jpg"]}"#);

        let error = collect_input_with_reader(args, &mut input).unwrap_err();

        assert_eq!(
            error.details.as_deref(),
            Some(&json!({ "duplicate_field": "image" }))
        );
    }

    #[test]
    fn duplicate_json_fields_are_rejected_at_every_object_depth() {
        for document in [
            r#"{"title":"one","title":"two"}"#,
            r#"{"attributes":{"condition":"good","condition":"poor"}}"#,
        ] {
            let error =
                collect_input_with_reader(empty_args(), &mut Cursor::new(document.as_bytes()))
                    .unwrap_err();

            assert_eq!(error.code, "cli.invalid_input");
            assert!(error.message.contains("duplicate JSON field"));
        }
    }

    #[test]
    fn singular_and_plural_json_image_fields_conflict() {
        let error = collect_input_with_reader(
            empty_args(),
            &mut Cursor::new(br#"{"image":"one.jpg","images":["two.jpg"]}"#),
        )
        .unwrap_err();

        assert_eq!(
            error.details.as_deref(),
            Some(&json!({ "duplicate_field": "image" }))
        );
    }
}
