use super::{
    http::ApiError,
    recovery::WorkflowConfig,
    types::{DraftState, model_error},
};
use crate::diagnostics;
use serde_json::{Map, Value};

mod create;
mod images;
mod inspect;
mod publish;
mod recovery;
mod update;

fn reconciled_mutation_warning(fields: &[String], response_model_drift: bool) -> String {
    let response = if response_model_drift {
        "an unrecognized successful mutation response"
    } else {
        "an ambiguous mutation response"
    };
    format!(
        "Tori returned {response}; authoritative observation confirmed persisted state for {}",
        fields.join(", ")
    )
}

fn record_mutation_response_drift(context: &diagnostics::WorkflowContext<'_>, error: &ApiError) {
    if !error
        .status
        .is_some_and(|status| (200..300).contains(&status))
    {
        return;
    }
    let details = error.details.as_deref();
    diagnostics::mutation_response_model_drift(
        context,
        error.status,
        details
            .and_then(|details| details.get("path"))
            .and_then(Value::as_str),
        details
            .and_then(|details| details.get("reason"))
            .and_then(Value::as_str),
    );
}

fn require_authoritative_revision(state: &DraftState) -> Result<&str, ApiError> {
    state
        .revision
        .as_deref()
        .filter(|revision| !revision.is_empty())
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.ad.revision",
                "draft revision is unavailable",
            )
        })
}

fn sanitize_listing_copy_values(values: &mut Map<String, Value>) -> Vec<String> {
    const OMITTED_FIELDS: &[&str] = &[
        "contact_email",
        "contact_phone",
        "email",
        "image",
        "images",
        "listing_id",
        "multi_image",
        "owner_id",
        "phone",
        "publication_state",
        "revision",
        "seller",
        "seller_id",
        "seller_name",
        "state",
    ];
    let mut omitted = Vec::new();
    for field in OMITTED_FIELDS {
        if values.remove(*field).is_some() {
            omitted.push((*field).to_owned());
        }
    }
    omitted
}

pub struct DraftWorkflow<A> {
    api: A,
    config: WorkflowConfig,
}

impl<A> DraftWorkflow<A> {
    pub fn new(api: A, config: WorkflowConfig) -> Self {
        Self { api, config }
    }
}
