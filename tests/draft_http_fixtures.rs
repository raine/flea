use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use flea::{
    api::adinput::{
        AdInputApi, AttachmentRecoveryStatus, DraftWorkflow, HttpAdInputApi, HttpRequest,
        HttpResponse, HttpTransport, ImageRecoveryOperation, ImageState, Method, ObservationStatus,
        ProcessingRecoveryStatus, RecoveryStatus, RequestBody, RetryPolicy, UploadRecoveryStatus,
        WorkflowConfig,
    },
    domain::field::{FieldStatus, FieldType, Requirement},
};
use serde_json::{Map, Value, json};

#[derive(Clone)]
struct FixtureTransport {
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, flea::api::adinput::ApiError>>>>,
    search_responses: Arc<Mutex<VecDeque<HttpResponse>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl FixtureTransport {
    fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().map(Ok).collect())),
            search_responses: Arc::new(Mutex::new(VecDeque::new())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn with_search_responses(self, responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        *self.search_responses.lock().unwrap() = responses.into_iter().collect();
        self
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl HttpTransport for FixtureTransport {
    async fn execute(
        &self,
        request: HttpRequest,
    ) -> Result<HttpResponse, flea::api::adinput::ApiError> {
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

fn response(status: u16, body: Value) -> HttpResponse {
    HttpResponse {
        status,
        etag: None,
        content_type: Some("application/json".to_owned()),
        location: None,
        body,
        body_is_unparseable: false,
    }
}

fn html_response(status: u16) -> HttpResponse {
    HttpResponse {
        status,
        etag: None,
        content_type: Some("text/html".to_owned()),
        location: None,
        body: Value::Null,
        body_is_unparseable: true,
    }
}

fn response_with_location(status: u16, location: &str) -> HttpResponse {
    HttpResponse {
        status,
        etag: None,
        content_type: None,
        location: Some(location.to_owned()),
        body: Value::Null,
        body_is_unparseable: false,
    }
}

fn image(id: &str, position: usize, state: &str, width: u32, height: u32) -> Value {
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

fn draft(etag: &str, extra: Value) -> Value {
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

struct HangingPollTransport {
    responses: Mutex<VecDeque<HttpResponse>>,
}

impl HttpTransport for HangingPollTransport {
    async fn execute(
        &self,
        request: HttpRequest,
    ) -> Result<HttpResponse, flea::api::adinput::ApiError> {
        if request.path.starts_with("/search?") {
            return Ok(response(200, json!({ "summaries": [], "total": 0 })));
        }
        if let Some(response) = self.responses.lock().unwrap().pop_front() {
            return Ok(response);
        }
        std::future::pending().await
    }
}

fn config() -> WorkflowConfig {
    WorkflowConfig {
        image_processing_timeout: Duration::from_secs(1),
        image_poll_interval: Duration::ZERO,
        image_poll_limit: 2,
        listing_observation_timeout: Duration::from_secs(1),
        listing_poll_interval: Duration::ZERO,
        listing_poll_limit: 0,
    }
}

fn delivery_page(selected: Option<&str>) -> Value {
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

fn shipping_page(products: &[&str]) -> Value {
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

fn publication_values() -> Value {
    json!({
        "category": "furniture/chairs",
        "title": "Chair",
        "description": "Solid birch chair",
        "trade_type": "sell",
        "price": 45,
        "postal_code": "00100"
    })
}

fn category_taxonomy(category_id: &str, selectable: bool) -> HttpResponse {
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

fn successful_publish_responses() -> Vec<HttpResponse> {
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

fn listing_collection(listing_id: &str, state: &str) -> HttpResponse {
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

fn observed_draft(id: &str, etag: &str, values: Value) -> Value {
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

fn item_update(id: &str, etag: &str, price: Value) -> Value {
    json!({
        "id": id,
        "etag": etag,
        "data": { "price": { "price_amount": price } },
        "violations": []
    })
}

fn decimal_field(key: &str) -> Value {
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

#[tokio::test]
async fn creation_normalizes_the_source_observed_json_success() {
    let transport = FixtureTransport::new([response(
        200,
        observed_draft("98231", "W/\"7\"", json!({ "title": "Chair" })),
    )]);
    let api = HttpAdInputApi::new(transport.clone());

    let state = api.create_draft().await.unwrap();

    assert_eq!(state.draft_id, "98231");
    assert_eq!(state.etag, "W/\"7\"");
    assert_eq!(state.values["title"], "Chair");
    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, Method::Post);
    assert_eq!(requests[0].path, "/adinput/ad/withModel/recommerce");
    assert_eq!(requests[0].body, RequestBody::Empty);
    assert_eq!(requests[0].retry, RetryPolicy::Never);
}

#[tokio::test]
async fn creation_observes_location_and_see_other_identity_without_reposting() {
    for status in [201, 303] {
        let transport = FixtureTransport::new([
            response_with_location(
                status,
                "https://apps-adinput.svc.tori.fi/adinput/ad/recommerce/98231",
            ),
            response(200, observed_draft("98231", "W/\"8\"", json!({}))),
        ]);
        let api = HttpAdInputApi::new(transport.clone());

        let state = api.create_draft().await.unwrap();

        assert_eq!(state.draft_id, "98231");
        assert_eq!(state.etag, "W/\"8\"");
        let requests = transport.requests();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].method, Method::Post);
        assert_eq!(requests[1].method, Method::Get);
        assert_eq!(requests[1].path, "/adinput/ad/withModel/98231");
    }
}

#[tokio::test]
async fn uncertain_creation_covers_unparseable_empty_and_missing_identity_successes() {
    let mut non_json = response(200, Value::Null);
    non_json.body_is_unparseable = true;
    non_json.content_type = Some("text/html".to_owned());
    let mut malformed_json = response(200, Value::Null);
    malformed_json.body_is_unparseable = true;
    for response in [
        non_json,
        malformed_json,
        response(204, Value::Null),
        response(200, json!({})),
    ] {
        let transport = FixtureTransport::new([response]);
        let api = HttpAdInputApi::new(transport.clone());

        let error = api.create_draft().await.unwrap_err();

        assert_eq!(error.code, "mutation.uncertain");
        assert!(!error.upstream_transient);
        assert!(!error.safe_to_retry);
        let details = error.details.unwrap();
        assert_eq!(details["stage"], "create_draft");
        assert_eq!(details["completed_steps"], json!([]));
        assert!(
            details["recovery_guidance"]
                .as_str()
                .unwrap()
                .contains("do not repeat")
        );
        assert_eq!(transport.requests().len(), 1);
        assert_eq!(transport.requests()[0].retry, RetryPolicy::Never);
    }
}

#[tokio::test]
async fn creation_observation_failure_preserves_authoritative_identity() {
    let transport = FixtureTransport::new([
        response_with_location(201, "/adinput/ad/recommerce/98231"),
        response(503, json!({ "message": "observation unavailable" })),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .create(Map::new(), &[] as &[&str])
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "98231");
    assert_eq!(
        recovery.completed_steps,
        ["create_draft", "establish_identity"]
    );
    assert_eq!(recovery.next_safe_actions, ["flea draft show 98231"]);
}

#[tokio::test]
async fn creation_rejects_noncanonical_redirects_without_following_them() {
    let mut redirected = response_with_location(307, "https://example.com/draft/98231");
    redirected.body = json!({ "message": "redirect" });
    let transport = FixtureTransport::new([redirected]);
    let api = HttpAdInputApi::new(transport.clone());

    let error = api.create_draft().await.unwrap_err();

    assert_eq!(error.status, Some(307));
    assert_eq!(transport.requests().len(), 1);
}

#[tokio::test]
async fn source_backed_shape_validation_happens_before_any_field_mutation() {
    let transport = FixtureTransport::new([response(
        200,
        draft(
            "one",
            json!({
                "values": { "title": 10 },
                "fields": [decimal_field("title")]
            }),
        ),
    )]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.as_ref().unwrap()["stage"], "validate_title");
    assert_eq!(error.details.as_ref().unwrap()["fields"], json!(["title"]));
    assert_eq!(
        error.details.as_ref().unwrap()["field_errors"][0]["source"],
        "local_schema"
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.absent_fields, ["title"]);
    assert_eq!(recovery.unattempted_fields, ["postal_code"]);
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.requests()[0].method, Method::Get);
}

#[tokio::test]
async fn structured_upstream_errors_name_the_active_field_and_preserve_progress() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "values": { "title": "Old", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "values": { "title": "Chair", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        response(
            422,
            json!({
                "errors": [{
                    "field": "item.price",
                    "code": "out_of_range",
                    "message": "Price is outside the accepted range"
                }]
            }),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("price".to_owned(), json!(25)),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    let details = error.details.unwrap();
    assert_eq!(details["stage"], "apply_price");
    assert_eq!(details["fields"], json!(["price"]));
    assert_eq!(details["field_errors"][0]["field"], "price");
    assert_eq!(details["field_errors"][0]["code"], "out_of_range");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.completed_steps, ["fetch_draft", "apply_title"]);
    assert_eq!(recovery.persisted_fields, ["title"]);
    assert_eq!(recovery.absent_fields, ["price"]);
    assert_eq!(recovery.unattempted_fields, ["postal_code"]);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea draft show draft-1",
            "flea draft update draft-1 --price VALUE"
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].method, Method::Put);
    assert_eq!(requests[2].method, Method::Patch);
}

#[tokio::test]
async fn html_5xx_observation_reports_partial_persistence_without_replaying() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "values": { "title": "Old", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "values": { "title": "Chair", "trade_type": "1", "price": 10 }
                }),
            ),
        ),
        html_response(502),
        response(
            200,
            draft(
                "three",
                json!({
                    "values": { "title": "Chair", "trade_type": "1", "price": 25 }
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("price".to_owned(), json!(25)),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    assert_eq!(error.details.as_ref().unwrap()["stage"], "apply_price");
    assert_eq!(error.details.as_ref().unwrap()["content_type"], "text/html");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.persisted_fields, ["title", "price"]);
    assert_eq!(recovery.failed_stage.as_deref(), Some("apply_price"));
    assert_eq!(recovery.observation.status, ObservationStatus::Observed);
    assert!(recovery.observation.observed_at.is_some());
    assert_eq!(recovery.observed_etag.as_deref(), Some("three"));
    assert_eq!(
        recovery
            .field_summary
            .iter()
            .map(|field| (field.field.as_str(), field.status))
            .collect::<Vec<_>>(),
        [
            ("price", RecoveryStatus::Persisted),
            ("title", RecoveryStatus::Persisted),
            ("postal_code", RecoveryStatus::Unattempted)
        ]
    );
    assert!(recovery.absent_fields.is_empty());
    assert!(recovery.indeterminate_fields.is_empty());
    assert_eq!(recovery.unattempted_fields, ["postal_code"]);
    let requests = transport.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == Method::Patch)
            .count(),
        1
    );
}

#[tokio::test]
async fn unchanged_etag_proves_an_ambiguous_field_absent_and_limits_recovery() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({ "values": { "trade_type": "1", "price": 10 } }),
            ),
        ),
        html_response(502),
        response(
            200,
            draft(
                "one",
                json!({ "values": { "trade_type": "1", "price": 10 } }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("price".to_owned(), json!(25))]),
        )
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.absent_fields, ["price"]);
    assert!(recovery.persisted_fields.is_empty());
    assert!(recovery.indeterminate_fields.is_empty());
    assert!(!recovery.safe_to_retry);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea draft show draft-1",
            "flea draft update draft-1 --price VALUE"
        ]
    );
    assert_eq!(error.details.unwrap()["observation"]["etag_changed"], false);
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.method == Method::Patch)
            .count(),
        1
    );
}

#[tokio::test]
async fn failed_observation_keeps_the_active_field_indeterminate() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({ "values": { "trade_type": "1", "price": 10 } }),
            ),
        ),
        html_response(502),
        response(503, json!({ "message": "observation unavailable" })),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("price".to_owned(), json!(25))]),
        )
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.active_step.as_deref(), Some("apply_price"));
    assert_eq!(recovery.fields, ["price"]);
    assert_eq!(recovery.indeterminate_fields, ["price"]);
    assert!(recovery.absent_fields.is_empty());
    assert!(recovery.manual_inspection_required);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.next_safe_actions, ["flea draft show draft-1"]);
    assert_eq!(recovery.observation.status, ObservationStatus::Unavailable);
    assert!(recovery.destructive_actions.is_empty());
    assert_eq!(
        recovery.field_summary[0].status,
        RecoveryStatus::Indeterminate
    );
    assert_eq!(transport.requests().len(), 3);
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.method == Method::Patch)
            .count(),
        1
    );
}

#[tokio::test]
async fn update_conflict_fetches_and_returns_fresh_remote_state() {
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({ "values": { "title": "old" } }))),
        response(412, json!({ "message": "etag mismatch" })),
        response(
            200,
            draft("two", json!({ "values": { "title": "other agent" } })),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("title".to_owned(), json!("mine"))]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "draft.conflict");
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(recovery.completed_steps, ["fetch_draft"]);
    assert!(!recovery.upstream_transient);
    assert!(recovery.safe_to_retry);
    assert_eq!(
        recovery.observation.status,
        ObservationStatus::ChangedByAnotherClient
    );
    assert_eq!(recovery.observed_etag.as_deref(), Some("two"));
    assert_eq!(recovery.next_safe_actions, ["flea draft show draft-1"]);
    assert!(recovery.destructive_actions.is_empty());
    assert_eq!(recovery.fresh_state.unwrap().values["title"], "other agent");
    let requests = transport.requests();
    assert_eq!(requests[1].if_match.as_deref(), Some("one"));
    assert_eq!(requests[1].retry, RetryPolicy::Never);
}

#[tokio::test]
async fn sale_price_uses_the_item_partial_update_and_authoritative_observation() {
    let item_response: Value =
        serde_json::from_str(include_str!("fixtures/drafts/item-price-update.json")).unwrap();
    let observed_response: Value =
        serde_json::from_str(include_str!("fixtures/drafts/priced-composer.json")).unwrap();
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft(
                "46031010",
                "W/\"1178961228\"",
                json!({ "category": 258, "trade_type": "1" }),
            ),
        ),
        response(200, item_response),
        response(200, observed_response),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "46031010",
            &Map::from_iter([("price".to_owned(), json!(5))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["price"], json!(5));
    assert_eq!(state.draft.values["trade_type"], "1");
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[0].method, Method::Get);
    assert_eq!(requests[1].method, Method::Patch);
    assert_eq!(requests[1].path, "/items/46031010");
    assert_eq!(requests[1].if_match.as_deref(), Some("W/\"1178961228\""));
    assert_eq!(requests[1].retry, RetryPolicy::Never);
    assert_eq!(
        requests[1].body,
        RequestBody::Json(json!({
            "data": { "price": { "price_amount": 5 } }
        }))
    );
    assert_eq!(requests[2].method, Method::Get);
    assert_eq!(requests[2].retry, RetryPolicy::BoundedRead);
}

#[tokio::test]
async fn decimal_sale_price_preserves_the_requested_amount() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
        response(200, item_update("draft-1", "two", json!(5.25))),
        response(
            200,
            observed_draft(
                "draft-1",
                "three",
                json!({ "trade_type": "1", "price": [{ "price_amount": "5.25" }] }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "draft-1",
            &Map::from_iter([("price".to_owned(), json!(5.25))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["price"], json!(5.25));
    assert_eq!(
        transport.requests()[1].body,
        RequestBody::Json(json!({
            "data": { "price": { "price_amount": 5.25 } }
        }))
    );
}

#[tokio::test]
async fn creation_applies_fields_before_the_dedicated_sale_price() {
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        response(200, draft("two", json!({ "values": { "category": 258 } }))),
        response(
            200,
            draft(
                "three",
                json!({ "values": { "category": 258, "title": "Helmet" } }),
            ),
        ),
        response(
            200,
            draft(
                "four",
                json!({
                    "values": {
                        "category": 258,
                        "title": "Helmet",
                        "description": "Safe helmet"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "five",
                json!({
                    "values": {
                        "category": 258,
                        "title": "Helmet",
                        "description": "Safe helmet",
                        "trade_type": "1"
                    }
                }),
            ),
        ),
        response(200, item_update("draft-1", "six", json!(5))),
        response(
            200,
            observed_draft(
                "draft-1",
                "seven",
                json!({
                    "category": 258,
                    "title": "Helmet",
                    "description": "Safe helmet",
                    "trade_type": "1",
                    "price": [{ "price_amount": "5" }]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let created = workflow
        .create(
            Map::from_iter([
                ("category".to_owned(), json!(258)),
                ("title".to_owned(), json!("Helmet")),
                ("description".to_owned(), json!("Safe helmet")),
                ("trade_type".to_owned(), json!("1")),
                ("price".to_owned(), json!(5)),
            ]),
            &[] as &[&str],
        )
        .await
        .unwrap();

    assert_eq!(created.draft.values["price"], json!(5));
    assert_eq!(
        created.completed_steps,
        [
            "create_draft",
            "apply_category",
            "apply_title",
            "apply_description",
            "apply_trade_type",
            "apply_price",
            "observe_price"
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 7);
    assert!(
        requests[1..5]
            .iter()
            .all(|request| request.method == Method::Put)
    );
    let RequestBody::Json(fields) = &requests[4].body else {
        panic!("expected composer field update")
    };
    assert_eq!(fields["trade_type"], "1");
    assert!(fields.get("price").is_none());
    assert_eq!(requests[5].method, Method::Patch);
    assert_eq!(requests[5].path, "/items/draft-1");
    assert_eq!(requests[5].retry, RetryPolicy::Never);
    assert_eq!(requests[6].method, Method::Get);
}

#[tokio::test]
async fn update_applies_field_groups_in_deterministic_protocol_order() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "values": {
                        "title": "Old",
                        "trade_type": "1",
                        "price": 10,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 10,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "three",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 10,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(200, item_update("draft-1", "four", json!(25))),
        response(
            200,
            draft(
                "five",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 25,
                        "postal_code": "00000"
                    }
                }),
            ),
        ),
        response(
            200,
            draft(
                "six",
                json!({
                    "values": {
                        "title": "Chair",
                        "trade_type": "1",
                        "price": 25,
                        "postal_code": "00100"
                    }
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("postal_code".to_owned(), json!("00100")),
                ("price".to_owned(), json!(25)),
                ("trade_type".to_owned(), json!("sell")),
                ("title".to_owned(), json!("Chair")),
            ]),
        )
        .await
        .unwrap();

    assert_eq!(
        result.completed_steps,
        [
            "fetch_draft",
            "apply_title",
            "apply_trade_type",
            "apply_price",
            "observe_price",
            "apply_postal_code"
        ]
    );
    let requests = transport.requests();
    assert_eq!(
        requests
            .iter()
            .map(|request| &request.method)
            .collect::<Vec<_>>(),
        [
            &Method::Get,
            &Method::Put,
            &Method::Put,
            &Method::Patch,
            &Method::Get,
            &Method::Put,
        ]
    );
}

#[tokio::test]
async fn give_away_price_combinations_fail_before_mutating() {
    let create_transport = FixtureTransport::new([]);
    let create_workflow =
        DraftWorkflow::new(HttpAdInputApi::new(create_transport.clone()), config());
    let error = create_workflow
        .create(
            Map::from_iter([
                ("trade_type".to_owned(), json!("2")),
                ("price".to_owned(), json!(5)),
            ]),
            &[] as &[&str],
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.price_trade_type_conflict");
    assert!(create_transport.requests().is_empty());

    let update_transport = FixtureTransport::new([response(
        200,
        observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
    )]);
    let update_workflow =
        DraftWorkflow::new(HttpAdInputApi::new(update_transport.clone()), config());
    let error = update_workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("trade_type".to_owned(), json!("2")),
                ("price".to_owned(), json!(5)),
            ]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.price_trade_type_conflict");
    assert_eq!(update_transport.requests().len(), 1);
    assert_eq!(update_transport.requests()[0].method, Method::Get);
}

#[tokio::test]
async fn switching_to_give_away_omits_the_sale_price() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft(
                "draft-1",
                "one",
                json!({ "trade_type": "1", "price": [{ "price_amount": "5" }] }),
            ),
        ),
        response(
            200,
            observed_draft("draft-1", "two", json!({ "trade_type": "2" })),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "draft-1",
            &Map::from_iter([("trade_type".to_owned(), json!("2"))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["trade_type"], "2");
    assert!(state.draft.values.get("price").is_none());
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].method, Method::Put);
    let RequestBody::Json(values) = &requests[1].body else {
        panic!("expected composer update")
    };
    assert!(values.get("price").is_none());
}

#[tokio::test]
async fn composer_updates_reencode_an_observed_sale_price_without_currency_guessing() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft(
                "draft-1",
                "one",
                json!({
                    "trade_type": "1",
                    "price": [{ "price_amount": "5.25" }]
                }),
            ),
        ),
        response(
            200,
            observed_draft(
                "draft-1",
                "two",
                json!({
                    "trade_type": "1",
                    "title": "Updated",
                    "price": [{ "price_amount": "5.25" }]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .update(
            "draft-1",
            &Map::from_iter([("title".to_owned(), json!("Updated"))]),
        )
        .await
        .unwrap();

    assert_eq!(state.draft.values["price"], json!(5.25));
    let RequestBody::Json(values) = &transport.requests()[1].body else {
        panic!("expected composer update")
    };
    assert_eq!(values["price"], json!([{ "price_amount": "5.25" }]));
    assert!(values["price"][0].get("currency").is_none());
}

#[tokio::test]
async fn malformed_source_price_shapes_are_rejected() {
    let transport = FixtureTransport::new([response(
        200,
        observed_draft(
            "draft-1",
            "one",
            json!({ "trade_type": "1", "price": { "amount": 5 } }),
        ),
    )]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.show("draft-1").await.unwrap_err();

    assert_eq!(error.code, "upstream.unexpected_response");
    assert_eq!(
        error.source.unwrap().details.unwrap()["stage"],
        "normalize_price"
    );
}

#[tokio::test]
async fn price_failure_preserves_transience_and_unsafe_recovery_details() {
    let mut failed = response(502, Value::Null);
    failed.content_type = Some("text/html".to_owned());
    failed.body_is_unparseable = true;
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
        failed,
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow
        .update("draft-1", &Map::from_iter([("price".to_owned(), json!(5))]))
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    let source = error.source.unwrap();
    assert_eq!(source.status, Some(502));
    assert!(source.upstream_transient);
    assert!(!source.safe_to_retry);
    assert_eq!(source.details.as_deref().unwrap()["stage"], "apply_price");
    assert_eq!(
        source.details.as_deref().unwrap()["content_type"],
        "text/html"
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.completed_steps, ["fetch_draft"]);
    assert!(recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.active_step.as_deref(), Some("apply_price"));
    assert_eq!(recovery.absent_fields, ["price"]);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea draft show draft-1",
            "flea draft update draft-1 --price VALUE"
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests[1].method, Method::Patch);
    assert_eq!(requests[1].retry, RetryPolicy::Never);
}

#[tokio::test]
async fn price_success_requires_an_authoritative_matching_observation() {
    let transport = FixtureTransport::new([
        response(
            200,
            observed_draft("draft-1", "one", json!({ "trade_type": "1" })),
        ),
        response(200, item_update("draft-1", "two", json!(5))),
        response(
            200,
            observed_draft(
                "draft-1",
                "three",
                json!({ "trade_type": "1", "price": [{ "price_amount": "4" }] }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .update("draft-1", &Map::from_iter([("price".to_owned(), json!(5))]))
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    assert_eq!(
        error.source.as_ref().unwrap().details.as_deref().unwrap()["stage"],
        "observe_price"
    );
    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.completed_steps, ["fetch_draft", "apply_price"]);
    assert_eq!(recovery.fresh_state.unwrap().values["price"], json!(4));
}

#[tokio::test]
async fn delivery_update_uses_authoritative_state_when_item_update_is_ignored() {
    let transport = FixtureTransport::new([
        response(200, draft("same", json!({ "values": { "title": "Old" } }))),
        response(200, draft("same", json!({ "values": { "title": "Old" } }))),
        response(200, delivery_page(None)),
        response(204, Value::Null),
        response(200, delivery_page(Some("pickup"))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([
                ("title".to_owned(), json!("New")),
                ("delivery".to_owned(), json!(["pickup"])),
            ]),
        )
        .await
        .unwrap();

    assert!(!result.etag_changed);
    assert_eq!(result.requested_delivery, ["pickup"]);
    assert_eq!(result.persisted_fields, ["delivery"]);
    assert_eq!(result.ignored_fields, ["title"]);
    assert_eq!(
        result.draft.delivery.unwrap().selected,
        ["pickup".to_owned()]
    );
    let requests = transport.requests();
    let RequestBody::Json(item) = &requests[1].body else {
        panic!("expected item update")
    };
    assert!(item.get("delivery").is_none());
    assert_eq!(requests[3].method, Method::Post);
    assert_eq!(requests[3].path, "/ads/draft-1/delivery");
}

#[tokio::test]
async fn shipping_update_discovers_package_options_and_provider_products() {
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, delivery_page(None)),
        response(200, shipping_page(&["POSTIPAKETTI", "MATKAHUOLTO_SHOP"])),
        response(204, Value::Null),
        response(200, delivery_page(Some("shipping:small"))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let result = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["shipping:small"]))]),
        )
        .await
        .unwrap();

    let delivery = result.draft.delivery.unwrap();
    assert_eq!(
        delivery
            .options
            .iter()
            .map(|option| option.value.as_str())
            .collect::<Vec<_>>(),
        [
            "pickup",
            "shipping:small",
            "shipping:medium",
            "shipping:large"
        ]
    );
    assert_eq!(delivery.selected, ["shipping:small"]);
    let requests = transport.requests();
    assert!(requests[2].path.starts_with("/ui/addelivery/shipping?"));
    let RequestBody::Json(body) = &requests[3].body else {
        panic!("expected delivery body")
    };
    assert_eq!(body["shipping"], true);
    assert_eq!(body["meetup"], false);
    assert_eq!(body["shippingInfo"]["size"], "SMALL");
    assert_eq!(
        body["shippingInfo"]["products"],
        json!(["MATKAHUOLTO_SHOP", "POSTIPAKETTI"])
    );
}

#[tokio::test]
async fn invalid_and_missing_delivery_options_fail_before_mutation() {
    let invalid_transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, delivery_page(None)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(invalid_transport.clone()), config());
    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["shipping:oversize"]))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.invalid_delivery");
    assert_eq!(
        error.details.unwrap()["allowed_values"],
        json!([
            "pickup",
            "shipping:small",
            "shipping:medium",
            "shipping:large"
        ])
    );
    assert_eq!(invalid_transport.requests().len(), 2);

    let empty_transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(
            200,
            json!({
                "context": {
                    "adId": "draft-1",
                    "meetup": false,
                    "shipping": false
                },
                "sections": { "deliveryOptions": {} }
            }),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(empty_transport.clone()), config());
    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["pickup"]))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.invalid_delivery");
    assert_eq!(error.details.unwrap()["allowed_values"], json!([]));
    assert_eq!(empty_transport.requests().len(), 2);

    let malformed_transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, json!({ "context": {}, "sections": {} })),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(malformed_transport.clone()), config());
    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["pickup"]))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.source.unwrap().details.unwrap()["path"],
        "$.context.adId"
    );
    assert_eq!(malformed_transport.requests().len(), 2);
}

#[tokio::test]
async fn dedicated_delivery_failure_requires_authoritative_recovery() {
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, delivery_page(None)),
        response(503, json!({ "message": "delivery unavailable" })),
        response(200, delivery_page(None)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["pickup"]))]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "mutation.uncertain");
    let recovery = error.recovery.unwrap();
    assert_eq!(
        recovery.completed_steps,
        ["fetch_draft", "fetch_delivery_options"]
    );
    assert!(recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.active_step.as_deref(), Some("apply_delivery"));
    assert_eq!(recovery.failed_stage.as_deref(), Some("apply_delivery"));
    assert_eq!(recovery.observation.status, ObservationStatus::Observed);
    assert_eq!(recovery.delivery, Some(RecoveryStatus::Absent));
    assert_eq!(recovery.absent_fields, ["delivery"]);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea draft show draft-1",
            "flea draft update draft-1 --delivery VALUE"
        ]
    );
}

#[tokio::test]
async fn discovered_numeric_category_is_sent_as_the_composer_machine_value() {
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        response(200, draft("two", json!({ "values": { "category": 258 } }))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    workflow
        .create(
            Map::from_iter([("category".to_owned(), json!("258"))]),
            &[] as &[&str],
        )
        .await
        .unwrap();

    let requests = transport.requests();
    assert_eq!(
        requests[1].body,
        RequestBody::Json(json!({ "category": 258 }))
    );
}

#[tokio::test]
async fn post_creation_failure_keeps_recovery_context() {
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        response(503, json!({ "message": "category service unavailable" })),
        response(200, draft("one", json!({}))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .create(
            Map::from_iter([("category".to_owned(), json!("furniture/chairs"))]),
            &[] as &[&str],
        )
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(recovery.completed_steps, ["create_draft"]);
    assert!(recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.active_step.as_deref(), Some("apply_category"));
    assert_eq!(recovery.absent_fields, ["category"]);
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea draft show draft-1",
            "flea draft update draft-1 --category VALUE"
        ]
    );
}

#[tokio::test]
async fn recovery_output_bounds_dynamic_fields_images_steps_and_local_paths() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("private-recovery-image.png");
    image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
    let paths = vec![path; 25];
    let mut values = Map::from_iter([("category".to_owned(), json!("chairs"))]);
    for index in 0..30 {
        values.insert(
            format!("dynamic_field_{index:02}"),
            json!(format!("private-value-{index}")),
        );
    }
    let transport = FixtureTransport::new([
        response(201, draft("one", json!({}))),
        response(503, json!({ "message": "category unavailable" })),
        response(200, draft("observed", json!({}))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.create(values, &paths).await.unwrap_err();

    let debug = format!("{error:?}");
    let recovery = error.recovery.unwrap();
    assert!(recovery.field_summary.len() <= 24);
    assert!(recovery.fields_omitted > 0);
    assert_eq!(recovery.images.len(), 20);
    assert_eq!(recovery.images_omitted, 5);
    assert!(recovery.completed_steps.len() <= 24);
    let rendered = serde_json::to_string(&recovery).unwrap();
    assert!(!rendered.contains("private-value"));
    assert!(!rendered.contains("private-recovery-image"));
    assert!(!debug.contains("private-value"));
    assert!(!debug.contains("private-recovery-image"));
}

#[tokio::test]
async fn copy_failure_identifies_both_source_listing_and_created_draft() {
    let transport = FixtureTransport::new([
        response(
            200,
            json!({ "listing_id": "listing-7", "values": { "title": "Chair" }, "images": [] }),
        ),
        response(201, draft("one", json!({}))),
        response(503, json!({ "message": "copy failed" })),
        response(200, draft("one", json!({}))),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.create_from_listing("listing-7").await.unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.draft_id, "draft-1");
    assert_eq!(recovery.source_listing_id.as_deref(), Some("listing-7"));
    assert_eq!(recovery.listing_id, None);
    assert_eq!(
        recovery.completed_steps,
        ["load_source_listing", "create_draft"]
    );
}

#[tokio::test]
async fn show_fetches_predictions_only_for_uncategorized_draft_with_images() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [{
                        "image_id": "image-1",
                        "position": 0,
                        "state": "ready"
                    }]
                }),
            ),
        ),
        response(
            200,
            json!([{ "category": "furniture/chairs", "confidence": 0.92 }]),
        ),
        response(200, delivery_page(None)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.show("draft-1").await.unwrap();

    assert_eq!(state.predictions[0].category, "furniture/chairs");
    assert_eq!(state.delivery.unwrap().options[0].value, "pickup");
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| request.method == Method::Get));
    assert!(
        requests
            .iter()
            .all(|request| request.retry == RetryPolicy::BoundedRead)
    );
}

#[tokio::test]
async fn image_upload_reads_dimensions_and_preserves_order() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("second.png");
    image::DynamicImage::new_rgb8(7, 11).save(&path).unwrap();
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap()
        .write_all(b"private-gps-metadata")
        .unwrap();
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [image("first", 0, "ready", 4, 6)]
                }),
            ),
        ),
        response_with_location(201, "https://img.tori.net/dynamic/default/second.png"),
        response(
            200,
            draft(
                "two",
                json!({
                    "images": [
                        image("first", 0, "ready", 4, 6),
                        image("second", 1, "processing", 7, 11)
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.add_images("draft-1", &[path]).await.unwrap();

    assert_eq!(state.images[1].state, ImageState::Processing);
    assert_eq!(state.image_processing.len(), 1);
    assert_eq!(state.image_processing[0].source_format, "png");
    assert_eq!(state.image_processing[0].uploaded_format, "png");
    assert_eq!(state.image_processing[0].final_width, 7);
    assert_eq!(state.image_processing[0].final_height, 11);
    assert!(state.image_processing[0].metadata_stripped);
    assert!(state.image_processing[0].recompressed);
    let requests = transport.requests();
    let RequestBody::Image {
        bytes,
        file_name,
        mime_type,
        width,
        height,
    } = &requests[1].body
    else {
        panic!("expected image upload")
    };
    assert_eq!((*width, *height), (7, 11));
    assert_eq!(file_name, "image.png");
    assert_eq!(mime_type, "image/png");
    assert!(
        !bytes
            .windows(b"private-gps-metadata".len())
            .any(|window| window == b"private-gps-metadata")
    );
    assert_eq!(requests[1].retry, RetryPolicy::Never);
    assert_eq!(requests[1].path, "/adinput/ad/recommerce/draft-1/upload");
    let RequestBody::Json(values) = &requests[2].body else {
        panic!("expected image field update")
    };
    assert_eq!(values["multi_image"].as_array().unwrap().len(), 2);
    assert_eq!(values["multi_image"][1]["width"], 7);
    assert_eq!(values["multi_image"][1]["height"], 11);
}

#[tokio::test]
async fn image_failure_recovery_uses_absolute_positions_and_lifecycle_states() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("private-{index}.png"));
            image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [
                        image("existing-a", 0, "ready", 4, 6),
                        image("existing-b", 1, "ready", 4, 6)
                    ]
                }),
            ),
        ),
        response_with_location(201, "https://img.tori.net/dynamic/default/new-a.jpg"),
        response(422, json!({ "message": "image rejected" })),
        response(
            200,
            draft(
                "observed",
                json!({
                    "images": [
                        image("existing-a", 0, "ready", 4, 6),
                        image("existing-b", 1, "ready", 4, 6)
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.add_images("draft-1", &paths).await.unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.failed_stage.as_deref(), Some("upload_image:3"));
    assert_eq!(recovery.completed_steps, ["fetch_draft", "upload_image:2"]);
    assert_eq!(recovery.observation.status, ObservationStatus::Observed);
    assert_eq!(recovery.observed_etag.as_deref(), Some("observed"));
    let image = |index| {
        recovery
            .images
            .iter()
            .find(|image| image.index == index)
            .unwrap()
    };
    assert_eq!(image(2).status, RecoveryStatus::Absent);
    assert_eq!(image(2).upload, UploadRecoveryStatus::Completed);
    assert_eq!(image(2).attachment, AttachmentRecoveryStatus::Absent);
    assert_eq!(image(3).status, RecoveryStatus::Rejected);
    assert_eq!(image(3).upload, UploadRecoveryStatus::Failed);
    assert_eq!(image(4).status, RecoveryStatus::Unattempted);
    assert_eq!(image(4).upload, UploadRecoveryStatus::Unattempted);
    assert!(
        recovery
            .next_safe_actions
            .iter()
            .all(|action| !action.contains("image add"))
    );
    assert_eq!(recovery.destructive_actions, ["flea draft delete draft-1"]);
}

#[tokio::test]
async fn attachment_recovery_distinguishes_processing_ready_and_failed_images() {
    let directory = tempfile::tempdir().unwrap();
    let paths = (0..3)
        .map(|index| {
            let path = directory.path().join(format!("private-{index}.png"));
            image::DynamicImage::new_rgb8(4, 6).save(&path).unwrap();
            path
        })
        .collect::<Vec<_>>();
    let mut uncertain = response(200, Value::Null);
    uncertain.body_is_unparseable = true;
    let mut failed = image("third", 2, "failed", 4, 6);
    failed["failure"] = json!("processing rejected");
    let transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response_with_location(201, "https://img.tori.net/dynamic/default/first.jpg"),
        response_with_location(201, "https://img.tori.net/dynamic/default/second.jpg"),
        response_with_location(201, "https://img.tori.net/dynamic/default/third.jpg"),
        uncertain,
        response(
            200,
            draft(
                "observed",
                json!({
                    "images": [
                        image("first", 0, "ready", 4, 6),
                        image("second", 1, "processing", 4, 6),
                        failed
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.add_images("draft-1", &paths).await.unwrap_err();

    let recovery = error.recovery.unwrap();
    assert_eq!(recovery.failed_stage.as_deref(), Some("attach_images"));
    let image = |index| {
        recovery
            .images
            .iter()
            .find(|image| image.index == index)
            .unwrap()
    };
    assert_eq!(image(0).status, RecoveryStatus::Persisted);
    assert_eq!(image(0).processing, ProcessingRecoveryStatus::Ready);
    assert_eq!(image(1).status, RecoveryStatus::Pending);
    assert_eq!(image(1).processing, ProcessingRecoveryStatus::Processing);
    assert_eq!(image(2).status, RecoveryStatus::Rejected);
    assert_eq!(image(2).processing, ProcessingRecoveryStatus::Failed);
}

#[tokio::test]
async fn uncertain_image_removal_reports_only_retained_images_as_safe_work() {
    let first = "https://img.tori.net/dynamic/default/first.jpg".to_owned();
    let second = "https://img.tori.net/dynamic/default/second.jpg".to_owned();
    let observed = json!({
        "images": [
            image("first", 0, "ready", 4, 6),
            image("second", 1, "ready", 4, 6)
        ]
    });
    let mut uncertain = response(200, Value::Null);
    uncertain.body_is_unparseable = true;
    let transport = FixtureTransport::new([
        response(200, draft("one", observed.clone())),
        uncertain,
        response(200, draft("observed", observed)),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow
        .remove_images("draft-1", &[first, second])
        .await
        .unwrap_err();

    let recovery = error.recovery.unwrap();
    assert!(recovery.images.iter().all(|image| {
        image.operation == ImageRecoveryOperation::Remove
            && image.status == RecoveryStatus::Absent
            && image.attachment == AttachmentRecoveryStatus::Attached
    }));
    assert_eq!(
        recovery.next_safe_actions,
        [
            "flea draft show draft-1",
            "flea draft image remove draft-1 IMAGE_ID..."
        ]
    );
}

#[tokio::test]
async fn remove_and_delete_use_ordered_non_retried_mutations() {
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [
                        image("third", 2, "ready", 3, 3),
                        image("first", 0, "ready", 1, 1),
                        image("second", 1, "ready", 2, 2)
                    ]
                }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({
                    "images": [
                        image("first", 0, "ready", 1, 1),
                        image("third", 1, "ready", 3, 3)
                    ]
                }),
            ),
        ),
        response(204, Value::Null),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .remove_images(
            "draft-1",
            &["https://img.tori.net/dynamic/default/second.jpg".to_owned()],
        )
        .await
        .unwrap();
    workflow.delete("draft-1").await.unwrap();

    assert!(state.images[1].image_id.ends_with("third.jpg"));
    let requests = transport.requests();
    let RequestBody::Json(values) = &requests[1].body else {
        panic!("expected image field update")
    };
    assert_eq!(values["multi_image"].as_array().unwrap().len(), 2);
    assert!(
        values["multi_image"][1]["url"]
            .as_str()
            .unwrap()
            .ends_with("third.jpg")
    );
    assert_eq!(requests[1].retry, RetryPolicy::Never);
    assert_eq!(requests[2].method, Method::Delete);
    assert_eq!(requests[2].retry, RetryPolicy::Never);
}

#[tokio::test]
async fn validate_reports_complete_drafts_and_uses_only_read_requests() {
    let state = draft(
        "one",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let transport = FixtureTransport::new([
        response(200, state),
        response(200, delivery_page(Some("pickup"))),
        category_taxonomy("furniture/chairs", true),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let report = workflow.validate("draft-1").await.unwrap();

    assert!(report.ready);
    assert_eq!(report.revision, "one");
    assert!(report.missing.is_empty());
    assert!(report.invalid.is_empty());
    assert!(report.pending.is_empty());
    assert!(report.unverifiable.is_empty());
    let requests = transport.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| {
        request.method == Method::Get && request.retry == RetryPolicy::BoundedRead
    }));
}

#[tokio::test]
async fn show_and_validate_expose_the_same_authoritative_revision() {
    let state = draft(
        "revision-42",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let shown = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state.clone()),
            response(200, delivery_page(Some("pickup"))),
        ])),
        config(),
    )
    .show("draft-1")
    .await
    .unwrap();
    let validated = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();

    assert_eq!(shown.revision.as_deref(), Some("revision-42"));
    assert_eq!(validated.revision, "revision-42");
}

#[tokio::test]
async fn validate_applies_category_specific_composer_requirements() {
    let mut composer = composer_fixture();
    composer["ad"]["values"]
        .as_object_mut()
        .unwrap()
        .remove("condition");
    composer["ad"]["values"]["multi_image"] = json!([{
        "url": "https://img.tori.net/dynamic/default/image-1.jpg",
        "width": 640,
        "height": 480,
        "type": "image/jpeg"
    }]);
    composer["model"]["sections"][2]["content"][1]["required"] = json!(true);
    composer["model"]["sections"][2]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "hidden_serial",
            "type": "simple",
            "required": true,
            "hidden": true
        }));
    let mut delivery = delivery_fixture();
    delivery["context"]["meetup"] = json!(true);
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, composer),
            response(200, delivery),
            category_taxonomy("258", true),
        ])),
        config(),
    );

    let report = workflow.validate("46000000").await.unwrap();

    assert!(!report.ready);
    assert_eq!(report.missing.len(), 1);
    assert_eq!(report.missing[0].field, "condition");
    assert_eq!(report.missing[0].source, "listing_composer");
}

#[tokio::test]
async fn validate_distinguishes_giveaway_sale_delivery_and_image_states() {
    let mut giveaway_values = publication_values();
    giveaway_values["trade_type"] = json!("give_away");
    giveaway_values.as_object_mut().unwrap().remove("price");
    let giveaway = draft(
        "one",
        json!({
            "values": giveaway_values,
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, giveaway),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert!(report.ready);

    let mut sale_values = publication_values();
    sale_values.as_object_mut().unwrap().remove("price");
    let sale = draft(
        "one",
        json!({
            "values": sale_values,
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, sale),
            response(200, delivery_page(None)),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert!(report.missing.iter().any(|issue| issue.field == "price"));
    assert!(report.missing.iter().any(|issue| issue.field == "delivery"));
    assert!(report.pending.iter().any(|issue| issue.field == "images"));
    for issue in report.missing.iter().chain(&report.pending) {
        assert!(!issue.reason.is_empty());
        assert!(!issue.source.is_empty());
        assert!(issue.command.starts_with("flea "));
    }
}

#[tokio::test]
async fn validate_separates_missing_pending_and_rejected_images() {
    for (images, status) in [
        (json!([]), "missing"),
        (
            json!([{ "image_id": "image-1", "position": 0, "state": "processing" }]),
            "pending",
        ),
        (
            json!([{ "image_id": "image-1", "position": 0, "state": "failed" }]),
            "invalid",
        ),
    ] {
        let state = draft(
            "one",
            json!({ "values": publication_values(), "images": images }),
        );
        let report = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([
                response(200, state),
                response(200, delivery_page(Some("pickup"))),
                category_taxonomy("furniture/chairs", true),
            ])),
            config(),
        )
        .validate("draft-1")
        .await
        .unwrap();
        let value = serde_json::to_value(report).unwrap();
        assert_eq!(value[status][0]["field"], "images");
    }
}

#[tokio::test]
async fn validate_reports_unselectable_and_unavailable_authoritative_models() {
    let state = draft(
        "one",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state.clone()),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", false),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert_eq!(report.invalid[0].field, "category");
    assert_eq!(report.invalid[0].source, "category_taxonomy");

    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, state),
            response(503, json!({ "message": "delivery unavailable" })),
            response(503, json!({ "message": "taxonomy unavailable" })),
        ])),
        config(),
    )
    .validate("draft-1")
    .await
    .unwrap();
    assert!(
        report
            .unverifiable
            .iter()
            .any(|issue| issue.field == "category")
    );
    assert!(
        report
            .unverifiable
            .iter()
            .any(|issue| issue.field == "delivery")
    );
}

#[tokio::test]
async fn validate_marks_unavailable_and_malformed_models_unverifiable() {
    for model in [Value::Null, json!({ "sections": "invalid" })] {
        let mut values = publication_values();
        values["multi_image"] = json!([{
            "url": "https://img.tori.net/dynamic/default/image-1.jpg",
            "width": 640,
            "height": 480,
            "type": "image/jpeg"
        }]);
        let source = json!({
            "ad": {
                "id": "draft-1",
                "etag": "one",
                "values": values,
                "meta-data": {},
                "locked-fields": []
            },
            "model": model
        });
        let report = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([
                response(200, source),
                response(200, delivery_page(Some("pickup"))),
                category_taxonomy("furniture/chairs", true),
            ])),
            config(),
        )
        .validate("draft-1")
        .await
        .unwrap();
        assert!(!report.ready);
        assert_eq!(report.unverifiable[0].field, "composer_model");
        assert_eq!(report.unverifiable[0].source, "listing_composer");
    }
}

#[tokio::test]
async fn publish_runs_the_complete_bounded_sequence() {
    let transport = FixtureTransport::new(successful_publish_responses());
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let published = workflow.publish("draft-1", "one").await.unwrap();

    assert_eq!(published.listing_id, "draft-1");
    assert_eq!(published.state, "pending");
    assert_eq!(
        published.completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy",
            "validate",
            "wait_for_images",
            "verify_revision",
            "patch_item_fields",
            "fetch_fresh_model",
            "update_recommerce",
            "apply_delivery",
            "observe_delivery",
            "fetch_product_context",
            "package_choice",
            "fetch_confirmation",
            "track_confirmation",
            "fetch_observed_listing"
        ]
    );
    assert!(published.warnings.is_empty());

    let requests = transport.requests();
    assert_eq!(requests.len(), 15);
    let observed_sequence = requests
        .iter()
        .map(|request| (request.method.clone(), request.path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed_sequence,
        [
            (Method::Get, "/search?limit=50&offset=0"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Get, "/ui/addelivery?adId=draft-1&editMode=false"),
            (Method::Get, "/categories/taxonomy"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Patch, "/items/draft-1"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Put, "/adinput/ad/recommerce/draft-1/update"),
            (Method::Post, "/ads/draft-1/delivery"),
            (Method::Get, "/ui/addelivery?adId=draft-1&editMode=false"),
            (
                Method::Get,
                "/adinput/product/recommerce/draft-1/productcontext?adRevision=revision-7"
            ),
            (Method::Post, "/adinput/order/choices/draft-1"),
            (Method::Get, "/orders/4/confirmation/draft-1"),
            (
                Method::Get,
                "/tracking/adconfirmation?adId=draft-1&orderId=4"
            ),
            (Method::Get, "/draft-1"),
        ]
    );
    assert_eq!(
        requests[8].body,
        RequestBody::Json(json!({
            "meetup": true,
            "shipping": false,
            "sellerPaysShipping": false,
            "client": "ANDROID",
            "buyNow": false
        }))
    );
    assert_eq!(requests[5].if_match.as_deref(), Some("one"));
    assert_eq!(
        requests[5].body,
        RequestBody::Json(json!({
            "data": {
                "title": "Chair",
                "description": "Solid birch chair",
                "price": { "price_amount": 45 }
            }
        }))
    );
    assert_eq!(requests[7].if_match.as_deref(), Some("two"));
    let RequestBody::Json(recommerce_values) = &requests[7].body else {
        panic!("expected recommerce update")
    };
    assert_eq!(
        recommerce_values["price"],
        json!([{ "price_amount": "45" }])
    );
    assert_eq!(
        requests[11].body,
        RequestBody::Form(vec![(
            "choices".to_owned(),
            "urn:product:package-specification:10".to_owned()
        )])
    );
    assert!(requests.iter().all(|request| {
        !matches!(
            request.method,
            Method::Post | Method::Patch | Method::Put | Method::Delete
        ) || request.retry == RetryPolicy::Never
    }));
}

#[tokio::test]
async fn publish_polls_detail_then_uses_the_active_collection_fallback() {
    let mut responses = successful_publish_responses();
    responses[13] = response(404, json!({ "message": "detail pending" }));
    responses.push(response(404, json!({ "message": "detail pending" })));
    let transport = FixtureTransport::new(responses).with_search_responses([
        response(200, json!({ "summaries": [], "total": 0 })),
        response(200, json!({ "summaries": [], "total": 0 })),
        listing_collection("draft-1", "ACTIVE"),
    ]);
    let mut workflow_config = config();
    workflow_config.listing_poll_limit = 1;
    let result = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), workflow_config)
        .publish("draft-1", "one")
        .await
        .unwrap();

    assert_eq!(result.publication, "persisted");
    assert!(result.mutations_performed);
    assert_eq!(result.observed_listing["observation_source"], "collection");
    assert_eq!(
        result.public_url,
        "https://www.tori.fi/recommerce/forsale/item/draft-1"
    );
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.path == "/draft-1")
            .count(),
        2
    );
}

#[tokio::test]
async fn publish_uses_collection_when_detail_response_model_is_unexpected() {
    let mut responses = successful_publish_responses();
    responses[13] = response(200, json!({ "unexpected": true }));
    let transport = FixtureTransport::new(responses).with_search_responses([
        response(200, json!({ "summaries": [], "total": 0 })),
        listing_collection("draft-1", "ACTIVE"),
    ]);
    let result = DraftWorkflow::new(HttpAdInputApi::new(transport), config())
        .publish("draft-1", "one")
        .await
        .unwrap();

    assert_eq!(result.publication, "persisted");
    assert_eq!(result.observed_listing["observation_source"], "collection");
}

#[tokio::test]
async fn publish_returns_idempotent_success_for_an_already_active_listing() {
    let transport =
        FixtureTransport::new([]).with_search_responses([listing_collection("46031010", "ACTIVE")]);
    let result = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config())
        .publish("46031010", "stale")
        .await
        .unwrap();

    assert_eq!(result.publication, "already_published");
    assert!(!result.mutations_performed);
    assert_eq!(result.state, "active");
    assert_eq!(result.completed_steps, ["check_active_listing"]);
    assert_eq!(
        result.public_url,
        "https://www.tori.fi/recommerce/forsale/item/46031010"
    );
    assert_eq!(transport.requests().len(), 1);
    assert_eq!(transport.requests()[0].method, Method::Get);
}

#[tokio::test]
async fn stale_publish_revision_is_a_read_only_conflict() {
    let mut responses = successful_publish_responses();
    responses.truncate(4);
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "stale").await.unwrap_err();

    assert_eq!(error.code, "draft.revision_conflict");
    assert_eq!(
        error.details.as_ref().unwrap()["expected_revision"],
        "stale"
    );
    assert_eq!(error.details.as_ref().unwrap()["observed_revision"], "one");
    assert_eq!(error.details.as_ref().unwrap()["safe_to_retry"], false);
    assert_eq!(
        error.details.as_ref().unwrap()["next_action"],
        "flea draft show draft-1"
    );
    let recovery = error.recovery.unwrap();
    assert!(!recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.failed_stage.as_deref(), Some("verify_revision"));
    assert_eq!(
        recovery.next_safe_actions,
        ["flea draft show draft-1", "flea draft validate draft-1"]
    );
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}

#[tokio::test]
async fn publish_detects_a_draft_changed_during_validation() {
    let mut responses = successful_publish_responses();
    responses[3].body["etag"] = json!("two");
    responses.truncate(4);
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.revision_conflict");
    assert_eq!(error.details.as_ref().unwrap()["expected_revision"], "one");
    assert_eq!(error.details.as_ref().unwrap()["observed_revision"], "two");
    assert_eq!(transport.requests().len(), 5);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}

#[tokio::test]
async fn publish_requires_an_authoritative_upstream_revision() {
    let state = draft(
        "",
        json!({
            "values": publication_values(),
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let transport = FixtureTransport::new([response(200, state)]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.source.unwrap().details.unwrap()["reason"],
        "draft revision is unavailable"
    );
    assert_eq!(transport.requests().len(), 2);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}

#[tokio::test]
async fn publication_model_failures_before_dispatch_are_not_mutation_uncertainty() {
    let transport =
        FixtureTransport::new([response(200, json!({ "model": { "sections": [{}] } }))]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_ne!(error.code, "mutation.uncertain");
    assert_eq!(error.source.unwrap().details.unwrap()["path"], "$");
    assert_eq!(transport.requests().len(), 2);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}

#[tokio::test]
async fn fresh_model_requires_ad_after_the_item_patch_without_claiming_a_new_mutation() {
    let mut responses = successful_publish_responses();
    responses[5] = response(200, json!({ "model": { "sections": [{}] } }));
    responses.truncate(7);
    responses.push(response(
        503,
        json!({ "message": "observation unavailable" }),
    ));
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    let recovery = error.recovery.unwrap();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(recovery.failed_stage.as_deref(), Some("fetch_fresh_model"));
    assert_eq!(recovery.item_patch, Some(RecoveryStatus::Persisted));
    assert_eq!(
        recovery.recommerce_update,
        Some(RecoveryStatus::Unattempted)
    );
    let mutations = transport
        .requests()
        .into_iter()
        .filter(|request| request.method != Method::Get)
        .collect::<Vec<_>>();
    assert_eq!(mutations.len(), 1);
    assert_eq!(mutations[0].path, "/items/draft-1");
}

#[tokio::test]
async fn recommerce_update_accepts_wrapped_and_flattened_success_shapes() {
    for wrapped in [false, true] {
        let mut responses = successful_publish_responses();
        if wrapped {
            let flat = responses[6].body.clone();
            responses[6].body = json!({ "ad": flat, "model": { "sections": [] } });
        }
        let published = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        )
        .publish("draft-1", "one")
        .await
        .unwrap();
        assert_eq!(published.revision, "revision-7");
    }
}

#[tokio::test]
async fn package_choice_parse_uncertainty_reports_distinct_persistence_stages() {
    let mut responses = successful_publish_responses();
    responses[10] = response(200, json!({ "is-completed": true }));
    responses.truncate(11);
    responses.extend([
        response(503, json!({ "message": "draft observation unavailable" })),
        response(404, json!({ "message": "listing unavailable" })),
    ]);
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    let recovery = error.recovery.unwrap();

    assert_eq!(error.code, "mutation.uncertain");
    assert_eq!(error.source.unwrap().details.unwrap()["path"], "$.order-id");
    assert_eq!(recovery.item_patch, Some(RecoveryStatus::Persisted));
    assert_eq!(recovery.recommerce_update, Some(RecoveryStatus::Persisted));
    assert_eq!(recovery.delivery, Some(RecoveryStatus::Persisted));
    assert_eq!(recovery.package_choice, Some(RecoveryStatus::Indeterminate));
    assert_eq!(recovery.confirmation, Some(RecoveryStatus::Unattempted));
    assert_eq!(
        transport
            .requests()
            .iter()
            .filter(|request| request.path == "/adinput/order/choices/draft-1")
            .count(),
        1
    );
}

#[tokio::test]
async fn rejected_publication_mutations_preserve_stage_boundaries() {
    for (index, stage, item, update, delivery) in [
        (
            4,
            "patch_item_fields",
            RecoveryStatus::Rejected,
            RecoveryStatus::Unattempted,
            RecoveryStatus::Pending,
        ),
        (
            6,
            "update_recommerce",
            RecoveryStatus::Persisted,
            RecoveryStatus::Rejected,
            RecoveryStatus::Pending,
        ),
        (
            7,
            "apply_delivery",
            RecoveryStatus::Persisted,
            RecoveryStatus::Persisted,
            RecoveryStatus::Rejected,
        ),
    ] {
        let mut responses = successful_publish_responses();
        responses[index] = response(400, json!({ "message": "rejected" }));
        let error = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        )
        .publish("draft-1", "one")
        .await
        .unwrap_err();
        let recovery = error.recovery.unwrap();
        assert_eq!(recovery.failed_stage.as_deref(), Some(stage));
        assert_eq!(recovery.item_patch, Some(item));
        assert_eq!(recovery.recommerce_update, Some(update));
        assert_eq!(recovery.delivery, Some(delivery));
        assert_eq!(recovery.package_choice, Some(RecoveryStatus::Unattempted));
    }
}

#[tokio::test]
async fn publish_waits_for_processing_images_and_reuses_the_validation_engine() {
    let mut responses = successful_publish_responses();
    responses[0].body["images"][0]["state"] = json!("processing");
    let mut ready = responses[0].body.clone();
    ready["images"][0]["state"] = json!("ready");
    responses.insert(3, response(200, ready));
    let transport = FixtureTransport::new(responses);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let published = workflow.publish("draft-1", "one").await.unwrap();

    assert_eq!(published.listing_id, "draft-1");
    assert_eq!(transport.requests()[3].method, Method::Get);
}

#[tokio::test]
async fn publish_validation_rejects_missing_fields_and_missing_delivery() {
    let mut missing_title_values = publication_values();
    missing_title_values
        .as_object_mut()
        .unwrap()
        .remove("title");
    let missing_title = draft(
        "one",
        json!({
            "values": missing_title_values,
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, missing_title),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );
    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["missing"][0]["field"], "title");
    let recovery = error.recovery.unwrap();
    assert!(!recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.failed_stage.as_deref(), Some("validate"));
    assert_eq!(recovery.publication, Some(RecoveryStatus::Unattempted));
    assert_eq!(
        recovery.completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy"
        ]
    );
    assert_eq!(
        recovery.next_safe_actions[0],
        "flea draft update draft-1 --title VALUE"
    );

    let no_delivery = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, no_delivery),
            response(200, delivery_page(None)),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );
    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["missing"][0]["field"], "delivery");
    assert_eq!(
        error.recovery.unwrap().next_safe_actions[0],
        "flea draft update draft-1 --delivery VALUE"
    );
}

#[tokio::test]
async fn truncated_composer_options_are_local_unverifiable_validation() {
    let truncated = draft(
        "one",
        json!({
            "values": {
                "category": "furniture/chairs",
                "title": "Chair",
                "description": "Solid birch chair",
                "trade_type": "sell",
                "price": 45,
                "postal_code": "00100",
                "condition": 49
            },
            "fields": [{
                "key": "condition",
                "label": "Condition",
                "type": "select",
                "requirement": "required",
                "status": "set",
                "value": 49,
                "section": "details",
                "option_count": 60,
                "options_returned": 1,
                "options_truncated": true
            }],
            "options": [{ "field": "condition", "value": 0, "label": "New" }],
            "required_fields": ["title", "delivery", "condition"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, truncated),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );

    let error = workflow.publish("draft-1", "one").await.unwrap_err();
    let details = error.details.unwrap();
    let recovery = error.recovery.unwrap();

    assert_eq!(details["unverifiable"][0]["field"], "condition");
    assert!(details.get("evidence_failures").is_none());
    assert!(!recovery.upstream_transient);
    assert!(!recovery.safe_to_retry);
    assert_eq!(recovery.failed_stage.as_deref(), Some("validate"));
    assert_eq!(recovery.publication, Some(RecoveryStatus::Unattempted));
    assert_eq!(recovery.next_safe_actions[0], "flea draft show draft-1");
}

#[tokio::test]
async fn publish_marks_transient_validation_evidence_reads_as_unverifiable() {
    let valid = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );

    for (responses, failed_stage, action, completed_steps) in [
        (
            vec![
                response(200, valid.clone()),
                response(503, json!({ "message": "delivery unavailable" })),
                category_taxonomy("furniture/chairs", true),
            ],
            "fetch_delivery_options",
            "flea draft show draft-1",
            vec!["fetch_draft", "fetch_category_taxonomy"],
        ),
        (
            vec![
                response(200, valid.clone()),
                response(200, delivery_page(Some("pickup"))),
                response(503, json!({ "message": "taxonomy unavailable" })),
            ],
            "fetch_category_taxonomy",
            "flea category list",
            vec!["fetch_draft", "fetch_delivery_options"],
        ),
    ] {
        let workflow = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        );

        let error = workflow.publish("draft-1", "one").await.unwrap_err();
        let details = error.details.unwrap();
        let recovery = error.recovery.unwrap();

        assert_eq!(error.code, "draft.validation_failed");
        assert_eq!(
            details["evidence_failures"][0]["failed_stage"],
            failed_stage
        );
        assert_eq!(
            details["unverifiable"][0]["field"],
            details["evidence_failures"][0]["field"]
        );
        assert!(recovery.upstream_transient);
        assert!(recovery.safe_to_retry);
        assert_eq!(recovery.failed_stage.as_deref(), Some(failed_stage));
        assert_eq!(recovery.publication, Some(RecoveryStatus::Unattempted));
        assert_eq!(recovery.completed_steps, completed_steps);
        assert_eq!(recovery.next_safe_actions[0], action);
    }
}

#[tokio::test]
async fn corrected_validation_state_can_publish_successfully() {
    let mut missing_title_values = publication_values();
    missing_title_values
        .as_object_mut()
        .unwrap()
        .remove("title");
    let invalid = draft(
        "one",
        json!({
            "values": missing_title_values,
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let invalid_workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, invalid),
            response(200, delivery_page(Some("pickup"))),
            category_taxonomy("furniture/chairs", true),
        ])),
        config(),
    );

    let error = invalid_workflow
        .publish("draft-1", "one")
        .await
        .unwrap_err();
    assert!(!error.recovery.unwrap().safe_to_retry);

    let corrected_workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(successful_publish_responses())),
        config(),
    );
    let published = corrected_workflow.publish("draft-1", "one").await.unwrap();
    assert_eq!(published.listing_id, "draft-1");
}

#[tokio::test]
async fn publish_reports_failed_image_and_preserves_its_identity() {
    let failed = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{
                "image_id": "image-broken",
                "position": 0,
                "state": "failed",
                "failure": "unsupported image"
            }]
        }),
    );
    let transport = FixtureTransport::new([
        response(200, failed),
        response(200, delivery_page(Some("pickup"))),
        category_taxonomy("furniture/chairs", true),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["invalid"][0]["field"], "images");
    assert_eq!(
        error.recovery.unwrap().completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy"
        ]
    );
}

#[tokio::test]
async fn publish_failures_report_each_completed_workflow_boundary() {
    let cases = [
        (0, &[][..], true),
        (
            3,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
            ][..],
            true,
        ),
        (
            4,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
            ][..],
            false,
        ),
        (
            5,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
            ][..],
            false,
        ),
        (
            6,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
            ][..],
            false,
        ),
        (
            7,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
            ][..],
            false,
        ),
        (
            8,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
                "apply_delivery",
            ][..],
            false,
        ),
        (
            9,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
                "apply_delivery",
                "observe_delivery",
            ][..],
            false,
        ),
        (
            13,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "fetch_category_taxonomy",
                "validate",
                "wait_for_images",
                "verify_revision",
                "patch_item_fields",
                "fetch_fresh_model",
                "update_recommerce",
                "apply_delivery",
                "observe_delivery",
                "fetch_product_context",
                "package_choice",
                "fetch_confirmation",
                "track_confirmation",
            ][..],
            false,
        ),
    ];

    for (failure_index, expected_steps, safe_to_retry) in cases {
        let mut responses = successful_publish_responses();
        responses[failure_index] = response(503, json!({ "message": "fixture failure" }));
        let workflow = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        );

        let error = workflow.publish("draft-1", "one").await.unwrap_err();
        let recovery = error.recovery.unwrap();
        assert_eq!(
            recovery.completed_steps, expected_steps,
            "failure index {failure_index}"
        );
        assert!(recovery.upstream_transient, "failure index {failure_index}");
        assert_eq!(
            recovery.safe_to_retry, safe_to_retry,
            "failure index {failure_index}"
        );
        if failure_index == 13 {
            assert_eq!(error.code, "publication.observation_uncertain");
            let details = error.details.unwrap();
            assert_eq!(
                details["listing_id"], "draft-1",
                "published listing identity must survive observation failure"
            );
            assert_eq!(details["publication"], "persisted");
            assert_eq!(details["observation_status"], "unavailable");
            assert_eq!(details["observation_attempts"], 1);
            assert!(details["observation_elapsed_ms"].is_number());
            assert_eq!(recovery.publication, Some(RecoveryStatus::Persisted));
            assert_eq!(recovery.next_safe_actions, ["flea listing show draft-1"]);
        } else {
            assert_eq!(recovery.next_safe_actions, ["flea draft show draft-1"]);
        }
    }
}

#[tokio::test]
async fn uncertain_publication_observes_before_recommending_any_continuation() {
    for observation_available in [true, false] {
        let mut responses = successful_publish_responses();
        responses.truncate(11);
        responses[10] = response(503, json!({ "message": "publication unavailable" }));
        responses.push(if observation_available {
            response(
                200,
                draft(
                    "observed-publication",
                    json!({
                        "values": {
                            "category": "furniture/chairs",
                            "title": "Chair",
                            "delivery": ["pickup"],
                            "revision": "revision-7"
                        },
                        "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
                    }),
                ),
            )
        } else {
            response(503, json!({ "message": "observation unavailable" }))
        });
        responses.push(response(404, json!({ "message": "listing unavailable" })));
        let workflow = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new(responses)),
            config(),
        );

        let error = workflow.publish("draft-1", "one").await.unwrap_err();

        let recovery = error.recovery.unwrap();
        assert_eq!(recovery.failed_stage.as_deref(), Some("package_choice"));
        assert_eq!(recovery.delivery, Some(RecoveryStatus::Persisted));
        assert_eq!(recovery.publication, Some(RecoveryStatus::Indeterminate));
        assert_eq!(
            recovery.next_safe_actions,
            ["flea draft show draft-1", "flea listing show draft-1"]
        );
        assert!(recovery.destructive_actions.is_empty());
        if observation_available {
            assert_eq!(recovery.observation.status, ObservationStatus::Observed);
            assert!(recovery.observation.observed_at.is_some());
            assert_eq!(
                recovery.observed_etag.as_deref(),
                Some("observed-publication")
            );
            assert_eq!(recovery.observed_revision.as_deref(), Some("revision-7"));
        } else {
            assert_eq!(recovery.observation.status, ObservationStatus::Unavailable);
            assert!(recovery.observation.observed_at.is_none());
        }
    }
}

#[tokio::test]
async fn confirmation_follow_ups_are_best_effort_and_observation_still_runs() {
    let mut confirmation_failure = successful_publish_responses();
    confirmation_failure[11] = response(503, json!({ "message": "confirmation unavailable" }));
    confirmation_failure.remove(12);
    let result = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(confirmation_failure)),
        config(),
    )
    .publish("draft-1", "one")
    .await
    .unwrap();
    assert_eq!(
        result.warnings,
        ["confirmation fetch failed: confirmation unavailable"]
    );
    assert_eq!(
        result.completed_steps.last().unwrap(),
        "fetch_observed_listing"
    );

    let mut tracking_failure = successful_publish_responses();
    tracking_failure[12] = response(503, json!({ "message": "tracking unavailable" }));
    let result = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(tracking_failure)),
        config(),
    )
    .publish("draft-1", "one")
    .await
    .unwrap();
    assert_eq!(
        result.warnings,
        ["confirmation tracking failed: tracking unavailable"]
    );
    assert!(
        !result
            .completed_steps
            .iter()
            .any(|step| step == "track_confirmation")
    );
}

#[tokio::test]
async fn rejects_resource_ids_before_constructing_transport_paths() {
    let transport = FixtureTransport::new([]);
    let api = HttpAdInputApi::new(transport.clone());

    let error = api.get_draft("../credentials").await.unwrap_err();

    assert_eq!(error.code, "draft.invalid_id");
    assert!(transport.requests().is_empty());
}

#[tokio::test]
async fn percent_encodes_upstream_revisions_in_signed_query_targets() {
    let transport = FixtureTransport::new([response(
        200,
        json!({
            "id": "draft-1",
            "choices": [{
                "package-identifier": 10,
                "specification-urn": "urn:product:package-specification:10"
            }]
        }),
    )]);
    let api = HttpAdInputApi::new(transport.clone());

    api.product_context("draft-1", "revision&admin=true")
        .await
        .unwrap();

    assert_eq!(
        transport.requests()[0].path,
        "/adinput/product/recommerce/draft-1/productcontext?adRevision=revision%26admin%3Dtrue"
    );
}

#[test]
fn request_debug_redacts_targets_raw_bytes_and_secret_json_values() {
    let image = HttpRequest {
        method: Method::Post,
        path: "/images".to_owned(),
        if_match: None,
        retry: RetryPolicy::Never,
        body: RequestBody::Image {
            bytes: b"raw-image-secret".to_vec(),
            file_name: "image.jpg".to_owned(),
            mime_type: "image/jpeg".to_owned(),
            width: 1,
            height: 1,
        },
    };
    let json = HttpRequest {
        method: Method::Post,
        path: "/drafts".to_owned(),
        if_match: None,
        retry: RetryPolicy::Never,
        body: RequestBody::Json(json!({ "access_token": "token-secret" })),
    };
    let delivery = HttpRequest {
        method: Method::Get,
        path: "/ui/addelivery/shipping?name=Private+Seller".to_owned(),
        if_match: None,
        retry: RetryPolicy::BoundedRead,
        body: RequestBody::Empty,
    };

    assert!(!format!("{image:?}").contains("raw-image-secret"));
    assert!(!format!("{json:?}").contains("token-secret"));
    assert!(!format!("{delivery:?}").contains("Private"));
}

#[tokio::test]
async fn publish_timeout_bounds_a_hung_poll_request() {
    let processing = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(HangingPollTransport {
            responses: Mutex::new(VecDeque::from([
                response(200, processing),
                response(200, delivery_page(Some("pickup"))),
                category_taxonomy("furniture/chairs", true),
            ])),
        }),
        WorkflowConfig {
            image_processing_timeout: Duration::from_millis(20),
            image_poll_interval: Duration::ZERO,
            image_poll_limit: usize::MAX,
            listing_observation_timeout: Duration::from_secs(1),
            listing_poll_interval: Duration::ZERO,
            listing_poll_limit: 0,
        },
    );

    let error = tokio::time::timeout(Duration::from_secs(1), workflow.publish("draft-1", "one"))
        .await
        .expect("workflow must enforce its own deadline")
        .unwrap_err();

    assert_eq!(error.code, "draft.image_processing");
    let recovery = error.recovery.unwrap();
    assert!(recovery.upstream_transient);
    assert!(recovery.safe_to_retry);
}

#[tokio::test]
async fn publish_timeout_is_bounded_and_recoverable() {
    let processing = draft(
        "one",
        json!({
            "values": publication_values(),
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let transport = FixtureTransport::new([
        response(200, processing.clone()),
        response(200, delivery_page(Some("pickup"))),
        category_taxonomy("furniture/chairs", true),
        response(200, processing.clone()),
        response(200, processing),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.image_processing");
    let recovery = error.recovery.unwrap();
    assert!(recovery.upstream_transient);
    assert!(recovery.safe_to_retry);
    assert_eq!(
        recovery.completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "fetch_category_taxonomy",
            "validate"
        ]
    );
    assert_eq!(
        recovery.next_safe_actions,
        ["flea draft show draft-1", "flea draft validate draft-1"]
    );
    assert_eq!(recovery.observation.status, ObservationStatus::Observed);
    assert_eq!(recovery.images[0].status, RecoveryStatus::Pending);
}

fn composer_fixture() -> Value {
    serde_json::from_str(include_str!(
        "fixtures/adinput/bicycle-accessory-composer-live.json"
    ))
    .unwrap()
}

fn composer_with_category_options(selected: &str, option_ids: &[String]) -> Value {
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

fn delivery_fixture() -> Value {
    serde_json::from_str(include_str!(
        "fixtures/adinput/bicycle-accessory-delivery-live.json"
    ))
    .unwrap()
}

#[tokio::test]
async fn source_observed_composer_exposes_required_fields_price_and_revision() {
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, composer_fixture())]));

    let state = api.get_draft("46000000").await.unwrap();

    assert_eq!(state.revision.as_deref(), Some("12345"));
    assert_eq!(state.values["price"], 25);
    assert_eq!(
        state.required_fields,
        [
            "trade_type",
            "category",
            "title",
            "description",
            "postal-code"
        ]
    );
    let title = state
        .fields
        .iter()
        .find(|field| field.key == "title")
        .unwrap();
    assert_eq!(title.label, "Ilmoituksen otsikko");
    assert_eq!(title.field_type, FieldType::String);
    assert_eq!(title.requirement, Requirement::Required);
    assert_eq!(title.status, FieldStatus::Set);
    assert_eq!(title.value, Some(json!("Polkupyörän vaijerilukko")));
    let price = state
        .fields
        .iter()
        .find(|field| field.key == "price_amount")
        .unwrap();
    assert_eq!(price.field_type, FieldType::Decimal);
    assert_eq!(price.value, Some(json!(25)));
    let postal_code = state
        .fields
        .iter()
        .find(|field| field.key == "postal-code")
        .unwrap();
    assert_eq!(postal_code.value, Some(json!("00100")));
    assert!(state.fields.iter().all(|field| field.key != "price_max"));
    assert!(state.fields.iter().all(|field| field.key != "bikes_type"));
}

#[tokio::test]
async fn show_merges_source_observed_delivery_options_with_machine_values() {
    let transport = FixtureTransport::new([
        response(200, composer_fixture()),
        response(200, delivery_fixture()),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.show("46000000").await.unwrap();

    let delivery = state
        .fields
        .iter()
        .find(|field| field.key == "delivery")
        .unwrap();
    assert_eq!(delivery.field_type, FieldType::MultiSelect);
    assert_eq!(delivery.requirement, Requirement::Required);
    assert_eq!(delivery.status, FieldStatus::Missing);
    assert_eq!(delivery.value, Some(json!([])));
    assert_eq!(delivery.option_count, 4);
    assert_eq!(delivery.options_returned, 4);
    assert!(!delivery.options_truncated);
    assert!(state.required_fields.contains(&"delivery".to_owned()));
    assert_eq!(state.values["delivery"], json!([]));
    assert_eq!(
        state
            .options
            .iter()
            .filter(|option| option.field == "delivery")
            .map(|option| option.value.clone())
            .collect::<Vec<_>>(),
        [
            json!("pickup"),
            json!("shipping:small"),
            json!("shipping:medium"),
            json!("shipping:large"),
        ]
    );
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].path,
        "/ui/addelivery?adId=46000000&editMode=false"
    );
}

#[tokio::test]
async fn publish_validates_the_same_source_observed_model_as_show() {
    let transport = FixtureTransport::new([
        response(200, composer_fixture()),
        response(200, delivery_fixture()),
        category_taxonomy("258", true),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let error = workflow.publish("46000000", "one").await.unwrap_err();

    assert_eq!(error.code, "draft.validation_failed");
    let details = error.details.unwrap();
    assert_eq!(details["missing"][0]["field"], "delivery");
    assert_eq!(details["missing"][1]["field"], "images");
    assert_eq!(transport.requests().len(), 4);
    assert!(
        transport
            .requests()
            .iter()
            .all(|request| request.method == Method::Get)
    );
}

#[tokio::test]
async fn composer_bounds_options_and_reports_truncation() {
    let mut fixture = composer_fixture();
    fixture["model"]["sections"][2]["content"][1]["items"] = Value::Array(
        (0..60)
            .map(|index| json!({ "label": format!("Condition {index}"), "value": index }))
            .collect(),
    );
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, fixture)]));

    let state = api.get_draft("46000000").await.unwrap();

    let condition = state
        .fields
        .iter()
        .find(|field| field.key == "condition")
        .unwrap();
    assert_eq!(condition.option_count, 60);
    assert_eq!(condition.options_returned, 50);
    assert!(condition.options_truncated);
    assert_eq!(
        state
            .options
            .iter()
            .filter(|option| option.field == "condition")
            .count(),
        50
    );
}

#[tokio::test]
async fn composer_keeps_a_selected_category_outside_the_option_bound() {
    let option_ids = (0..60).map(|index| index.to_string()).collect::<Vec<_>>();
    let api = HttpAdInputApi::new(FixtureTransport::new([response(
        200,
        composer_with_category_options("59", &option_ids),
    )]));

    let state = api.get_draft("46000000").await.unwrap();
    let category = state
        .fields
        .iter()
        .find(|field| field.key == "category")
        .unwrap();
    let options = state
        .options
        .iter()
        .filter(|option| option.field == "category")
        .collect::<Vec<_>>();

    assert_eq!(category.option_count, 60);
    assert_eq!(category.options_returned, 50);
    assert!(category.options_truncated);
    assert_eq!(options.len(), 50);
    assert!(options.iter().any(|option| option.value == json!("59")));
    assert_eq!(options.last().unwrap().label, "Category 59");
}

#[tokio::test]
async fn validate_uses_exact_taxonomy_lookup_inside_and_outside_the_option_bound() {
    for selected in ["1", "59"] {
        let option_ids = (0..60).map(|index| index.to_string()).collect::<Vec<_>>();
        let report = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([
                response(200, composer_with_category_options(selected, &option_ids)),
                response(200, delivery_fixture()),
                category_taxonomy(selected, true),
            ])),
            config(),
        )
        .validate("46000000")
        .await
        .unwrap();

        let category = report.category_validation.as_ref().unwrap();
        assert_eq!(category.value, selected);
        assert_eq!(category.label.as_deref(), Some("Fixture category"));
        assert_eq!(category.exists, Some(true));
        assert_eq!(category.selectable, Some(true));
        assert_eq!(category.compatible, Some(true));
        assert_eq!(
            category.existence_source.as_deref(),
            Some("category_taxonomy")
        );
        assert_eq!(
            category.selectability_source.as_deref(),
            Some("category_taxonomy")
        );
        assert_eq!(
            category.compatibility_source.as_deref(),
            Some("listing_composer")
        );
        assert!(report.invalid.iter().all(|issue| issue.field != "category"));
        assert!(
            report
                .unverifiable
                .iter()
                .all(|issue| issue.field != "category")
        );
    }
}

#[tokio::test]
async fn validate_rejects_removed_categories_with_an_exact_recovery_lookup() {
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(
                200,
                composer_with_category_options("258", &["258".to_owned()]),
            ),
            response(200, delivery_fixture()),
            category_taxonomy("999", true),
        ])),
        config(),
    )
    .validate("46000000")
    .await
    .unwrap();

    let category = report.category_validation.as_ref().unwrap();
    assert_eq!(category.exists, Some(false));
    assert_eq!(category.selectable, None);
    assert_eq!(category.compatible, Some(true));
    let issue = report
        .invalid
        .iter()
        .find(|issue| issue.field == "category")
        .unwrap();
    assert!(issue.reason.contains("absent from or inaccessible"));
    assert_eq!(issue.command, "flea category search 258");
}

#[tokio::test]
async fn validate_rejects_category_schema_disagreement() {
    let report = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(
                200,
                composer_with_category_options("258", &["257".to_owned()]),
            ),
            response(200, delivery_fixture()),
            category_taxonomy("258", true),
        ])),
        config(),
    )
    .validate("46000000")
    .await
    .unwrap();

    let category = report.category_validation.as_ref().unwrap();
    assert_eq!(category.exists, Some(true));
    assert_eq!(category.selectable, Some(true));
    assert_eq!(category.compatible, Some(false));
    assert!(report.invalid.iter().any(|issue| {
        issue.field == "category"
            && issue.source == "listing_composer"
            && issue.reason.contains("incompatible")
    }));
}

#[tokio::test]
async fn composer_preserves_safe_unknown_types_without_protocol_details() {
    let mut fixture = composer_fixture();
    fixture["model"]["sections"][2]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "id": "future_gain",
            "type": "future-slider",
            "sub-type": "v2",
            "label": "Future gain",
            "required": false,
            "owner": { "email": "owner@example.invalid" },
            "protocol-token": "protocol-secret"
        }));
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, fixture)]));

    let state = api.get_draft("46000000").await.unwrap();

    let unknown = state
        .fields
        .iter()
        .find(|field| field.key == "future_gain")
        .unwrap();
    assert_eq!(
        unknown.field_type,
        FieldType::Unknown("future-slider".to_owned())
    );
    assert_eq!(
        unknown.raw,
        Some(json!({
            "type": "future-slider",
            "sub_type": "v2",
            "has_children": false,
            "has_options": false
        }))
    );
    let output = serde_json::to_string(&state).unwrap();
    assert!(!output.contains("owner@example.invalid"));
    assert!(!output.contains("protocol-secret"));
}

#[tokio::test]
async fn composer_distinguishes_empty_malformed_and_evolved_models() {
    let mut malformed = composer_fixture();
    malformed["model"]
        .as_object_mut()
        .unwrap()
        .remove("sections");
    malformed["owner"] = json!({ "email": "owner@example.invalid" });
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, malformed)]));

    let error = api.get_draft("46000000").await.unwrap_err();

    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.details.as_deref().unwrap()["path"],
        "$.model.sections"
    );
    assert!(!format!("{error:?}").contains("owner@example.invalid"));

    let unavailable = json!({
        "draft_id": "46000000",
        "etag": "12345",
        "values": {}
    });
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, unavailable)]));
    let error = api.get_draft("46000000").await.unwrap_err();
    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(error.details.as_deref().unwrap()["path"], "$.fields");

    let mut conflicting_revision = composer_fixture();
    conflicting_revision["ad"]["etag"] = json!("W/\"54321\"");
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, conflicting_revision)]));
    let error = api.get_draft("46000000").await.unwrap_err();
    assert_eq!(error.code, "upstream.unrecognized_model");
    assert_eq!(
        error.details.as_deref().unwrap()["reason"],
        "draft revision sources disagree"
    );

    let mut empty = composer_fixture();
    empty["model"]["sections"] = json!([]);
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, empty)]));
    let state = api.get_draft("46000000").await.unwrap();
    assert!(state.fields.is_empty());
    assert!(state.options.is_empty());
    assert!(state.required_fields.is_empty());

    let mut evolved = composer_fixture();
    let condition = evolved["model"]["sections"][2]["content"][1]
        .as_object_mut()
        .unwrap();
    let mut options = condition.remove("items").unwrap();
    options[0]["value"] = json!(1);
    condition.insert("options".to_owned(), options);
    evolved["model"]["schema-version"] = json!(2);
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, evolved)]));
    let state = api.get_draft("46000000").await.unwrap();
    assert!(state.options.iter().any(|option| {
        option.field == "condition" && option.value == json!(1) && option.label == "Uusi"
    }));
}

#[tokio::test]
async fn delivery_composer_bounds_options_and_reports_truncation() {
    let mut fixture = delivery_fixture();
    fixture["sections"]["shipping"]["packageSizes"] = Value::Object(
        (0..60)
            .map(|index| {
                (
                    format!("package-{index}"),
                    json!({
                        "title": format!("Package {index}"),
                        "size": format!("SIZE_{index}")
                    }),
                )
            })
            .collect(),
    );
    let api = HttpAdInputApi::new(FixtureTransport::new([response(200, fixture)]));

    let composer = api.delivery_composer("46000000").await.unwrap();

    assert_eq!(composer.state.option_count, 61);
    assert_eq!(composer.state.options_returned, 50);
    assert!(composer.state.options_truncated);
    assert_eq!(composer.state.options.len(), 50);
}
