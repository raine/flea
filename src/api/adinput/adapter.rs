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
    async fn publication_draft_for_inspection(
        &self,
        draft_id: &str,
        _include_all_options: bool,
    ) -> Result<PublicationDraftState, ApiError> {
        self.publication_draft(draft_id).await
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
    async fn patch_item_fields(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
    ) -> Result<String, ApiError>;
    async fn update_recommerce(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
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
    async fn delivery_composer_for_inspection(
        &self,
        draft_id: &str,
        _include_all_options: bool,
    ) -> Result<DeliveryComposer, ApiError> {
        self.delivery_composer(draft_id).await
    }
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
    async fn confirmation(&self, publication: &Publication) -> Result<Confirmation, ApiError>;
    async fn track_confirmation(&self, confirmation: &Confirmation) -> Result<(), ApiError>;
    async fn active_listing(&self, listing_id: &str) -> Result<Option<Value>, ApiError>;
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

fn unrecognized_read(mut error: ApiError, source: &str, status: u16) -> ApiError {
    error = error.with_observation(
        Observation::unrecognized_response(source, Some(status)),
        ObservationOperation::Read,
    );
    error
}

pub(super) fn observation_source(path: &str) -> &'static str {
    if path.starts_with("/adinput/ad/withModel/") {
        "draft_detail"
    } else if path.starts_with("/ui/addelivery") || path.contains("/delivery") {
        "delivery_composer"
    } else if path == "/categories/taxonomy" {
        "category_taxonomy"
    } else if path.starts_with("/my/listings/")
        || path.starts_with("/listings/")
        || path.strip_prefix('/').is_some_and(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
    {
        "listing_detail"
    } else if path.starts_with("/orders/") || path.starts_with("/tracking/") {
        "publication_confirmation"
    } else {
        "draft_service"
    }
}

impl<T: HttpTransport> HttpAdInputApi<T> {
    async fn json(&self, request: HttpRequest) -> Result<HttpResponse, ApiError> {
        debug_assert!(!request.method.is_mutation() || request.retry == RetryPolicy::Never);
        let retry_context = request.retry_context();
        let source = observation_source(&request.path);
        let response = self.transport.execute(request).await?;
        if (200..300).contains(&response.status) {
            Ok(response)
        } else {
            Err(ApiError::response(&response, retry_context, source))
        }
    }

    async fn draft_request(
        &self,
        request: HttpRequest,
        require_authoritative_model: bool,
    ) -> Result<DraftState, ApiError> {
        let is_mutation = request.method.is_mutation();
        let source = observation_source(&request.path);
        let response = self.json(request).await?;
        if response.body_is_unparseable {
            let operation = if is_mutation {
                ObservationOperation::Mutation
            } else {
                ObservationOperation::Read
            };
            let mut error = unexpected_representation("receive_draft_state", &response)
                .with_observation(
                    Observation::unrecognized_response(source, Some(response.status)),
                    operation,
                );
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
            let operation = if is_mutation {
                ObservationOperation::Mutation
            } else {
                ObservationOperation::Read
            };
            let observation = Observation::unrecognized_response(source, Some(response.status));
            let classification = observation.retry_classification(operation);
            error.upstream_transient = classification.upstream_transient;
            error.safe_to_retry = classification.safe_to_retry;
            error.observation = Some(observation);
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

    async fn find_listing_summary(&self, listing_id: &str) -> Result<Option<Value>, ApiError> {
        const PAGE_SIZE: usize = 50;
        const PAGE_LIMIT: usize = 10_000;
        let mut offset = 0;
        let mut expected_total = None;
        for _ in 0..PAGE_LIMIT {
            let response = self
                .json(HttpRequest::read(format!(
                    "/search?limit={PAGE_SIZE}&offset={offset}"
                )))
                .await?;
            if response.body_is_unparseable {
                return Err(listing_observation_model_error(
                    response.status,
                    "collection_unparseable",
                ));
            }
            let summaries = response
                .body
                .get("summaries")
                .or_else(|| response.body.get("listings"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    listing_observation_model_error(response.status, "collection_unrecognized")
                })?;
            let total = response
                .body
                .get("total")
                .and_then(Value::as_u64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| {
                    listing_observation_model_error(response.status, "collection_total_invalid")
                })?;
            let stable_total = *expected_total.get_or_insert(total);
            if total != stable_total {
                return Err(listing_observation_model_error(
                    response.status,
                    "collection_total_changed",
                ));
            }
            for summary in summaries {
                if listing_value_id_matches(summary.get("id"), listing_id) {
                    return Ok(Some(normalize_observed_summary(summary, listing_id)));
                }
            }
            offset += summaries.len();
            if offset >= total {
                return Ok(None);
            }
            if summaries.is_empty() {
                return Err(listing_observation_model_error(
                    response.status,
                    "collection_pagination_incomplete",
                ));
            }
        }
        Err(listing_observation_model_error(
            200,
            "collection_pagination_bounded",
        ))
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
            return Err(ApiError::response(
                &response,
                retry_context,
                "draft_creation",
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

    async fn get_draft(&self, draft_id: &str) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        self.draft_request(
            HttpRequest::read(format!("/adinput/ad/withModel/{draft_id}")),
            true,
        )
        .await
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
        let response = self
            .json(HttpRequest::read(format!(
                "/adinput/ad/withModel/{draft_id}"
            )))
            .await?;
        if response.body_is_unparseable {
            return Err(malformed_read_response("publication_draft"));
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
        parsed.map_err(|error| unrecognized_read(error, "draft_detail", status))
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
            .map_err(|error| unrecognized_read(error, "category_taxonomy", response.status))
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
        self.delivery_composer_for_inspection(draft_id, false).await
    }

    async fn delivery_composer_for_inspection(
        &self,
        draft_id: &str,
        include_all_options: bool,
    ) -> Result<DeliveryComposer, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let draft_id_query: String =
            url::form_urlencoded::byte_serialize(draft_id.as_bytes()).collect();
        let response = self
            .json(HttpRequest::read(format!(
                "/ui/addelivery?adId={draft_id_query}&editMode=false"
            )))
            .await?;
        let status = response.status;
        let parsed = if include_all_options {
            normalize_delivery_composer_with_limit(response.body, draft_id, usize::MAX)
        } else {
            normalize_delivery_composer(response.body, draft_id)
        };
        parsed.map_err(|error| unrecognized_read(error, "delivery_composer", status))
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
        let encoded_revision: String =
            url::form_urlencoded::byte_serialize(revision.as_bytes()).collect();
        let response = self
            .json(HttpRequest::read(format!(
                "/adinput/product/recommerce/{draft_id}/productcontext?adRevision={encoded_revision}"
            )))
            .await?;
        let status = response.status;
        normalize_product_context(response.body, draft_id, revision)
            .map_err(|error| unrecognized_read(error, "publication_product_context", status))
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
            .json(HttpRequest::read(format!(
                "/orders/{}/confirmation/{}",
                publication.order_id, publication.listing_id
            )))
            .await?;
        if response.body_is_unparseable || !response.body.is_object() {
            return Err(malformed_read_response("confirmation"));
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
        self.json(HttpRequest::read(format!(
            "/tracking/adconfirmation?{}",
            query.finish()
        )))
        .await
        .map(|_| ())
    }

    async fn active_listing(&self, listing_id: &str) -> Result<Option<Value>, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        Ok(self
            .find_listing_summary(listing_id)
            .await?
            .filter(listing_is_active))
    }

    async fn observed_listing(&self, listing_id: &str) -> Result<Value, ApiError> {
        validate_resource_id(listing_id, "listing")?;
        let detail = self.json(HttpRequest::read(format!("/{listing_id}"))).await;
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
        if let Some(summary) = self.find_listing_summary(listing_id).await? {
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
                    Observation::unrecognized_response("listing_detail", Some(response.status)),
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
                        "listing_detail",
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

fn listing_value_id_matches(value: Option<&Value>, listing_id: &str) -> bool {
    match value {
        Some(Value::String(value)) => value == listing_id,
        Some(Value::Number(value)) => value.to_string() == listing_id,
        _ => false,
    }
}

fn observed_detail_matches(value: &Value, listing_id: &str) -> bool {
    listing_value_id_matches(
        value.get("id").or_else(|| value.get("listing_id")),
        listing_id,
    )
}

fn listing_is_active(value: &Value) -> bool {
    let state = value.get("state");
    let candidates = [
        state.and_then(Value::as_str),
        state
            .and_then(|state| state.get("type"))
            .and_then(Value::as_str),
        state
            .and_then(|state| state.get("display"))
            .and_then(Value::as_str),
        state
            .and_then(|state| state.get("label"))
            .and_then(Value::as_str),
    ];
    candidates.into_iter().flatten().any(|state| {
        matches!(
            state.trim().to_ascii_lowercase().as_str(),
            "active" | "published"
        )
    })
}

fn normalize_observed_summary(summary: &Value, listing_id: &str) -> Value {
    let data = summary.get("data").unwrap_or(&Value::Null);
    json!({
        "listing_id": listing_id,
        "title": data.get("title"),
        "price": data.get("subtitle"),
        "state": summary.get("state"),
        "location": data.get("location").or_else(|| data.get("area")).or_else(|| data.get("place")),
        "image_url": data.get("image"),
        "public_url": public_listing_url(listing_id),
        "observation_source": "collection",
    })
}

fn public_listing_url(listing_id: &str) -> String {
    format!("https://www.tori.fi/recommerce/forsale/item/{listing_id}")
}

fn listing_observation_model_error(status: u16, model: &str) -> ApiError {
    let mut error = ApiError::new(
        "upstream.unrecognized_model",
        "Tori returned an unrecognized listing collection model",
    )
    .with_observation(
        Observation::unrecognized_response("listing_collection", Some(status)),
        ObservationOperation::Read,
    );
    error.status = Some(status);
    error.details = Some(Box::new(json!({
        "status": status,
        "response_model": model,
    })));
    error
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

fn item_creation_fields(values: &Map<String, Value>) -> Result<Map<String, Value>, ApiError> {
    let mut data = Map::new();
    for key in ["title", "description"] {
        if let Some(value) = values.get(key).filter(|value| !value.is_null()) {
            data.insert(key.to_owned(), value.clone());
        }
    }
    if let Some(condition) = values.get("condition").filter(|value| !value.is_null()) {
        data.insert("condition".to_owned(), numeric_string(condition));
    }
    if let Some(price) = values.get("price") {
        validate_price(price)?;
        if price.as_f64().is_some_and(|amount| amount > 0.0) {
            data.insert("price".to_owned(), json!({ "price_amount": price }));
        }
    }
    let excluded = [
        "category",
        "delivery",
        "image",
        "location",
        "multi_image",
        "price",
        "postal_code",
        "postal-code",
        "postalCode",
        "title",
        "description",
        "condition",
        "trade_type",
        "revision",
    ];
    for (key, value) in values {
        if excluded.contains(&key.as_str())
            || !matches!(value, Value::String(_) | Value::Number(_) | Value::Bool(_))
        {
            continue;
        }
        data.insert(key.clone(), numeric_string(value));
    }
    Ok(data)
}

fn numeric_string(value: &Value) -> Value {
    value
        .as_str()
        .and_then(|value| value.parse::<i64>().ok())
        .map_or_else(|| value.clone(), |value| json!(value))
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

fn uncertain_item_update(
    response: &HttpResponse,
    stage: &str,
    path: &str,
    reason: &str,
) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The item patch may have succeeded, but its resulting revision is unknown",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": stage,
        "path": path,
        "status": response.status,
        "content_type": response.content_type,
        "reason": reason,
    })));
    error
}

fn normalize_item_update(
    response: HttpResponse,
    draft_id: &str,
    stage: &str,
) -> Result<String, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_item_update(
            &response,
            stage,
            "$",
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
            stage,
            "$.id",
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
                stage,
                "$.etag",
                "successful response did not contain an authoritative ETag",
            )
        })
}

fn normalize_recommerce_update(
    response: HttpResponse,
    draft_id: &str,
) -> Result<DraftState, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_mutation_model(
            &response,
            "update_recommerce",
            "$",
            "successful response was not valid JSON",
        ));
    }
    let body = if response.body.get("ad").is_some() {
        response.body.clone()
    } else {
        json!({ "ad": response.body.clone(), "model": { "sections": [] } })
    };
    normalize_source_draft_state(body, response.etag.as_deref())
        .and_then(|state| {
            (state.draft_id == draft_id)
                .then_some(state)
                .ok_or_else(|| {
                    model_error(
                        "update_recommerce",
                        "$.id",
                        "successful response identified a different draft",
                    )
                })
        })
        .map_err(|error| {
            let path = error
                .details
                .as_deref()
                .and_then(|details| details.get("path"))
                .and_then(Value::as_str)
                .unwrap_or("$");
            uncertain_mutation_model(&response, "update_recommerce", path, &error.message)
        })
}

fn uncertain_mutation_model(
    response: &HttpResponse,
    stage: &str,
    path: &str,
    reason: &str,
) -> ApiError {
    let mut error = ApiError::new(
        "mutation.uncertain",
        "The mutation may have succeeded, but its resulting state is unknown",
    );
    error.status = Some(response.status);
    error.details = Some(Box::new(json!({
        "stage": stage,
        "path": path,
        "reason": reason,
        "status": response.status,
    })));
    error
}

fn normalize_product_context(
    body: Value,
    draft_id: &str,
    revision: &str,
) -> Result<ProductContext, ApiError> {
    let id = body.get("id").and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    });
    if id.as_deref() != Some(draft_id) {
        return Err(read_model_error(
            "product_context",
            "$.id",
            "product context identified a different draft",
        ));
    }
    let basic_package_urn = body
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| {
            choices.iter().find_map(|choice| {
                (choice.get("package-identifier").and_then(Value::as_i64) == Some(10))
                    .then(|| choice.get("specification-urn").and_then(Value::as_str))
                    .flatten()
            })
        })
        .filter(|urn| *urn == "urn:product:package-specification:10")
        .ok_or_else(|| {
            read_model_error(
                "product_context",
                "$.choices[*].specification-urn",
                "free Basic package is unavailable",
            )
        })?;
    Ok(ProductContext {
        revision: revision.to_owned(),
        basic_package_urn: basic_package_urn.to_owned(),
    })
}

fn normalize_publication(
    response: HttpResponse,
    draft_id: &str,
    revision: &str,
) -> Result<Publication, ApiError> {
    if response.body_is_unparseable {
        return Err(uncertain_mutation_model(
            &response,
            "package_choice",
            "$",
            "successful response was not valid JSON",
        ));
    }
    let order_id = response
        .body
        .get("order-id")
        .and_then(|value| match value {
            Value::String(value) if !value.is_empty() => Some(value.clone()),
            Value::Number(value) => Some(value.to_string()),
            _ => None,
        })
        .ok_or_else(|| {
            uncertain_mutation_model(
                &response,
                "package_choice",
                "$.order-id",
                "successful response omitted the order identity",
            )
        })?;
    if response.body.get("is-completed").and_then(Value::as_bool) != Some(true) {
        return Err(uncertain_mutation_model(
            &response,
            "package_choice",
            "$.is-completed",
            "package order completion is unavailable",
        ));
    }
    Ok(Publication {
        listing_id: draft_id.to_owned(),
        revision: revision.to_owned(),
        state: "pending".to_owned(),
        order_id,
    })
}

fn read_model_error(stage: &str, path: &str, reason: &str) -> ApiError {
    let mut error = model_error(stage, path, reason).retry_classification(classify(
        FailureKind::MalformedSuccess,
        RetryContext::read(OperationMethod::Get),
    ));
    error.code = "upstream.unexpected_response".to_owned();
    error
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
    let source = match stage {
        "publication_draft" => "draft_detail",
        "delivery_composer" => "delivery_composer",
        "observed_listing" => "listing_detail",
        "confirmation" => "publication_confirmation",
        "category_predictions" => "draft_category_predictions",
        "source_listing" => "listing_detail",
        _ => "draft_service",
    };
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an invalid success response",
    )
    .with_observation(
        Observation::unrecognized_response(source, Some(200)),
        ObservationOperation::Read,
    );
    error.details = Some(Box::new(json!({ "stage": stage })));
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
