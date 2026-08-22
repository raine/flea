use serde_json::{Value, json};
use tori::{
    domain::envelope::Envelope,
    error::{AppError, ExitClass},
    output::{OutputFormat, render},
};

#[test]
fn renders_success_envelope_as_json() {
    let envelope = Envelope::success(json!({
        "draft_id": "36443414",
        "state": "draft"
    }));

    let document = render(&envelope, OutputFormat::Json).expect("JSON should render");
    let decoded: Value = serde_json::from_str(&document).expect("JSON should parse");

    assert_eq!(decoded["ok"], true);
    assert_eq!(decoded["data"]["draft_id"], "36443414");
    assert_eq!(decoded["warnings"], json!([]));
    assert_eq!(decoded["next_actions"], json!([]));
}

#[test]
fn renders_error_envelope_as_toon() {
    let mut error = AppError::new(
        "draft.validation_failed",
        "Required fields are missing",
        ExitClass::Validation,
    );
    error.details = Some(json!({ "missing_fields": ["title", "delivery"] }));
    let envelope = Envelope::failure(error);

    let document = render(&envelope, OutputFormat::Toon).expect("TOON should render");
    let decoded: Value = toon_format::decode_default(&document).expect("TOON should parse");

    assert_eq!(decoded["ok"], false);
    assert_eq!(decoded["error"]["code"], "draft.validation_failed");
    assert_eq!(
        decoded["error"]["details"]["missing_fields"],
        json!(["title", "delivery"])
    );
}

#[test]
fn syntax_failures_use_the_requested_output_format() {
    let result = tori::run(["tori", "--format", "json", "draft", "show"]);
    let decoded: Value = serde_json::from_str(&result.document).expect("JSON should parse");

    assert_eq!(result.exit_code, 2);
    assert_eq!(decoded["ok"], false);
    assert_eq!(decoded["error"]["code"], "cli.invalid_usage");
}
