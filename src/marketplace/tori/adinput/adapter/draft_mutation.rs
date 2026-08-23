use super::*;

impl<T: HttpTransport> DraftMutation for HttpAdInputApi<T> {
    async fn create_draft(&self) -> Result<DraftState, ApiError> {
        let request = HttpRequest::mutation(
            ObservationSource::DraftCreation,
            Method::Post,
            "/adinput/ad/withModel/recommerce",
            RequestBody::Empty,
        );
        let retry_context = request.retry_context();
        let response = self.transport.execute(request).await?;
        if response.status == 303 {
            let draft_id =
                draft_id_from_location(response.location.as_deref()).ok_or_else(|| {
                    uncertain_creation(&response, "redirect response did not identify a draft")
                })?;
            return self
                .observe_created_draft(&draft_id, &["create_draft", "establish_identity"])
                .await;
        }
        if !(200..300).contains(&response.status) {
            return Err(ApiError::response(
                &response,
                retry_context,
                ObservationSource::DraftCreation,
            ));
        }
        if response.body_is_unparseable {
            return Err(uncertain_creation(
                &response,
                "successful response was not valid JSON",
            ));
        }

        let body_id = draft_id_from_body(&response.body);
        let location_id = draft_id_from_location(response.location.as_deref());
        if body_id.is_some() && location_id.is_some() && body_id != location_id {
            return Err(uncertain_creation(
                &response,
                "response body and Location identified different drafts",
            ));
        }
        let draft_id = body_id.or(location_id).ok_or_else(|| {
            uncertain_creation(&response, "successful response did not identify a draft")
        })?;

        if draft_id_from_body(&response.body).is_some() {
            return normalize_authoritative_draft_state(response.body, response.etag.as_deref());
        }
        self.observe_created_draft(&draft_id, &["create_draft", "establish_identity"])
            .await
    }
    async fn update_item(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let values = composer_values(values)?;
        let mut request = HttpRequest::mutation(
            ObservationSource::DraftService,
            Method::Put,
            format!("/adinput/ad/recommerce/{draft_id}/update"),
            RequestBody::Json(Value::Object(values)),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request, false).await
    }
    async fn update_sale_price(
        &self,
        draft_id: &str,
        etag: &str,
        price: &Value,
    ) -> Result<String, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        validate_price(price)?;
        let mut request = HttpRequest::mutation(
            ObservationSource::DraftService,
            Method::Patch,
            format!("/items/{draft_id}"),
            RequestBody::Json(json!({
                "data": {
                    "price": {
                        "price_amount": price
                    }
                }
            })),
        );
        request.if_match = Some(etag.to_owned());
        let response = self
            .json(request)
            .await
            .map_err(|error| error_at_stage(error, "apply_price"))?;
        normalize_item_update(response, draft_id, "apply_price")
    }
    async fn patch_item_fields(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<String, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let data = item_creation_fields(values)?;
        let mut request = HttpRequest::mutation(
            ObservationSource::DraftService,
            Method::Patch,
            format!("/items/{draft_id}"),
            RequestBody::Json(json!({ "data": data })),
        );
        request.if_match = Some(etag.to_owned());
        let response = self
            .json(request)
            .await
            .map_err(|error| error_at_stage(error, "patch_item_fields"))?;
        normalize_item_update(response, draft_id, "patch_item_fields")
    }
    async fn update_recommerce(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let body = composer_values(values)?;
        let mut request = HttpRequest::mutation(
            ObservationSource::DraftService,
            Method::Put,
            format!("/adinput/ad/recommerce/{draft_id}/update"),
            RequestBody::Json(Value::Object(body)),
        );
        request.if_match = Some(etag.to_owned());
        let response = self
            .json(request)
            .await
            .map_err(|error| error_at_stage(error, "update_recommerce"))?;
        normalize_recommerce_update(response, draft_id)
    }
    async fn delete_draft(&self, draft_id: &str) -> Result<(), ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.get_draft(draft_id).await?;
        let path = ListingObservations::new(&self.transport)
            .draft_delete_action(draft_id)
            .await?;
        self.json(
            HttpRequest::mutation(
                ObservationSource::DraftService,
                Method::Delete,
                path,
                RequestBody::Empty,
            )
            .with_service(compatibility::SERVICE_AD_ACTION),
        )
        .await
        .map(|_| ())
    }
}
