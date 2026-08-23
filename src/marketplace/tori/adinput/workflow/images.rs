use super::{DraftWorkflow, reconciled_mutation_warning, record_mutation_response_drift};
use crate::diagnostics;
use crate::marketplace::tori::adinput::{
    adapter::AdInputApi,
    fields::{RecoveryAttempt, RecoveryImageIntent, set_recovery_images},
    images::{PreparedImage, prepare_image, uploaded_from_draft_image},
    recovery::{AddImagesResult, ObservationStatus, WorkflowError},
    types::{DraftState, ImageState, UploadedImage},
};
use std::path::Path;

impl<A: AdInputApi> DraftWorkflow<A> {
    pub async fn add_images(
        &self,
        draft_id: &str,
        paths: &[impl AsRef<Path>],
    ) -> Result<AddImagesResult, WorkflowError> {
        let state = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?
            .draft;
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

    pub(super) async fn add_prepared_images(
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
                    image.file_name,
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
        let mut warnings = Vec::new();
        let updated = match self
            .api
            .set_images(&state.draft_id, &state.etag, &state.values, &ordered)
            .await
        {
            Ok(updated) => updated,
            Err(error) => {
                let response_model_drift = error
                    .status
                    .is_some_and(|status| (200..300).contains(&status));
                let context = diagnostics::WorkflowContext {
                    workflow: "draft_image_add",
                    step: "attach_images",
                    draft_id: Some(&state.draft_id),
                    listing_id: None,
                    fields: &[],
                };
                record_mutation_response_drift(&context, &error);
                let workflow = WorkflowError::for_draft(&state.draft_id, completed, error, false);
                let reconciled = self
                    .recover_after_failure(
                        workflow,
                        &state.draft_id,
                        "attach_images",
                        Some(&state.etag),
                        &intent,
                        None,
                        None,
                    )
                    .await;
                let every_image_persisted = reconciled.recovery.as_ref().is_some_and(|recovery| {
                    recovery.fresh_state.as_ref().is_some_and(|fresh| {
                        intent.iter().all(|requested| {
                            requested.image_id.as_ref().is_some_and(|image_id| {
                                fresh.images.iter().any(|image| {
                                    image.image_id == *image_id && image.state == ImageState::Ready
                                })
                            })
                        })
                    })
                });
                if !every_image_persisted {
                    return Err(reconciled);
                }
                diagnostics::workflow_step(&context, "reconciled");
                warnings.push(reconciled_mutation_warning(
                    &["images".to_owned()],
                    response_model_drift,
                ));
                reconciled
                    .recovery
                    .and_then(|recovery| recovery.fresh_state)
                    .expect("persisted image reconciliation has authoritative state")
            }
        };
        completed.push("attach_images".to_owned());
        let observed = if warnings.is_empty() {
            self.api
                .publication_draft(&state.draft_id)
                .await
                .map_err(|error| {
                    self.post_image_observation_error(&updated, completed, &intent, error)
                })?
                .draft
        } else {
            updated
        };
        completed.push("observe_attached_images".to_owned());
        Ok(AddImagesResult {
            draft: observed,
            image_processing,
            warnings,
        })
    }

    pub async fn remove_images(
        &self,
        draft_id: &str,
        remove: &[String],
    ) -> Result<DraftState, WorkflowError> {
        let state = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?
            .draft;
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
            Ok(updated) => {
                let completed = ["fetch_draft".to_owned(), "remove_images".to_owned()];
                self.api
                    .publication_draft(draft_id)
                    .await
                    .map(|publication| publication.draft)
                    .map_err(|error| {
                        let intent = RecoveryImageIntent::removals(remove);
                        self.post_image_observation_error(&updated, &completed, &intent, error)
                    })
            }
            Err(error) => {
                let context = diagnostics::WorkflowContext {
                    workflow: "draft_image_remove",
                    step: "remove_images",
                    draft_id: Some(draft_id),
                    listing_id: None,
                    fields: &[],
                };
                record_mutation_response_drift(&context, &error);
                let workflow =
                    WorkflowError::for_draft(draft_id, &["fetch_draft".to_owned()], error, false);
                let intent = RecoveryImageIntent::removals(remove);
                let reconciled = self
                    .recover_after_failure(
                        workflow,
                        draft_id,
                        "remove_images",
                        Some(&state.etag),
                        &intent,
                        None,
                        None,
                    )
                    .await;
                let removed = reconciled.recovery.as_ref().is_some_and(|recovery| {
                    recovery.fresh_state.as_ref().is_some_and(|fresh| {
                        remove.iter().all(|image_id| {
                            fresh.images.iter().all(|image| image.image_id != *image_id)
                        })
                    })
                });
                if removed {
                    diagnostics::workflow_step(&context, "reconciled");
                    Ok(reconciled
                        .recovery
                        .and_then(|recovery| recovery.fresh_state)
                        .expect("reconciled image removal has authoritative state"))
                } else {
                    Err(reconciled)
                }
            }
        }
    }
}
