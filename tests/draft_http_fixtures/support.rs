pub(crate) use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

pub(crate) use flea::{
    domain::{
        field::{FieldStatus, FieldType, Requirement},
        observation::ObservationState,
    },
    marketplace::tori::adinput::{
        AttachmentRecoveryStatus, DraftDeliveryApi, DraftImages, DraftListingObservation,
        DraftMutation, DraftPublication, DraftRead, DraftWorkflow, HttpAdInputApi, HttpRequest,
        HttpResponse, HttpTransport, ImageRecoveryOperation, ImageState, Method, ObservationStatus,
        ProcessingRecoveryStatus, RecoveryStatus, RequestBody, RetryPolicy, UploadRecoveryStatus,
        WorkflowConfig,
    },
};
pub(crate) use serde_json::{Map, Value, json};

#[derive(Clone)]
pub(crate) struct FixtureTransport {
    responses:
        Arc<Mutex<VecDeque<Result<HttpResponse, flea::marketplace::tori::adinput::ApiError>>>>,
    search_responses: Arc<Mutex<VecDeque<HttpResponse>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl FixtureTransport {
    pub(crate) fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().map(Ok).collect())),
            search_responses: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub(crate) fn with_search_responses(
        self,
        responses: impl IntoIterator<Item = HttpResponse>,
    ) -> Self {
        *self.search_responses.lock().unwrap() = responses.into_iter().collect();
        self
    }

    pub(crate) fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for FixtureTransport {
    async fn execute(
        &self,
        request: HttpRequest,
    ) -> Result<HttpResponse, flea::marketplace::tori::adinput::ApiError> {
        let is_search = request.path.starts_with("/search?");
        self.requests.lock().unwrap().push(request);
        if is_search {
            return Ok(self
                .search_responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| response(200, json!({ "summaries": [], "total": 0 }))));
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("fixture response")
    }
}

pub(crate) fn response(status: u16, body: Value) -> HttpResponse {
    HttpResponse {
        status,
        etag: None,
        content_type: Some("application/json".to_owned()),
        location: None,
        body,
        body_is_unparseable: false,
    }
}

pub(crate) fn html_response(status: u16) -> HttpResponse {
    HttpResponse {
        status,
        etag: None,
        content_type: Some("text/html".to_owned()),
        location: None,
        body: Value::Null,
        body_is_unparseable: true,
    }
}

pub(crate) fn response_with_location(status: u16, location: &str) -> HttpResponse {
    HttpResponse {
        status,
        etag: None,
        content_type: None,
        location: Some(location.to_owned()),
        body: Value::Null,
        body_is_unparseable: false,
    }
}

pub(crate) fn image(id: &str, position: usize, state: &str, width: u32, height: u32) -> Value {
    let url = format!("https://img.tori.net/dynamic/default/{id}.jpg");
    json!({
        "image_id": url,
        "url": url,
        "position": position,
        "state": state,
        "width": width,
        "height": height,
        "mime_type": "image/jpeg"
    })
}

pub(crate) fn draft(etag: &str, extra: Value) -> Value {
    let mut value = json!({
        "draft_id": "draft-1",
        "etag": etag,
        "values": {},
        "fields": [],
        "options": [],
        "required_fields": [],
        "images": [],
        "cleared_fields": [],
        "predictions": []
    });
    let Value::Object(target) = &mut value else {
        unreachable!()
    };
    let Value::Object(extra) = extra else {
        panic!("extra must be an object")
    };
    target.extend(extra);
    value
}

pub(crate) struct HangingPollTransport {
    pub(crate) responses: Mutex<VecDeque<HttpResponse>>,
}

impl HttpTransport for HangingPollTransport {
    async fn execute(
        &self,
        request: HttpRequest,
    ) -> Result<HttpResponse, flea::marketplace::tori::adinput::ApiError> {
        if request.path.starts_with("/search?") {
            return Ok(response(200, json!({ "summaries": [], "total": 0 })));
        }
        if let Some(response) = self.responses.lock().unwrap().pop_front() {
            return Ok(response);
        }
        std::future::pending().await
    }
}

pub(crate) fn config() -> WorkflowConfig {
    WorkflowConfig {
        image_processing_timeout: Duration::from_secs(1),
        image_poll_interval: Duration::ZERO,
        image_poll_limit: 2,
        listing_observation_timeout: Duration::from_secs(1),
        listing_poll_interval: Duration::ZERO,
        listing_poll_limit: 0,
    }
}

pub(crate) fn delivery_page(selected: Option<&str>) -> Value {
    let shipping = selected.is_some_and(|value| value.starts_with("shipping:"));
    let meetup = selected == Some("pickup");
    let package_size = selected
        .and_then(|value| value.strip_prefix("shipping:"))
        .map(str::to_ascii_uppercase);
    json!({
        "context": {
            "adId": "draft-1",
            "shipping": shipping,
            "meetup": meetup,
            "packageSize": package_size,
            "shippingProducts": [],
            "sellerPaysShipping": false,
            "buyNow": false,
            "defaultBuyNow": true
        },
        "sections": {
            "deliveryOptions": {
                "shipping": { "title": "Tori delivery" },
                "meetup": { "title": "Pickup or direct arrangement" }
            },
            "shipping": {
                "address": {
                    "name": "Fixture Seller",
                    "address": "Fixture address 1",
                    "streetName": "Fixture address",
                    "streetNo": "1",
                    "postalCode": "00100",
                    "city": "Helsinki",
                    "phoneNumber": "0400000000"
                },
                "packageSizes": {
                    "small": { "title": "Small package", "size": "SMALL" },
                    "medium": { "title": "Medium package", "size": "MEDIUM" },
                    "large": { "title": "Large package", "size": "LARGE" }
                },
                "checkBoxes": { "saveAddress": { "checked": true } }
            }
        }
    })
}

pub(crate) fn shipping_page(products: &[&str]) -> Value {
    json!({
        "sections": {
            "shipping": {
                "providers": {
                    "options": products
                        .iter()
                        .map(|product| json!({ "product": product }))
                        .collect::<Vec<_>>()
                }
            }
        }
    })
}

pub(crate) fn publication_values() -> Value {
    json!({
        "category": "furniture/chairs",
        "title": "Chair",
        "description": "Solid birch chair",
        "trade_type": "sell",
        "price": 45,
        "postal_code": "00100"
    })
}

pub(crate) fn category_taxonomy(category_id: &str, selectable: bool) -> HttpResponse {
    response(
        200,
        json!({
            "categories": [{
                "id": category_id,
                "label": "Fixture category",
                "isSelectable": selectable
            }]
        }),
    )
}

pub(crate) fn successful_publish_responses() -> Vec<HttpResponse> {
    let valid = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["category", "title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let fresh = draft(
        "two",
        json!({
            "values": valid["values"].clone(),
            "images": valid["images"].clone()
        }),
    );
    vec![
        response(200, valid.clone()),
        response(200, delivery_page(Some("pickup"))),
        category_taxonomy("furniture/chairs", true),
        response(200, valid.clone()),
        response(200, item_update("draft-1", "two", json!(45))),
        response(200, fresh),
        response(
            200,
            json!({
                "id": "draft-1",
                "ad-type": "recommerce",
                "etag": "revision-7",
                "values": valid["values"].clone()
            }),
        ),
        response(204, Value::Null),
        response(200, delivery_page(Some("pickup"))),
        response(
            200,
            json!({
                "id": "draft-1",
                "choices": [{
                    "package-identifier": 10,
                    "specification-urn": "urn:product:package-specification:10"
                }]
            }),
        ),
        response(200, json!({ "order-id": 4, "is-completed": true })),
        response(200, json!({ "title": "Published" })),
        response(200, json!({ "transactionId": 4 })),
        response(200, json!({ "listing_id": "draft-1", "state": "pending" })),
    ]
}

pub(crate) fn listing_collection(listing_id: &str, state: &str) -> HttpResponse {
    response(
        200,
        json!({
            "summaries": [{
                "id": listing_id,
                "state": { "type": state },
                "data": {
                    "title": "Published chair",
                    "subtitle": "45 €",
                    "location": "Helsinki",
                    "image": "https://img.example/chair.jpg"
                }
            }],
            "total": 1
        }),
    )
}

pub(crate) fn observed_draft(id: &str, etag: &str, values: Value) -> Value {
    json!({
        "ad": {
            "id": id,
            "ad-type": "recommerce",
            "etag": etag,
            "values": values,
            "meta-data": {},
            "locked-fields": []
        },
        "model": { "sections": [] }
    })
}

pub(crate) fn item_update(id: &str, etag: &str, price: Value) -> Value {
    json!({
        "id": id,
        "etag": etag,
        "data": { "price": { "price_amount": price } },
        "violations": []
    })
}

pub(crate) fn decimal_field(key: &str) -> Value {
    json!({
        "key": key,
        "label": key,
        "type": "decimal",
        "requirement": "optional",
        "status": "set",
        "value": 10,
        "section": "details"
    })
}

pub(crate) fn select_field(key: &str, value: Value) -> Value {
    json!({
        "key": key,
        "label": key,
        "type": "select",
        "requirement": "required",
        "status": "set",
        "value": value,
        "section": "details"
    })
}

pub(crate) fn composer_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../fixtures/adinput/bicycle-accessory-composer-live.json"
    ))
    .unwrap()
}

pub(crate) fn composer_with_category_options(selected: &str, option_ids: &[String]) -> Value {
    let mut fixture = composer_fixture();
    fixture["ad"]["values"]["category"] = json!(selected);
    fixture["model"]["sections"][1]["content"][1]["value-nodes"] = Value::Array(
        option_ids
            .iter()
            .map(|id| {
                json!({
                    "id": id,
                    "label": format!("Category {id}"),
                    "persistable": true
                })
            })
            .collect(),
    );
    fixture
}

pub(crate) fn delivery_fixture() -> Value {
    serde_json::from_str(include_str!(
        "../fixtures/adinput/bicycle-accessory-delivery-live.json"
    ))
    .unwrap()
}

pub(crate) fn remove_composer_field(fixture: &mut Value, field: &str) {
    fixture["ad"]["values"]
        .as_object_mut()
        .unwrap()
        .remove(field);
    for section in fixture["model"]["sections"].as_array_mut().unwrap() {
        section["content"]
            .as_array_mut()
            .unwrap()
            .retain(|widget| widget["id"] != field);
    }
}
