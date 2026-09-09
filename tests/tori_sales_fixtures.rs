use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use flea::{
    dependencies::{
        ApplicationDependencies, HttpError, HttpResponse, RequestBody, RequestSpec, ToriClient,
        TransportError, TransportErrorKind,
    },
    run_with_dependencies,
};
use reqwest::{Method, StatusCode};
use serde_json::{Value, json};

struct FixtureClient {
    responses: Mutex<VecDeque<Result<HttpResponse, HttpError>>>,
    calls: Mutex<Vec<RequestSpec>>,
}

impl ToriClient for FixtureClient {
    fn execute(
        &self,
        request: RequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>> {
        self.calls.lock().unwrap().push(request);
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("only one summary GET per invocation");
        Box::pin(async move { response })
    }
}

fn page() -> Value {
    serde_json::from_str(include_str!("fixtures/tori/sales/page.json")).unwrap()
}

fn response(value: Value) -> Result<HttpResponse, HttpError> {
    Ok(HttpResponse {
        status: StatusCode::OK,
        headers: Default::default(),
        body: serde_json::to_vec(&value).unwrap(),
    })
}

fn setup(
    responses: Vec<Result<HttpResponse, HttpError>>,
) -> (ApplicationDependencies, Arc<FixtureClient>) {
    let client = Arc::new(FixtureClient {
        responses: Mutex::new(responses.into()),
        calls: Mutex::new(Vec::new()),
    });
    (
        ApplicationDependencies::production().with_tori_client(client.clone()),
        client,
    )
}

fn run(dependencies: &ApplicationDependencies, options: &[&str]) -> flea::RunResult {
    run_with_dependencies(
        ["flea", "--format", "json", "tori", "sales", "list"]
            .into_iter()
            .chain(options.iter().copied()),
        dependencies,
    )
}

#[test]
fn sold_summaries_use_one_get_and_native_continuation_with_allowlisted_output() {
    let mut last = page();
    last["summaries"] = json!([{"id":91003,"state":{"type":"DISPOSED"},"data":null}]);
    last["query"]["offset"] = json!(2);
    let (dependencies, client) = setup(vec![response(page()), response(last)]);
    let result = run(&dependencies, &[]);
    assert_eq!(result.exit_code, 0, "{}", result.document);
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    assert_eq!(
        envelope["data"],
        json!({
            "sales":[{"listing_id":"91001","title":"Fixture chair","state":"DISPOSED","subtitle":"Display text only","image":"https://images.example.test/chair.jpg"},
            {"listing_id":"91002","title":null,"state":"FUTURE_STATE","subtitle":null,"image":null}],
            "count":2,"total":3,"offset":0,"limit":50,"next_offset":2
        })
    );
    let command = envelope["next_actions"][0]["command"].as_str().unwrap();
    assert_eq!(command, "flea tori sales list --offset 2 --limit 50");
    for absent in [
        "private-",
        "price",
        "transaction_id",
        "sold_at",
        "updated",
        "created",
        "expires",
        "statistics",
    ] {
        assert!(
            !result.document.contains(absent),
            "unexpected output: {absent}"
        );
    }
    let result = run_with_dependencies(
        ["flea", "--format", "json"]
            .into_iter()
            .chain(command.split_whitespace().skip(1)),
        &dependencies,
    );
    assert_eq!(result.exit_code, 0, "{}", result.document);
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    assert_eq!(envelope["data"]["sales"][0]["listing_id"], "91003");
    assert_eq!(envelope["data"]["next_offset"], Value::Null);
    assert!(envelope.get("next_actions").is_none());
    let calls = client.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    for (call, offset) in calls.iter().zip([0, 2]) {
        assert_eq!(call.method, Method::GET);
        assert_eq!(call.service, "AD-SUMMARIES");
        assert_eq!(
            call.path_and_query,
            format!("/search?facet=DISPOSED&offset={offset}&limit=50")
        );
        assert!(matches!(call.body, RequestBody::Empty));
        assert!(call.if_match.is_none());
        assert!(call.headers.is_empty());
    }
}

#[test]
fn toon_and_explicit_limit_use_the_same_summary_read() {
    let mut raw = page();
    raw["query"]["limit"] = json!(2);
    let (dependencies, client) = setup(vec![response(raw)]);
    let result = run_with_dependencies(
        [
            "flea", "--format", "toon", "tori", "sales", "list", "--limit", "2",
        ],
        &dependencies,
    );
    assert_eq!(result.exit_code, 0, "{}", result.document);
    assert!(result.document.contains("91001"));
    assert!(result.document.contains("next_offset: 2"));
    assert!(!result.document.contains("private-"));
    assert_eq!(
        client.calls.lock().unwrap()[0].path_and_query,
        "/search?facet=DISPOSED&offset=0&limit=2"
    );
}

#[test]
fn empty_and_out_of_range_pages_do_not_offer_continuation() {
    for (offset, total) in [(0, 0), (3, 3), (99, 3)] {
        let raw = json!({"summaries":[],"total":total,"query":{"facet":"DISPOSED","offset":offset,"limit":50}});
        let (dependencies, client) = setup(vec![response(raw)]);
        let result = run(&dependencies, &["--offset", &offset.to_string()]);
        assert_eq!(result.exit_code, 0, "{}", result.document);
        let envelope: Value = serde_json::from_str(&result.document).unwrap();
        assert_eq!(envelope["data"]["count"], 0);
        assert_eq!(envelope["data"]["total"], total);
        assert_eq!(envelope["data"]["next_offset"], Value::Null);
        assert!(envelope.get("next_actions").is_none());
        assert_eq!(client.calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn malformed_summaries_and_pagination_are_redacted_errors() {
    for (pointer, value) in [
        ("/summaries", Value::Null),
        ("/summaries", json!({})),
        ("/summaries", json!([])),
        ("/summaries/0", json!(false)),
        ("/summaries/0/id", json!(0)),
        ("/summaries/0/id", json!(-1)),
        ("/summaries/0/id", json!(1.5)),
        ("/summaries/0/id", json!("private-id")),
        ("/summaries/0/id", json!(u64::MAX)),
        ("/summaries/0/id", Value::Null),
        ("/summaries/0/id", json!({"private-id":1})),
        ("/summaries/1/id", json!(91001)),
        ("/summaries/0/state", Value::Null),
        ("/summaries/0/state", json!({})),
        ("/summaries/0/state/type", json!("")),
        ("/summaries/0/state/type", json!({"private-state":1})),
        ("/summaries/0/data", json!(false)),
        ("/summaries/0/data/title", json!({"private-title":1})),
        ("/summaries/0/data/subtitle", json!([])),
        ("/summaries/0/data/image", json!(1)),
        ("/total", json!(0)),
        ("/total", json!(-1)),
        ("/total", json!("3")),
        ("/total", json!(2147483648_u64)),
        ("/query", Value::Null),
        ("/query", json!({})),
        ("/query/facet", json!("ACTIVE")),
        ("/query/offset", json!(1)),
        ("/query/offset", json!(-1)),
        ("/query/offset", json!("0")),
        ("/query/limit", json!(0)),
        ("/query/limit", json!(1)),
    ] {
        let mut raw = page();
        *raw.pointer_mut(pointer).unwrap() = value;
        assert_invalid(raw, pointer);
    }
    for field in ["summaries", "total", "query"] {
        let mut raw = page();
        raw.as_object_mut().unwrap().remove(field);
        assert_invalid(raw, field);
    }
    for field in ["id", "state", "data"] {
        let mut raw = page();
        raw["summaries"][0].as_object_mut().unwrap().remove(field);
        assert_invalid(raw, field);
    }
    let mut raw = page();
    raw["query"]["limit"] = json!(1);
    let (dependencies, _) = setup(vec![response(raw)]);
    assert_eq!(run(&dependencies, &["--limit", "1"]).exit_code, 40);
    let mut raw = page();
    raw["query"]["offset"] = json!(2);
    let (dependencies, _) = setup(vec![response(raw)]);
    assert_eq!(run(&dependencies, &["--offset", "2"]).exit_code, 40);
}

fn assert_invalid(raw: Value, label: &str) {
    let (dependencies, client) = setup(vec![response(raw)]);
    let result = run(&dependencies, &[]);
    assert_eq!(result.exit_code, 40, "{label}: {}", result.document);
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "tori_sales.unexpected_response");
    assert!(!result.document.contains("private-"));
    assert_eq!(client.calls.lock().unwrap().len(), 1);
}

#[test]
fn auth_http_and_transport_failures_remain_errors_without_private_bodies() {
    for (status, exit) in [(401, 10), (403, 10), (404, 40), (429, 40), (500, 40)] {
        let (dependencies, _) = setup(vec![Ok(HttpResponse {
            status: StatusCode::from_u16(status).unwrap(),
            headers: Default::default(),
            body: b"private-upstream-body".to_vec(),
        })]);
        let result = run(&dependencies, &[]);
        assert_eq!(result.exit_code, exit);
        assert!(!result.document.contains("private-"));
        let envelope: Value = serde_json::from_str(&result.document).unwrap();
        assert_eq!(envelope["ok"], false);
        if exit == 10 {
            assert_eq!(
                envelope["next_actions"][0]["command"],
                "flea tori auth login"
            );
        }
    }
    for error in [
        HttpError::InvalidRequest,
        HttpError::ResponseTooLarge,
        HttpError::Transport(TransportError::request(TransportErrorKind::Timeout)),
    ] {
        let (dependencies, _) = setup(vec![Err(error)]);
        assert_eq!(run(&dependencies, &[]).exit_code, 40);
    }
    let (dependencies, _) = setup(vec![Ok(HttpResponse {
        status: StatusCode::OK,
        headers: Default::default(),
        body: b"private-invalid-json".to_vec(),
    })]);
    let result = run(&dependencies, &[]);
    assert_eq!(result.exit_code, 40);
    assert!(!result.document.contains("private-"));
}

#[test]
fn invalid_options_and_foreign_market_options_do_not_make_requests() {
    let (dependencies, client) = setup(vec![]);
    for options in [
        ["--offset", "-1"],
        ["--offset", "2147483648"],
        ["--limit", "0"],
        ["--limit", "2147483648"],
        ["--limit", "abc"],
        ["--page", "1"],
        ["--status", "completed"],
        ["--portal", "fi"],
    ] {
        assert_eq!(run(&dependencies, &options).exit_code, 2, "{options:?}");
    }
    assert!(client.calls.lock().unwrap().is_empty());
}
