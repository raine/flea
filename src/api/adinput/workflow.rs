use super::{
    adapter::*, delivery::*, fields::*, http::*, images::*, normalization::*, recovery::*,
    types::*, validation::*, *,
};

pub struct DraftWorkflow<A> {
    api: A,
    config: WorkflowConfig,
}

impl<A> DraftWorkflow<A> {
    pub fn new(api: A, config: WorkflowConfig) -> Self {
        Self { api, config }
    }
}

impl<A: AdInputApi> DraftWorkflow<A> {
    async fn recover_after_failure(
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
        let observation = self.api.get_draft(draft_id).await;
        let recovery = error
            .recovery
            .get_or_insert_with(|| Recovery::base(draft_id, &[], Some(&error.code)));
        recovery.active_step = Some(failed_stage.to_owned());
        recovery.failed_stage = Some(bounded_recovery_text(failed_stage));
        recovery.delivery = delivery;
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
                let show = format!("flea draft show {draft_id}");
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
                        actions.push(format!("flea draft image remove {draft_id} IMAGE_ID..."));
                    }
                    if failed_stage == "wait_for_images"
                        && recovery.images.iter().any(|image| {
                            image.operation == ImageRecoveryOperation::Add
                                && image.status == RecoveryStatus::Pending
                        })
                    {
                        actions.push(format!("flea draft publish {draft_id}"));
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
                recovery.observation = RecoveryObservation {
                    status: ObservationStatus::Unavailable,
                    observed_at: None,
                    error_code: Some(observation_error.code),
                };
                recovery.fresh_state = None;
                recovery.destructive_actions.clear();
                recovery.next_safe_actions = vec![format!("flea draft show {draft_id}")];
                recovery.refresh_field_summary();
                set_recovery_images(recovery, images, true);
            }
        }
        error
    }

    fn add_unattempted_images(error: &mut WorkflowError, start: usize, count: usize) {
        if let Some(recovery) = &mut error.recovery {
            let intent = RecoveryImageIntent::additions(start, count);
            set_recovery_images(recovery, &intent, recovery.fresh_state.is_none());
        }
    }

    fn enrich_validation_recovery(
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
        let (active_persisted, active_absent) = classify_fields(draft, mutation);
        let mut persisted = progress.persisted.clone();
        persisted.extend(active_persisted);
        let mut absent = progress.absent.clone();
        absent.extend(active_absent);
        let stage = mutation.step.replacen("apply_", "validate_", 1);
        let mut api = ApiError::new(
            "draft.validation_failed",
            "Draft fields do not match the source-backed composer schema",
        );
        api.details = Some(Box::new(json!({
            "stage": stage,
            "fields": mutation.fields,
            "field_errors": issues,
        })));
        WorkflowError {
            code: api.code.clone(),
            message: api.message.clone(),
            source: Some(api),
            recovery: Some(field_recovery(
                &draft.draft_id,
                completed,
                FieldBoundary {
                    step: &stage,
                    fields: &mutation.fields,
                },
                FieldOutcomes {
                    persisted,
                    absent,
                    indeterminate: Vec::new(),
                    unattempted: pending_fields(mutations, progress, &mutation.fields),
                },
                false,
                false,
                Some(draft.clone()),
                false,
            )),
            details: Some(json!({
                "stage": stage,
                "fields": mutation.fields,
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
            recovery.next_safe_actions = vec![format!("flea draft show {}", draft_before.draft_id)];
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
    ) -> WorkflowError {
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
            return match self.api.get_draft(&draft.draft_id).await {
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
            };
        }
        if mutation_is_ambiguous(&error) {
            return match self.api.get_draft(&draft.draft_id).await {
                Ok(fresh) => self.observed_field_error(
                    draft,
                    fresh,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    "mutation.uncertain",
                    "A draft field mutation returned an ambiguous response",
                    &validation,
                    false,
                ),
                Err(observation_error) => self.unavailable_field_observation(
                    draft,
                    mutations,
                    mutation,
                    completed,
                    progress,
                    error,
                    observation_error,
                ),
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
        WorkflowError {
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
        }
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
                    format!("flea draft show {}", draft.draft_id),
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
                        vec![format!("flea draft show {}", draft.draft_id)];
                } else {
                    recovery.next_safe_actions = vec![
                        format!("flea draft show {}", draft.draft_id),
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

    async fn apply_field_mutations(
        &self,
        mut draft: DraftState,
        mutations: Vec<FieldMutation>,
        completed: &mut Vec<String>,
        workflow: &str,
        listing_id: Option<&str>,
    ) -> Result<AppliedFieldMutations, WorkflowError> {
        let mut progress = FieldProgress::default();
        let category_first = mutations
            .first()
            .is_some_and(|mutation| mutation.key == "category");
        let initial_validation_end = if category_first { 1 } else { mutations.len() };
        for mutation in &mutations[..initial_validation_end] {
            let issues = schema_validation_issues(&draft, mutation);
            if !issues.is_empty() {
                return Err(self
                    .local_field_validation_error(
                        &draft, &mutations, mutation, completed, &progress, issues,
                    )
                    .with_optional_source_listing_id(listing_id));
            }
        }

        for (index, mutation) in mutations.iter().enumerate() {
            if category_first && index == 1 {
                for pending in &mutations[index..] {
                    let issues = schema_validation_issues(&draft, pending);
                    if !issues.is_empty() {
                        return Err(self
                            .local_field_validation_error(
                                &draft, &mutations, pending, completed, &progress, issues,
                            )
                            .with_optional_source_listing_id(listing_id));
                    }
                }
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
                            diagnostics::workflow_step(&context, "failed");
                            return Err(self
                                .field_mutation_error(
                                    &draft, &mutations, mutation, completed, &progress, error,
                                )
                                .await
                                .with_optional_source_listing_id(listing_id));
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
                                Ok(fresh) if field_is_persisted(&fresh, mutation, "price") => {
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
                            diagnostics::workflow_step(&context, "failed");
                            return Err(self
                                .field_mutation_error(
                                    &draft, &mutations, mutation, completed, &progress, error,
                                )
                                .await
                                .with_optional_source_listing_id(listing_id));
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
        Ok(AppliedFieldMutations { draft, progress })
    }

    async fn delivery_composer(
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
                                    format!("flea draft show {draft_id}"),
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
                "recovery_guidance": format!("Inspect the draft with `flea draft show {draft_id}`; do not repeat publication")
            })));
            return Err(WorkflowError::for_draft(&draft_id, completed, error, false));
        }
        completed.push("observe_delivery".to_owned());
        state.delivery = Some(observed.state);
        Ok(state)
    }

    pub async fn create(
        &self,
        values: Map<String, Value>,
        image_paths: &[impl AsRef<Path>],
    ) -> Result<CreateResult, WorkflowError> {
        let mut images = Vec::with_capacity(image_paths.len());
        for path in image_paths {
            match prepare_image(path.as_ref()) {
                Ok(image) => images.push(image),
                Err(error) => return Err(WorkflowError::before_creation(error)),
            }
        }
        self.create_prepared(values, images).await
    }

    pub async fn create_prepared(
        &self,
        values: Map<String, Value>,
        images: Vec<PreparedImage>,
    ) -> Result<CreateResult, WorkflowError> {
        requested_sale_price(&values).map_err(WorkflowError::input)?;
        let draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["create_draft".to_owned()];
        let applied = match self
            .apply_field_mutations(
                draft,
                ordered_field_mutations(values),
                &mut completed,
                "draft_create",
                None,
            )
            .await
        {
            Ok(applied) => applied,
            Err(mut error) => {
                Self::add_unattempted_images(&mut error, 0, images.len());
                return Err(error);
            }
        };
        let mut draft = applied.draft;
        let mut image_processing = Vec::new();
        if !images.is_empty() {
            let result = self
                .add_prepared_images(&draft, images, &mut completed)
                .await?;
            draft = result.draft;
            image_processing = result.image_processing;
        }
        Ok(CreateResult {
            draft,
            completed_steps: completed,
            image_processing,
        })
    }

    pub async fn create_from_listing(
        &self,
        listing_id: &str,
    ) -> Result<CreateResult, WorkflowError> {
        let seed = self
            .api
            .source_listing(listing_id)
            .await
            .map_err(WorkflowError::before_creation)?;
        requested_sale_price(&seed.values).map_err(WorkflowError::input)?;
        let source_image_count = seed.images.len();
        let draft = self
            .api
            .create_draft()
            .await
            .map_err(WorkflowError::before_creation)?;
        let mut completed = vec!["load_source_listing".to_owned(), "create_draft".to_owned()];
        let applied = match self
            .apply_field_mutations(
                draft,
                ordered_field_mutations(seed.values),
                &mut completed,
                "draft_create_from_listing",
                Some(listing_id),
            )
            .await
        {
            Ok(applied) => applied,
            Err(mut error) => {
                Self::add_unattempted_images(&mut error, 0, source_image_count);
                return Err(error);
            }
        };
        let mut draft = applied.draft;

        let mut ordered = Vec::new();
        let mut image_processing = Vec::new();
        for source in seed.images {
            let image = prepare_image_bytes(source.bytes).map_err(|error| {
                WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                    .with_source_listing_id(listing_id)
            })?;
            image_processing.push(image.processing_report().clone());
            let uploaded = self
                .api
                .upload_image(
                    &draft.draft_id,
                    &image.file_name,
                    image.bytes,
                    image.width,
                    image.height,
                )
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                        .with_source_listing_id(listing_id)
                })?;
            ordered.push(uploaded);
            completed.push(format!("upload_image:{}", ordered.len() - 1));
        }
        if !ordered.is_empty() {
            draft = self
                .api
                .set_images(&draft.draft_id, &draft.etag, &draft.values, &ordered)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(&draft.draft_id, &completed, error, false)
                        .with_source_listing_id(listing_id)
                })?;
            completed.push("attach_images".to_owned());
        }
        Ok(CreateResult {
            draft,
            completed_steps: completed,
            image_processing,
        })
    }

    pub async fn show(&self, draft_id: &str) -> Result<DraftState, WorkflowError> {
        let mut state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        if state.category_is_unset() && !state.images.is_empty() {
            state.predictions = self
                .api
                .category_predictions(draft_id)
                .await
                .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
            completed.push("fetch_category_predictions".to_owned());
        }
        let composer = self.delivery_composer(draft_id, &completed).await?;
        completed.push("fetch_delivery_options".to_owned());
        attach_delivery_model(&mut state, &composer)
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        Ok(state)
    }

    pub async fn validate(&self, draft_id: &str) -> Result<PublicationValidation, WorkflowError> {
        let publication = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut state = publication.draft;
        let delivery_verifiable = match self.api.delivery_composer(draft_id).await {
            Ok(composer) => attach_delivery_model(&mut state, &composer).is_ok(),
            Err(_) => false,
        };
        let categories = self.api.publication_categories().await.ok();
        Ok(evaluate_publication(
            &state,
            categories.as_deref(),
            publication.composer_model,
            delivery_verifiable,
        ))
    }

    pub async fn update(
        &self,
        draft_id: &str,
        patch: &Map<String, Value>,
    ) -> Result<UpdateResult, WorkflowError> {
        let current = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        let mut requested_values = current.values.clone();
        requested_values.extend(patch.clone());
        requested_values.remove("delivery");
        if patch.contains_key("price") {
            requested_sale_price(&requested_values)
                .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, false))?;
        }
        let requested_delivery = patch
            .get("delivery")
            .and_then(delivery_values)
            .unwrap_or_default();
        let applied = self
            .apply_field_mutations(
                current.clone(),
                ordered_field_mutations(patch.clone()),
                &mut completed,
                "draft_update",
                None,
            )
            .await?;
        let mut requested_fields = patch.keys().cloned().collect::<Vec<_>>();
        requested_fields.sort();
        Ok(UpdateResult {
            etag_changed: applied.draft.etag != current.etag,
            draft: applied.draft,
            requested_fields,
            requested_delivery,
            persisted_fields: applied.progress.persisted,
            ignored_fields: applied.progress.absent,
            completed_steps: completed,
        })
    }

    pub async fn delete(&self, draft_id: &str) -> Result<(), WorkflowError> {
        self.api
            .delete_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, false))
    }

    pub async fn add_images(
        &self,
        draft_id: &str,
        paths: &[impl AsRef<Path>],
    ) -> Result<AddImagesResult, WorkflowError> {
        let state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut completed = vec!["fetch_draft".to_owned()];
        self.add_images_from_paths(&state, paths, &mut completed)
            .await
    }

    async fn add_images_from_paths(
        &self,
        state: &DraftState,
        paths: &[impl AsRef<Path>],
        completed: &mut Vec<String>,
    ) -> Result<AddImagesResult, WorkflowError> {
        let start = state.images.len();
        let mut images = Vec::with_capacity(paths.len());
        for (offset, path) in paths.iter().enumerate() {
            match prepare_image(path.as_ref()) {
                Ok(image) => images.push(image),
                Err(error) => {
                    let mut intent = RecoveryImageIntent::additions(start, paths.len());
                    intent[offset].upload = RecoveryAttempt::Attempting;
                    let mut workflow =
                        WorkflowError::for_draft(&state.draft_id, completed, error, false);
                    if let Some(recovery) = &mut workflow.recovery {
                        recovery.active_step = Some(format!("upload_image:{}", start + offset));
                        recovery.failed_stage = recovery.active_step.clone();
                        recovery.observe(state, ObservationStatus::Observed);
                        set_recovery_images(recovery, &intent, false);
                    }
                    return Err(workflow);
                }
            }
        }
        self.add_prepared_images(state, images, completed).await
    }

    async fn add_prepared_images(
        &self,
        state: &DraftState,
        images: Vec<PreparedImage>,
        completed: &mut Vec<String>,
    ) -> Result<AddImagesResult, WorkflowError> {
        let mut existing = state.images.iter().collect::<Vec<_>>();
        existing.sort_by_key(|image| image.position);
        let mut ordered: Vec<UploadedImage> = existing
            .into_iter()
            .map(uploaded_from_draft_image)
            .collect();
        let start = ordered.len();
        let mut intent = RecoveryImageIntent::additions(start, images.len());
        let mut image_processing = Vec::with_capacity(images.len());
        for (offset, image) in images.into_iter().enumerate() {
            image_processing.push(image.processing_report().clone());
            intent[offset].upload = RecoveryAttempt::Attempting;
            let image_index = start + offset;
            let uploaded = match self
                .api
                .upload_image(
                    &state.draft_id,
                    &image.file_name,
                    image.bytes,
                    image.width,
                    image.height,
                )
                .await
            {
                Ok(uploaded) => uploaded,
                Err(error) => {
                    let workflow =
                        WorkflowError::for_draft(&state.draft_id, completed, error, false);
                    return Err(self
                        .recover_after_failure(
                            workflow,
                            &state.draft_id,
                            &format!("upload_image:{image_index}"),
                            Some(&state.etag),
                            &intent,
                            None,
                            None,
                        )
                        .await);
                }
            };
            intent[offset].upload = RecoveryAttempt::Completed;
            intent[offset].image_id = Some(uploaded.image_id.clone());
            ordered.push(uploaded);
            completed.push(format!("upload_image:{image_index}"));
        }
        for image in &mut intent {
            image.attachment = RecoveryAttempt::Attempting;
        }
        let updated = match self
            .api
            .set_images(&state.draft_id, &state.etag, &state.values, &ordered)
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                let workflow = WorkflowError::for_draft(&state.draft_id, completed, error, false);
                return Err(self
                    .recover_after_failure(
                        workflow,
                        &state.draft_id,
                        "attach_images",
                        Some(&state.etag),
                        &intent,
                        None,
                        None,
                    )
                    .await);
            }
        };
        completed.push("attach_images".to_owned());
        Ok(AddImagesResult {
            draft: updated,
            image_processing,
        })
    }

    pub async fn remove_images(
        &self,
        draft_id: &str,
        remove: &[String],
    ) -> Result<DraftState, WorkflowError> {
        let state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut retained = state
            .images
            .iter()
            .filter(|image| !remove.contains(&image.image_id))
            .collect::<Vec<_>>();
        retained.sort_by_key(|image| image.position);
        let ordered: Vec<UploadedImage> = retained
            .into_iter()
            .map(uploaded_from_draft_image)
            .collect();
        match self
            .api
            .set_images(draft_id, &state.etag, &state.values, &ordered)
            .await
        {
            Ok(updated) => Ok(updated),
            Err(error) => {
                let workflow =
                    WorkflowError::for_draft(draft_id, &["fetch_draft".to_owned()], error, false);
                let intent = RecoveryImageIntent::removals(remove);
                Err(self
                    .recover_after_failure(
                        workflow,
                        draft_id,
                        "remove_images",
                        Some(&state.etag),
                        &intent,
                        None,
                        None,
                    )
                    .await)
            }
        }
    }

    pub async fn publish(&self, draft_id: &str) -> Result<PublishResult, WorkflowError> {
        let mut completed = Vec::new();
        let publication = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        let composer_model = publication.composer_model;
        let mut state = publication.draft;
        let publication_images = observed_image_intent(&state);
        completed.push("fetch_draft".to_owned());

        let composer = self.api.delivery_composer(draft_id).await.ok();
        let mut delivery_verifiable = match composer.as_ref() {
            Some(composer) if attach_delivery_model(&mut state, composer).is_ok() => {
                completed.push("fetch_delivery_options".to_owned());
                true
            }
            _ => false,
        };
        let categories = self.api.publication_categories().await.ok();
        if categories.is_some() {
            completed.push("fetch_category_taxonomy".to_owned());
        }
        let report = evaluate_publication(
            &state,
            categories.as_deref(),
            composer_model,
            delivery_verifiable,
        );
        if !report.missing.is_empty()
            || !report.invalid.is_empty()
            || !report.unverifiable.is_empty()
        {
            return Err(Self::enrich_validation_recovery(
                WorkflowError::validation(&completed, report),
                &state,
                &publication_images,
            ));
        }
        completed.push("validate".to_owned());

        state = self.wait_for_images(state, &completed).await?;
        completed.push("wait_for_images".to_owned());
        if delivery_verifiable && state.delivery.is_none() {
            delivery_verifiable = composer
                .as_ref()
                .is_some_and(|composer| attach_delivery_model(&mut state, composer).is_ok());
        }
        let report = evaluate_publication(
            &state,
            categories.as_deref(),
            composer_model,
            delivery_verifiable,
        );
        if !report.ready {
            return Err(Self::enrich_validation_recovery(
                WorkflowError::validation(&completed, report),
                &state,
                &publication_images,
            ));
        }
        let composer = composer.expect("ready publication has a delivery composer");
        let requested_delivery = composer.state.selected.clone();
        let delivery = requested_delivery
            .first()
            .expect("ready publication has one delivery selection")
            .clone();

        state.values.remove("delivery");
        if let Err(error) = self
            .api
            .update_item(draft_id, &state.etag, &state.values)
            .await
        {
            let workflow = WorkflowError::for_draft(draft_id, &completed, error, false);
            return Err(self
                .recover_after_failure(
                    workflow,
                    draft_id,
                    "patch_item_fields",
                    Some(&state.etag),
                    &publication_images,
                    Some(RecoveryStatus::Pending),
                    Some(RecoveryStatus::Unattempted),
                )
                .await);
        }
        completed.push("patch_item_fields".to_owned());

        state = match self.api.get_draft(draft_id).await {
            Ok(state) => state,
            Err(error) => {
                let workflow = WorkflowError::for_draft(draft_id, &completed, error, true);
                return Err(self
                    .recover_after_failure(
                        workflow,
                        draft_id,
                        "fetch_fresh_etag",
                        Some(&state.etag),
                        &publication_images,
                        Some(RecoveryStatus::Pending),
                        Some(RecoveryStatus::Unattempted),
                    )
                    .await);
            }
        };
        completed.push("fetch_fresh_etag".to_owned());

        state = match self.api.submit_adinput(draft_id, &state.etag, &state).await {
            Ok(state) => state,
            Err(error) => {
                let workflow = WorkflowError::for_draft(draft_id, &completed, error, false);
                return Err(self
                    .recover_after_failure(
                        workflow,
                        draft_id,
                        "submit_adinput",
                        Some(&state.etag),
                        &publication_images,
                        Some(RecoveryStatus::Pending),
                        Some(RecoveryStatus::Unattempted),
                    )
                    .await);
            }
        };
        completed.push("submit_adinput".to_owned());

        let revision = state.revision.clone().ok_or_else(|| {
            WorkflowError::for_draft(
                draft_id,
                &completed,
                model_error(
                    "listing_composer",
                    "$.ad.revision",
                    "draft revision is unavailable",
                ),
                false,
            )
        })?;
        if let Err(error) = self
            .api
            .apply_delivery(draft_id, &composer, &delivery)
            .await
        {
            let delivery_status = if mutation_is_ambiguous(&error) {
                RecoveryStatus::Indeterminate
            } else {
                RecoveryStatus::Rejected
            };
            let workflow = WorkflowError::for_draft(draft_id, &completed, error, false);
            return Err(self
                .recover_after_failure(
                    workflow,
                    draft_id,
                    "apply_delivery",
                    Some(&state.etag),
                    &publication_images,
                    Some(delivery_status),
                    Some(RecoveryStatus::Unattempted),
                )
                .await);
        }
        completed.push("apply_delivery".to_owned());
        let observed_delivery = self.delivery_composer(draft_id, &completed).await?;
        if observed_delivery.state.selected != [delivery.clone()] {
            let mut error = ApiError::new(
                "mutation.uncertain",
                "Tori accepted the delivery mutation without returning the requested state",
            );
            error.details = Some(Box::new(json!({
                "requested_values": [delivery],
                "observed_values": observed_delivery.state.selected.clone(),
                "allowed_values": allowed_delivery_values(&observed_delivery.state),
                "recovery_guidance": format!("Inspect the draft with `flea draft show {draft_id}`; do not repeat publication")
            })));
            let workflow = WorkflowError::for_draft(draft_id, &completed, error, false);
            return Err(self
                .recover_after_failure(
                    workflow,
                    draft_id,
                    "observe_delivery",
                    Some(&state.etag),
                    &publication_images,
                    Some(RecoveryStatus::Indeterminate),
                    Some(RecoveryStatus::Unattempted),
                )
                .await);
        }
        completed.push("observe_delivery".to_owned());

        let context = match self.api.product_context(draft_id, &revision).await {
            Ok(context) => context,
            Err(error) => {
                let workflow = WorkflowError::for_draft(draft_id, &completed, error, true);
                return Err(self
                    .recover_after_failure(
                        workflow,
                        draft_id,
                        "fetch_product_context",
                        Some(&state.etag),
                        &publication_images,
                        Some(RecoveryStatus::Persisted),
                        Some(RecoveryStatus::Unattempted),
                    )
                    .await);
            }
        };
        completed.push("fetch_product_context".to_owned());

        let publication = match self.api.publish_basic(draft_id, &context).await {
            Ok(publication) => publication,
            Err(error) => {
                let publication_status = if mutation_is_ambiguous(&error) {
                    RecoveryStatus::Indeterminate
                } else {
                    RecoveryStatus::Rejected
                };
                let workflow = WorkflowError::for_draft(draft_id, &completed, error, false);
                return Err(self
                    .recover_after_failure(
                        workflow,
                        draft_id,
                        "publish_basic",
                        Some(&state.etag),
                        &publication_images,
                        Some(RecoveryStatus::Persisted),
                        Some(publication_status),
                    )
                    .await);
            }
        };
        completed.push("publish_basic".to_owned());

        let mut warnings = Vec::new();
        match self.api.confirmation(&publication.listing_id).await {
            Ok(confirmation) => {
                completed.push("fetch_confirmation".to_owned());
                if let Err(error) = self.api.track_confirmation(&confirmation).await {
                    warnings.push(format!("confirmation tracking failed: {}", error.message));
                } else {
                    completed.push("track_confirmation".to_owned());
                }
            }
            Err(error) => warnings.push(format!("confirmation fetch failed: {}", error.message)),
        }

        let observed_listing = match self.api.observed_listing(&publication.listing_id).await {
            Ok(listing) => listing,
            Err(error) => {
                let observation_error_code = error.code.clone();
                let mut workflow = WorkflowError::for_draft(draft_id, &completed, error, true);
                if let Some(recovery) = &mut workflow.recovery {
                    recovery.listing_id = Some(publication.listing_id.clone());
                    recovery.failed_stage = Some("fetch_observed_listing".to_owned());
                    recovery.observe(&state, ObservationStatus::Observed);
                    recovery.observation.status = ObservationStatus::Unavailable;
                    recovery.observation.error_code = Some(observation_error_code);
                    recovery.delivery = Some(RecoveryStatus::Persisted);
                    recovery.publication = Some(RecoveryStatus::Persisted);
                    recovery.destructive_actions.clear();
                    set_recovery_images(recovery, &publication_images, false);
                    recovery.next_safe_actions =
                        vec![format!("flea listing show {}", publication.listing_id)];
                }
                workflow.details = Some(json!({
                    "listing_id": publication.listing_id,
                    "revision": publication.revision,
                }));
                return Err(workflow);
            }
        };
        completed.push("fetch_observed_listing".to_owned());

        Ok(PublishResult {
            draft_id: draft_id.to_owned(),
            listing_id: publication.listing_id,
            revision: publication.revision,
            state: publication.state,
            completed_steps: completed,
            warnings,
            observed_listing,
        })
    }

    async fn wait_for_images(
        &self,
        mut state: DraftState,
        completed: &[String],
    ) -> Result<DraftState, WorkflowError> {
        let started = tokio::time::Instant::now();
        for poll in 0..=self.config.image_poll_limit {
            if !state.images.is_empty()
                && state
                    .images
                    .iter()
                    .all(|image| image.state != ImageState::Processing)
            {
                return Ok(state);
            }
            if poll == self.config.image_poll_limit
                || started.elapsed() >= self.config.image_processing_timeout
            {
                return Err(image_processing_timeout(&state, completed, true, None));
            }
            let remaining = self
                .config
                .image_processing_timeout
                .saturating_sub(started.elapsed());
            tokio::time::sleep(self.config.image_poll_interval.min(remaining)).await;
            let remaining = self
                .config
                .image_processing_timeout
                .saturating_sub(started.elapsed());
            if remaining.is_zero() {
                return Err(image_processing_timeout(&state, completed, false, None));
            }
            state = match tokio::time::timeout(remaining, self.api.get_draft(&state.draft_id)).await
            {
                Ok(Ok(state)) => state,
                Ok(Err(error)) => {
                    return Err(image_processing_timeout(
                        &state,
                        completed,
                        false,
                        Some(error.code),
                    ));
                }
                Err(_) => {
                    return Err(image_processing_timeout(&state, completed, false, None));
                }
            };
        }
        unreachable!("bounded image loop always returns")
    }
}

pub(super) fn image_processing_timeout(
    state: &DraftState,
    completed: &[String],
    observation_is_current: bool,
    observation_error: Option<String>,
) -> WorkflowError {
    let mut error = ApiError::new(
        "draft.image_processing",
        "Images did not finish processing before the bounded timeout",
    );
    error.upstream_transient = true;
    error.safe_to_retry = true;
    let mut workflow = WorkflowError::for_draft(&state.draft_id, completed, error, true);
    if let Some(recovery) = &mut workflow.recovery {
        recovery.active_step = Some("wait_for_images".to_owned());
        recovery.failed_stage = Some("wait_for_images".to_owned());
        recovery.publication = Some(RecoveryStatus::Pending);
        if observation_is_current {
            recovery.observe(state, ObservationStatus::Observed);
            recovery.next_safe_actions = vec![
                format!("flea draft show {}", state.draft_id),
                format!("flea draft publish {}", state.draft_id),
            ];
        } else {
            recovery.observed_etag =
                (!state.etag.is_empty()).then(|| bounded_recovery_text(&state.etag));
            recovery.observed_revision = state
                .revision
                .as_deref()
                .map(bounded_recovery_text)
                .or_else(|| state.values.get("revision").and_then(recovery_scalar));
            recovery.observation = RecoveryObservation {
                status: ObservationStatus::Unavailable,
                observed_at: Some(observation_timestamp()),
                error_code: observation_error.or_else(|| Some("draft.image_processing".to_owned())),
            };
            recovery.fresh_state = Some(state.clone());
            recovery.destructive_actions.clear();
            recovery.next_safe_actions = vec![format!("flea draft show {}", state.draft_id)];
        }
        set_recovery_images(recovery, &observed_image_intent(state), false);
    }
    workflow
}
