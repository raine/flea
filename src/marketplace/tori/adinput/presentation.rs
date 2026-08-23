use serde::Serialize;
use serde_json::{Map, Value};

use crate::{
    domain::{
        envelope::NextAction,
        field::{Field, FieldStatus},
    },
    error::AppError,
};

use super::{
    CategoryPrediction, CategoryValidation, DeliveryOption, DraftDelivery, DraftImage, DraftState,
    FieldOption, PublicationRequirement, PublicationValidation, ValidationEvidenceFailure,
};

#[derive(Debug, Serialize)]
pub struct DraftInspectionOutput {
    draft_id: String,
    etag: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    revision: Option<String>,
    pub(crate) values: Map<String, Value>,
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
    #[serde(skip)]
    pub(crate) next_actions: Vec<NextAction>,
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

pub fn draft_inspection(
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

fn input_error(message: impl Into<String>) -> AppError {
    let mut error = AppError::usage(message);
    error.code = "cli.invalid_input".to_owned();
    error
}
