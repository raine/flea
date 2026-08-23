use super::{DraftWorkflow, require_authoritative_revision};
use crate::marketplace::tori::adinput::{
    adapter::AdInputApi,
    normalization::attach_delivery_model,
    recovery::WorkflowError,
    types::{DraftState, PublicationValidation},
    validation::evaluate_publication,
};

impl<A: AdInputApi> DraftWorkflow<A> {
    #[cfg(test)]
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

    pub async fn inspect(
        &self,
        draft_id: &str,
        include_all_options: bool,
    ) -> Result<(DraftState, PublicationValidation), WorkflowError> {
        let publication = self
            .api
            .publication_draft_for_inspection(draft_id, include_all_options)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let composer_model = publication.composer_model;
        let mut state = publication.draft;
        if state.category_is_unset() && !state.images.is_empty() {
            state.predictions = self
                .api
                .category_predictions(draft_id)
                .await
                .map_err(|error| {
                    WorkflowError::for_draft(draft_id, &["fetch_draft".to_owned()], error, true)
                })?;
        }
        let mut evidence_failures = Vec::new();
        let delivery_verifiable = match self
            .api
            .delivery_composer_for_inspection(draft_id, include_all_options)
            .await
        {
            Ok(composer) => match attach_delivery_model(&mut state, &composer) {
                Ok(()) => true,
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
            Err(error) => {
                evidence_failures.push(Self::validation_evidence_failure(
                    "delivery",
                    "fetch_delivery_options",
                    &error,
                    format!("flea tori draft show {draft_id}"),
                ));
                false
            }
        };
        let categories = match self.api.publication_categories().await {
            Ok(categories) => Some(categories),
            Err(error) => {
                evidence_failures.push(Self::validation_evidence_failure(
                    "category",
                    "fetch_category_taxonomy",
                    &error,
                    "flea tori category search QUERY".to_owned(),
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
        Ok((state, report))
    }

    pub async fn validate(&self, draft_id: &str) -> Result<PublicationValidation, WorkflowError> {
        let publication = self
            .api
            .publication_draft(draft_id)
            .await
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut state = publication.draft;
        require_authoritative_revision(&state)
            .map_err(|error| WorkflowError::for_draft(draft_id, &[], error, true))?;
        let mut evidence_failures = Vec::new();
        let delivery_verifiable = match self.api.delivery_composer(draft_id).await {
            Ok(composer) => match attach_delivery_model(&mut state, &composer) {
                Ok(()) => true,
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
            Err(error) => {
                evidence_failures.push(Self::validation_evidence_failure(
                    "delivery",
                    "fetch_delivery_options",
                    &error,
                    format!("flea tori draft show {draft_id}"),
                ));
                false
            }
        };
        let categories = match self.api.publication_categories().await {
            Ok(categories) => Some(categories),
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
            publication.composer_model,
            delivery_verifiable,
        );
        report.evidence_failures = evidence_failures;
        Ok(report)
    }
}
