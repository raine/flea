use flea::{
    domain::envelope::{Diagnostics, Envelope, NextAction},
    error::{AppError, ExitClass},
    output::{OutputFormat, render},
};
use serde_json::{Value, json};

#[test]
fn success_envelope_matches_json_and_toon_snapshots() {
    let mut envelope = Envelope::success(json!({
        "draft_id": "36443414",
        "state": "draft",
        "fields": [{ "key": "title", "status": "set", "value": "Chair" }]
    }));
    envelope.next_actions.push(NextAction {
        command: "flea draft show 36443414".to_owned(),
    });

    assert_snapshot(
        render(&envelope, OutputFormat::Json).unwrap(),
        include_str!("snapshots/envelope-success.json"),
    );
    assert_snapshot(
        render(&envelope, OutputFormat::Toon).unwrap(),
        include_str!("snapshots/envelope-success.toon"),
    );
}

#[test]
fn search_success_matches_json_and_toon_snapshots() {
    let mut envelope = Envelope::success(json!({
        "query":"tuoli",
        "location":{"id":"1.100018.110091","name":"Helsinki","parent":"Uusimaa"},
        "results":[{
            "listing_id":"42346404", "title":"Baden tuoli",
            "price":{"amount":37,"currency":"EUR"},
            "location":"Helsinki, Uusimaa",
            "url":"https://www.tori.fi/recommerce/forsale/item/42346404",
            "published_at":"2026-08-22T10:23:36Z", "image_count":1,
            "distance":1200.0, "condition":"Hyvä", "shipping":true, "seller":"private"
        }],
        "pagination":{"page":1,"limit":20,"returned":1,"total":1200,"has_next":true,"next_page":2}
    }));
    envelope.next_actions.push(NextAction {
        command: "flea search 'tuoli' --page 2 --limit 20".to_owned(),
    });

    assert_snapshot(
        render(&envelope, OutputFormat::Json).unwrap(),
        include_str!("snapshots/search-success.json"),
    );
    assert_snapshot(
        render(&envelope, OutputFormat::Toon).unwrap(),
        include_str!("snapshots/search-success.toon"),
    );
}

#[test]
fn failure_envelope_matches_json_and_toon_snapshots() {
    let mut error = AppError::new(
        "draft.validation_failed",
        "Required fields are missing",
        ExitClass::Validation,
    )
    .with_details(json!({ "missing_fields": ["title", "delivery"] }))
    .with_partial(json!({
        "draft_id": "36443414",
        "completed_steps": ["fetch_draft"]
    }));
    error.diagnostics = Some(Box::new(Diagnostics {
        trace_id: "trace-fixture".to_owned(),
        correlation_id: "correlation-fixture".to_owned(),
        log_path: "/state/flea/logs/flea.fixture.jsonl".to_owned(),
    }));
    error.next_actions.push(NextAction {
        command: "flea draft update 36443414 --input PATH".to_owned(),
    });
    let envelope = Envelope::failure(error);

    assert_snapshot(
        render(&envelope, OutputFormat::Json).unwrap(),
        include_str!("snapshots/envelope-failure.json"),
    );
    let toon = render(&envelope, OutputFormat::Toon).unwrap();
    assert_snapshot(
        toon.clone(),
        include_str!("snapshots/envelope-failure.toon"),
    );
    let decoded: Value = toon_format::decode_default(&toon).unwrap();
    assert_eq!(decoded["partial"]["draft_id"], "36443414");
    assert_eq!(decoded["diagnostics"]["trace_id"], "trace-fixture");
}

#[test]
fn failure_envelopes_preserve_diagnostics_and_partial_recovery() {
    let mut error = AppError::partial(
        "draft.publish_failed",
        "publication stopped after image upload",
        json!({ "draft_id": "draft-1", "completed_steps": ["images"] }),
    );
    error.diagnostics = Some(Box::new(Diagnostics {
        trace_id: "trace-1".to_owned(),
        correlation_id: "correlation-1".to_owned(),
        log_path: "/state/flea/logs/flea.jsonl".to_owned(),
    }));
    error.next_actions.push(NextAction {
        command: "flea draft show draft-1".to_owned(),
    });

    let envelope = Envelope::failure(error);
    let decoded = serde_json::to_value(envelope).expect("envelope should serialize");

    assert_eq!(decoded["partial"]["draft_id"], "draft-1");
    assert_eq!(decoded["diagnostics"]["trace_id"], "trace-1");
    assert_eq!(decoded["diagnostics"]["correlation_id"], "correlation-1");
    assert_eq!(
        decoded["next_actions"][0]["command"],
        "flea draft show draft-1"
    );
}

fn assert_snapshot(actual: String, expected: &str) {
    assert_eq!(actual, expected.trim_end());
}

#[test]
fn syntax_failures_use_clap_presentation() {
    let result = flea::run(["flea", "--format", "json", "draft", "show"]);

    assert_eq!(result.exit_code, 2);
    assert_eq!(result.presentation, flea::Presentation::PlainStderr);
    assert!(result.document.contains("Usage: flea draft show"));
    assert!(!result.document.contains("cli.invalid_usage"));
}
