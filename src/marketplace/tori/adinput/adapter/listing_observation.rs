use super::*;

impl<T: HttpTransport> DraftListingObservation for HttpAdInputApi<T> {
    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let Some(_) = ListingObservations::new(&self.transport)
            .find_summary(listing_id)
            .await?
        else {
            return Err(listing_not_copyable(
                listing_id,
                "not_in_authenticated_seller_collection",
                None,
            ));
        };
        let response = self
            .json(HttpRequest::read(
                ObservationSource::ListingCopyEligibility,
                format!("/listings/{listing_id}/draft-source"),
            ))
            .await
            .map_err(|error| {
                if error.status == Some(404) {
                    listing_not_copyable(listing_id, "copy_source_unavailable", Some(404))
                } else {
                    copy_source_error(error, listing_id)
                }
            })?;
        let seed: ListingDraftSeed = serde_json::from_value(response.body)
            .map_err(|_| malformed_copy_source(listing_id, response.status))?;
        if seed.listing_id != listing_id {
            return Err(malformed_copy_source(listing_id, response.status));
        }
        Ok(seed)
    }
    async fn active_listing(&self, listing_id: &str) -> Result<Option<Value>, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        Ok(ListingObservations::new(&self.transport)
            .find_summary(listing_id)
            .await?
            .filter(listing_is_active))
    }
    async fn observed_listing(&self, listing_id: &str) -> Result<Value, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let detail = self
            .json(HttpRequest::read(
                ObservationSource::ListingDetail,
                format!("/{listing_id}"),
            ))
            .await;
        if let Ok(response) = &detail
            && !response.body_is_unparseable
            && observed_detail_matches(&response.body, listing_id)
        {
            let mut body = response.body.clone();
            if let Some(object) = body.as_object_mut() {
                object.insert(
                    "public_url".to_owned(),
                    Value::String(public_listing_url(listing_id)),
                );
                object.insert(
                    "observation_source".to_owned(),
                    Value::String("detail".to_owned()),
                );
            }
            return Ok(body);
        }
        if let Some(summary) = ListingObservations::new(&self.transport)
            .find_summary(listing_id)
            .await?
        {
            return Ok(summary);
        }
        let (detail_status, observation) = match detail {
            Ok(response) => {
                let status = if response.body_is_unparseable {
                    "unparseable"
                } else {
                    "unrecognized_model"
                };
                (
                    status,
                    Observation::unrecognized_response(
                        ObservationSource::ListingDetail,
                        Some(response.status),
                    ),
                )
            }
            Err(error) => {
                let status = if error.status == Some(404) {
                    "not_found"
                } else {
                    "unavailable"
                };
                let observation = error.observation.unwrap_or_else(|| {
                    Observation::temporarily_unavailable(
                        ObservationSource::ListingDetail,
                        error.status,
                        error.status.is_some(),
                    )
                });
                (status, observation)
            }
        };
        let mut error = ApiError::new(
            "listing.observation_pending",
            "published listing is not visible through an authoritative observation path yet",
        )
        .with_observation(observation, ObservationOperation::PostMutationVerification);
        error.details = Some(Box::new(json!({
            "listing_id": listing_id,
            "detail_status": detail_status,
            "collection_status": "not_found",
        })));
        Err(error)
    }
}
