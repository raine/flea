use super::*;

impl<T: AdInputProtocol> DraftRead for HttpAdInputApi<T> {
    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        match self
            .draft_request(
                HttpRequest::read(
                    ObservationSource::DraftDetail,
                    format!("/adinput/ad/withModel/{draft_id}"),
                ),
                true,
            )
            .await
        {
            Err(error) if error.status == Some(404) => {
                Err(self.reconcile_missing_draft(draft_id, error).await)
            }
            result => result,
        }
    }
    async fn publication_draft(&self, draft_id: &str) -> Result<PublicationDraftState, ApiError> {
        self.publication_draft_for_inspection(draft_id, false).await
    }
    async fn publication_draft_for_inspection(
        &self,
        draft_id: &str,
        include_all_options: bool,
    ) -> Result<PublicationDraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = match self
            .json(HttpRequest::read(
                ObservationSource::DraftDetail,
                format!("/adinput/ad/withModel/{draft_id}"),
            ))
            .await
        {
            Err(error) if error.status == Some(404) => {
                return Err(self.reconcile_missing_draft(draft_id, error).await);
            }
            result => result?,
        };
        if response.body_is_unparseable {
            return Err(malformed_read_response(
                "publication_draft",
                ObservationSource::DraftDetail,
            ));
        }
        let status = response.status;
        let parsed = if include_all_options {
            normalize_publication_draft_with_limit(
                response.body,
                response.etag.as_deref(),
                usize::MAX,
            )
        } else {
            normalize_publication_draft(response.body, response.etag.as_deref())
        };
        parsed.map_err(|error| unrecognized_read(error, ObservationSource::DraftDetail, status))
    }
    async fn category_predictions(
        &self,
        draft_id: &str,
    ) -> Result<Vec<CategoryPrediction>, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::read(
                ObservationSource::DraftCategoryPredictions,
                format!("/drafts/{draft_id}/category-predictions"),
            ))
            .await?;
        serde_json::from_value(response.body).map_err(|_| {
            malformed_read_response(
                "category_predictions",
                ObservationSource::DraftCategoryPredictions,
            )
        })
    }
    async fn publication_categories(&self) -> Result<Vec<PublicationCategory>, ApiError> {
        let response = self
            .json(HttpRequest::read(
                ObservationSource::CategoryTaxonomy,
                "/categories/taxonomy",
            ))
            .await?;
        normalize_publication_categories(&response.body).map_err(|error| {
            unrecognized_read(error, ObservationSource::CategoryTaxonomy, response.status)
        })
    }
}
