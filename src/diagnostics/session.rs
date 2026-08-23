use std::{
    io,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use tracing::{Subscriber, info, info_span};
use tracing_subscriber::{EnvFilter, Layer, layer::SubscriberExt};
use uuid::Uuid;

use super::files::{RetentionPolicy, active_log_path, prepare_log};
use crate::{
    domain::envelope::Diagnostics,
    error::{AppError, ExitClass},
    storage::discover_state_root,
};

#[derive(Clone, Debug)]
pub struct DiagnosticsContext {
    pub trace_id: String,
    pub correlation_id: String,
    pub log_path: PathBuf,
}

impl DiagnosticsContext {
    pub fn envelope(&self) -> Diagnostics {
        Diagnostics {
            trace_id: self.trace_id.clone(),
            correlation_id: self.correlation_id.clone(),
            log_path: self.log_path.to_string_lossy().into_owned(),
        }
    }
}

pub struct DiagnosticsSession {
    context: DiagnosticsContext,
    subscriber: Arc<dyn Subscriber + Send + Sync>,
}

impl std::fmt::Debug for DiagnosticsSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiagnosticsSession")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

impl DiagnosticsSession {
    pub fn initialize() -> Result<Self, DiagnosticsInitError> {
        let state_dir = state_dir().map_err(DiagnosticsInitError::without_path)?;
        Self::initialize_at(&state_dir, RetentionPolicy::default())
    }

    pub(super) fn initialize_at(
        state_dir: &Path,
        retention: RetentionPolicy,
    ) -> Result<Self, DiagnosticsInitError> {
        let context = DiagnosticsContext {
            trace_id: Uuid::new_v4().to_string(),
            correlation_id: Uuid::new_v4().to_string(),
            log_path: active_log_path(state_dir),
        };
        initialize_with_context(context, retention)
    }

    pub fn context(&self) -> &DiagnosticsContext {
        &self.context
    }

    pub fn run<T>(&self, command: &str, operation: impl FnOnce() -> (T, u8)) -> T {
        tracing::subscriber::with_default(Arc::clone(&self.subscriber), || {
            let span = info_span!(
                "command",
                command,
                trace_id = %self.context.trace_id,
                correlation_id = %self.context.correlation_id
            );
            let _entered = span.enter();
            let started = Instant::now();
            info!(event = "command.started");
            let (result, exit_code) = operation();
            info!(
                event = "command.finished",
                status = if exit_code == 0 { "success" } else { "failure" },
                exit_code,
                duration_ms = started.elapsed().as_millis() as u64
            );
            result
        })
    }
}

#[derive(Debug)]
pub struct DiagnosticsInitError {
    context: Option<DiagnosticsContext>,
    source: io::Error,
}

impl DiagnosticsInitError {
    fn without_path(source: io::Error) -> Self {
        Self {
            context: None,
            source,
        }
    }

    pub fn into_app_error(self) -> AppError {
        let mut error = AppError::new(
            "diagnostics.initialization_failed",
            "failed to initialize diagnostics",
            ExitClass::Upstream,
        )
        .with_source(self.source);
        if let Some(context) = self.context {
            error.diagnostics = Some(Box::new(context.envelope()));
        }
        error
    }
}

fn initialize_with_context(
    context: DiagnosticsContext,
    retention: RetentionPolicy,
) -> Result<DiagnosticsSession, DiagnosticsInitError> {
    let error_context = context.clone();
    let result = (|| {
        let writer = prepare_log(&context.log_path, retention)?;
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("flea=info"));
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter),
        );
        Ok(DiagnosticsSession {
            context,
            subscriber: Arc::new(subscriber),
        })
    })();

    result.map_err(|source| DiagnosticsInitError {
        context: Some(error_context),
        source,
    })
}

fn state_dir() -> io::Result<PathBuf> {
    discover_state_root()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::Value;
    use tempfile::tempdir;
    use tracing::info;

    use super::*;
    use crate::diagnostics::{WorkflowContext, mutation_response_model_drift, workflow_step};

    #[test]
    fn subscriber_writes_redacted_jsonl_with_correlation_fields() {
        let state = tempdir().expect("temporary state directory");
        let session = DiagnosticsSession::initialize_at(state.path(), RetentionPolicy::default())
            .expect("diagnostics should initialize");
        session.run("draft show", || {
            info!(authorization = "Bearer top-secret", "request");
            let fields = vec!["price".to_owned()];
            let context = WorkflowContext {
                workflow: "draft_update",
                step: "apply_price",
                draft_id: Some("draft-1"),
                listing_id: None,
                fields: &fields,
            };
            workflow_step(&context, "started");
            mutation_response_model_drift(
                &context,
                Some(200),
                Some("$.ad"),
                Some("ad data is unavailable"),
            );
            ((), 0)
        });
        let contents = fs::read_to_string(&session.context().log_path).expect("log should exist");
        assert!(!contents.contains("top-secret"));
        let events: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
            .collect();
        assert!(
            events
                .iter()
                .all(|event| event["span"]["trace_id"] == session.context().trace_id)
        );
        assert!(
            events
                .iter()
                .all(|event| event["span"]["correlation_id"] == session.context().correlation_id)
        );
        assert!(events.iter().all(|event| {
            event["timestamp"].is_string()
                && event["level"].is_string()
                && event["target"].is_string()
                && event["span"]["command"] == "draft show"
        }));
        assert!(
            events
                .iter()
                .any(|event| { event["event"] == "command.started" })
        );
        assert!(events.iter().any(|event| {
            event["event"] == "command.finished"
                && event["status"] == "success"
                && event["exit_code"] == 0
                && event["duration_ms"].is_number()
        }));
        assert!(events.iter().any(|event| {
            event["event"] == "workflow.step"
                && event["workflow"] == "draft_update"
                && event["step"] == "apply_price"
                && event["fields"] == "[\"price\"]"
                && event["status"] == "started"
        }));
        assert!(events.iter().any(|event| {
            event["event"] == "mutation.response_model_drift"
                && event["http.status"] == 200
                && event["model.path"] == "$.ad"
        }));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&session.context().log_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}
