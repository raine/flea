use super::super::adapter::{AdInputApi, error_at_stage};
use super::super::delivery::allowed_delivery_values;
use super::super::fields::{
    AppliedFieldMutations, FieldBoundary, FieldMutation, FieldMutationKind, FieldOutcomes,
    FieldProgress, RecoveryImageIntent, category_validation_issues, classify_fields,
    field_error_details, field_is_persisted, field_recovery, mutation_is_ambiguous, pending_fields,
    retry_field_action, schema_validation_issues, set_recovery_images,
    structured_validation_issues,
};
use super::super::http::ApiError;
use super::super::recovery::{
    CreateRecoveryContract, ImageRecoveryOperation, ObservationStatus, Recovery,
    RecoveryObservation, RecoveryStatus, WorkflowError, bounded_recovery_text,
    completed_steps_have_mutation,
};
use super::super::types::{DeliveryComposer, DraftState, ValidationEvidenceFailure};
use super::super::validation::delivery_values;
use super::{DraftWorkflow, reconciled_mutation_warning, record_mutation_response_drift};
use crate::diagnostics;
use crate::domain::field::ValidationIssue;
use crate::domain::field::stable_field_key;
use crate::retry::FailureKind;
use crate::retry::OperationMethod;
use crate::retry::RetryContext;
use crate::retry::classify;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;

impl<A: AdInputApi> DraftWorkflow<A> {
    pub(super) fn validation_evidence_failure(
        field: &str,
        failed_stage: &str,
        error: &ApiError,
        command: String,
    ) -> ValidationEvidenceFailure {
        ValidationEvidenceFailure {
            field: field.to_owned(),
            failed_stage: failed_stage.to_owned(),
            code: error.code.clone(),
            upstream_transient: error.upstream_transient,
            safe_to_retry: error.safe_to_retry,
            command,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn recover_after_failure(
        &self,
        mut error: WorkflowError,
        draft_id: &str,
        failed_stage: &str,
        expected_etag: Option<&str>,
        images: &[RecoveryImageIntent],
        delivery: Option<RecoveryStatus>,
        publication: Option<RecoveryStatus>,
    ) -> WorkflowError {
        let mutation_status = error.source.as_ref().and_then(|source| source.status);
        let ambiguous = error.code == "mutation.uncertain"
            || error.source.as_ref().is_some_and(mutation_is_ambiguous);
        let observation = if matches!(failed_stage, "attach_images" | "remove_images") {
            self.api
                .publication_draft(draft_id)
                .await
                .map(|publication| publication.draft)
        } else {
            self.api.get_draft(draft_id).await
        };
        let listing_observation = if failed_stage == "package_choice" && ambiguous {
            Some(self.api.observed_listing(draft_id).await)
        } else {
            None
        };
        let recovery = error
            .recovery
            .get_or_insert_with(|| Recovery::base(draft_id, &[], Some(&error.code)));
        recovery.active_step = Some(failed_stage.to_owned());
        recovery.failed_stage = Some(bounded_recovery_text(failed_stage));
        let failed_mutation = if ambiguous {
            RecoveryStatus::Indeterminate
        } else {
            RecoveryStatus::Rejected
        };
        recovery.item_patch = Some(
            if recovery
                .completed_steps
                .iter()
                .any(|step| step == "patch_item_fields")
            {
                RecoveryStatus::Persisted
            } else if failed_stage == "patch_item_fields" {
                failed_mutation
            } else {
                RecoveryStatus::Unattempted
            },
        );
        recovery.recommerce_update = Some(
            if recovery
                .completed_steps
                .iter()
                .any(|step| step == "update_recommerce")
            {
                RecoveryStatus::Persisted
            } else if failed_stage == "update_recommerce" {
                failed_mutation
            } else {
                RecoveryStatus::Unattempted
            },
        );
        recovery.delivery = delivery;
        recovery.package_choice = Some(
            if recovery
                .completed_steps
                .iter()
                .any(|step| step == "package_choice")
            {
                RecoveryStatus::Persisted
            } else if failed_stage == "package_choice" {
                failed_mutation
            } else {
                RecoveryStatus::Unattempted
            },
        );
        recovery.confirmation = Some(
            if recovery
                .completed_steps
                .iter()
                .any(|step| step == "fetch_confirmation")
            {
                RecoveryStatus::Persisted
            } else {
                RecoveryStatus::Unattempted
            },
        );
        recovery.publication = publication;
        match observation {
            Ok(state) => {
                let changed = mutation_status == Some(412)
                    || expected_etag.is_some_and(|etag| {
                        failed_stage.starts_with("validate_")
                            && !state.etag.is_empty()
                            && state.etag != etag
                    });
                recovery.observe(
                    &state,
                    if changed {
                        ObservationStatus::ChangedByAnotherClient
                    } else {
                        ObservationStatus::Observed
                    },
                );
                recovery.refresh_field_summary();
                set_recovery_images(recovery, images, false);
                let show = format!("flea tori draft show {draft_id}");
                let mut actions = vec![show.clone()];
                if !changed {
                    actions.extend(
                        recovery
                            .next_safe_actions
                            .iter()
                            .filter(|action| **action != show)
                            .cloned(),
                    );
                    if recovery.images.iter().any(|image| {
                        image.operation == ImageRecoveryOperation::Remove
                            && image.status == RecoveryStatus::Absent
                    }) {
                        actions.push(format!(
                            "flea tori draft image remove {draft_id} IMAGE_ID..."
                        ));
                    }
                    if failed_stage == "wait_for_images"
                        && recovery.images.iter().any(|image| {
                            image.operation == ImageRecoveryOperation::Add
                                && image.status == RecoveryStatus::Pending
                        })
                    {
                        actions.push(format!("flea tori draft validate {draft_id}"));
                    }
                } else {
                    recovery.destructive_actions.clear();
                }
                actions.dedup();
                recovery.next_safe_actions = actions;
                if publication == Some(RecoveryStatus::Indeterminate) {
                    recovery.destructive_actions.clear();
                }
            }
            Err(observation_error) => {
                recovery.observation =
                    RecoveryObservation::from_error(&observation_error, "draft_detail");
                recovery.fresh_state = None;
                recovery.destructive_actions.clear();
                recovery.next_safe_actions = vec![format!("flea tori draft show {draft_id}")];
                recovery.refresh_field_summary();
                set_recovery_images(recovery, images, true);
            }
        }
        if let Some(listing_observation) = listing_observation {
            recovery.destructive_actions.clear();
            match listing_observation {
                Ok(_) => {
                    recovery.listing_id = Some(draft_id.to_owned());
                    recovery.package_choice = Some(RecoveryStatus::Persisted);
                    recovery.publication = Some(RecoveryStatus::Persisted);
                    recovery.next_safe_actions = vec![format!("flea tori listing show {draft_id}")];
                }
                Err(listing_error) => {
                    recovery.observation.error_code = Some(listing_error.code);
                    recovery.next_safe_actions = vec![
                        format!("flea tori draft show {draft_id}"),
                        format!("flea tori listing show {draft_id}"),
                    ];
                }
            }
        }
        error
    }

    pub(super) fn post_image_observation_error(
        &self,
        draft: &DraftState,
        completed: &[String],
        intent: &[RecoveryImageIntent],
        error: ApiError,
    ) -> WorkflowError {
        let observation = RecoveryObservation::from_error(&error, "draft_detail");
        let mut workflow = WorkflowError::for_draft(&draft.draft_id, completed, error, false);
        if let Some(recovery) = &mut workflow.recovery {
            recovery.observe(draft, ObservationStatus::Observed);
            recovery.requested_values = draft.values.clone();
            recovery.observation = observation;
            recovery.active_step = Some("observe_attached_images".to_owned());
            recovery.failed_stage = recovery.active_step.clone();
            recovery.safe_to_retry = false;
            recovery.destructive_actions.clear();
            recovery.next_safe_actions = vec![
                format!("flea tori draft show {}", draft.draft_id),
                "flea tori listing list".to_owned(),
            ];
            set_recovery_images(recovery, intent, false);
        }
        workflow
    }

    pub(super) fn add_unattempted_images(error: &mut WorkflowError, start: usize, count: usize) {
        if let Some(recovery) = &mut error.recovery {
            let intent = RecoveryImageIntent::additions(start, count);
            set_recovery_images(recovery, &intent, recovery.fresh_state.is_none());
        }
    }

    pub(super) fn create_preflight_error(issues: Vec<ValidationIssue>) -> WorkflowError {
        let fields = issues
            .iter()
            .map(|issue| issue.field.clone())
            .collect::<Vec<_>>();
        WorkflowError {
            code: "draft.validation_failed".to_owned(),
            message: "Draft input failed pre-allocation validation".to_owned(),
            source: None,
            recovery: None,
            details: Some(json!({
                "stage": "create_preflight",
                "fields": fields,
                "field_errors": issues,
                "allocation": "unattempted",
            })),
        }
    }

    pub(super) fn create_incomplete(
        mut error: WorkflowError,
        draft_id: &str,
        completed: &[String],
        mutations: &[FieldMutation],
        persisted_fields: &[String],
    ) -> WorkflowError {
        let cause_code = error.code.clone();
        let recovery = error
            .recovery
            .get_or_insert_with(|| Recovery::base(draft_id, completed, Some(&cause_code)));
        recovery.create = Some(CreateRecoveryContract {
            allocation: RecoveryStatus::Persisted,
            retry_create: false,
            duplicate_draft_risk: true,
            continuation: "update_existing_draft".to_owned(),
        });
        recovery.safe_to_retry = false;

        let classified = recovery
            .persisted_fields
            .iter()
            .chain(&recovery.absent_fields)
            .chain(&recovery.indeterminate_fields)
            .chain(&recovery.unattempted_fields)
            .cloned()
            .collect::<BTreeSet<_>>();
        for field in mutations
            .iter()
            .flat_map(|mutation| mutation.fields.iter())
            .filter(|field| !classified.contains(*field))
        {
            if persisted_fields.contains(field) {
                recovery.persisted_fields.push(field.clone());
            } else {
                recovery.unattempted_fields.push(field.clone());
            }
        }
        recovery.refresh_field_summary();

        let show = format!("flea tori draft show {draft_id}");
        let mut actions = vec![show];
        let mut resumable_fields = recovery.absent_fields.clone();
        resumable_fields.extend(recovery.unattempted_fields.clone());
        resumable_fields.sort();
        resumable_fields.dedup();
        if !resumable_fields.is_empty() {
            let action = if resumable_fields.len() == 1 {
                retry_field_action(draft_id, &resumable_fields)
            } else {
                format!(
                    "flea tori draft update {draft_id} --input PATH_WITH_ABSENT_AND_UNATTEMPTED_FIELDS"
                )
            };
            actions.push(action);
        }
        if recovery.images.iter().any(|image| {
            image.operation == ImageRecoveryOperation::Add
                && image.status == RecoveryStatus::Unattempted
        }) {
            actions.push(format!(
                "flea tori draft image add {draft_id} --image PATH..."
            ));
        }
        actions.dedup();
        recovery.next_safe_actions = actions;

        let details = error.details.get_or_insert_with(|| json!({}));
        if let Some(details) = details.as_object_mut() {
            details.insert("cause_code".to_owned(), Value::String(cause_code));
            details.insert(
                "allocation".to_owned(),
                Value::String("persisted".to_owned()),
            );
            details.insert("duplicate_draft_risk".to_owned(), Value::Bool(true));
        }
        error.code = "draft.create_incomplete".to_owned();
        error.message =
            "The draft was allocated, but requested work remains; continue with the returned draft ID"
                .to_owned();
        error
    }

    pub(super) fn enrich_validation_recovery(
        mut error: WorkflowError,
        state: &DraftState,
        images: &[RecoveryImageIntent],
    ) -> WorkflowError {
        if let Some(recovery) = &mut error.recovery {
            recovery.observe(state, ObservationStatus::Observed);
            set_recovery_images(recovery, images, false);
            recovery.delivery = recovery
                .field_summary
                .iter()
                .find(|field| field.field == "delivery")
                .map(|field| field.status)
                .or(Some(RecoveryStatus::Persisted));
            recovery.publication = Some(RecoveryStatus::Unattempted);
        }
        error
    }

    fn local_field_validation_error(
        &self,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        issues: Vec<ValidationIssue>,
    ) -> WorkflowError {
        let mut invalid_fields = issues
            .iter()
            .map(|issue| issue.field.clone())
            .collect::<Vec<_>>();
        invalid_fields.sort();
        invalid_fields.dedup();
        let mut persisted = progress.persisted.clone();
        persisted.retain(|field| !invalid_fields.contains(field));
        let mut absent = progress.absent.clone();
        absent.extend(invalid_fields.iter().cloned());
        absent.sort();
        absent.dedup();
        let stage = if invalid_fields == mutation.fields {
            mutation.step.replacen("apply_", "validate_", 1)
        } else {
            "validate_fields".to_owned()
        };
        let mut api = ApiError::new(
            "draft.validation_failed",
            "Draft fields do not match the source-backed composer schema",
        );
        api.details = Some(Box::new(json!({
            "stage": stage,
            "fields": invalid_fields,
            "field_errors": issues,
        })));
        let mut recovery = field_recovery(
            &draft.draft_id,
            completed,
            FieldBoundary {
                step: &stage,
                fields: &invalid_fields,
            },
            FieldOutcomes {
                persisted,
                absent,
                indeterminate: Vec::new(),
                unattempted: pending_fields(mutations, progress, &invalid_fields),
            },
            false,
            false,
            Some(draft.clone()),
            false,
        );
        if mutation.key == "category" {
            let category = match &mutation.value {
                Value::String(value) => value.clone(),
                Value::Number(value) => value.to_string(),
                _ => "QUERY".to_owned(),
            };
            recovery.next_safe_actions = vec![
                format!("flea tori category search {category}"),
                format!("flea tori draft show {}", draft.draft_id),
            ];
        }
        WorkflowError {
            code: api.code.clone(),
            message: api.message.clone(),
            source: Some(api),
            recovery: Some(recovery),
            details: Some(json!({
                "stage": stage,
                "fields": invalid_fields,
                "field_errors": issues,
            })),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn observed_field_error(
        &self,
        draft_before: &DraftState,
        fresh: DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        error: ApiError,
        code: &str,
        message: &str,
        validation: &[ValidationIssue],
        safe_to_retry: bool,
    ) -> WorkflowError {
        let (active_persisted, active_absent) = classify_fields(&fresh, mutation);
        let mut persisted = progress.persisted.clone();
        persisted.extend(active_persisted);
        let mut absent = progress.absent.clone();
        absent.extend(active_absent);
        let observation = json!({
            "status": "succeeded",
            "etag_before": draft_before.etag,
            "etag_after": fresh.etag,
            "etag_changed": draft_before.etag != fresh.etag,
        });
        let mut recovery = field_recovery(
            &draft_before.draft_id,
            completed,
            FieldBoundary {
                step: &mutation.step,
                fields: &mutation.fields,
            },
            FieldOutcomes {
                persisted,
                absent,
                indeterminate: Vec::new(),
                unattempted: pending_fields(mutations, progress, &mutation.fields),
            },
            error.upstream_transient,
            safe_to_retry,
            Some(fresh),
            false,
        );
        if code == "draft.conflict" {
            recovery.observation.status = ObservationStatus::ChangedByAnotherClient;
            recovery.next_safe_actions =
                vec![format!("flea tori draft show {}", draft_before.draft_id)];
            recovery.destructive_actions.clear();
        }
        WorkflowError {
            code: code.to_owned(),
            message: message.to_owned(),
            source: Some(error.clone()),
            recovery: Some(recovery),
            details: Some(field_error_details(
                &mutation.step,
                &mutation.fields,
                &error,
                validation,
                Some(observation),
            )),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn unavailable_field_observation(
        &self,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        mutation_error: ApiError,
        observation_error: ApiError,
    ) -> WorkflowError {
        let validation = structured_validation_issues(&mutation_error, draft);
        let observation = json!({
            "status": "failed",
            "error": {
                "code": observation_error.code,
                "status": observation_error.status,
                "details": observation_error.details,
            },
            "guidance": "Inspect the authoritative draft before retrying any indeterminate field",
        });
        WorkflowError {
            code: "mutation.uncertain".to_owned(),
            message: "A draft field mutation returned an ambiguous response and authoritative state is unavailable".to_owned(),
            source: Some(mutation_error.clone()),
            recovery: Some(field_recovery(
                &draft.draft_id,
                completed,
                FieldBoundary {
                    step: &mutation.step,
                    fields: &mutation.fields,
                },
                FieldOutcomes {
                    persisted: progress.persisted.clone(),
                    absent: progress.absent.clone(),
                    indeterminate: mutation.fields.clone(),
                    unattempted: pending_fields(mutations, progress, &mutation.fields),
                },
                mutation_error.upstream_transient,
                false,
                None,
                true,
            )),
            details: Some(field_error_details(
                &mutation.step,
                &mutation.fields,
                &mutation_error,
                &validation,
                Some(observation),
            )),
        }
    }

    async fn field_mutation_error(
        &self,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        completed: &[String],
        progress: &FieldProgress,
        error: ApiError,
    ) -> Result<DraftState, WorkflowError> {
        let mut validation = structured_validation_issues(&error, draft);
        for issue in &mut validation {
            let candidate = stable_field_key(&issue.field);
            if candidate == mutation.key {
                issue.field = mutation.key.clone();
            } else {
                let attribute = format!("attributes.{candidate}");
                if mutation.fields.contains(&attribute) {
                    issue.field = attribute;
                }
            }
        }
        if error.status == Some(412) {
            return Err(match self.api.get_draft(&draft.draft_id).await {
                Ok(fresh) => {
                    let mut context = RetryContext::mutation(match mutation.kind {
                        FieldMutationKind::Price => OperationMethod::Patch,
                        FieldMutationKind::Delivery => OperationMethod::Post,
                        FieldMutationKind::Composer => OperationMethod::Put,
                    })
                    .with_etag()
                    .with_authoritative_observation();
                    if completed_steps_have_mutation(completed) {
                        context = context.with_completed_mutation_steps();
                    }
                    let classification = classify(FailureKind::PreconditionFailed, context);
                    self.observed_field_error(
                        draft,
                        fresh,
                        mutations,
                        mutation,
                        completed,
                        progress,
                        error,
                        "draft.conflict",
                        "The draft changed while the field update was being applied",
                        &validation,
                        classification.safe_to_retry,
                    )
                }
                Err(observation_error) => self.unavailable_field_observation(
                    draft,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    observation_error,
                ),
            });
        }
        if mutation_is_ambiguous(&error) {
            return match self.api.get_draft(&draft.draft_id).await {
                Ok(fresh) if field_is_persisted(&fresh, mutation) => Ok(fresh),
                Ok(fresh) => Err(self.observed_field_error(
                    draft,
                    fresh,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    "mutation.uncertain",
                    "Authoritative draft state conflicts with the requested mutation",
                    &validation,
                    false,
                )),
                Err(observation_error) => Err(self.unavailable_field_observation(
                    draft,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    observation_error,
                )),
            };
        }

        let (active_persisted, active_absent) = classify_fields(draft, mutation);
        let mut persisted = progress.persisted.clone();
        persisted.extend(active_persisted);
        let mut absent = progress.absent.clone();
        absent.extend(active_absent);
        let is_validation = error
            .status
            .is_some_and(|status| (400..500).contains(&status))
            && !validation.is_empty();
        Err(WorkflowError {
            code: if is_validation {
                "draft.validation_failed".to_owned()
            } else {
                error.code.clone()
            },
            message: if is_validation {
                "Tori rejected one or more draft fields".to_owned()
            } else {
                error.message.clone()
            },
            source: Some(error.clone()),
            recovery: Some(field_recovery(
                &draft.draft_id,
                completed,
                FieldBoundary {
                    step: &mutation.step,
                    fields: &mutation.fields,
                },
                FieldOutcomes {
                    persisted,
                    absent,
                    indeterminate: Vec::new(),
                    unattempted: pending_fields(mutations, progress, &mutation.fields),
                },
                error.upstream_transient,
                false,
                Some(draft.clone()),
                false,
            )),
            details: Some(field_error_details(
                &mutation.step,
                &mutation.fields,
                &error,
                &validation,
                None,
            )),
        })
    }

    fn enrich_field_error(
        &self,
        mut error: WorkflowError,
        draft: &DraftState,
        mutations: &[FieldMutation],
        mutation: &FieldMutation,
        progress: &FieldProgress,
    ) -> WorkflowError {
        let validation_failure = matches!(
            error.code.as_str(),
            "draft.validation_failed"
                | "draft.invalid_delivery"
                | "draft.delivery_options_unavailable"
        );
        if let Some(recovery) = &mut error.recovery {
            recovery.active_step = Some(if validation_failure {
                "validate_delivery".to_owned()
            } else {
                mutation.step.clone()
            });
            recovery.fields = mutation.fields.clone();
            let active_persisted = std::mem::take(&mut recovery.persisted_fields);
            let active_absent = std::mem::take(&mut recovery.absent_fields);
            let active_indeterminate = std::mem::take(&mut recovery.indeterminate_fields);
            recovery.persisted_fields = progress.persisted.clone();
            recovery.persisted_fields.extend(active_persisted);
            recovery.absent_fields = progress.absent.clone();
            recovery.absent_fields.extend(active_absent);
            recovery.indeterminate_fields = active_indeterminate;
            recovery.unattempted_fields = pending_fields(mutations, progress, &mutation.fields);
            if validation_failure {
                if !recovery
                    .absent_fields
                    .iter()
                    .any(|field| mutation.fields.contains(field))
                {
                    recovery.absent_fields.extend(mutation.fields.clone());
                }
                recovery.next_safe_actions = vec![
                    format!("flea tori draft show {}", draft.draft_id),
                    retry_field_action(&draft.draft_id, &recovery.absent_fields),
                ];
            } else if error.code == "mutation.uncertain" {
                if recovery
                    .persisted_fields
                    .iter()
                    .all(|field| !mutation.fields.contains(field))
                    && recovery
                        .absent_fields
                        .iter()
                        .all(|field| !mutation.fields.contains(field))
                    && recovery.indeterminate_fields.is_empty()
                {
                    recovery.indeterminate_fields = mutation.fields.clone();
                }
                recovery.manual_inspection_required = !recovery.indeterminate_fields.is_empty();
                if recovery.manual_inspection_required || recovery.absent_fields.is_empty() {
                    recovery.next_safe_actions =
                        vec![format!("flea tori draft show {}", draft.draft_id)];
                } else {
                    recovery.next_safe_actions = vec![
                        format!("flea tori draft show {}", draft.draft_id),
                        retry_field_action(&draft.draft_id, &recovery.absent_fields),
                    ];
                }
            }
            recovery.refresh_field_summary();
        }
        let details = error.details.get_or_insert_with(|| json!({}));
        if let Some(details) = details.as_object_mut() {
            details.insert(
                "stage".to_owned(),
                Value::String(if validation_failure {
                    "validate_delivery".to_owned()
                } else {
                    mutation.step.clone()
                }),
            );
            details.insert(
                "fields".to_owned(),
                Value::Array(mutation.fields.iter().cloned().map(Value::String).collect()),
            );
        }
        error
    }

    pub(super) async fn apply_field_mutations(
        &self,
        mut draft: DraftState,
        mutations: Vec<FieldMutation>,
        completed: &mut Vec<String>,
        workflow: &str,
        listing_id: Option<&str>,
    ) -> Result<AppliedFieldMutations, WorkflowError> {
        let mut progress = FieldProgress::default();
        let mut warnings = Vec::new();
        let category_issues =
            if let Some(mutation) = mutations.iter().find(|mutation| mutation.key == "category") {
                let categories = self.api.publication_categories().await.map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, completed, error, true)
                        .with_optional_source_listing_id(listing_id)
                })?;
                completed.push("fetch_category_taxonomy".to_owned());
                category_validation_issues(&draft, mutation, &categories)
            } else {
                Vec::new()
            };
        let mut local_validation = None;
        let mut local_issues = Vec::new();
        for mutation in &mutations {
            let issues = if mutation.key == "category" {
                category_issues.clone()
            } else {
                schema_validation_issues(&draft, mutation)
            };
            if !issues.is_empty() {
                local_validation.get_or_insert_with(|| mutation.clone());
                local_issues.extend(issues);
                progress.absent.extend(mutation.fields.clone());
                continue;
            }
            let context = diagnostics::WorkflowContext {
                workflow,
                step: &mutation.step,
                draft_id: Some(&draft.draft_id),
                listing_id,
                fields: &mutation.fields,
            };
            diagnostics::workflow_step(&context, "started");
            match mutation.kind {
                FieldMutationKind::Composer => {
                    let mut values = draft.values.clone();
                    values.insert(mutation.key.clone(), mutation.value.clone());
                    match self
                        .api
                        .update_item(&draft.draft_id, &draft.etag, &values)
                        .await
                    {
                        Ok(updated) => {
                            diagnostics::workflow_step(&context, "completed");
                            completed.push(mutation.step.clone());
                            let (persisted, absent) = classify_fields(&updated, mutation);
                            progress.persisted.extend(persisted);
                            progress.absent.extend(absent);
                            draft = updated;
                        }
                        Err(error) => {
                            let response_model_drift = error
                                .status
                                .is_some_and(|status| (200..300).contains(&status));
                            record_mutation_response_drift(&context, &error);
                            match self
                                .field_mutation_error(
                                    &draft, &mutations, mutation, completed, &progress, error,
                                )
                                .await
                            {
                                Ok(fresh) => {
                                    diagnostics::workflow_step(&context, "reconciled");
                                    completed.push(mutation.step.clone());
                                    completed.push(format!("observe_{}", mutation.key));
                                    progress.persisted.extend(mutation.fields.clone());
                                    warnings.push(reconciled_mutation_warning(
                                        &mutation.fields,
                                        response_model_drift,
                                    ));
                                    draft = fresh;
                                }
                                Err(error) => {
                                    diagnostics::workflow_step(&context, "failed");
                                    return Err(error.with_optional_source_listing_id(listing_id));
                                }
                            }
                        }
                    }
                }
                FieldMutationKind::Price => {
                    match self
                        .api
                        .update_sale_price(&draft.draft_id, &draft.etag, &mutation.value)
                        .await
                    {
                        Ok(_) => {
                            completed.push(mutation.step.clone());
                            match self.api.get_draft(&draft.draft_id).await {
                                Ok(fresh) if field_is_persisted(&fresh, mutation) => {
                                    diagnostics::workflow_step(&context, "completed");
                                    completed.push("observe_price".to_owned());
                                    progress.persisted.push("price".to_owned());
                                    draft = fresh;
                                }
                                Ok(fresh) => {
                                    diagnostics::workflow_step(&context, "failed");
                                    let mut error = ApiError::new(
                                        "mutation.uncertain",
                                        "The authoritative draft price does not match the requested price",
                                    );
                                    error.details = Some(Box::new(json!({
                                        "stage": "observe_price",
                                        "requested_price": mutation.value,
                                        "observed_price": fresh.values.get("price"),
                                    })));
                                    return Err(self
                                        .observed_field_error(
                                            &draft,
                                            fresh,
                                            &mutations,
                                            mutation,
                                            completed,
                                            &progress,
                                            error,
                                            "mutation.uncertain",
                                            "The authoritative draft price does not match the requested price",
                                            &[],
                                            false,
                                        )
                                        .with_optional_source_listing_id(listing_id));
                                }
                                Err(observation_error) => {
                                    diagnostics::workflow_step(&context, "failed");
                                    let mutation_error = ApiError::new(
                                        "mutation.uncertain",
                                        "The price mutation succeeded but authoritative state is unavailable",
                                    );
                                    return Err(self
                                        .unavailable_field_observation(
                                            &draft,
                                            &mutations,
                                            mutation,
                                            completed,
                                            &progress,
                                            mutation_error,
                                            error_at_stage(observation_error, "observe_price"),
                                        )
                                        .with_optional_source_listing_id(listing_id));
                                }
                            }
                        }
                        Err(error) => {
                            let response_model_drift = error
                                .status
                                .is_some_and(|status| (200..300).contains(&status));
                            record_mutation_response_drift(&context, &error);
                            match self
                                .field_mutation_error(
                                    &draft, &mutations, mutation, completed, &progress, error,
                                )
                                .await
                            {
                                Ok(fresh) => {
                                    diagnostics::workflow_step(&context, "reconciled");
                                    completed.push(mutation.step.clone());
                                    completed.push(format!("observe_{}", mutation.key));
                                    progress.persisted.extend(mutation.fields.clone());
                                    warnings.push(reconciled_mutation_warning(
                                        &mutation.fields,
                                        response_model_drift,
                                    ));
                                    draft = fresh;
                                }
                                Err(error) => {
                                    diagnostics::workflow_step(&context, "failed");
                                    return Err(error.with_optional_source_listing_id(listing_id));
                                }
                            }
                        }
                    }
                }
                FieldMutationKind::Delivery => {
                    match self
                        .apply_delivery_selection(draft.clone(), &mutation.value, completed)
                        .await
                    {
                        Ok(updated) => {
                            diagnostics::workflow_step(&context, "completed");
                            progress.persisted.extend(mutation.fields.clone());
                            draft = updated;
                        }
                        Err(error) => {
                            diagnostics::workflow_step(&context, "failed");
                            return Err(self
                                .enrich_field_error(error, &draft, &mutations, mutation, &progress)
                                .with_optional_source_listing_id(listing_id));
                        }
                    }
                }
            }
        }
        progress.persisted.sort();
        progress.persisted.dedup();
        progress.absent.sort();
        progress.absent.dedup();
        if let Some(mutation) = local_validation {
            return Err(self
                .local_field_validation_error(
                    &draft,
                    &mutations,
                    &mutation,
                    completed,
                    &progress,
                    local_issues,
                )
                .with_optional_source_listing_id(listing_id));
        }
        Ok(AppliedFieldMutations {
            draft,
            progress,
            warnings,
        })
    }

    pub(super) async fn delivery_composer(
        &self,
        draft_id: &str,
        completed: &[String],
    ) -> Result<DeliveryComposer, WorkflowError> {
        self.api
            .delivery_composer(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, completed, error, true))
    }

    async fn apply_delivery_selection(
        &self,
        mut state: DraftState,
        requested: &Value,
        completed: &mut Vec<String>,
    ) -> Result<DraftState, WorkflowError> {
        let draft_id = state.draft_id.clone();
        let composer = self.delivery_composer(&draft_id, completed).await?;
        completed.push("fetch_delivery_options".to_owned());
        let requested_values = delivery_values(requested).unwrap_or_default();
        let selected = requested_values
            .first()
            .filter(|_| requested_values.len() == 1)
            .filter(|requested| {
                composer
                    .state
                    .options
                    .iter()
                    .any(|option| option.value.as_str() == requested.as_str())
            })
            .cloned()
            .ok_or_else(|| {
                WorkflowError::delivery_validation(
                    &draft_id,
                    completed,
                    &composer.state,
                    requested_values.clone(),
                )
            })?;
        if let Err(error) = self
            .api
            .apply_delivery(&draft_id, &composer, &selected)
            .await
        {
            if mutation_is_ambiguous(&error) {
                return match self.delivery_composer(&draft_id, completed).await {
                    Ok(observed) => {
                        let persisted = observed.state.selected == [selected.clone()];
                        state.delivery = Some(observed.state);
                        let mut workflow = WorkflowError::for_draft(
                            &draft_id,
                            completed,
                            error_at_stage(error, "apply_delivery"),
                            false,
                        );
                        if let Some(recovery) = &mut workflow.recovery {
                            recovery.active_step = Some("apply_delivery".to_owned());
                            recovery.failed_stage = Some("apply_delivery".to_owned());
                            recovery.fields = vec!["delivery".to_owned()];
                            if persisted {
                                recovery.persisted_fields = vec!["delivery".to_owned()];
                                recovery.delivery = Some(RecoveryStatus::Persisted);
                            } else {
                                recovery.absent_fields = vec!["delivery".to_owned()];
                                recovery.delivery = Some(RecoveryStatus::Absent);
                                recovery.next_safe_actions = vec![
                                    format!("flea tori draft show {draft_id}"),
                                    retry_field_action(&draft_id, &recovery.absent_fields),
                                ];
                            }
                            recovery.observe(&state, ObservationStatus::Observed);
                            recovery.refresh_field_summary();
                        }
                        Err(workflow)
                    }
                    Err(observation_error) => {
                        let mut workflow = WorkflowError::for_draft(
                            &draft_id,
                            completed,
                            error_at_stage(error, "apply_delivery"),
                            false,
                        );
                        if let Some(recovery) = &mut workflow.recovery {
                            recovery.active_step = Some("apply_delivery".to_owned());
                            recovery.failed_stage = Some("apply_delivery".to_owned());
                            recovery.fields = vec!["delivery".to_owned()];
                            recovery.indeterminate_fields = vec!["delivery".to_owned()];
                            recovery.delivery = Some(RecoveryStatus::Indeterminate);
                            recovery.manual_inspection_required = true;
                            recovery.observation.error_code = Some(observation_error.code.clone());
                            recovery.refresh_field_summary();
                        }
                        workflow.details = Some(json!({
                            "stage": "apply_delivery",
                            "fields": ["delivery"],
                            "observation": {
                                "status": "failed",
                                "error": {
                                    "code": observation_error.code,
                                    "status": observation_error.source.as_ref().and_then(|source| source.status),
                                }
                            }
                        }));
                        Err(workflow)
                    }
                };
            }
            let workflow = WorkflowError::for_draft(
                &draft_id,
                completed,
                error_at_stage(error, "apply_delivery"),
                false,
            );
            return Err(self
                .recover_after_failure(
                    workflow,
                    &draft_id,
                    "apply_delivery",
                    Some(&state.etag),
                    &[],
                    Some(RecoveryStatus::Rejected),
                    Some(RecoveryStatus::Unattempted),
                )
                .await);
        }
        completed.push("apply_delivery".to_owned());
        let observed = self.delivery_composer(&draft_id, completed).await?;
        if observed.state.selected != [selected.clone()] {
            let mut error = ApiError::new(
                "mutation.uncertain",
                "Tori accepted the delivery mutation without returning the requested state",
            );
            error.details = Some(Box::new(json!({
                "requested_values": [selected],
                "observed_values": observed.state.selected.clone(),
                "allowed_values": allowed_delivery_values(&observed.state),
                "recovery_guidance": format!("Inspect the draft with `flea tori draft show {draft_id}`; do not repeat publication")
            })));
            return Err(WorkflowError::for_draft(&draft_id, completed, error, false));
        }
        completed.push("observe_delivery".to_owned());
        state.delivery = Some(observed.state);
        Ok(state)
    }
}
