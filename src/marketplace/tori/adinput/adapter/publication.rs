use super::*;

impl<T: HttpTransport> DraftPublication for HttpAdInputApi<T> {
    async fn product_context(
        &self,
        draft_id: &str,
        revision: &str,
    ) -> Result<ProductContext, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let encoded_revision: String =
            url::form_urlencoded::byte_serialize(revision.as_bytes()).collect();
        let response = self
            .json(HttpRequest::read(
                ObservationSource::PublicationProductContext,
                format!(
                    "/adinput/product/recommerce/{draft_id}/productcontext?adRevision={encoded_revision}"
                ),
            ))
            .await?;
        let status = response.status;
        normalize_product_context(response.body, draft_id, revision).map_err(|error| {
            unrecognized_read(error, ObservationSource::PublicationProductContext, status)
        })
    }
    async fn publish_basic(
        &self,
        draft_id: &str,
        context: &ProductContext,
    ) -> Result<Publication, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::mutation(
                ObservationSource::DraftService,
                Method::Post,
                format!("/adinput/order/choices/{draft_id}"),
                RequestBody::Form(vec![(
                    "choices".to_owned(),
                    context.basic_package_urn.clone(),
                )]),
            ))
            .await
            .map_err(|error| error_at_stage(error, "package_choice"))?;
        normalize_publication(response, draft_id, &context.revision)
    }
    async fn confirmation(&self, publication: &Publication) -> Result<Confirmation, ApiError> {
        validate_resource_id(&publication.listing_id, "listing")?;
        validate_resource_id(&publication.order_id, "order")?;
        let response = self
            .json(HttpRequest::read(
                ObservationSource::PublicationConfirmation,
                format!(
                    "/orders/{}/confirmation/{}",
                    publication.order_id, publication.listing_id
                ),
            ))
            .await?;
        if response.body_is_unparseable || !response.body.is_object() {
            return Err(malformed_read_response(
                "confirmation",
                ObservationSource::PublicationConfirmation,
            ));
        }
        Ok(Confirmation {
            listing_id: publication.listing_id.clone(),
            order_id: publication.order_id.clone(),
            details: response.body,
        })
    }
    async fn track_confirmation(&self, confirmation: &Confirmation) -> Result<(), ApiError> {
        let mut query = url::form_urlencoded::Serializer::new(String::new());
        query.append_pair("adId", &confirmation.listing_id);
        query.append_pair("orderId", &confirmation.order_id);
        self.json(HttpRequest::read(
            ObservationSource::PublicationConfirmation,
            format!("/tracking/adconfirmation?{}", query.finish()),
        ))
        .await
        .map(|_| ())
    }
}
