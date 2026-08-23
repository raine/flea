use super::{images::*, normalization::values_semantically_equal, recovery::*, validation::*, *};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum FieldMutationKind {
    Composer,
    Price,
    Delivery,
}

#[derive(Clone, Debug)]
pub(super) struct FieldMutation {
    pub(super) key: String,
    pub(super) value: Value,
    pub(super) step: String,
    pub(super) fields: Vec<String>,
    pub(super) kind: FieldMutationKind,
}

#[derive(Default)]
pub(super) struct FieldProgress {
    pub(super) persisted: Vec<String>,
    pub(super) absent: Vec<String>,
}

pub(super) struct AppliedFieldMutations {
    pub(super) draft: DraftState,
    pub(super) progress: FieldProgress,
    pub(super) warnings: Vec<String>,
}

pub(super) struct FieldBoundary<'a> {
    pub(super) step: &'a str,
    pub(super) fields: &'a [String],
}

pub(super) struct FieldOutcomes {
    pub(super) persisted: Vec<String>,
    pub(super) absent: Vec<String>,
    pub(super) indeterminate: Vec<String>,
    pub(super) unattempted: Vec<String>,
}

pub(super) fn requested_sale_price(values: &Map<String, Value>) -> Result<Option<Value>, ApiError> {
    let Some(price) = values.get("price") else {
        return Ok(None);
    };
    validate_price(price)?;
    let trade_type = values
        .get("trade_type")
        .and_then(|value| normalized_select_to_machine("trade_type", value));
    if trade_type.as_ref() != Some(&Value::String("1".to_owned())) {
        return Err(ApiError::new(
            "draft.price_trade_type_conflict",
            "Sale price requires the sale trade type",
        ));
    }
    Ok(Some(price.clone()))
}

pub(super) fn create_preflight_issues(values: &Map<String, Value>) -> Vec<ValidationIssue> {
    values
        .iter()
        .filter_map(|(field, value)| {
            let valid = match field.as_str() {
                "category" => {
                    value.as_str().is_some_and(|value| !value.trim().is_empty())
                        || value.as_u64().is_some()
                }
                "title" | "description" | "trade_type" | "postal_code" => value.is_string(),
                "attributes" => value.is_object(),
                "delivery" => delivery_values(value).is_some(),
                "price" => validate_price(value).is_ok(),
                _ => true,
            };
            (!valid).then(|| ValidationIssue {
                field: field.clone(),
                code: "invalid_type".to_owned(),
                message: "the value has an invalid shape for draft creation".to_owned(),
                source: Some("create_preflight".to_owned()),
                raw: None,
            })
        })
        .collect()
}

fn prices_equal(left: &Value, right: &Value) -> bool {
    match (left.as_f64(), right.as_f64()) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

pub(super) fn ordered_field_mutations(mut values: Map<String, Value>) -> Vec<FieldMutation> {
    let attributes = values
        .remove("attributes")
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default();
    let order = [
        "category",
        "title",
        "description",
        "trade_type",
        "price",
        "postal_code",
        "delivery",
    ];
    let mut values = values.into_iter().collect::<Vec<_>>();
    values.extend(attributes);
    values.sort_by(|(left, _), (right, _)| {
        let rank = |key: &str| {
            if key == "delivery" {
                usize::MAX
            } else {
                order
                    .iter()
                    .position(|candidate| *candidate == key)
                    .unwrap_or(order.len() - 1)
            }
        };
        rank(left).cmp(&rank(right)).then_with(|| left.cmp(right))
    });
    values
        .into_iter()
        .map(|(key, value)| {
            let value = if key == "category" {
                normalize_category(value)
            } else {
                value
            };
            let stage_key: String = key
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || character == '_' {
                        character.to_ascii_lowercase()
                    } else {
                        '_'
                    }
                })
                .collect();
            let fields = if order.contains(&key.as_str()) {
                vec![key.clone()]
            } else {
                vec![format!("attributes.{key}")]
            };
            let kind = match key.as_str() {
                "price" => FieldMutationKind::Price,
                "delivery" => FieldMutationKind::Delivery,
                _ => FieldMutationKind::Composer,
            };
            FieldMutation {
                step: format!("apply_{stage_key}"),
                fields,
                key,
                value,
                kind,
            }
        })
        .collect()
}

pub(super) fn pending_fields(
    mutations: &[FieldMutation],
    progress: &FieldProgress,
    active_fields: &[String],
) -> Vec<String> {
    let classified = progress
        .persisted
        .iter()
        .chain(&progress.absent)
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let active = active_fields
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    mutations
        .iter()
        .flat_map(|mutation| mutation.fields.iter())
        .filter(|field| !classified.contains(field.as_str()) && !active.contains(field.as_str()))
        .cloned()
        .collect()
}

pub(super) fn field_is_persisted(state: &DraftState, mutation: &FieldMutation) -> bool {
    if mutation.kind == FieldMutationKind::Delivery {
        let requested = delivery_values(&mutation.value).unwrap_or_default();
        return state
            .delivery
            .as_ref()
            .is_some_and(|delivery| delivery.selected == requested);
    }
    let observed = state.values.get(&mutation.key);
    if mutation.value.is_null() {
        return observed.is_none_or(Value::is_null);
    }
    let Some(observed) = observed else {
        return false;
    };
    match mutation.key.as_str() {
        "price" => prices_equal(observed, &mutation.value),
        "trade_type" => select_values_equal("trade_type", observed, &mutation.value),
        "category" => normalize_category(observed.clone()) == mutation.value,
        _ => observed == &mutation.value,
    }
}

pub(super) fn classify_fields(
    state: &DraftState,
    mutation: &FieldMutation,
) -> (Vec<String>, Vec<String>) {
    mutation
        .fields
        .iter()
        .cloned()
        .partition(|_| field_is_persisted(state, mutation))
}

pub(super) fn retry_field_action(draft_id: &str, fields: &[String]) -> String {
    let single_flag = match fields {
        [field] => match field.as_str() {
            "category" => Some("--category VALUE"),
            "title" => Some("--title VALUE"),
            "description" => Some("--description VALUE"),
            "price" => Some("--price VALUE"),
            "trade_type" => Some("--trade-type VALUE"),
            "postal_code" => Some("--postal-code VALUE"),
            "delivery" => Some("--delivery VALUE"),
            _ => None,
        },
        _ => None,
    };
    single_flag.map_or_else(
        || format!("flea tori draft update {draft_id} --input PATH_WITH_ONLY_ABSENT_FIELDS"),
        |flag| format!("flea tori draft update {draft_id} {flag}"),
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn field_recovery(
    draft_id: &str,
    completed_steps: &[String],
    boundary: FieldBoundary<'_>,
    outcomes: FieldOutcomes,
    upstream_transient: bool,
    safe_to_retry: bool,
    fresh_state: Option<DraftState>,
    force_inspection: bool,
) -> Recovery {
    let manual_inspection_required = force_inspection || !outcomes.indeterminate.is_empty();
    let next_safe_actions = if manual_inspection_required || outcomes.absent.is_empty() {
        vec![format!("flea tori draft show {draft_id}")]
    } else {
        vec![
            format!("flea tori draft show {draft_id}"),
            retry_field_action(draft_id, &outcomes.absent),
        ]
    };
    let mut recovery = Recovery {
        active_step: Some(boundary.step.to_owned()),
        failed_stage: Some(boundary.step.to_owned()),
        fields: boundary.fields.to_vec(),
        persisted_fields: outcomes.persisted,
        absent_fields: outcomes.absent,
        indeterminate_fields: outcomes.indeterminate,
        unattempted_fields: outcomes.unattempted,
        manual_inspection_required,
        upstream_transient,
        safe_to_retry,
        next_safe_actions,
        ..Recovery::base(draft_id, completed_steps, None)
    };
    if let Some(state) = fresh_state {
        recovery.observe(&state, ObservationStatus::Observed);
    }
    recovery.refresh_field_summary();
    recovery
}

pub(super) fn schema_validation_issues(
    state: &DraftState,
    mutation: &FieldMutation,
) -> Vec<ValidationIssue> {
    let input_field = mutation
        .fields
        .first()
        .map(String::as_str)
        .unwrap_or(&mutation.key);
    let optional = input_field.starts_with("attributes.");
    let Some(field) = state.fields.iter().find(|field| field.key == mutation.key) else {
        return optional
            .then(|| ValidationIssue {
                field: input_field.to_owned(),
                code: "absent_in_composer".to_owned(),
                message: format!(
                    "field is absent from this category's composer; inspect with `flea tori draft show {} --include-fields`",
                    state.draft_id
                ),
                source: Some("listing_composer".to_owned()),
                raw: None,
            })
            .into_iter()
            .collect();
    };
    if optional
        && (field.requirement != Requirement::Optional
            || matches!(field.field_type, FieldType::Unknown(_)))
    {
        return vec![ValidationIssue {
            field: input_field.to_owned(),
            code: "unsupported_by_cli".to_owned(),
            message: "composer field is not a supported optional input type".to_owned(),
            source: Some("listing_composer".to_owned()),
            raw: None,
        }];
    }
    schema_validation_issue(state, field, &mutation.value)
        .map(|mut issue| {
            issue.field = input_field.to_owned();
            issue
        })
        .into_iter()
        .collect()
}

pub(super) fn schema_validation_issue(
    state: &DraftState,
    field: &Field,
    value: &Value,
) -> Option<ValidationIssue> {
    if value.is_null() {
        return None;
    }
    let valid_shape = match field.field_type {
        FieldType::String | FieldType::Text | FieldType::Date => value.is_string(),
        FieldType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
        FieldType::Decimal => value.as_f64().is_some_and(f64::is_finite),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Select => !value.is_array() && !value.is_object(),
        FieldType::MultiSelect => value.is_array(),
        FieldType::Unknown(_) => true,
    };
    if !valid_shape {
        return Some(ValidationIssue {
            field: field.key.clone(),
            code: "invalid_type".to_owned(),
            message: format!("expected {}", field_type_name(&field.field_type)),
            source: Some("local_schema".to_owned()),
            raw: None,
        });
    }
    if matches!(field.field_type, FieldType::Select | FieldType::MultiSelect) {
        let allowed = if field.validation_options.is_empty() {
            state
                .options
                .iter()
                .filter(|option| option.field == field.key)
                .map(|option| &option.value)
                .collect::<Vec<_>>()
        } else {
            field.validation_options.iter().collect::<Vec<_>>()
        };
        let supplied = value
            .as_array()
            .map(|values| values.iter().collect::<Vec<_>>())
            .unwrap_or_else(|| vec![value]);
        if !allowed.is_empty()
            && supplied.iter().any(|value| {
                !allowed
                    .iter()
                    .any(|option| select_values_equal(&field.key, value, option))
            })
        {
            let options = state
                .options
                .iter()
                .filter(|option| option.field == field.key)
                .take(13)
                .collect::<Vec<_>>();
            let message = if !field.options_truncated && options.len() <= 12 {
                let allowed = options
                    .iter()
                    .map(|option| {
                        let value = option
                            .value
                            .as_str()
                            .map_or_else(|| option.value.to_string(), str::to_owned);
                        format!("{value} ({})", option.label)
                    })
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "value is not present in the source-backed field options; allowed values: {allowed}"
                )
            } else {
                format!(
                    "value is not present in the source-backed field options; inspect with `flea tori draft show {} --include-options {}`",
                    state.draft_id, field.key
                )
            };
            return Some(ValidationIssue {
                field: field.key.clone(),
                code: "invalid_option".to_owned(),
                message,
                source: Some("listing_composer".to_owned()),
                raw: None,
            });
        }
    }
    None
}

pub(super) fn category_validation_issues(
    state: &DraftState,
    mutation: &FieldMutation,
    categories: &[PublicationCategory],
) -> Vec<ValidationIssue> {
    let category_id = match &mutation.value {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    };
    let Some(category_id) = category_id else {
        return vec![ValidationIssue {
            field: "category".to_owned(),
            code: "invalid_type".to_owned(),
            message: "expected a category machine value".to_owned(),
            source: Some("category_taxonomy".to_owned()),
            raw: None,
        }];
    };

    let mut issues = Vec::new();
    let taxonomy_valid = match categories
        .iter()
        .find(|category| category.category_id == category_id)
    {
        None => {
            issues.push(ValidationIssue {
                field: "category".to_owned(),
                code: "category_not_found".to_owned(),
                message: "category is absent from the current taxonomy".to_owned(),
                source: Some("category_taxonomy".to_owned()),
                raw: None,
            });
            false
        }
        Some(category) if !category.selectable => {
            issues.push(ValidationIssue {
                field: "category".to_owned(),
                code: "category_not_selectable".to_owned(),
                message: "category cannot contain listings".to_owned(),
                source: Some("category_taxonomy".to_owned()),
                raw: None,
            });
            false
        }
        Some(_) => true,
    };

    if taxonomy_valid && let Some(field) = state.fields.iter().find(|field| field.key == "category")
    {
        let matching_presentation_option = state.options.iter().any(|option| {
            option.field == field.key && values_semantically_equal(&mutation.value, &option.value)
        });
        let issue = if field.status == FieldStatus::Invalid {
            Some((
                "category_incompatible",
                "category is incompatible with the current listing composer",
            ))
        } else if !field.validation_options.is_empty() {
            (!field
                .validation_options
                .iter()
                .any(|option| values_semantically_equal(&mutation.value, option)))
            .then_some((
                "category_incompatible",
                "category is incompatible with the current listing composer",
            ))
        } else if matching_presentation_option || field.option_count == 0 {
            None
        } else if field.options_truncated {
            Some((
                "category_compatibility_unverifiable",
                "complete category options are unavailable from the listing composer",
            ))
        } else {
            Some((
                "category_incompatible",
                "category is incompatible with the current listing composer",
            ))
        };
        if let Some((code, message)) = issue {
            issues.push(ValidationIssue {
                field: "category".to_owned(),
                code: code.to_owned(),
                message: message.to_owned(),
                source: Some("listing_composer".to_owned()),
                raw: None,
            });
        }
    }
    issues
}

fn field_type_name(field_type: &FieldType) -> &'static str {
    match field_type {
        FieldType::String | FieldType::Text => "a string",
        FieldType::Integer => "an integer",
        FieldType::Decimal => "a number",
        FieldType::Boolean => "a boolean",
        FieldType::Select => "one selectable value",
        FieldType::MultiSelect => "an array of selectable values",
        FieldType::Date => "a date string",
        FieldType::Unknown(_) => "a value accepted by the composer",
    }
}

pub(super) fn structured_validation_issues(
    error: &ApiError,
    state: &DraftState,
) -> Vec<ValidationIssue> {
    let Some(upstream) = error
        .details
        .as_deref()
        .and_then(|details| details.get("upstream"))
    else {
        return Vec::new();
    };
    let mut errors = Vec::new();
    collect_validation_errors(upstream, &mut errors);
    let mut issues = map_validation_errors(errors, &state.fields);
    for issue in &mut issues {
        issue.field = stable_field_key(&issue.field);
    }
    issues.sort_by(|left, right| {
        left.field
            .cmp(&right.field)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
    });
    issues.dedup_by(|left, right| {
        left.field == right.field && left.code == right.code && left.message == right.message
    });
    issues
}

fn collect_validation_errors(value: &Value, output: &mut Vec<UpstreamValidationError>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in [
        "errors",
        "field_errors",
        "fieldErrors",
        "validation_errors",
        "validationErrors",
        "invalid_params",
        "invalid-params",
        "violations",
        "validation",
        "fields",
    ] {
        let Some(errors) = object.get(key) else {
            continue;
        };
        match errors {
            Value::Array(errors) => {
                for error in errors {
                    collect_validation_error_item(error, None, output);
                }
            }
            Value::Object(errors) => {
                for (field, error) in errors {
                    match error {
                        Value::Array(errors) => {
                            for error in errors {
                                collect_validation_error_item(error, Some(field), output);
                            }
                        }
                        error => collect_validation_error_item(error, Some(field), output),
                    }
                }
            }
            _ => {}
        }
    }
    for key in ["error", "details"] {
        if let Some(nested) = object.get(key) {
            collect_validation_errors(nested, output);
        }
    }
}

fn collect_validation_error_item(
    value: &Value,
    fallback_field: Option<&str>,
    output: &mut Vec<UpstreamValidationError>,
) {
    if let Some(message) = value.as_str() {
        if let Some(field) = fallback_field {
            output.push(UpstreamValidationError {
                source: field.to_owned(),
                code: "invalid".to_owned(),
                message: message.to_owned(),
                raw: Some(value.clone()),
            });
        }
        return;
    }
    let Some(object) = value.as_object() else {
        return;
    };
    let source = [
        "field",
        "path",
        "name",
        "property",
        "parameter",
        "attribute",
        "key",
        "source",
    ]
    .into_iter()
    .find_map(|key| {
        object.get(key).and_then(|value| {
            value.as_str().or_else(|| {
                value.as_object().and_then(|source| {
                    ["pointer", "parameter", "field", "path"]
                        .into_iter()
                        .find_map(|key| source.get(key).and_then(Value::as_str))
                })
            })
        })
    })
    .or(fallback_field);
    let Some(source) = source else {
        return;
    };
    let message = ["message", "reason", "detail", "description"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .unwrap_or("Tori rejected the field");
    let code = ["code", "type", "kind"]
        .into_iter()
        .find_map(|key| object.get(key).and_then(Value::as_str))
        .unwrap_or("invalid");
    output.push(UpstreamValidationError {
        source: source.to_owned(),
        code: code.to_owned(),
        message: message.to_owned(),
        raw: Some(value.clone()),
    });
}

pub(super) fn mutation_is_ambiguous(error: &ApiError) -> bool {
    error.code == "mutation.uncertain"
        || error.status.is_none()
        || matches!(error.status, Some(408 | 425 | 500..=599))
}

pub(super) fn field_error_details(
    stage: &str,
    fields: &[String],
    error: &ApiError,
    validation: &[ValidationIssue],
    observation: Option<Value>,
) -> Value {
    let mut details = json!({
        "stage": stage,
        "fields": fields,
        "status": error.status,
        "content_type": error.details.as_deref().and_then(|details| details.get("content_type")),
        "body_is_unparseable": error.details.as_deref().and_then(|details| details.get("body_is_unparseable")),
        "upstream_error": error.details,
    });
    let object = details
        .as_object_mut()
        .expect("field error details are an object");
    if !validation.is_empty() {
        object.insert(
            "field_errors".to_owned(),
            serde_json::to_value(validation).expect("validation issues serialize"),
        );
    }
    if let Some(observation) = observation {
        object.insert("observation".to_owned(), observation);
    }
    details
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum RecoveryAttempt {
    Completed,
    Attempting,
    Unattempted,
}

#[derive(Clone, Debug)]
pub(super) struct RecoveryImageIntent {
    pub(super) index: usize,
    pub(super) operation: ImageRecoveryOperation,
    pub(super) image_id: Option<String>,
    pub(super) upload: RecoveryAttempt,
    pub(super) attachment: RecoveryAttempt,
}

impl RecoveryImageIntent {
    pub(super) fn additions(start: usize, count: usize) -> Vec<Self> {
        (0..count)
            .map(|offset| Self {
                index: start + offset,
                operation: ImageRecoveryOperation::Add,
                image_id: None,
                upload: RecoveryAttempt::Unattempted,
                attachment: RecoveryAttempt::Unattempted,
            })
            .collect()
    }

    pub(super) fn removals(image_ids: &[String]) -> Vec<Self> {
        image_ids
            .iter()
            .enumerate()
            .map(|(index, image_id)| Self {
                index,
                operation: ImageRecoveryOperation::Remove,
                image_id: Some(image_id.clone()),
                upload: RecoveryAttempt::Completed,
                attachment: RecoveryAttempt::Attempting,
            })
            .collect()
    }
}

fn summarize_recovery_images(
    intent: &[RecoveryImageIntent],
    observed: Option<&DraftState>,
    uncertain: bool,
) -> Vec<ImageRecovery> {
    intent
        .iter()
        .map(|requested| {
            let observed_image = requested.image_id.as_deref().and_then(|image_id| {
                observed
                    .and_then(|state| state.images.iter().find(|image| image.image_id == image_id))
            });
            let upload = match requested.upload {
                RecoveryAttempt::Completed => UploadRecoveryStatus::Completed,
                RecoveryAttempt::Attempting if uncertain => UploadRecoveryStatus::Indeterminate,
                RecoveryAttempt::Attempting => UploadRecoveryStatus::Failed,
                RecoveryAttempt::Unattempted => UploadRecoveryStatus::Unattempted,
            };
            let attachment = if observed_image.is_some() {
                AttachmentRecoveryStatus::Attached
            } else {
                match requested.attachment {
                    RecoveryAttempt::Completed | RecoveryAttempt::Attempting
                        if observed.is_some() =>
                    {
                        AttachmentRecoveryStatus::Absent
                    }
                    RecoveryAttempt::Completed | RecoveryAttempt::Attempting => {
                        AttachmentRecoveryStatus::Indeterminate
                    }
                    RecoveryAttempt::Unattempted
                        if requested.upload == RecoveryAttempt::Completed && observed.is_some() =>
                    {
                        AttachmentRecoveryStatus::Absent
                    }
                    RecoveryAttempt::Unattempted => AttachmentRecoveryStatus::Unattempted,
                }
            };
            let processing = match observed_image.map(|image| &image.state) {
                Some(ImageState::Ready) => ProcessingRecoveryStatus::Ready,
                Some(ImageState::Processing) => ProcessingRecoveryStatus::Processing,
                Some(ImageState::Failed) => ProcessingRecoveryStatus::Failed,
                None if attachment == AttachmentRecoveryStatus::Indeterminate => {
                    ProcessingRecoveryStatus::Indeterminate
                }
                None => ProcessingRecoveryStatus::Unattempted,
            };
            let status = match requested.operation {
                ImageRecoveryOperation::Remove
                    if observed.is_some() && observed_image.is_none() =>
                {
                    RecoveryStatus::Persisted
                }
                ImageRecoveryOperation::Remove if observed_image.is_some() => {
                    RecoveryStatus::Absent
                }
                ImageRecoveryOperation::Remove => RecoveryStatus::Indeterminate,
                ImageRecoveryOperation::Add => match processing {
                    ProcessingRecoveryStatus::Ready => RecoveryStatus::Persisted,
                    ProcessingRecoveryStatus::Processing => RecoveryStatus::Pending,
                    ProcessingRecoveryStatus::Failed => RecoveryStatus::Rejected,
                    ProcessingRecoveryStatus::Indeterminate => RecoveryStatus::Indeterminate,
                    ProcessingRecoveryStatus::Unattempted => match upload {
                        UploadRecoveryStatus::Completed => RecoveryStatus::Absent,
                        UploadRecoveryStatus::Failed => RecoveryStatus::Rejected,
                        UploadRecoveryStatus::Indeterminate => RecoveryStatus::Indeterminate,
                        UploadRecoveryStatus::Unattempted => RecoveryStatus::Unattempted,
                    },
                },
            };
            ImageRecovery {
                index: requested.index,
                operation: requested.operation,
                status,
                upload,
                attachment,
                processing,
                image_id: requested.image_id.as_deref().map(bounded_recovery_text),
            }
        })
        .collect()
}

pub(super) fn observed_image_intent(state: &DraftState) -> Vec<RecoveryImageIntent> {
    state
        .images
        .iter()
        .map(|image| RecoveryImageIntent {
            index: image.position,
            operation: ImageRecoveryOperation::Add,
            image_id: Some(image.image_id.clone()),
            upload: RecoveryAttempt::Completed,
            attachment: RecoveryAttempt::Completed,
        })
        .collect()
}

pub(super) fn set_recovery_images(
    recovery: &mut Recovery,
    intent: &[RecoveryImageIntent],
    uncertain: bool,
) {
    let mut images = summarize_recovery_images(intent, recovery.fresh_state.as_ref(), uncertain);
    images.sort_by_key(|image| (recovery_priority(image.status), image.index));
    recovery.images_omitted = images.len().saturating_sub(RECOVERY_IMAGE_LIMIT);
    images.truncate(RECOVERY_IMAGE_LIMIT);
    recovery.images = images;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_condition_machine_values_match_the_source_options() {
        let field = Field::new(
            "condition",
            "Kunto",
            FieldType::Select,
            Requirement::Optional,
            None,
            "attributes",
        );
        let options = [
            ("1", "Uusi"),
            ("2", "Kuin uusi"),
            ("3", "Hyvä"),
            ("4", "Kohtalainen"),
            ("5", "Vaatii korjausta"),
        ]
        .into_iter()
        .map(|(value, label)| FieldOption {
            field: "condition".to_owned(),
            value: json!(value),
            label: label.to_owned(),
        })
        .collect();
        let state = DraftState {
            draft_id: "draft-1".to_owned(),
            etag: "etag".to_owned(),
            revision: None,
            values: Map::new(),
            fields: vec![field.clone()],
            options,
            required_fields: Vec::new(),
            images: Vec::new(),
            cleared_fields: Vec::new(),
            predictions: Vec::new(),
            delivery: None,
        };

        for value in ["1", "2", "3", "4", "5"] {
            assert!(schema_validation_issue(&state, &field, &json!(value)).is_none());
        }
    }
}
