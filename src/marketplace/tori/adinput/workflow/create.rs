use super::{DraftWorkflow, sanitize_listing_copy_values};
use crate::marketplace::tori::adinput::{
    adapter::AdInputApi,
    fields::{create_preflight_issues, ordered_field_mutations, requested_sale_price},
    images::{PreparedImage, prepare_image, prepare_image_bytes},
    recovery::{CreateResult, ListingCopyReport, WorkflowError},
};
use serde_json::{Map, Value};
use std::path::Path;

impl<A: AdInputApi> DraftWorkflow<A> {
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
        let issues = create_preflight_issues(&values);
        if !issues.is_empty() {
            return Err(Self::create_preflight_error(issues));
        }
        let mutations = ordered_field_mutations(values);
        let image_count = images.len();
        let draft = match self.api.create_draft().await {
            Ok(draft) => draft,
            Err(error) => {
                let mut workflow = WorkflowError::before_creation(error);
                let Some(draft_id) = workflow
                    .recovery
                    .as_ref()
                    .map(|recovery| recovery.draft_id.clone())
                else {
                    return Err(workflow);
                };
                let completed = workflow
                    .recovery
                    .as_ref()
                    .map(|recovery| recovery.completed_steps.clone())
                    .unwrap_or_default();
                Self::add_unattempted_images(&mut workflow, 0, image_count);
                return Err(Self::create_incomplete(
                    workflow,
                    &draft_id,
                    &completed,
                    &mutations,
                    &[],
                ));
            }
        };
        let draft_id = draft.draft_id.clone();
        let mut completed = vec!["create_draft".to_owned()];
        let applied = match self
            .apply_field_mutations(
                draft,
                mutations.clone(),
                &mut completed,
                "draft_create",
                None,
            )
            .await
        {
            Ok(applied) => applied,
            Err(mut error) => {
                Self::add_unattempted_images(&mut error, 0, image_count);
                return Err(Self::create_incomplete(
                    error,
                    &draft_id,
                    &completed,
                    &mutations,
                    &[],
                ));
            }
        };
        let persisted_fields = applied.progress.persisted.clone();
        let mut draft = applied.draft;
        let mut warnings = applied.warnings;
        let mut image_processing = Vec::new();
        if !images.is_empty() {
            match self
                .add_prepared_images(&draft, images, &mut completed)
                .await
            {
                Ok(result) => {
                    draft = result.draft;
                    image_processing = result.image_processing;
                    warnings.extend(result.warnings);
                }
                Err(error) => {
                    return Err(Self::create_incomplete(
                        error,
                        &draft_id,
                        &completed,
                        &mutations,
                        &persisted_fields,
                    ));
                }
            }
        }
        Ok(CreateResult {
            draft,
            completed_steps: completed,
            image_processing,
            listing_copy: None,
            warnings,
        })
    }

    pub async fn create_from_listing(
        &self,
        listing_id: &str,
    ) -> Result<CreateResult, WorkflowError> {
        let mut seed = self
            .api
            .source_listing(listing_id)
            .await
            .map_err(WorkflowError::before_creation)?;
        let omitted_fields = sanitize_listing_copy_values(&mut seed.values);
        requested_sale_price(&seed.values).map_err(WorkflowError::input)?;
        let issues = create_preflight_issues(&seed.values);
        if !issues.is_empty() {
            return Err(Self::create_preflight_error(issues));
        }
        let copied_fields = seed.values.keys().cloned().collect();
        let source_image_count = seed.images.len();
        let mut source_images = Vec::with_capacity(source_image_count);
        for source in seed.images {
            source_images
                .push(prepare_image_bytes(source.bytes).map_err(WorkflowError::before_creation)?);
        }
        let mutations = ordered_field_mutations(seed.values);
        let draft = match self.api.create_draft().await {
            Ok(draft) => draft,
            Err(error) => {
                let mut workflow = WorkflowError::before_creation(error);
                let Some(recovery) = workflow.recovery.as_ref() else {
                    return Err(workflow);
                };
                let draft_id = recovery.draft_id.clone();
                let completed = recovery.completed_steps.clone();
                Self::add_unattempted_images(&mut workflow, 0, source_image_count);
                return Err(Self::create_incomplete(
                    workflow.with_source_listing_id(listing_id),
                    &draft_id,
                    &completed,
                    &mutations,
                    &[],
                ));
            }
        };
        let draft_id = draft.draft_id.clone();
        let mut completed = vec!["load_source_listing".to_owned(), "create_draft".to_owned()];
        let applied = match self
            .apply_field_mutations(
                draft,
                mutations.clone(),
                &mut completed,
                "draft_create_from_listing",
                Some(listing_id),
            )
            .await
        {
            Ok(applied) => applied,
            Err(mut error) => {
                Self::add_unattempted_images(&mut error, 0, source_image_count);
                return Err(Self::create_incomplete(
                    error,
                    &draft_id,
                    &completed,
                    &mutations,
                    &[],
                ));
            }
        };
        let persisted_fields = applied.progress.persisted.clone();
        let mut draft = applied.draft;
        let mut warnings = applied.warnings;
        let mut image_processing = Vec::new();
        if !source_images.is_empty() {
            match self
                .add_prepared_images(&draft, source_images, &mut completed)
                .await
            {
                Ok(result) => {
                    draft = result.draft;
                    image_processing = result.image_processing;
                    warnings.extend(result.warnings);
                }
                Err(error) => {
                    return Err(Self::create_incomplete(
                        error.with_source_listing_id(listing_id),
                        &draft_id,
                        &completed,
                        &mutations,
                        &persisted_fields,
                    ));
                }
            }
        }
        Ok(CreateResult {
            draft,
            completed_steps: completed,
            image_processing,
            listing_copy: Some(ListingCopyReport {
                source_listing_id: listing_id.to_owned(),
                source_scope: "authenticated_seller_listings".to_owned(),
                copied_fields,
                omitted_fields,
                source_image_count,
                image_handling: "fresh_upload_from_source_bytes".to_owned(),
            }),
            warnings,
        })
    }
}
