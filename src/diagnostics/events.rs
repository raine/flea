use std::time::Duration;

use tracing::info;

use super::redaction::sanitized_upstream_body;

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

#[derive(Debug)]
pub struct HttpContext<'a> {
    pub method: &'a str,
    pub service: &'a str,
    pub path: &'a str,
    pub status: Option<u16>,
    pub latency: Duration,
    pub retry_count: u32,
    pub upstream_body: Option<&'a [u8]>,
}

pub fn http_event(context: &HttpContext<'_>) {
    let upstream_body = context.upstream_body.map(sanitized_upstream_body);
    info!(
        event = "http.request",
        http.method = context.method,
        http.service = context.service,
        http.path = context.path,
        http.status = context.status,
        http.latency_ms = context.latency.as_millis() as u64,
        http.retry_count = context.retry_count,
        upstream.body = upstream_body
    );
}
