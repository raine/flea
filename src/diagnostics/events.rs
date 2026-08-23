use tracing::info;

#[derive(Debug, Default)]
pub struct WorkflowContext<'a> {
    pub workflow: &'a str,
    pub step: &'a str,
    pub draft_id: Option<&'a str>,
    pub listing_id: Option<&'a str>,
    pub fields: &'a [String],
}

pub fn workflow_step(context: &WorkflowContext<'_>, status: &str) {
    info!(
        event = "workflow.step",
        workflow = context.workflow,
        step = context.step,
        draft_id = context.draft_id,
        listing_id = context.listing_id,
        fields = ?context.fields,
        status
    );
}

pub fn mutation_response_model_drift(
    context: &WorkflowContext<'_>,
    status: Option<u16>,
    path: Option<&str>,
    reason: Option<&str>,
) {
    info!(
        event = "mutation.response_model_drift",
        workflow = context.workflow,
        step = context.step,
        draft_id = context.draft_id,
        listing_id = context.listing_id,
        fields = ?context.fields,
        http.status = status,
        model.path = path,
        model.reason = reason,
    );
}
