use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::{
    domain::{
        envelope::{NextAction, Warning},
        observation::Observation,
    },
    error::{AppError, ExitClass},
};

use super::{
    AdInputApi, AddImagesResult, CreateResult, DraftInput, DraftInspectionOutput, DraftState,
    DraftWorkflow, PublicationValidation, PublishResult, UpdateResult, WorkflowConfig,
    WorkflowError, WorkflowWarning, completed_steps_have_mutation, draft_inspection, prepare,
};

pub enum DraftRequest {
    Create {
        input: DraftInput,
    },
    CreateFromListing {
        listing_id: String,
    },
    Show {
        draft_id: String,
        include_fields: bool,
        include_options: Option<String>,
    },
    Update {
        draft_id: String,
        input: DraftInput,
    },
    AddImages {
        draft_id: String,
        paths: Vec<PathBuf>,
    },
    RemoveImages {
        draft_id: String,
        image_ids: Vec<String>,
    },
    Validate {
        draft_id: String,
    },
    Publish {
        draft_id: String,
        if_revision: String,
    },
    Delete {
        draft_id: String,
    },
}

#[derive(Debug, Serialize)]
pub struct DraftDeleteOutput {
    pub draft_id: String,
    pub deleted: bool,
}

pub enum DraftResultData {
    Create(CreateResult),
    Inspection(DraftInspectionOutput),
    Update(UpdateResult),
    AddImages(AddImagesResult),
    State(DraftState),
    Validation(PublicationValidation),
    Publish(PublishResult),
    Delete(DraftDeleteOutput),
}

pub struct DraftResult {
    pub data: DraftResultData,
    pub next_actions: Vec<NextAction>,
    pub observation: Observation,
    pub warnings: Vec<Warning>,
}

pub struct DraftExecution<A> {
    api: A,
    config: WorkflowConfig,
}

impl<A: AdInputApi> DraftExecution<A> {
    pub fn new(api: A, config: WorkflowConfig) -> Self {
        Self { api, config }
    }

    pub async fn execute(self, request: DraftRequest) -> Result<DraftResult, AppError> {
        let confirms_absence = matches!(&request, DraftRequest::Delete { .. });
        let mut observation_source = match &request {
            DraftRequest::Show { .. } => "draft_detail",
            DraftRequest::Validate { .. } => "draft_validation",
            DraftRequest::Publish { .. } => "listing_detail",
            DraftRequest::AddImages { .. } | DraftRequest::RemoveImages { .. } => "draft_images",
            DraftRequest::Create { .. } | DraftRequest::CreateFromListing { .. } => {
                "draft_creation_response"
            }
            DraftRequest::Update { .. } => "draft_update_response",
            DraftRequest::Delete { .. } => "draft_delete_response",
        };
        let workflow = DraftWorkflow::new(self.api, self.config);
        let (data, warnings, next_actions) = match request {
            DraftRequest::CreateFromListing { listing_id } => {
                let mut result = workflow
                    .create_from_listing(&listing_id)
                    .await
                    .map_err(workflow_error)?;
                normalize_draft_state(&mut result.draft);
                let warnings = output_warnings(&result.warnings);
                (DraftResultData::Create(result), warnings, Vec::new())
            }
            DraftRequest::Create { input } => {
                let input = prepare(input, true)?;
                let mut result = workflow
                    .create_prepared(input.values, input.images)
                    .await
                    .map_err(workflow_error)?;
                normalize_draft_state(&mut result.draft);
                let warnings = output_warnings(&result.warnings);
                (DraftResultData::Create(result), warnings, Vec::new())
            }
            DraftRequest::Show {
                draft_id,
                include_fields,
                include_options,
            } => {
                let (state, validation) = workflow
                    .inspect(&draft_id, include_options.is_some())
                    .await
                    .map_err(workflow_error)?;
                let mut inspection = draft_inspection(
                    state,
                    validation,
                    include_fields,
                    include_options.as_deref(),
                )?;
                crate::domain::commerce::normalize_commerce_map(&mut inspection.values);
                let next_actions = std::mem::take(&mut inspection.next_actions);
                (
                    DraftResultData::Inspection(inspection),
                    Vec::new(),
                    next_actions,
                )
            }
            DraftRequest::Update { draft_id, input } => {
                if !input.image_paths.is_empty() {
                    return Err(input_error(
                        "draft update does not accept images; use `draft image add`",
                    ));
                }
                let input = prepare(input, false)?;
                let mut result = workflow
                    .update(&draft_id, &input.values)
                    .await
                    .map_err(workflow_error)?;
                normalize_draft_state(&mut result.draft);
                let warnings = output_warnings(&result.warnings);
                (DraftResultData::Update(result), warnings, Vec::new())
            }
            DraftRequest::AddImages { draft_id, paths } => {
                let mut result = workflow
                    .add_images(&draft_id, &paths)
                    .await
                    .map_err(workflow_error)?;
                normalize_draft_state(&mut result.draft);
                let warnings = output_warnings(&result.warnings);
                (DraftResultData::AddImages(result), warnings, Vec::new())
            }
            DraftRequest::RemoveImages {
                draft_id,
                image_ids,
            } => {
                let mut result = workflow
                    .remove_images(&draft_id, &image_ids)
                    .await
                    .map_err(workflow_error)?;
                normalize_draft_state(&mut result);
                (DraftResultData::State(result), Vec::new(), Vec::new())
            }
            DraftRequest::Validate { draft_id } => (
                DraftResultData::Validation(
                    workflow.validate(&draft_id).await.map_err(workflow_error)?,
                ),
                Vec::new(),
                Vec::new(),
            ),
            DraftRequest::Publish {
                draft_id,
                if_revision,
            } => {
                let mut result = workflow
                    .publish(&draft_id, &if_revision)
                    .await
                    .map_err(workflow_error)?;
                normalize_observed_listing_output(&mut result.observed_listing);
                let warnings = output_warnings(&result.warnings);
                (DraftResultData::Publish(result), warnings, Vec::new())
            }
            DraftRequest::Delete { draft_id } => {
                workflow.delete(&draft_id).await.map_err(workflow_error)?;
                (
                    DraftResultData::Delete(DraftDeleteOutput {
                        draft_id,
                        deleted: true,
                    }),
                    Vec::new(),
                    Vec::new(),
                )
            }
        };
        let reconciled = warnings.iter().any(|warning| {
            matches!(
                warning.code.as_str(),
                "mutation.response_model_drift" | "mutation.observed_success"
            )
        });
        if reconciled
            && matches!(
                observation_source,
                "draft_creation_response" | "draft_update_response"
            )
        {
            observation_source = "draft_detail";
        }
        let observation = if confirms_absence {
            Observation::confirmed_absent(observation_source, None)
        } else {
            Observation::confirmed_present(observation_source, None)
        };
        Ok(DraftResult {
            data,
            warnings,
            next_actions,
            observation,
        })
    }
}

fn output_warnings(warnings: &[WorkflowWarning]) -> Vec<Warning> {
    warnings
        .iter()
        .map(|warning| Warning {
            code: warning.code.to_owned(),
            message: warning.message.clone(),
        })
        .collect()
}

fn normalize_draft_state(state: &mut DraftState) {
    crate::domain::commerce::normalize_commerce_map(&mut state.values);
}

fn normalize_observed_listing_output(value: &mut Value) {
    let Some(observed) = value.as_object_mut() else {
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

fn input_error(message: impl Into<String>) -> AppError {
    let mut error = AppError::usage(message);
    error.code = "cli.invalid_input".to_owned();
    error
}
