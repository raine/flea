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
    first: Mutex<Option<HttpResponse>>,
}

impl HttpTransport for HangingPollTransport {
    async fn execute(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpResponse, flea::api::adinput::ApiError> {
        if let Some(response) = self.first.lock().unwrap().take() {
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

fn successful_publish_responses() -> Vec<HttpResponse> {
    let valid = draft(
        "one",
        json!({
            "values": {
                "category": "furniture/chairs",
                "title": "Chair",
                "delivery": ["pickup"]
            },
            "required_fields": ["category", "title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    vec![
        response(200, valid.clone()),
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
                        "delivery": ["pickup"],
                        "revision": "revision-7"
                    }
                }),
            ),
        ),
        response(204, Value::Null),
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
        assert!(!error.retryable);
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
    assert!(!recovery.retryable);
    assert_eq!(recovery.fresh_state.unwrap().values["title"], "other agent");
    let requests = transport.requests();
    assert_eq!(requests[1].if_match.as_deref(), Some("one"));
    assert_eq!(requests[1].retry, RetryPolicy::Never);
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
    assert!(!recovery.retryable);
    assert_eq!(recovery.next_safe_actions, ["flea draft show draft-1"]);
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
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.show("draft-1").await.unwrap();

    assert_eq!(state.predictions[0].category, "furniture/chairs");
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
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
    let valid = draft(
        "one",
        json!({
            "values": {
                "category": "furniture/chairs",
                "title": "Chair",
                "delivery": ["pickup"]
            },
            "required_fields": ["category", "title", "delivery"],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        }),
    );
    let submitted = draft(
        "three",
        json!({
            "values": {
                "category": "furniture/chairs",
                "title": "Chair",
                "delivery": ["pickup"],
                "revision": "revision-7"
            }
        }),
    );
    let transport = FixtureTransport::new([
        response(200, valid.clone()),
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
        response(200, submitted),
        response(204, Value::Null),
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
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let published = workflow.publish("draft-1").await.unwrap();

    assert_eq!(published.listing_id, "listing-9");
    assert_eq!(published.state, "pending");
    assert_eq!(
        published.completed_steps,
        [
            "fetch_draft",
            "validate",
            "wait_for_images",
            "patch_item_fields",
            "fetch_fresh_etag",
            "submit_adinput",
            "apply_delivery",
            "fetch_product_context",
            "publish_basic",
            "fetch_confirmation",
            "track_confirmation",
            "fetch_observed_listing"
        ]
    );
    assert!(published.warnings.is_empty());

    let requests = transport.requests();
    assert_eq!(requests.len(), 10);
    let observed_sequence = requests
        .iter()
        .map(|request| (request.method.clone(), request.path.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        observed_sequence,
        [
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Put, "/adinput/ad/recommerce/draft-1/update"),
            (Method::Get, "/adinput/ad/withModel/draft-1"),
            (Method::Put, "/drafts/draft-1/adinput"),
            (Method::Put, "/drafts/draft-1/delivery"),
            (Method::Get, "/drafts/draft-1/products?revision=revision-7"),
            (Method::Post, "/drafts/draft-1/publish"),
            (Method::Get, "/listings/listing-9/confirmation"),
            (Method::Post, "/tracking/confirmation"),
            (Method::Get, "/listings/listing-9"),
        ]
    );
    assert_eq!(
        requests[4].body,
        RequestBody::Json(json!({
            "revision": "revision-7",
            "delivery": ["pickup"]
        }))
    );
    assert_eq!(
        requests[6].body,
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
async fn publish_validation_rejects_missing_fields_and_implicit_delivery() {
    for (values, required, expected_missing) in [
        (
            json!({ "delivery": ["pickup"] }),
            json!(["title", "delivery"]),
            json!(["title"]),
        ),
        (
            json!({ "title": "Chair", "delivery": [] }),
            json!(["title"]),
            json!(["delivery"]),
        ),
    ] {
        let state = draft(
            "one",
            json!({
                "values": values,
                "required_fields": required,
                "images": []
            }),
        );
        let workflow = DraftWorkflow::new(
            HttpAdInputApi::new(FixtureTransport::new([response(200, state)])),
            config(),
        );

        let error = workflow.publish("draft-1").await.unwrap_err();
        assert_eq!(error.code, "draft.validation_failed");
        assert_eq!(error.details.unwrap()["missing_fields"], expected_missing);
        assert_eq!(error.recovery.unwrap().completed_steps, ["fetch_draft"]);
    }
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
    let transport = FixtureTransport::new([response(200, failed)]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1").await.unwrap_err();

    assert_eq!(error.code, "draft.image_failed");
    assert_eq!(
        error.source.unwrap().details.unwrap()["image_id"],
        "image-broken"
    );
    assert_eq!(
        error.recovery.unwrap().completed_steps,
        ["fetch_draft", "validate"]
    );
}

#[tokio::test]
async fn publish_failures_report_each_completed_workflow_boundary() {
    let cases = [
        (0, &[][..], true),
        (
            1,
            &["fetch_draft", "validate", "wait_for_images"][..],
            false,
        ),
        (
            2,
            &[
                "fetch_draft",
                "validate",
                "wait_for_images",
                "patch_item_fields",
            ][..],
            true,
        ),
        (
            3,
            &[
                "fetch_draft",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
            ][..],
            false,
        ),
        (
            4,
            &[
                "fetch_draft",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
            ][..],
            false,
        ),
        (
            5,
            &[
                "fetch_draft",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
                "apply_delivery",
            ][..],
            true,
        ),
        (
            6,
            &[
                "fetch_draft",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
                "apply_delivery",
                "fetch_product_context",
            ][..],
            false,
        ),
        (
            9,
            &[
                "fetch_draft",
                "validate",
                "wait_for_images",
                "patch_item_fields",
                "fetch_fresh_etag",
                "submit_adinput",
                "apply_delivery",
                "fetch_product_context",
                "publish_basic",
                "fetch_confirmation",
                "track_confirmation",
            ][..],
            true,
        ),
    ];

    for (failure_index, expected_steps, retryable) in cases {
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
        assert_eq!(
            recovery.retryable, retryable,
            "failure index {failure_index}"
        );
        if failure_index == 9 {
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
    confirmation_failure[7] = response(503, json!({ "message": "confirmation unavailable" }));
    confirmation_failure.remove(8);
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
    tracking_failure[8] = response(503, json!({ "message": "tracking unavailable" }));
    let result = DraftWorkflow::new(
        HttpAdInputApi::new(FixtureTransport::new(tracking_failure)),
        config(),
    )
    .publish("draft-1")
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
fn image_request_debug_never_contains_raw_bytes_or_secret_json_values() {
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

    assert!(!format!("{image:?}").contains("raw-image-secret"));
    assert!(!format!("{json:?}").contains("token-secret"));
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
            first: Mutex::new(Some(response(200, processing))),
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
    assert!(error.recovery.unwrap().retryable);
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
        response(200, processing.clone()),
        response(200, processing),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport), config());

    let error = workflow.publish("draft-1").await.unwrap_err();

    assert_eq!(error.code, "draft.image_processing");
    let recovery = error.recovery.unwrap();
    assert!(recovery.retryable);
    assert_eq!(recovery.completed_steps, ["fetch_draft", "validate"]);
    assert_eq!(recovery.next_safe_actions, ["flea draft show draft-1"]);
}
