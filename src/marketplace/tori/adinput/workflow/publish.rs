use super::{DraftWorkflow, require_authoritative_revision};
use crate::marketplace::tori::adinput::{
    adapter::AdInputApi,
    delivery::allowed_delivery_values,
    fields::{
        RecoveryImageIntent, mutation_is_ambiguous, observed_image_intent, set_recovery_images,
    },
    http::ApiError,
    normalization::attach_delivery_model,
    recovery::{
        ObservationStatus, PublishResult, RecoveryObservation, RecoveryStatus, WorkflowError,
        WorkflowWarning, bounded_recovery_text, recovery_scalar,
    },
    types::{DraftState, ImageState, Publication, model_error},
    validation::evaluate_publication,
};
use serde_json::{Value, json};
use std::time::Duration;

impl<A: AdInputApi> DraftWorkflow<A> {
    #[allow(clippy::too_many_arguments)]
    fn publication_observation_error(
        &self,
        draft_id: &str,
        completed: &[String],
        state: &DraftState,
        publication: &Publication,
        publication_images: &[RecoveryImageIntent],
        attempts: usize,
        elapsed: Duration,
        error: ApiError,
    ) -> WorkflowError {
        let listing_observation = RecoveryObservation::from_error(&error, "listing_detail");
        let upstream_transient = error.upstream_transient;
        let observation_details = error.details.as_deref().cloned();
        let mut workflow = WorkflowError::for_draft(draft_id, completed, error, true);
        workflow.code = "publication.observation_uncertain".to_owned();
        workflow.message =
            "Tori persisted the publication, but listing observation remains unavailable"
                .to_owned();
        if let Some(source) = &mut workflow.source {
            source.code = "publication.observation_uncertain".to_owned();
            source.message = workflow.message.clone();
            source.safe_to_retry = false;
            source.upstream_transient = upstream_transient;
        }
        if let Some(recovery) = &mut workflow.recovery {
            recovery.listing_id = Some(publication.listing_id.clone());
            recovery.failed_stage = Some("fetch_observed_listing".to_owned());
            recovery.observe(state, ObservationStatus::Observed);
            recovery.observation = listing_observation;
            recovery.delivery = Some(RecoveryStatus::Persisted);
            recovery.publication = Some(RecoveryStatus::Persisted);
            recovery.safe_to_retry = false;
            recovery.upstream_transient = upstream_transient;
            recovery.destructive_actions.clear();
            set_recovery_images(recovery, publication_images, false);
            recovery.next_safe_actions =
                vec![format!("flea tori listing show {}", publication.listing_id)];
        }
        workflow.details = Some(json!({
            "listing_id": publication.listing_id,
            "revision": publication.revision,
            "publication": "persisted",
            "observation_status": "unavailable",
            "observation_attempts": attempts,
            "observation_elapsed_ms": elapsed.as_millis(),
            "observation_timeout_ms": self.config.listing_observation_timeout.as_millis(),
            "poll_interval_ms": self.config.listing_poll_interval.as_millis(),
            "last_observation": observation_details,
        }));
        workflow
    }

    pub async fn publish(
        &self,
        draft_id: &str,
        expected_revision: &str,
    ) -> Result<PublishResult, WorkflowError> {
        let mut completed = Vec::new();
        let active = self
            .api
            .active_listing(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        if let Some(observed_listing) = active {
            completed.push("check_active_listing".to_owned());
            return Ok(PublishResult {
                draft_id: draft_id.to_owned(),
                listing_id: draft_id.to_owned(),
                revision: String::new(),
                state: "active".to_owned(),
                publication: "already_published".to_owned(),
                mutations_performed: false,
                public_url: format!("https://www.tori.fi/recommerce/forsale/item/{draft_id}"),
                completed_steps: completed,
                warnings: vec![WorkflowWarning::best_effort(
                    "listing is already active; no mutation was performed",
                )],
                observed_listing,
            });
        }
        let publication = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        let composer_model = publication.composer_model;
        let mut state = publication.draft;
        require_authoritative_revision(&state)
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        let publication_images = observed_image_intent(&state);
        completed.push("fetch_draft".to_owned());

        let mut evidence_failures = Vec::new();
        let composer = match self.api.delivery_composer(draft_id).await {
            Ok(composer) => Some(composer),
            Err(error) => {
                evidence_failures.push(Self::validation_evidence_failure(
                    "delivery",
                    "fetch_delivery_options",
                    &error,
                    format!("flea tori draft show {draft_id}"),
                ));
                None
            }
        };
        let mut delivery_verifiable = match composer.as_ref() {
            Some(composer) => match attach_delivery_model(&mut state, composer) {
                Ok(()) => {
                    completed.push("fetch_delivery_options".to_owned());
                    true
                }
                Err(error) => {
                    evidence_failures.push(Self::validation_evidence_failure(
                        "delivery",
                        "fetch_delivery_options",
                        &error,
                        format!("flea tori draft show {draft_id}"),
                    ));
                    false
                }
            },
            None => false,
        };
        let categories = match self.api.publication_categories().await {
            Ok(categories) => {
                completed.push("fetch_category_taxonomy".to_owned());
                Some(categories)
            }
            Err(error) => {
                evidence_failures.push(Self::validation_evidence_failure(
                    "category",
                    "fetch_category_taxonomy",
                    &error,
                    "flea tori category list".to_owned(),
                ));
                None
            }
        };
        let mut report = evaluate_publication(
            &state,
            categories.as_deref(),
            composer_model,
            delivery_verifiable,
        );
        report.evidence_failures = evidence_failures;
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

        state = self
            .api
            .get_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        let observed_revision = require_authoritative_revision(&state)
            .map_err(|error| WorkflowError::for_draft(draft_id, &completed, error, true))?;
        if observed_revision != expected_revision {
            return Err(WorkflowError::revision_conflict(
                draft_id,
                &completed,
                expected_revision,
                &state,
            ));
        }
        completed.push("verify_revision".to_owned());

        state.values.remove("delivery");
        if let Err(error) = self
            .api
            .patch_item_fields(draft_id, &state.etag, &state.values)
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
                        "fetch_fresh_model",
                        Some(&state.etag),
                        &publication_images,
                        Some(RecoveryStatus::Pending),
                        Some(RecoveryStatus::Unattempted),
                    )
                    .await);
            }
        };
        completed.push("fetch_fresh_model".to_owned());

        state = match self
            .api
            .update_recommerce(draft_id, &state.etag, &state.values)
            .await
        {
            Ok(state) => state,
            Err(error) => {
                let workflow = WorkflowError::for_draft(draft_id, &completed, error, false);
                return Err(self
                    .recover_after_failure(
                        workflow,
                        draft_id,
                        "update_recommerce",
                        Some(&state.etag),
                        &publication_images,
                        Some(RecoveryStatus::Pending),
                        Some(RecoveryStatus::Unattempted),
                    )
                    .await);
            }
        };
        completed.push("update_recommerce".to_owned());

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
                "recovery_guidance": format!("Inspect the draft with `flea tori draft show {draft_id}`; do not repeat publication")
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
                        "package_choice",
                        Some(&state.etag),
                        &publication_images,
                        Some(RecoveryStatus::Persisted),
                        Some(publication_status),
                    )
                    .await);
            }
        };
        completed.push("package_choice".to_owned());

        let mut warnings = Vec::new();
        match self.api.confirmation(&publication).await {
            Ok(confirmation) => {
                completed.push("fetch_confirmation".to_owned());
                if let Err(error) = self.api.track_confirmation(&confirmation).await {
                    warnings.push(WorkflowWarning::best_effort(format!(
                        "confirmation tracking failed: {}",
                        error.message
                    )));
                } else {
                    completed.push("track_confirmation".to_owned());
                }
            }
            Err(error) => warnings.push(WorkflowWarning::best_effort(format!(
                "confirmation fetch failed: {}",
                error.message
            ))),
        }

        let observation_started = tokio::time::Instant::now();
        let mut observation_attempts = 0;
        let observed_listing = loop {
            observation_attempts += 1;
            match self.api.observed_listing(&publication.listing_id).await {
                Ok(listing) => break listing,
                Err(mut error)
                    if observation_attempts <= self.config.listing_poll_limit
                        && observation_started.elapsed()
                            < self.config.listing_observation_timeout =>
                {
                    let remaining = self
                        .config
                        .listing_observation_timeout
                        .saturating_sub(observation_started.elapsed());
                    tokio::time::sleep(self.config.listing_poll_interval.min(remaining)).await;
                    if remaining.is_zero() {
                        error.code = "publication.observation_uncertain".to_owned();
                        return Err(self.publication_observation_error(
                            draft_id,
                            &completed,
                            &state,
                            &publication,
                            &publication_images,
                            observation_attempts,
                            observation_started.elapsed(),
                            error,
                        ));
                    }
                }
                Err(mut error) => {
                    error.code = "publication.observation_uncertain".to_owned();
                    error.message = "Tori persisted the publication, but listing observation remains unavailable".to_owned();
                    error.safe_to_retry = false;
                    return Err(self.publication_observation_error(
                        draft_id,
                        &completed,
                        &state,
                        &publication,
                        &publication_images,
                        observation_attempts,
                        observation_started.elapsed(),
                        error,
                    ));
                }
            }
        };
        completed.push("fetch_observed_listing".to_owned());

        let public_url = format!(
            "https://www.tori.fi/recommerce/forsale/item/{}",
            publication.listing_id
        );
        let state = if observed_listing_is_active(&observed_listing) {
            "active".to_owned()
        } else {
            publication.state
        };
        Ok(PublishResult {
            draft_id: draft_id.to_owned(),
            listing_id: publication.listing_id,
            revision: publication.revision,
            state,
            publication: "persisted".to_owned(),
            mutations_performed: true,
            public_url,
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

fn observed_listing_is_active(listing: &Value) -> bool {
    let state = listing.get("state");
    [
        state.and_then(Value::as_str),
        state
            .and_then(|state| state.get("type"))
            .and_then(Value::as_str),
        state
            .and_then(|state| state.get("display"))
            .and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    .any(|state| {
        matches!(
            state.trim().to_ascii_lowercase().as_str(),
            "active" | "published"
        )
    })
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
                format!("flea tori draft show {}", state.draft_id),
                format!("flea tori draft validate {}", state.draft_id),
            ];
        } else {
            recovery.observed_etag =
                (!state.etag.is_empty()).then(|| bounded_recovery_text(&state.etag));
            recovery.observed_revision = state
                .revision
                .as_deref()
                .map(bounded_recovery_text)
                .or_else(|| state.values.get("revision").and_then(recovery_scalar));
            recovery.observation = RecoveryObservation::temporarily_unavailable(
                "draft_images",
                observation_error
                    .as_deref()
                    .or(Some("draft.image_processing")),
            );
            recovery.fresh_state = Some(state.clone());
            recovery.destructive_actions.clear();
            recovery.next_safe_actions = vec![format!("flea tori draft show {}", state.draft_id)];
        }
        set_recovery_images(recovery, &observed_image_intent(state), false);
    }
    workflow
}
