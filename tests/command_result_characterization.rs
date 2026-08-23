use flea::{
    Presentation,
    cli::{Command, CommandFuture, CommandRuntime},
    run_with_runtime,
};
use serde_json::{Value, json};

struct MetadataRuntime;

impl CommandRuntime for MetadataRuntime {
    fn execute(&self, _command: Command) -> CommandFuture<'_> {
        Box::pin(async {
            Ok(json!({
                "result": "kept",
                "_next_actions": [{ "command": "flea marketplaces" }],
                "_observation": {
                    "state": "confirmed_present",
                    "source": "fixture",
                    "observed_at": "2026-01-02T03:04:05Z",
                    "status_evidence": {
                        "http_status": 200,
                        "response_received": true,
                        "model_parsed": true
                    }
                }
            }))
        })
    }
}

struct WarningRuntime;

impl CommandRuntime for WarningRuntime {
    fn execute(&self, _command: Command) -> CommandFuture<'_> {
        Box::pin(async {
            Ok(json!({
                "warnings": [
                    "Tori returned an unrecognized successful mutation response: fixture",
                    "Tori returned an ambiguous mutation response: fixture",
                    "confirmation tracking failed: fixture"
                ]
            }))
        })
    }
}

struct InvalidAuthRuntime;

impl CommandRuntime for InvalidAuthRuntime {
    fn execute(&self, _command: Command) -> CommandFuture<'_> {
        Box::pin(async { Ok(json!({ "authenticated": false })) })
    }
}

fn structured(runtime: &dyn CommandRuntime, format: &str) -> flea::RunResult {
    run_with_runtime(
        ["flea", "tori", "capabilities", "--format", format],
        runtime,
    )
}

#[test]
fn reserved_metadata_fields_produce_the_exact_json_envelope() {
    let result = structured(&MetadataRuntime, "json");

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::Structured);
    assert_eq!(
        serde_json::from_str::<Value>(&result.document).unwrap(),
        json!({
            "ok": true,
            "context": { "marketplace": "tori", "portal": "fi" },
            "data": { "result": "kept" },
            "observation": {
                "state": "confirmed_present",
                "source": "fixture",
                "observed_at": "2026-01-02T03:04:05Z",
                "status_evidence": {
                    "http_status": 200,
                    "response_received": true,
                    "model_parsed": true
                }
            },
            "next_actions": [{ "command": "flea marketplaces" }]
        })
    );
}

#[test]
fn reserved_metadata_fields_produce_the_exact_toon_envelope() {
    let result = structured(&MetadataRuntime, "toon");

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::Structured);
    assert_eq!(
        toon_format::decode_default::<Value>(&result.document).unwrap(),
        json!({
            "ok": true,
            "context": { "marketplace": "tori", "portal": "fi" },
            "data": { "result": "kept" },
            "observation": {
                "state": "confirmed_present",
                "source": "fixture",
                "observed_at": "2026-01-02T03:04:05Z",
                "status_evidence": {
                    "http_status": 200,
                    "response_received": true,
                    "model_parsed": true
                }
            },
            "next_actions": [{ "command": "flea marketplaces" }]
        })
    );
}

#[test]
fn warning_messages_map_to_the_exact_envelope_codes() {
    let result = structured(&WarningRuntime, "json");
    let envelope: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(
        envelope["warnings"],
        json!([
            {
                "code": "mutation.response_model_drift",
                "message": "Tori returned an unrecognized successful mutation response: fixture"
            },
            {
                "code": "mutation.observed_success",
                "message": "Tori returned an ambiguous mutation response: fixture"
            },
            {
                "code": "workflow.best_effort_failed",
                "message": "confirmation tracking failed: fixture"
            }
        ])
    );
    assert_eq!(
        envelope["data"]["warnings"],
        json!([
            "Tori returned an unrecognized successful mutation response: fixture",
            "Tori returned an ambiguous mutation response: fixture",
            "confirmation tracking failed: fixture"
        ])
    );
}

#[test]
fn invalid_plain_auth_output_falls_back_to_the_exact_structured_failure() {
    let result = run_with_runtime(
        ["flea", "tori", "auth", "login", "--format", "toon"],
        &InvalidAuthRuntime,
    );

    assert_eq!(result.exit_code, 40);
    assert_eq!(result.presentation, Presentation::Structured);
    assert_eq!(
        result.document,
        "ok: false\ncontext:\n  marketplace: tori\n  portal: fi\nerror:\n  code: output.serialization_failed\n  message: authentication login output has an invalid status\n  upstream_transient: false\n  safe_to_retry: false"
    );
}
