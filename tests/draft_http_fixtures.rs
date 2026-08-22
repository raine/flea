use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use flea::api::adinput::{
    AdInputApi, DraftWorkflow, HttpAdInputApi, HttpRequest, HttpResponse, HttpTransport,
    ImageState, Method, RequestBody, RetryPolicy, WorkflowConfig,
};
use serde_json::{Map, Value, json};

#[derive(Clone)]
struct FixtureTransport {
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, flea::api::adinput::ApiError>>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
}

impl FixtureTransport {
    fn new(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().map(Ok).collect())),
            requests: Arc::new(Mutex::new(Vec::new())),
        }
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
        self.requests.lock().unwrap().push(request);
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
        _request: HttpRequest,
    ) -> Result<HttpResponse, flea::api::adinput::ApiError> {
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

fn successful_publish_responses() -> Vec<HttpResponse> {
    let valid = draft(
        "one",
        json!({
            "values": {
                "category": "furniture/chairs",
                "title": "Chair"
            },
            "required_fields": ["category", "title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    vec![
        response(200, valid.clone()),
        response(200, delivery_page(Some("pickup"))),
        response(
            200,
            draft(
                "two",
                json!({ "values": valid["values"].clone(), "images": valid["images"].clone() }),
            ),
        ),
        response(
            200,
            draft(
                "two",
                json!({ "values": valid["values"].clone(), "images": valid["images"].clone() }),
            ),
        ),
        response(
            200,
            draft(
                "three",
                json!({
                    "values": {
                        "category": "furniture/chairs",
                        "title": "Chair",
                        "revision": "revision-7"
                    }
                }),
            ),
        ),
        response(204, Value::Null),
        response(200, delivery_page(Some("pickup"))),
        response(
            200,
            json!({ "revision": "revision-7", "context": { "currency": "EUR" } }),
        ),
        response(
            201,
            json!({ "listing_id": "listing-9", "revision": "revision-7", "state": "pending" }),
        ),
        response(200, json!({ "order_id": "order-4", "details": {} })),
        response(204, Value::Null),
        response(
            200,
            json!({ "listing_id": "listing-9", "state": "pending" }),
        ),
    ]
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
        "model": {}
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
        ["flea draft update draft-1 --price VALUE"]
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
        ["flea draft update draft-1 --price VALUE"]
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
        ["flea draft update draft-1 --price VALUE"]
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

    let missing_transport = FixtureTransport::new([
        response(200, draft("one", json!({}))),
        response(200, json!({ "context": {}, "sections": {} })),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(missing_transport.clone()), config());
    let error = workflow
        .update(
            "draft-1",
            &Map::from_iter([("delivery".to_owned(), json!(["pickup"]))]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "draft.invalid_delivery");
    assert_eq!(error.details.unwrap()["allowed_values"], json!([]));
    assert_eq!(missing_transport.requests().len(), 2);
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
    assert_eq!(recovery.absent_fields, ["delivery"]);
    assert_eq!(
        recovery.next_safe_actions,
        ["flea draft update draft-1 --delivery VALUE"]
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
        ["flea draft update draft-1 --category VALUE"]
    );
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
    assert_eq!(recovery.listing_id.as_deref(), Some("listing-7"));
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
async fn publish_runs_the_complete_bounded_sequence() {
    let transport = FixtureTransport::new(successful_publish_responses());
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let published = workflow.publish("draft-1").await.unwrap();

    assert_eq!(published.listing_id, "listing-9");
    assert_eq!(published.state, "pending");
    assert_eq!(
        published.completed_steps,
        [
            "fetch_draft",
            "fetch_delivery_options",
            "validate",
            "wait_for_images",
            "patch_item_fields",
            "fetch_fresh_etag",
            "submit_adinput",
            "apply_delivery",
            "observe_delivery",
            "fetch_product_context",
            "publish_basic",
            "fetch_confirmation",
            "track_confirmation",
            "fetch_observed_listing"
        ]
    );
    assert!(published.warnings.is_empty());

    let requests = transport.requests();
    assert_eq!(requests.len(), 12);
    let observed_sequence = requests
        .iter()
        .map(|request| (request.method.clone(), request.path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed_sequence,
        [
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Get, "/ui/addelivery?adId=draft-1&editMode=false"),
            (Method::Put, "/adinput/ad/recommerce/draft-1/update"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Put, "/drafts/draft-1/adinput"),
            (Method::Post, "/ads/draft-1/delivery"),
            (Method::Get, "/ui/addelivery?adId=draft-1&editMode=false"),
            (Method::Get, "/drafts/draft-1/products?revision=revision-7"),
            (Method::Post, "/drafts/draft-1/publish"),
            (Method::Get, "/listings/listing-9/confirmation"),
            (Method::Post, "/tracking/confirmation"),
            (Method::Get, "/listings/listing-9"),
        ]
    );
    assert_eq!(
        requests[5].body,
        RequestBody::Json(json!({
            "meetup": true,
            "shipping": false,
            "sellerPaysShipping": false,
            "client": "ANDROID",
            "buyNow": false
        }))
    );
    let RequestBody::Json(item_values) = &requests[2].body else {
        panic!("expected item update")
    };
    assert!(item_values.get("delivery").is_none());
    assert_eq!(
        requests[8].body,
        RequestBody::Json(json!({
            "package": "basic",
            "revision": "revision-7",
            "context": { "currency": "EUR" }
        }))
    );
    assert!(requests.iter().all(|request| {
        !matches!(
            request.method,
            Method::Post | Method::Patch | Method::Put | Method::Delete
        ) || request.retry == RetryPolicy::Never
    }));
}

#[tokio::test]
async fn publish_validation_rejects_missing_fields_and_missing_delivery() {
    let missing_title = draft(
        "one",
        json!({
            "values": {},
            "required_fields": ["title", "delivery"],
            "images": []
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, missing_title),
            response(200, delivery_page(Some("pickup"))),
        ])),
        config(),
    );
    let error = workflow.publish("draft-1").await.unwrap_err();
    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["missing_fields"], json!(["title"]));
    assert_eq!(
        error.recovery.unwrap().completed_steps,
        ["fetch_draft", "fetch_delivery_options"]
    );

    let no_delivery = draft(
        "one",
        json!({
            "values": { "title": "Chair" },
            "required_fields": ["title", "delivery"],
            "images": []
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new([
            response(200, no_delivery),
            response(200, delivery_page(None)),
        ])),
        config(),
    );
    let error = workflow.publish("draft-1").await.unwrap_err();
    assert_eq!(error.code, "draft.validation_failed");
    assert_eq!(error.details.unwrap()["allowed_values"][0], "pickup");
    assert_eq!(
        error.recovery.unwrap().next_safe_actions[0],
        "flea draft update draft-1 --delivery pickup"
    );
}

#[tokio::test]
async fn publish_reports_failed_image_and_preserves_its_identity() {
    let failed = draft(
        "one",
        json!({
            "values": { "title": "Chair", "delivery": ["pickup"] },
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
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1").await.unwrap_err();

    assert_eq!(error.code, "draft.image_failed");
    assert_eq!(
        error.source.unwrap().details.unwrap()["image_id"],
        "image-broken"
    );
    assert_eq!(
        error.recovery.unwrap().completed_steps,
        ["fetch_draft", "fetch_delivery_options", "validate"]
    );
}

#[tokio::test]
async fn publish_failures_report_each_completed_workflow_boundary() {
    let cases = [
        (0, &[][..], true),
        (1, &["fetch_draft"][..], true),
        (
            2,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
            ][..],
            false,
        ),
        (
            3,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
                "patch_item_fields",
            ][..],
            false,
        ),
        (
            4,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
            ][..],
            false,
        ),
        (
            5,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
            ][..],
            false,
        ),
        (
            6,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
                "apply_delivery",
            ][..],
            false,
        ),
        (
            7,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
                "apply_delivery",
                "observe_delivery",
            ][..],
            false,
        ),
        (
            8,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
                "apply_delivery",
                "observe_delivery",
                "fetch_product_context",
            ][..],
            false,
        ),
        (
            11,
            &[
                "fetch_draft",
                "fetch_delivery_options",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
                "apply_delivery",
                "observe_delivery",
                "fetch_product_context",
                "publish_basic",
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

        let error = workflow.publish("draft-1").await.unwrap_err();
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
        if failure_index == 11 {
            assert_eq!(
                error.details.unwrap()["listing_id"],
                "listing-9",
                "published listing identity must survive observation failure"
            );
            assert_eq!(recovery.next_safe_actions, ["flea listing show listing-9"]);
        } else {
            assert_eq!(recovery.next_safe_actions, ["flea draft show draft-1"]);
        }
    }
}

#[tokio::test]
async fn confirmation_follow_ups_are_best_effort_and_observation_still_runs() {
    let mut confirmation_failure = successful_publish_responses();
    confirmation_failure[9] = response(503, json!({ "message": "confirmation unavailable" }));
    confirmation_failure.remove(10);
    let result = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(confirmation_failure)),
        config(),
    )
    .publish("draft-1")
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
    tracking_failure[10] = response(503, json!({ "message": "tracking unavailable" }));
    let result = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(tracking_failure)),
        config(),
    )
    .publish("draft-1")
    .await
    .unwrap();
    assert_eq!(
        result.warnings,
        [
            "confirmation tracking failed: The upstream failure may be temporary, but the mutation outcome is unknown"
        ]
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
        json!({ "revision": "revision&admin=true", "context": {} }),
    )]);
    let api = HttpAdInputApi::new(transport.clone());

    api.product_context("draft-1", "revision&admin=true")
        .await
        .unwrap();

    assert_eq!(
        transport.requests()[0].path,
        "/drafts/draft-1/products?revision=revision%26admin%3Dtrue"
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
            "values": { "title": "Chair", "delivery": ["pickup"] },
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let workflow = DraftWorkflow::new(
        HttpAdInputApi::new(HangingPollTransport {
            responses: Mutex::new(VecDeque::from([
                response(200, processing),
                response(200, delivery_page(Some("pickup"))),
            ])),
        }),
        WorkflowConfig {
            image_processing_timeout: Duration::from_millis(20),
            image_poll_interval: Duration::ZERO,
            image_poll_limit: usize::MAX,
        },
    );

    let error = tokio::time::timeout(Duration::from_secs(1), workflow.publish("draft-1"))
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
            "values": { "title": "Chair", "delivery": ["pickup"] },
            "required_fields": ["title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "processing" }]
        }),
    );
    let transport = FixtureTransport::new([
        response(200, processing.clone()),
        response(200, delivery_page(Some("pickup"))),
        response(200, processing.clone()),
        response(200, processing),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1").await.unwrap_err();

    assert_eq!(error.code, "draft.image_processing");
    let recovery = error.recovery.unwrap();
    assert!(recovery.upstream_transient);
    assert!(recovery.safe_to_retry);
    assert_eq!(
        recovery.completed_steps,
        ["fetch_draft", "fetch_delivery_options", "validate"]
    );
    assert_eq!(recovery.next_safe_actions, ["flea draft show draft-1"]);
}
