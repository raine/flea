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
    api::listings::ListingsApi,
    cli::draft_input,
    domain::{
        envelope::NextAction,
        field::{Field, FieldStatus},
        observation::Observation,
    },
    error::{AppError, ExitClass},
    marketplace::tori::adinput::{
        AdInputApi, CategoryPrediction, CategoryValidation, DeliveryOption, DraftDelivery,
        DraftImage, DraftState, DraftWorkflow, FieldOption, PublicationRequirement,
        PublicationValidation, ValidationEvidenceFailure, WorkflowConfig, WorkflowError,
        completed_steps_have_mutation,
    },
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
    /// Condition machine value returned by draft field discovery.
    #[arg(long, value_name = "VALUE")]
    pub condition: Option<String>,
    /// Delivery machine value returned by draft inspection.
    #[arg(long, value_name = "VALUE")]
    pub delivery: Vec<String>,
    /// JPEG, PNG, HEIC, or HEIF path. Images are bounded and stripped of private metadata.
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
        long_about = "Validate explicit listing input before allocation, then create a remote draft or copy a listing from the authenticated seller's listing collection into a fresh draft. Public listings owned by another seller are not copyable. If allocation succeeds but later work fails, continue with the returned draft ID instead of repeating creation."
    )]
    Create {
        /// Authenticated seller listing ID to copy into a fresh draft.
        #[arg(long, conflicts_with_all = ["category", "title", "description", "description_file", "price", "trade_type", "postal_code", "condition", "delivery", "image", "input"])]
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
        /// Include the complete normalized field schema.
        #[arg(long)]
        include_fields: bool,
        /// Include all option rows, or only rows for one field.
        #[arg(long, value_name = "FIELD", num_args = 0..=1, default_missing_value = "*")]
        include_options: Option<String>,
    },
    #[command(
        about = "Update a remote draft",
        long_about = "Apply explicit fields to the latest remote draft in deterministic atomic field groups while preserving unspecified values. An uncertain mutation is inspected but never replayed automatically."
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
        about = "Validate publication readiness",
        long_about = "Check the latest remote draft, listing-composer requirements, category, delivery, and images without changing remote state."
    )]
    Validate {
        /// Tori draft identifier.
        draft_id: String,
    },
    #[command(
        about = "Publish a remote draft",
        long_about = "Validate the latest remote draft and publish it with the free Basic package only when its authoritative revision matches the inspected revision."
    )]
    Publish {
        /// Tori draft identifier.
        draft_id: String,
        /// Authoritative revision returned by draft show or draft validate.
        #[arg(long, value_name = "VALUE")]
        if_revision: String,
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

#[derive(Debug, Serialize)]
struct DraftInspectionOutput {
    draft_id: String,
    etag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    values: Map<String, Value>,
    ready: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    missing: Vec<PublicationRequirement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    invalid: Vec<PublicationRequirement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pending: Vec<PublicationRequirement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    indeterminate: Vec<PublicationRequirement>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    evidence_failures: Vec<ValidationEvidenceFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<CategoryValidation>,
    images: Vec<DraftImage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    delivery: Option<SelectedDelivery>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cleared_fields: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    category_predictions: Vec<CategoryPrediction>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    option_sets: Vec<OptionSetSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    fields: Option<Vec<Field>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    options: Option<Vec<FieldOption>>,
    #[serde(rename = "_next_actions", skip_serializing_if = "Vec::is_empty")]
    next_actions: Vec<NextAction>,
}

#[derive(Debug, Serialize)]
struct SelectedDelivery {
    available: bool,
    selected: Vec<String>,
    selected_options: Vec<DeliveryOption>,
    option_count: usize,
    options_returned: usize,
    options_truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    unavailable_reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct OptionSetSummary {
    field: String,
    option_count: usize,
    options_returned: usize,
    options_truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    allowed_values: Vec<FieldOption>,
    #[serde(skip_serializing_if = "Option::is_none")]
    discovery_command: Option<String>,
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
        long_about = "Privately normalize, bound, upload, and attach JPEG, PNG, HEIC, or HEIF files to a remote draft in argument order."
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
    if let Some(price) = values.get("price") {
        validate_price_value(price, "input field `price` must be a non-negative number")?;
    }

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
    insert_attribute_flag(&mut values, "condition", args.condition.map(Value::String))?;
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

fn insert_attribute_flag(
    values: &mut Map<String, Value>,
    field: &'static str,
    value: Option<Value>,
) -> Result<(), AppError> {
    let Some(value) = value else {
        return Ok(());
    };
    let attributes = values
        .entry("attributes".to_owned())
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| input_error("input field `attributes` must be an object"))?;
    if attributes.contains_key(field) {
        return Err(duplicate_field(field));
    }
    attributes.insert(field.to_owned(), value);
    Ok(())
}

pub fn parse_price(input: &str) -> Result<Value, AppError> {
    let value: Value = serde_json::from_str(input)
        .map_err(|_| input_error("--price must be a non-negative number"))?;
    validate_price_value(&value, "--price must be a non-negative number")?;
    Ok(value)
}

fn validate_price_value(value: &Value, message: &'static str) -> Result<(), AppError> {
    if !value.is_number()
        || value
            .as_f64()
            .is_none_or(|price| !price.is_finite() || price < 0.0)
    {
        return Err(input_error(message));
    }
    Ok(())
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

fn draft_inspection(
    state: DraftState,
    validation: PublicationValidation,
    include_fields: bool,
    include_options: Option<&str>,
) -> Result<DraftInspectionOutput, AppError> {
    const INLINE_ALLOWED_VALUES: usize = 12;

    if let Some(field) = include_options.filter(|field| *field != "*")
        && !state.fields.iter().any(|candidate| candidate.key == field)
    {
        return Err(input_error(format!(
            "--include-options field `{field}` is absent from the draft schema"
        )));
    }

    let mut option_sets = state
        .fields
        .iter()
        .filter(|field| field.option_count > 0)
        .map(|field| {
            let relevant = matches!(field.status, FieldStatus::Missing | FieldStatus::Invalid);
            let small = field.option_count <= INLINE_ALLOWED_VALUES && !field.options_truncated;
            let allowed_values = if relevant && small {
                state
                    .options
                    .iter()
                    .filter(|option| option.field == field.key)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            OptionSetSummary {
                field: field.key.clone(),
                option_count: field.option_count,
                options_returned: field.options_returned,
                options_truncated: field.options_truncated,
                allowed_values,
                discovery_command: (relevant && !small).then(|| {
                    if field.key == "category" {
                        "flea tori category search QUERY".to_owned()
                    } else {
                        format!(
                            "flea tori draft show {} --include-options {}",
                            state.draft_id, field.key
                        )
                    }
                }),
            }
        })
        .collect::<Vec<_>>();
    option_sets.sort_by(|left, right| left.field.cmp(&right.field));

    let delivery = state.delivery.as_ref().map(selected_delivery);
    let options = include_options.map(|field| {
        state
            .options
            .iter()
            .filter(|option| field == "*" || option.field == field)
            .cloned()
            .collect::<Vec<_>>()
    });
    let mut commands = validation
        .missing
        .iter()
        .chain(&validation.invalid)
        .chain(&validation.pending)
        .chain(&validation.unverifiable)
        .map(|requirement| requirement.command.clone())
        .chain(
            validation
                .evidence_failures
                .iter()
                .map(|failure| failure.command.clone()),
        )
        .collect::<std::collections::BTreeSet<_>>();
    if validation.ready {
        commands.insert(format!(
            "flea tori draft publish {} --if-revision {}",
            state.draft_id, validation.revision
        ));
    }

    Ok(DraftInspectionOutput {
        draft_id: state.draft_id,
        etag: state.etag,
        revision: state.revision,
        values: state.values,
        ready: validation.ready,
        missing: validation.missing,
        invalid: validation.invalid,
        pending: validation.pending,
        indeterminate: validation.unverifiable,
        evidence_failures: validation.evidence_failures,
        category: validation.category_validation,
        images: state.images,
        delivery,
        cleared_fields: state.cleared_fields,
        category_predictions: state.predictions,
        option_sets,
        fields: include_fields.then_some(state.fields),
        options,
        next_actions: commands
            .into_iter()
            .map(|command| NextAction { command })
            .collect(),
    })
}

fn selected_delivery(delivery: &DraftDelivery) -> SelectedDelivery {
    let selected_options = delivery
        .options
        .iter()
        .filter(|option| delivery.selected.contains(&option.value))
        .cloned()
        .collect();
    SelectedDelivery {
        available: delivery.available,
        selected: delivery.selected.clone(),
        selected_options,
        option_count: delivery.option_count,
        options_returned: delivery.options_returned,
        options_truncated: delivery.options_truncated,
        unavailable_reason: delivery.unavailable_reason.clone(),
    }
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
    draft_input::preview(collect_input(values)?, verify_category, taxonomy).map(|mut value| {
        crate::domain::commerce::normalize_values_output(&mut value);
        value
    })
}

pub async fn execute<A: AdInputApi>(
    command: DraftCommand,
    api: A,
    config: WorkflowConfig,
) -> Result<Value, AppError> {
    if matches!(&command, DraftCommand::Preview { .. }) {
        return execute_preview(command, None);
    }
    let confirms_absence = matches!(&command, DraftCommand::Delete { .. });
    let observation_source = match &command {
        DraftCommand::Show { .. } => "draft_detail",
        DraftCommand::Validate { .. } => "draft_validation",
        DraftCommand::Publish { .. } => "listing_detail",
        DraftCommand::Image(_) => "draft_images",
        DraftCommand::Create { .. } => "draft_creation_response",
        DraftCommand::Update { .. } => "draft_update_response",
        DraftCommand::Delete { .. } => "draft_delete_response",
        DraftCommand::Preview { .. } => unreachable!(),
    };
    let workflow = DraftWorkflow::new(api, config);
    let result = match command {
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
        DraftCommand::Show {
            draft_id,
            include_fields,
            include_options,
        } => {
            let (state, validation) = workflow
                .inspect(&draft_id, include_options.is_some())
                .await
                .map_err(workflow_error)?;
            serde_json::to_value(draft_inspection(
                state,
                validation,
                include_fields,
                include_options.as_deref(),
            )?)
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
        DraftCommand::Validate { draft_id } => {
            serde_json::to_value(workflow.validate(&draft_id).await.map_err(workflow_error)?)
                .map_err(|error| AppError::output(error.to_string()))
        }
        DraftCommand::Publish {
            draft_id,
            if_revision,
        } => serde_json::to_value(
            workflow
                .publish(&draft_id, &if_revision)
                .await
                .map_err(workflow_error)?,
        )
        .map_err(|error| AppError::output(error.to_string())),
        DraftCommand::Delete { draft_id } => {
            workflow.delete(&draft_id).await.map_err(workflow_error)?;
            Ok(json!({ "draft_id": draft_id, "deleted": true }))
        }
    };
    result.map(|mut value| {
        crate::domain::commerce::normalize_values_output(&mut value);
        normalize_observed_listing_output(&mut value);
        if let Some(object) = value.as_object_mut() {
            let reconciled = object
                .get("warnings")
                .and_then(Value::as_array)
                .is_some_and(|warnings| {
                    warnings.iter().any(|warning| {
                        warning.as_str().is_some_and(|warning| {
                            warning.contains("authoritative observation confirmed persisted state")
                        })
                    })
                });
            let observation_source = if reconciled
                && matches!(
                    observation_source,
                    "draft_creation_response" | "draft_update_response"
                ) {
                "draft_detail"
            } else {
                observation_source
            };
            object.insert(
                "_observation".to_owned(),
                serde_json::to_value(if confirms_absence {
                    Observation::confirmed_absent(observation_source, None)
                } else {
                    Observation::confirmed_present(observation_source, None)
                })
                .expect("observation is serializable"),
            );
        }
        value
    })
}

fn normalize_observed_listing_output(value: &mut Value) {
    let Some(observed) = value
        .as_object_mut()
        .and_then(|object| object.get_mut("observed_listing"))
        .and_then(Value::as_object_mut)
    else {
        return;
    };
    let fields = observed
        .get("fields")
        .or_else(|| observed.get("values"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_else(|| observed.clone());
    let (trade_type, price) = crate::domain::commerce::normalize_commerce_fields(&fields);
    observed.insert(
        "trade_type".to_owned(),
        serde_json::to_value(trade_type).expect("trade type serializes"),
    );
    observed.insert(
        "price".to_owned(),
        serde_json::to_value(price).expect("price serializes"),
    );
    if let Some(Value::Object(fields)) = observed.get_mut("fields") {
        for key in [
            "price",
            "price_amount",
            "priceAmount",
            "currency",
            "currencyCode",
            "currency_code",
            "trade_type",
            "tradeType",
            "adViewTypeLabel",
            "subtitle",
        ] {
            fields.remove(key);
        }
    }
}

fn workflow_error(error: WorkflowError) -> AppError {
    let mutation_was_attempted = error.source.as_ref().is_some_and(|source| {
        source
            .details
            .as_deref()
            .and_then(|details| details.get("stage"))
            .and_then(Value::as_str)
            == Some("apply_price")
    });
    let has_remote_mutation = error
        .recovery
        .as_ref()
        .is_some_and(|recovery| completed_steps_have_mutation(&recovery.completed_steps));
    let exit_class = match error.code.as_str() {
        "draft.conflict" if has_remote_mutation => ExitClass::Partial,
        "draft.conflict" | "draft.revision_conflict" => ExitClass::Conflict,
        "draft.validation_failed" if has_remote_mutation => ExitClass::Partial,
        "draft.validation_failed"
        | "listing.not_copyable"
        | "draft.invalid_delivery"
        | "draft.delivery_options_unavailable"
        | "draft.invalid_image"
        | "draft.invalid_price"
        | "draft.price_trade_type_conflict" => ExitClass::Validation,
        "draft.image_processing" | "mutation.uncertain" => ExitClass::Partial,
        _ if has_remote_mutation || mutation_was_attempted => ExitClass::Partial,
        code if code.starts_with("draft.image_") || code.starts_with("draft.heic_") => {
            ExitClass::Validation
        }
        _ => ExitClass::Upstream,
    };
    let classification = error
        .recovery
        .as_ref()
        .map(|recovery| (recovery.upstream_transient, recovery.safe_to_retry))
        .or_else(|| {
            error
                .source
                .as_ref()
                .map(|source| (source.upstream_transient, source.safe_to_retry))
        })
        .unwrap_or((false, false));
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
    app.upstream_transient = classification.0;
    app.safe_to_retry = classification.1;
    app.observation = error
        .source
        .as_ref()
        .and_then(|source| source.observation.clone())
        .or_else(|| {
            error.recovery.as_ref().map(|recovery| Observation {
                state: recovery.observation.state,
                source: recovery.observation.source.clone(),
                observed_at: recovery
                    .observation
                    .observed_at
                    .clone()
                    .unwrap_or_else(crate::domain::observation::observation_timestamp),
                status_evidence: recovery.observation.status_evidence.clone(),
            })
        });
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
            condition: None,
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
    fn condition_flag_uses_the_optional_attribute_namespace() {
        let mut args = empty_args();
        args.condition = Some("2".to_owned());

        let collected = collect_input_with_reader(args, &mut Cursor::new(b"{}")).unwrap();

        assert_eq!(collected.values["attributes"]["condition"], "2");
    }

    #[test]
    fn condition_flag_conflicts_with_json_attribute() {
        let mut args = empty_args();
        args.condition = Some("2".to_owned());

        let error = collect_input_with_reader(
            args,
            &mut Cursor::new(br#"{"attributes":{"condition":"3"}}"#),
        )
        .unwrap_err();

        assert_eq!(
            error.details.as_deref(),
            Some(&json!({ "duplicate_field": "condition" }))
        );
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
    fn prices_accept_integer_and_decimal_numbers() {
        assert_eq!(parse_price("5").unwrap(), json!(5));
        assert_eq!(parse_price("5.25").unwrap(), json!(5.25));

        for document in [r#"{"price":5}"#, r#"{"price":5.25}"#] {
            let collected =
                collect_input_with_reader(empty_args(), &mut Cursor::new(document.as_bytes()))
                    .unwrap();
            assert!(collected.values["price"].is_number());
        }
    }

    #[test]
    fn malformed_and_negative_prices_are_rejected_before_dispatch() {
        for price in ["-1", "NaN", "Infinity", "1e400", "\"5\"", "{}", "[]"] {
            let error = parse_price(price).unwrap_err();
            assert_eq!(error.code, "cli.invalid_input");
            assert_eq!(error.exit_class, ExitClass::Usage);
        }

        for document in [
            r#"{"price":-1}"#,
            r#"{"price":"5"}"#,
            r#"{"price":{"price_amount":5}}"#,
            r#"{"price":null}"#,
        ] {
            let error =
                collect_input_with_reader(empty_args(), &mut Cursor::new(document.as_bytes()))
                    .unwrap_err();
            assert_eq!(error.code, "cli.invalid_input");
            assert_eq!(error.exit_class, ExitClass::Usage);
        }
    }

    #[test]
    fn trade_type_flags_preserve_semantic_input_values() {
        for (trade_type, expected) in [
            (TradeType::Sell, "sell"),
            (TradeType::GiveAway, "give_away"),
            (TradeType::Wanted, "wanted"),
        ] {
            let mut args = empty_args();
            args.trade_type = Some(trade_type);
            let collected = collect_input_with_reader(args, &mut Cursor::new(b"{}")).unwrap();
            assert_eq!(collected.values["trade_type"], expected);
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
