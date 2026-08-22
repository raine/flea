use serde_json::{Value, json};
use tori::{
    domain::envelope::{Diagnostics, Envelope, NextAction},
    error::{AppError, ExitClass},
    output::{OutputFormat, render},
};

#[test]
fn success_envelope_matches_json_and_toon_snapshots() {
    let mut envelope = Envelope::success(json!({
        "draft_id": "36443414",
        "state": "draft",
        "fields": [{ "key": "title", "status": "set", "value": "Chair" }]
    }));
    envelope.next_actions.push(NextAction {
        command: "tori draft show 36443414".to_owned(),
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
        "results":[{
            "listing_id":"42346404", "title":"Baden tuoli",
            "price":{"amount":37,"currency":"EUR","display":"37 €"},
            "location":"Helsinki, Uusimaa", "distance":1200.0,
            "url":"https://www.tori.fi/recommerce/forsale/item/42346404",
            "image_urls":["https://img.tori.net/item/one"],
            "published_at_ms":1787394216000_i64, "labels":[], "flags":["private"], "extras":[]
        }],
        "pagination":{"page":1,"limit":20,"returned":1,"total":1200,"total_pages":60,"accessible_pages":50,"upstream_page_limit":50,"capped":true,"has_previous":false,"has_next":true,"next_page":2},
        "applied_filters":[], "facets":[],
        "resolved_location":{"id":"1.100018.110091","name":"Helsinki","parent":"Uusimaa","depth":1}
    }));
    envelope.next_actions.push(NextAction {
        command: "tori search 'tuoli' --page 2 --limit 20".to_owned(),
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
        log_path: "/state/tori-cli/logs/tori-cli.fixture.jsonl".to_owned(),
    }));
    error.next_actions.push(NextAction {
        command: "tori draft update 36443414 --input PATH".to_owned(),
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
        log_path: "/state/tori-cli/logs/tori-cli.jsonl".to_owned(),
    }));
    error.next_actions.push(NextAction {
        command: "tori draft show draft-1".to_owned(),
    });

    let envelope = Envelope::failure(error);
    let decoded = serde_json::to_value(envelope).expect("envelope should serialize");

    assert_eq!(decoded["partial"]["draft_id"], "draft-1");
    assert_eq!(decoded["diagnostics"]["trace_id"], "trace-1");
    assert_eq!(decoded["diagnostics"]["correlation_id"], "correlation-1");
    assert_eq!(
        decoded["next_actions"][0]["command"],
        "tori draft show draft-1"
    );
}

fn assert_snapshot(actual: String, expected: &str) {
    assert_eq!(actual, expected.trim_end());
}

#[test]
fn syntax_failures_use_clap_presentation() {
    let result = tori::run(["tori", "--format", "json", "draft", "show"]);

    assert_eq!(result.exit_code, 2);
    assert_eq!(result.presentation, tori::Presentation::PlainStderr);
    assert!(result.document.contains("Usage: tori draft show"));
    assert!(!result.document.contains("cli.invalid_usage"));
}
