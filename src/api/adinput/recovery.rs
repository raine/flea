use super::{delivery::*, http::*, types::*, *};

#[derive(Clone, Debug)]
pub struct WorkflowConfig {
    pub image_processing_timeout: Duration,
    pub image_poll_interval: Duration,
    pub image_poll_limit: usize,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            image_processing_timeout: Duration::from_secs(120),
            image_poll_interval: Duration::from_secs(2),
            image_poll_limit: 60,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryStatus {
    Persisted,
    Pending,
    Absent,
    Rejected,
    Unattempted,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationStatus {
    Observed,
    Unavailable,
    ChangedByAnotherClient,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RecoveryObservation {
    pub status: ObservationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FieldRecovery {
    pub field: String,
    pub status: RecoveryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_text: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageRecoveryOperation {
    Add,
    Remove,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UploadRecoveryStatus {
    Completed,
    Failed,
    Unattempted,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentRecoveryStatus {
    Attached,
    Absent,
    Unattempted,
    Indeterminate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingRecoveryStatus {
    Ready,
    Processing,
    Failed,
    Unattempted,
    Indeterminate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageRecovery {
    pub index: usize,
    pub operation: ImageRecoveryOperation,
    pub status: RecoveryStatus,
    pub upload: UploadRecoveryStatus,
    pub attachment: AttachmentRecoveryStatus,
    pub processing: ProcessingRecoveryStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Recovery {
    pub draft_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_listing_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_etag: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_revision: Option<String>,
    pub completed_steps: Vec<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub completed_steps_omitted: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed_stage: Option<String>,
    pub observation: RecoveryObservation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_step: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub persisted_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub absent_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub indeterminate_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unattempted_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub field_summary: Vec<FieldRecovery>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub fields_omitted: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ImageRecovery>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub images_omitted: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<RecoveryStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication: Option<RecoveryStatus>,
    #[serde(default, skip_serializing_if = "is_false")]
    pub manual_inspection_required: bool,
    pub upstream_transient: bool,
    pub safe_to_retry: bool,
    pub next_safe_actions: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destructive_actions: Vec<String>,
    #[serde(skip)]
    pub fresh_state: Option<DraftState>,
}

const RECOVERY_FIELD_LIMIT: usize = 24;
pub(super) const RECOVERY_IMAGE_LIMIT: usize = 20;
const RECOVERY_STEP_LIMIT: usize = 24;

pub(super) const fn is_zero(value: &usize) -> bool {
    *value == 0
}

fn is_false(value: &bool) -> bool {
    !value
}

fn bounded_recovery_steps(steps: &[String]) -> (Vec<String>, usize) {
    if steps.len() <= RECOVERY_STEP_LIMIT {
        return (steps.to_vec(), 0);
    }
    const PREFIX: usize = 8;
    let mut bounded = steps.iter().take(PREFIX).cloned().collect::<Vec<_>>();
    bounded.extend(
        steps
            .iter()
            .skip(steps.len() - (RECOVERY_STEP_LIMIT - PREFIX))
            .cloned(),
    );
    (bounded, steps.len() - RECOVERY_STEP_LIMIT)
}

pub(super) fn bounded_recovery_text(value: &str) -> String {
    const LIMIT: usize = 160;
    if value.chars().count() <= LIMIT {
        return value.to_owned();
    }
    let mut bounded = value.chars().take(LIMIT - 3).collect::<String>();
    bounded.push_str("...");
    bounded
}

fn safe_listing_text(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>();
    bounded_recovery_text(normalized.trim())
}

pub(super) fn recovery_scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(bounded_recovery_text(value)),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn bound_recovery_names(values: &mut Vec<String>) {
    let mut seen = BTreeSet::new();
    values.retain(|value| seen.insert(value.clone()));
    values.truncate(RECOVERY_FIELD_LIMIT);
}

pub(super) fn recovery_priority(status: RecoveryStatus) -> u8 {
    match status {
        RecoveryStatus::Absent => 0,
        RecoveryStatus::Pending => 1,
        RecoveryStatus::Indeterminate => 2,
        RecoveryStatus::Rejected => 3,
        RecoveryStatus::Persisted => 4,
        RecoveryStatus::Unattempted => 5,
    }
}

pub(super) fn observation_timestamp() -> String {
    time::OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string()
        })
}

impl Recovery {
    pub(super) fn base(
        draft_id: &str,
        completed_steps: &[String],
        error_code: Option<&str>,
    ) -> Self {
        let (completed_steps, completed_steps_omitted) = bounded_recovery_steps(completed_steps);
        Self {
            draft_id: draft_id.to_owned(),
            source_listing_id: None,
            listing_id: None,
            observed_etag: None,
            observed_revision: None,
            completed_steps,
            completed_steps_omitted,
            failed_stage: None,
            observation: RecoveryObservation {
                status: ObservationStatus::Unavailable,
                observed_at: None,
                error_code: error_code.map(str::to_owned),
            },
            active_step: None,
            fields: Vec::new(),
            persisted_fields: Vec::new(),
            absent_fields: Vec::new(),
            indeterminate_fields: Vec::new(),
            unattempted_fields: Vec::new(),
            field_summary: Vec::new(),
            fields_omitted: 0,
            images: Vec::new(),
            images_omitted: 0,
            delivery: None,
            publication: None,
            manual_inspection_required: false,
            upstream_transient: false,
            safe_to_retry: false,
            next_safe_actions: vec![format!("flea draft show {draft_id}")],
            destructive_actions: Vec::new(),
            fresh_state: None,
        }
    }

    pub(super) fn observe(&mut self, state: &DraftState, status: ObservationStatus) {
        self.observed_etag = (!state.etag.is_empty()).then(|| bounded_recovery_text(&state.etag));
        self.observed_revision = state
            .revision
            .as_deref()
            .map(bounded_recovery_text)
            .or_else(|| state.values.get("revision").and_then(recovery_scalar));
        self.observation = RecoveryObservation {
            status,
            observed_at: Some(observation_timestamp()),
            error_code: None,
        };
        self.fresh_state = Some(state.clone());
        if status == ObservationStatus::Observed && self.listing_id.is_none() {
            self.destructive_actions = vec![format!("flea draft delete {}", self.draft_id)];
        }
    }

    pub(super) fn refresh_field_summary(&mut self) {
        let state = self.fresh_state.as_ref();
        let mut summary = Vec::new();
        let groups = [
            (&self.absent_fields, RecoveryStatus::Absent),
            (&self.indeterminate_fields, RecoveryStatus::Indeterminate),
            (&self.persisted_fields, RecoveryStatus::Persisted),
            (&self.unattempted_fields, RecoveryStatus::Unattempted),
        ];
        for (fields, status) in groups {
            for field in fields {
                let safe_text = (status == RecoveryStatus::Persisted
                    && matches!(field.as_str(), "title" | "description"))
                .then(|| {
                    state
                        .and_then(|state| state.values.get(field))
                        .and_then(Value::as_str)
                        .map(safe_listing_text)
                })
                .flatten();
                summary.push(FieldRecovery {
                    field: bounded_recovery_text(field),
                    status,
                    safe_text,
                });
            }
        }
        summary.sort_by(|left, right| {
            recovery_priority(left.status)
                .cmp(&recovery_priority(right.status))
                .then_with(|| left.field.cmp(&right.field))
        });
        summary.dedup_by(|left, right| left.field == right.field);
        self.fields_omitted = summary.len().saturating_sub(RECOVERY_FIELD_LIMIT);
        summary.truncate(RECOVERY_FIELD_LIMIT);
        self.field_summary = summary;
        bound_recovery_names(&mut self.fields);
        bound_recovery_names(&mut self.persisted_fields);
        bound_recovery_names(&mut self.absent_fields);
        bound_recovery_names(&mut self.indeterminate_fields);
        bound_recovery_names(&mut self.unattempted_fields);
        self.failed_stage = self.active_step.clone();
    }
}

#[derive(Clone, PartialEq)]
pub struct WorkflowError {
    pub code: String,
    pub message: String,
    pub source: Option<ApiError>,
    pub recovery: Option<Recovery>,
    pub details: Option<Value>,
}

impl WorkflowError {
    pub(super) fn input(error: ApiError) -> Self {
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery: None,
            details: None,
        }
    }

    pub(super) fn before_creation(error: ApiError) -> Self {
        let recovery = error.details.as_deref().and_then(|details| {
            let draft_id = details.get("draft_id")?.as_str()?.to_owned();
            let completed_steps = details
                .get("completed_steps")?
                .as_array()?
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect::<Vec<_>>();
            Some(Recovery {
                upstream_transient: error.upstream_transient,
                ..Recovery::base(&draft_id, &completed_steps, Some(&error.code))
            })
        });
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery,
            details: None,
        }
    }

    pub(super) fn for_draft(
        draft_id: &str,
        completed_steps: &[String],
        error: ApiError,
        safe_to_retry: bool,
    ) -> Self {
        let safe_to_retry =
            safe_to_retry && error.safe_to_retry && !completed_steps_have_mutation(completed_steps);
        let upstream_transient = error.upstream_transient;
        let error_code = error.code.clone();
        Self {
            code: error.code.clone(),
            message: error.message.clone(),
            source: Some(error),
            recovery: Some(Recovery {
                upstream_transient,
                safe_to_retry,
                ..Recovery::base(draft_id, completed_steps, Some(&error_code))
            }),
            details: None,
        }
    }

    pub(super) fn with_source_listing_id(mut self, listing_id: &str) -> Self {
        if let Some(recovery) = &mut self.recovery {
            recovery.source_listing_id = Some(listing_id.to_owned());
        }
        self
    }

    pub(super) fn with_optional_source_listing_id(self, listing_id: Option<&str>) -> Self {
        match listing_id {
            Some(listing_id) => self.with_source_listing_id(listing_id),
            None => self,
        }
    }

    pub(super) fn validation(completed_steps: &[String], report: PublicationValidation) -> Self {
        let deterministic = !report.missing.is_empty() || !report.invalid.is_empty();
        let upstream_transient = report
            .evidence_failures
            .iter()
            .any(|failure| failure.upstream_transient);
        let safe_to_retry = if deterministic {
            false
        } else if !report.evidence_failures.is_empty() {
            report
                .evidence_failures
                .iter()
                .all(|failure| failure.safe_to_retry)
        } else {
            !report.pending.is_empty() && report.unverifiable.is_empty()
        };
        let failed_stage = if deterministic {
            "validate".to_owned()
        } else {
            report
                .evidence_failures
                .first()
                .map(|failure| failure.failed_stage.clone())
                .unwrap_or_else(|| "validate".to_owned())
        };
        let mut next_safe_actions = report
            .missing
            .iter()
            .chain(&report.invalid)
            .chain(&report.pending)
            .chain(&report.unverifiable)
            .map(|requirement| requirement.command.clone())
            .chain(
                report
                    .evidence_failures
                    .iter()
                    .map(|failure| failure.command.clone()),
            )
            .collect::<Vec<_>>();
        next_safe_actions.push(format!("flea draft show {}", report.draft_id));
        let mut seen_actions = BTreeSet::new();
        next_safe_actions.retain(|action| seen_actions.insert(action.clone()));
        let absent_fields = report
            .missing
            .iter()
            .map(|requirement| requirement.field.clone())
            .take(RECOVERY_FIELD_LIMIT)
            .collect::<Vec<_>>();
        let rejected_fields = report
            .invalid
            .iter()
            .map(|requirement| requirement.field.clone())
            .take(RECOVERY_FIELD_LIMIT)
            .collect::<Vec<_>>();
        let indeterminate_fields = report
            .unverifiable
            .iter()
            .map(|requirement| requirement.field.clone())
            .take(RECOVERY_FIELD_LIMIT)
            .collect::<Vec<_>>();
        let pending_fields = report
            .pending
            .iter()
            .map(|requirement| requirement.field.clone())
            .take(RECOVERY_FIELD_LIMIT)
            .collect::<Vec<_>>();
        let mut field_summary = absent_fields
            .iter()
            .map(|field| (field, RecoveryStatus::Absent))
            .chain(
                rejected_fields
                    .iter()
                    .map(|field| (field, RecoveryStatus::Rejected)),
            )
            .chain(
                indeterminate_fields
                    .iter()
                    .map(|field| (field, RecoveryStatus::Indeterminate)),
            )
            .chain(
                pending_fields
                    .iter()
                    .map(|field| (field, RecoveryStatus::Pending)),
            )
            .map(|(field, status)| FieldRecovery {
                field: bounded_recovery_text(field),
                status,
                safe_text: None,
            })
            .collect::<Vec<_>>();
        field_summary.sort_by(|left, right| {
            recovery_priority(left.status)
                .cmp(&recovery_priority(right.status))
                .then_with(|| left.field.cmp(&right.field))
        });
        field_summary.dedup_by(|left, right| left.field == right.field);
        let fields_omitted = field_summary.len().saturating_sub(RECOVERY_FIELD_LIMIT);
        field_summary.truncate(RECOVERY_FIELD_LIMIT);
        let details = serde_json::to_value(&report).ok();
        Self {
            code: "draft.validation_failed".to_owned(),
            message: "The draft is not ready for publication".to_owned(),
            source: None,
            recovery: Some(Recovery {
                upstream_transient,
                safe_to_retry,
                next_safe_actions,
                failed_stage: Some(failed_stage),
                absent_fields,
                indeterminate_fields,
                field_summary,
                fields_omitted,
                observation: RecoveryObservation {
                    status: ObservationStatus::Observed,
                    observed_at: Some(observation_timestamp()),
                    error_code: None,
                },
                ..Recovery::base(&report.draft_id, completed_steps, None)
            }),
            details,
        }
    }

    pub(super) fn delivery_validation(
        draft_id: &str,
        completed_steps: &[String],
        delivery: &DraftDelivery,
        requested: Vec<String>,
    ) -> Self {
        let allowed = allowed_delivery_values(delivery);
        let next_safe_actions = allowed
            .first()
            .map(|value| format!("flea draft update {draft_id} --delivery {value}"))
            .into_iter()
            .chain(std::iter::once(format!("flea draft show {draft_id}")))
            .collect();
        let missing = requested.is_empty();
        Self {
            code: if missing {
                "draft.validation_failed".to_owned()
            } else {
                "draft.invalid_delivery".to_owned()
            },
            message: if missing {
                "An explicit delivery selection is required".to_owned()
            } else {
                "The requested delivery value is unavailable for this draft".to_owned()
            },
            source: None,
            recovery: Some(Recovery {
                active_step: Some("validate_delivery".to_owned()),
                failed_stage: Some("validate_delivery".to_owned()),
                fields: vec!["delivery".to_owned()],
                absent_fields: vec!["delivery".to_owned()],
                field_summary: vec![FieldRecovery {
                    field: "delivery".to_owned(),
                    status: RecoveryStatus::Absent,
                    safe_text: None,
                }],
                delivery: Some(RecoveryStatus::Absent),
                observation: RecoveryObservation {
                    status: ObservationStatus::Observed,
                    observed_at: Some(observation_timestamp()),
                    error_code: None,
                },
                next_safe_actions,
                ..Recovery::base(draft_id, completed_steps, None)
            }),
            details: Some(json!({
                "missing_fields": if missing { vec!["delivery"] } else { Vec::<&str>::new() },
                "requested_values": requested,
                "allowed_values": allowed,
                "options_available": delivery.available,
                "unavailable_reason": delivery.unavailable_reason,
                "recovery_guidance": if delivery.available {
                    "Select one of the allowed machine values"
                } else {
                    "Open the draft delivery composer in Tori and make delivery options available"
                },
            })),
        }
    }
}

pub(crate) fn completed_steps_have_mutation(completed_steps: &[String]) -> bool {
    completed_steps.iter().any(|step| {
        step == "create_draft"
            || step.starts_with("apply_")
            || step == "copy_fields"
            || step.starts_with("upload_image:")
            || matches!(
                step.as_str(),
                "attach_images"
                    | "update_item_fields"
                    | "apply_price"
                    | "patch_item_fields"
                    | "submit_adinput"
                    | "apply_delivery"
                    | "publish_basic"
                    | "track_confirmation"
            )
    })
}

impl fmt::Debug for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut details = self.details.clone();
        let mut recovery = self
            .recovery
            .as_ref()
            .and_then(|recovery| serde_json::to_value(recovery).ok());
        if let Some(details) = &mut details {
            diagnostics::redact_diagnostic_value(details);
        }
        if let Some(recovery) = &mut recovery {
            diagnostics::redact_diagnostic_value(recovery);
        }
        formatter
            .debug_struct("WorkflowError")
            .field("code", &self.code)
            .field(
                "message",
                &diagnostics::redact_diagnostic_text(&self.message),
            )
            .field("source", &self.source)
            .field("recovery", &recovery)
            .field("details", &details)
            .finish()
    }
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for WorkflowError {}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CreateResult {
    pub draft: DraftState,
    pub completed_steps: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub image_processing: Vec<ImageProcessingReport>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AddImagesResult {
    #[serde(flatten)]
    pub draft: DraftState,
    pub image_processing: Vec<ImageProcessingReport>,
}

impl std::ops::Deref for AddImagesResult {
    type Target = DraftState;

    fn deref(&self) -> &Self::Target {
        &self.draft
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct UpdateResult {
    pub draft: DraftState,
    pub requested_fields: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requested_delivery: Vec<String>,
    pub persisted_fields: Vec<String>,
    pub ignored_fields: Vec<String>,
    pub etag_changed: bool,
    pub completed_steps: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PublishResult {
    pub draft_id: String,
    pub listing_id: String,
    pub revision: String,
    pub state: String,
    pub completed_steps: Vec<String>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub observed_listing: Value,
}
