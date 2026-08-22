use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde_json::{Map, Value, json};
use tori::api::adinput::{
    DraftWorkflow, HttpAdInputApi, HttpRequest, HttpResponse, HttpTransport, ImageState, Method,
    RequestBody, RetryPolicy, WorkflowConfig,
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

fn config() -> WorkflowConfig {
    WorkflowConfig {
        image_processing_timeout: Duration::from_secs(1),
        image_poll_interval: Duration::ZERO,
        image_poll_limit: 2,
    }
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
                        { "image_id": "first", "position": 0, "state": "ready" },
                        { "image_id": "second", "position": 1, "state": "ready" },
                        { "image_id": "third", "position": 2, "state": "ready" }
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
    assert_eq!(requests[6].path, "/drafts/draft-1/publish");
    assert!(requests.iter().all(|request| {
        !matches!(
            request.method,
            Method::Post | Method::Patch | Method::Put | Method::Delete
        ) || request.retry == RetryPolicy::Never
    }));
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
