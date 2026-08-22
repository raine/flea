use super::{delivery::*, http::*, normalization::*, types::*, *};

#[allow(async_fn_in_trait)]
pub trait AdInputApi: Send + Sync {
    async fn create_draft(&self) -> Result<DraftState, ApiError>;
    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError>;
    async fn publication_draft(&self, draft_id: &str) -> Result<PublicationDraftState, ApiError> {
        self.get_draft(draft_id)
            .await
            .map(|draft| PublicationDraftState {
                draft,
                composer_model: ComposerModelStatus::Available,
            })
    }
    async fn update_item(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<DraftState, ApiError>;
    async fn update_sale_price(
        &self,
        draft_id: &str,
        etag: &str,
        price: &Value,
    ) -> Result<String, ApiError>;
    async fn submit_adinput(
        &self,
        draft_id: &str,
        etag: &str,
        state: &DraftState,
    ) -> Result<DraftState, ApiError>;
    async fn delete_draft(&self, draft_id: &str) -> Result<(), ApiError>;
    async fn upload_image(
        &self,
        draft_id: &str,
        file_name: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<UploadedImage, ApiError>;
    async fn set_images(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
        images: &[UploadedImage],
    ) -> Result<DraftState, ApiError>;
    async fn category_predictions(
        &self,
        draft_id: &str,
    ) -> Result<Vec<CategoryPrediction>, ApiError>;
    async fn publication_categories(&self) -> Result<Vec<PublicationCategory>, ApiError>;
    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError>;
    async fn delivery_composer(&self, draft_id: &str) -> Result<DeliveryComposer, ApiError>;
    async fn apply_delivery(
        &self,
        draft_id: &str,
        composer: &DeliveryComposer,
        delivery: &str,
    ) -> Result<(), ApiError>;
    async fn product_context(
        &self,
        draft_id: &str,
        revision: &str,
    ) -> Result<ProductContext, ApiError>;
    async fn publish_basic(
        &self,
        draft_id: &str,
        context: &ProductContext,
    ) -> Result<Publication, ApiError>;
    async fn confirmation(&self, listing_id: &str) -> Result<Confirmation, ApiError>;
    async fn track_confirmation(&self, confirmation: &Confirmation) -> Result<(), ApiError>;
    async fn observed_listing(&self, listing_id: &str) -> Result<Value, ApiError>;
}

pub struct HttpAdInputApi<T> {
    transport: T,
}

impl<T> HttpAdInputApi<T> {
    pub const fn new(transport: T) -> Self {
        Self { transport }
    }
}

impl<T: HttpTransport> HttpAdInputApi<T> {
    async fn json(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        debug_assert!(!request.method.is_mutation() || request.retry == RetryPolicy::Never);
        let retry_context = request.retry_context();
        let response = self.transport.execute(request).await?;
        if (200..300).contains(&response.status) {
            Ok(response)
        } else {
            Err(ApiError::response(&response, retry_context))
        }
    }

    async fn draft_request(
        &self,
        request: HttpRequest,
        require_authoritative_model: bool,
    ) -> Result<DraftState, ApiError> {
        let is_mutation = request.method.is_mutation();
        let retry_context = request.retry_context();
        let response = self.json(request).await?;
        if response.body_is_unparseable {
            let mut error = unexpected_representation("receive_draft_state", &response)
                .retry_classification(classify(FailureKind::MalformedSuccess, retry_context));
            if is_mutation {
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "The draft mutation may have succeeded, but its resulting state is unknown"
                        .to_owned();
            }
            return Err(error);
        }
        let normalized = if require_authoritative_model {
            normalize_authoritative_draft_state(response.body, response.etag.as_deref())
        } else {
            normalize_draft_state(response.body, response.etag.as_deref())
        };
        normalized.map_err(|mut error| {
            let classification = classify(FailureKind::MalformedSuccess, retry_context);
            error.upstream_transient = classification.upstream_transient;
            error.safe_to_retry = classification.safe_to_retry;
            if is_mutation {
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "The draft mutation may have succeeded, but its resulting state is unknown"
                        .to_owned();
                error.status = Some(response.status);
            }
            error
        })
    }

    async fn observe_created_draft(
        &self,
        draft_id: &str,
        completed_steps: &[&str],
    ) -> Result<DraftState, ApiError> {
        self.get_draft(draft_id).await.map_err(|mut error| {
            error.details = Some(Box::new(json!({
                "stage": "observe_created_draft",
                "draft_id": draft_id,
                "completed_steps": completed_steps,
                "recovery_guidance": format!(
                    "Inspect the draft with `flea draft show {draft_id}`; do not repeat creation"
                )
            })));
            error
        })
    }
}

#[allow(async_fn_in_trait)]
impl<T: HttpTransport> AdInputApi for HttpAdInputApi<T> {
    async fn create_draft(&self) -> Result<DraftState, ApiError> {
        let request = HttpRequest::mutation(
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
            return Err(ApiError::response(&response, retry_context));
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

    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.draft_request(
            HttpRequest::read(format!("/adinput/ad/withModel/{draft_id}")),
            true,
        )
        .await
    }

    async fn publication_draft(&self, draft_id: &str) -> Result<PublicationDraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/adinput/ad/withModel/{draft_id}"
            )))
            .await?;
        if response.body_is_unparseable {
            return Err(malformed_read_response("publication_draft"));
        }
        normalize_publication_draft(response.body, response.etag.as_deref())
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
        normalize_item_update(response, draft_id)
    }

    async fn submit_adinput(
        &self,
        draft_id: &str,
        etag: &str,
        state: &DraftState,
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut body = serde_json::to_value(state).expect("draft state serializes");
        if let Some(body) = body.as_object_mut() {
            body.remove("delivery");
        }
        let mut request = HttpRequest::mutation(
            Method::Put,
            format!("/drafts/{draft_id}/adinput"),
            RequestBody::Json(body),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request, false).await
    }

    async fn delete_draft(&self, draft_id: &str) -> Result<(), ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.json(HttpRequest::mutation(
            Method::Delete,
            format!("/drafts/{draft_id}"),
            RequestBody::Empty,
        ))
        .await
        .map(|_| ())
    }

    async fn upload_image(
        &self,
        draft_id: &str,
        file_name: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<UploadedImage, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mime_type = if file_name.ends_with(".png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        let response = self
            .json(HttpRequest::mutation(
                Method::Post,
                format!("/adinput/ad/recommerce/{draft_id}/upload"),
                RequestBody::Image {
                    bytes,
                    file_name: file_name.to_owned(),
                    mime_type: mime_type.to_owned(),
                    width,
                    height,
                },
            ))
            .await?;
        let location = response
            .location
            .as_deref()
            .or_else(|| response.body.get("location").and_then(Value::as_str))
            .and_then(valid_image_location)
            .ok_or_else(|| {
                let mut error = unexpected_representation("upload_image", &response);
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "Image upload succeeded without an authoritative image location".to_owned();
                error
            })?;
        Ok(UploadedImage {
            image_id: location.clone(),
            state: ImageState::Processing,
            url: Some(location),
            width,
            height,
            mime_type: Some(mime_type.to_owned()),
        })
    }

    async fn set_images(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
        images: &[UploadedImage],
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut values = composer_values(values)?;
        let mut image = Vec::with_capacity(images.len());
        let mut multi_image = Vec::with_capacity(images.len());
        for uploaded in images {
            let url = uploaded
                .url
                .as_deref()
                .and_then(valid_image_location)
                .ok_or_else(|| {
                    ApiError::new("draft.invalid_image", "Tori image location is invalid")
                })?;
            let path = url
                .strip_prefix("https://img.tori.net/dynamic/default/")
                .expect("validated image location has the canonical prefix");
            let mime_type = uploaded.mime_type.as_deref().unwrap_or("image/jpeg");
            image.push(json!({
                "height": uploaded.height.to_string(),
                "type": mime_type,
                "uri": path,
                "width": uploaded.width.to_string()
            }));
            multi_image.push(json!({
                "description": "",
                "height": uploaded.height,
                "path": path,
                "type": mime_type,
                "url": url,
                "width": uploaded.width
            }));
        }
        values.insert("image".to_owned(), Value::Array(image));
        values.insert("multi_image".to_owned(), Value::Array(multi_image));
        let mut request = HttpRequest::mutation(
            Method::Put,
            format!("/adinput/ad/recommerce/{draft_id}/update"),
            RequestBody::Json(Value::Object(values)),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request, false).await
    }

    async fn category_predictions(
        &self,
        draft_id: &str,
    ) -> Result<Vec<CategoryPrediction>, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/drafts/{draft_id}/category-predictions"
            )))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|_| malformed_read_response("category_predictions"))
    }

    async fn publication_categories(&self) -> Result<Vec<PublicationCategory>, ApiError> {
        let response = self.json(HttpRequest::read("/categories/taxonomy")).await?;
        normalize_publication_categories(&response.body)
    }

    async fn source_listing(&self, listing_id: &str) -> Result<ListingDraftSeed, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/listings/{listing_id}/draft-source"
            )))
            .await?;
        serde_json::from_value(response.body).map_err(|_| malformed_read_response("source_listing"))
    }

    async fn delivery_composer(&self, draft_id: &str) -> Result<DeliveryComposer, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let draft_id_query: String =
            url::form_urlencoded::byte_serialize(draft_id.as_bytes()).collect();
        let response = self
            .json(HttpRequest::read(format!(
                "/ui/addelivery?adId={draft_id_query}&editMode=false"
            )))
            .await?;
        normalize_delivery_composer(response.body, draft_id)
    }

    async fn apply_delivery(
        &self,
        draft_id: &str,
        composer: &DeliveryComposer,
        delivery: &str,
    ) -> Result<(), ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let body = if delivery == "pickup" {
            json!({
                "meetup": true,
                "shipping": false,
                "sellerPaysShipping": false,
                "client": "ANDROID",
                "buyNow": false
            })
        } else {
            let package_size = composer
                .state
                .options
                .iter()
                .find(|option| option.value == delivery && option.mode == "shipping")
                .and_then(|option| option.package_size.as_deref())
                .ok_or_else(|| invalid_delivery_api(&composer.state, delivery))?;
            let address = composer
                .source
                .pointer("/sections/shipping/address")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    shipping_unavailable(&composer.state, "seller address is missing")
                })?;
            let required_string = |key: &str| {
                address
                    .get(key)
                    .and_then(Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        shipping_unavailable(
                            &composer.state,
                            &format!("seller address field `{key}` is missing"),
                        )
                    })
            };
            let postal_code = required_string("postalCode")?;
            let city = required_string("city")?;
            let name = required_string("name")?;
            let phone_number = address
                .get("phoneNumber")
                .or_else(|| address.get("mobilePhone"))
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    shipping_unavailable(&composer.state, "seller phone number is missing")
                })?;
            let street_name = address
                .get("streetName")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let street_no = address
                .get("streetNo")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query.append_pair("streetName", street_name);
            query.append_pair("streetNo", street_no);
            query.append_pair("postalCode", &postal_code);
            query.append_pair("city", &city);
            query.append_pair("adId", draft_id);
            query.append_pair("size", package_size);
            query.append_pair("name", &name);
            let response = self
                .json(HttpRequest::read(format!(
                    "/ui/addelivery/shipping?{}",
                    query.finish()
                )))
                .await?;
            let products = shipping_products(&response.body);
            if products.is_empty() {
                return Err(shipping_unavailable(
                    &composer.state,
                    "no shipping providers support the selected package size",
                ));
            }
            let context = composer.source.get("context").and_then(Value::as_object);
            let seller_pays_shipping = context
                .and_then(|context| context.get("sellerPaysShipping"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let buy_now = context
                .and_then(|context| {
                    context
                        .get("buyNow")
                        .and_then(Value::as_bool)
                        .filter(|selected| *selected)
                        .or_else(|| context.get("defaultBuyNow").and_then(Value::as_bool))
                })
                .unwrap_or(false);
            let save_address = composer
                .source
                .pointer("/sections/shipping/checkBoxes/saveAddress/checked")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let shipping_info = json!({
                "size": package_size,
                "streetName": street_name,
                "streetNo": street_no,
                "houseType": address.get("houseType").cloned().unwrap_or(Value::Null),
                "floorType": address.get("floorType").cloned().unwrap_or(Value::Null),
                "floorNo": address.get("floorNo").cloned().unwrap_or(Value::Null),
                "flatNo": address.get("flatNo").cloned().unwrap_or(Value::Null),
                "deliveryPointId": address.get("deliveryPointId").cloned().unwrap_or(Value::Null),
                "postalCode": postal_code,
                "city": city,
                "products": products,
                "saveAddress": save_address,
                "address": address.get("address").cloned().unwrap_or(Value::Null),
                "name": name,
                "phoneNumber": phone_number
            });
            json!({
                "meetup": false,
                "shipping": true,
                "sellerPaysShipping": seller_pays_shipping,
                "shippingInfo": shipping_info,
                "client": "ANDROID",
                "buyNow": buy_now
            })
        };
        self.json(HttpRequest::mutation(
            Method::Post,
            format!("/ads/{draft_id}/delivery"),
            RequestBody::Json(body),
        ))
        .await
        .map(|_| ())
    }

    async fn product_context(
        &self,
        draft_id: &str,
        revision: &str,
    ) -> Result<ProductContext, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let revision: String = url::form_urlencoded::byte_serialize(revision.as_bytes()).collect();
        let response = self
            .json(HttpRequest::read(format!(
                "/drafts/{draft_id}/products?revision={revision}"
            )))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|_| malformed_read_response("product_context"))
    }

    async fn publish_basic(
        &self,
        draft_id: &str,
        context: &ProductContext,
    ) -> Result<Publication, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let response = self
            .json(HttpRequest::mutation(
                Method::Post,
                format!("/drafts/{draft_id}/publish"),
                RequestBody::Json(json!({
                    "package": "basic",
                    "revision": context.revision,
                    "context": context.context,
                })),
            ))
            .await?;
        serde_json::from_value(response.body)
            .map_err(|_| uncertain_mutation_response("publish_basic"))
    }

    async fn confirmation(&self, listing_id: &str) -> Result<Confirmation, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let response = self
            .json(HttpRequest::read(format!(
                "/listings/{listing_id}/confirmation"
            )))
            .await?;
        serde_json::from_value(response.body).map_err(|_| malformed_read_response("confirmation"))
    }

    async fn track_confirmation(&self, confirmation: &Confirmation) -> Result<(), ApiError> {
        self.json(HttpRequest::mutation(
            Method::Post,
            "/tracking/confirmation",
            RequestBody::Json(json!({ "order_id": confirmation.order_id })),
        ))
        .await
        .map(|_| ())
    }

    async fn observed_listing(&self, listing_id: &str) -> Result<Value, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        self.json(HttpRequest::read(format!("/listings/{listing_id}")))
            .await
            .map(|response| response.body)
    }
}

pub(super) fn validate_price(price: &Value) -> Result<(), ApiError> {
    if !price.is_number()
        || price
            .as_f64()
            .is_none_or(|amount| !amount.is_finite() || amount < 0.0)
    {
        return Err(ApiError::new(
            "draft.invalid_price",
            "Price must be a non-negative number",
        ));
    }
    Ok(())
}

pub(super) fn composer_trade_type(value: &str) -> &str {
    match value {
        "sell" | "SELL" => "1",
        "give_away" | "GIVE_AWAY" => "2",
        "wanted" | "WANTED" => "3",
        value => value,
    }
}

fn composer_values(values: &Map<String, Value>) -> Result<Map<String, Value>, ApiError> {
    let mut encoded = values.clone();
    if let Some(trade_type) = encoded.get_mut("trade_type")
        && let Some(value) = trade_type.as_str()
    {
        *trade_type = Value::String(composer_trade_type(value).to_owned());
    }

    let Some(price) = encoded.remove("price") else {
        return Ok(encoded);
    };
    validate_price(&price)?;
    let price_text = price.to_string();
    match encoded.get("trade_type").and_then(Value::as_str) {
        Some("1") => {
            encoded.insert("price".to_owned(), json!([{ "price_amount": price_text }]));
        }
        Some("2") => {}
        Some("3") => {
            encoded.insert("price".to_owned(), json!([{ "price_max": price_text }]));
        }
        _ => {
            return Err(ApiError::new(
                "draft.price_trade_type_conflict",
                "Price requires a recognized sale or wanted trade type",
            ));
        }
    }
    Ok(encoded)
}

pub(super) fn error_at_stage(mut error: ApiError, stage: &str) -> ApiError {
    let mut details = error
        .details
        .take()
        .map(|details| *details)
        .unwrap_or_else(|| json!({}));
    if let Some(details) = details.as_object_mut() {
        details.insert("stage".to_owned(), Value::String(stage.to_owned()));
    }
    error.details = Some(Box::new(details));
    error
}

fn uncertain_item_update(response: &HttpResponse, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The price mutation may have succeeded, but its resulting revision is unknown",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": "apply_price",
        "status": response.status,
        "content_type": response.content_type,
        "reason": reason,
    })));
    error
}

fn normalize_item_update(response: HttpResponse, draft_id: &str) -> Result<String, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_item_update(
            &response,
            "successful response was not valid JSON",
        ));
    }
    let response_id = response.body.get("id").and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    });
    if response_id.as_deref() != Some(draft_id) {
        return Err(uncertain_item_update(
            &response,
            "successful response identified a different item",
        ));
    }
    response
        .body
        .get("etag")
        .and_then(Value::as_str)
        .filter(|etag| !etag.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            uncertain_item_update(
                &response,
                "successful response did not contain an authoritative ETag",
            )
        })
}

fn unexpected_representation(stage: &str, response: &HttpResponse) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an unsupported response representation",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": stage,
        "status": response.status,
        "content_type": response.content_type,
    })));
    error
}

pub(super) fn malformed_read_response(stage: &str) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an invalid success response",
    )
    .retry_classification(classify(
        FailureKind::MalformedSuccess,
        RetryContext::read(OperationMethod::Get),
    ));
    error.details = Some(Box::new(json!({ "stage": stage })));
    error
}

fn uncertain_mutation_response(stage: &str) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The mutation may have succeeded, but its resulting state is unknown",
    );
    error.details = Some(Box::new(json!({
        "stage": stage,
        "recovery_guidance": "Inspect authoritative state before continuing; do not repeat the mutation"
    })));
    error
}

fn uncertain_creation(response: &HttpResponse, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "Draft creation may have succeeded, but its remote identity could not be established",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": "create_draft",
        "status": response.status,
        "content_type": response.content_type,
        "completed_steps": [],
        "reason": reason,
        "recovery_guidance": "Inspect drafts in Tori before continuing; do not repeat draft creation"
    })));
    error
}

pub(super) fn draft_id_from_body(body: &Value) -> Option<String> {
    let ad = body.get("ad").unwrap_or(body);
    [ad.get("id"), ad.get("draft_id"), body.get("draft_id")]
        .into_iter()
        .flatten()
        .find_map(|value| match value {
            Value::String(value) if validate_resource_id(value, "draft").is_ok() => {
                Some(value.clone())
            }
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
}

fn draft_id_from_location(location: Option<&str>) -> Option<String> {
    let location = location?;
    let path = if location.starts_with('/') {
        location.to_owned()
    } else {
        let parsed = url::Url::parse(location).ok()?;
        if parsed.scheme() != "https" || parsed.host_str()? != "apps-adinput.svc.tori.fi" {
            return None;
        }
        parsed.path().to_owned()
    };
    let id = path
        .strip_prefix("/adinput/ad/recommerce/")?
        .trim_end_matches('/');
    (!id.contains('/') && validate_resource_id(id, "draft").is_ok()).then(|| id.to_owned())
}

pub(super) fn valid_image_location(location: &str) -> Option<String> {
    let parsed = url::Url::parse(location).ok()?;
    (parsed.scheme() == "https"
        && parsed.host_str() == Some("img.tori.net")
        && parsed.path().starts_with("/dynamic/default/")
        && parsed.query().is_none()
        && parsed.fragment().is_none())
    .then(|| location.to_owned())
}
