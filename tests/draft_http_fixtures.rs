use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Map, Value, json};
use tori::api::adinput::{
    AdInputApi, DraftWorkflow, HttpAdInputApi, HttpRequest, HttpResponse, HttpTransport,
    ImageState, Method, RequestBody, RetryPolicy, WorkflowConfig,
};

#[derive(Clone)]
struct FixtureTransport {
    responses: Arc<Mutex<VecDeque<Result<HttpResponse, tori::api::adinput::ApiError>>>>,
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
    ) -> Result<HttpResponse, tori::api::adinput::ApiError> {
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
        body,
    }
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
    ) -> Result<HttpResponse, tori::api::adinput::ApiError> {
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
    assert_eq!(recovery.next_safe_actions, ["tori draft show draft-1"]);
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
    let transport = FixtureTransport::new([
        response(
            200,
            draft(
                "one",
                json!({
                    "images": [{
                        "image_id": "first",
                        "position": 0,
                        "state": "ready"
                    }]
                }),
            ),
        ),
        response(201, json!({ "image_id": "second", "state": "processing" })),
        response(
            200,
            draft(
                "two",
                json!({
                    "images": [
                        { "image_id": "first", "position": 0, "state": "ready" },
                        { "image_id": "second", "position": 1, "state": "processing" }
                    ]
                }),
            ),
        ),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow.add_images("draft-1", &[path]).await.unwrap();

    assert_eq!(state.images[1].state, ImageState::Processing);
    let requests = transport.requests();
    let RequestBody::Image { width, height, .. } = &requests[1].body else {
        panic!("expected image upload")
    };
    assert_eq!((*width, *height), (7, 11));
    assert_eq!(requests[1].retry, RetryPolicy::Never);
    assert_eq!(
        requests[2].body,
        RequestBody::Json(json!({
            "image_ids": ["first", "second"]
        }))
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
                        { "image_id": "third", "position": 2, "state": "ready" },
                        { "image_id": "first", "position": 0, "state": "ready" },
                        { "image_id": "second", "position": 1, "state": "ready" }
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
                        { "image_id": "first", "position": 0, "state": "ready" },
                        { "image_id": "third", "position": 1, "state": "ready" }
                    ]
                }),
            ),
        ),
        response(204, Value::Null),
    ]);
    let workflow = DraftWorkflow::new(HttpAdInputApi::new(transport.clone()), config());

    let state = workflow
        .remove_images("draft-1", &["second".to_owned()])
        .await
        .unwrap();
    workflow.delete("draft-1").await.unwrap();

    assert_eq!(state.images[1].image_id, "third");
    let requests = transport.requests();
    assert_eq!(
        requests[1].body,
        RequestBody::Json(json!({ "image_ids": ["first", "third"] }))
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
            (Method::Get, "/drafts/draft-1/with-model"),
            (Method::Patch, "/drafts/draft-1/item"),
            (Method::Get, "/drafts/draft-1/with-model"),
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
            assert_eq!(recovery.next_safe_actions, ["tori listing show listing-9"]);
        } else {
            assert_eq!(recovery.next_safe_actions, ["tori draft show draft-1"]);
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
    assert_eq!(recovery.next_safe_actions, ["tori draft show draft-1"]);
}
