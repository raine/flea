use flea::{
    Presentation,
    cli::{
        outcome::{CommandData, CommandOutcome},
        runtime::ApplicationDependencies,
    },
    domain::{
        envelope::{NextAction, Warning},
        observation::Observation,
    },
    run_with_dependencies,
};
use serde_json::{Value, json};

fn metadata_dependencies() -> ApplicationDependencies {
    ApplicationDependencies::production().with_tori_auth_handler(|_| async {
        let mut observation = Observation::confirmed_present("fixture", Some(200));
        observation.observed_at = "2026-01-02T03:04:05Z".to_owned();
        Ok(
            CommandOutcome::new(CommandData::Raw(json!({ "result": "kept" })))
                .with_next_actions(vec![NextAction {
                    command: "flea marketplaces".to_owned(),
                }])
                .with_observation(observation),
        )
    })
}

fn warning_dependencies() -> ApplicationDependencies {
    ApplicationDependencies::production().with_tori_auth_handler(|_| async {
        let messages = [
            (
                "mutation.response_model_drift",
                "Tori returned an unrecognized successful mutation response: fixture",
            ),
            (
                "mutation.observed_success",
                "Tori returned an ambiguous mutation response: fixture",
            ),
            (
                "workflow.best_effort_failed",
                "confirmation tracking failed: fixture",
            ),
        ];
        Ok(CommandOutcome::new(CommandData::Raw(json!({
            "warnings": messages.map(|(_, message)| message)
        })))
        .with_warnings(
            messages
                .map(|(code, message)| Warning {
                    code: code.to_owned(),
                    message: message.to_owned(),
                })
                .into_iter()
                .collect(),
        ))
    })
}

fn invalid_auth_dependencies() -> ApplicationDependencies {
    ApplicationDependencies::production().with_tori_auth_handler(|_| async {
        Ok(CommandOutcome::new(CommandData::Raw(
            json!({ "authenticated": false }),
        )))
    })
}

fn structured(dependencies: &ApplicationDependencies, format: &str) -> flea::RunResult {
    run_with_dependencies(
        ["flea", "tori", "auth", "status", "--format", format],
        dependencies,
    )
}

#[test]
fn typed_metadata_produces_the_exact_json_envelope() {
    let result = structured(&metadata_dependencies(), "json");

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
fn typed_metadata_produces_the_exact_toon_envelope() {
    let result = structured(&metadata_dependencies(), "toon");

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
fn typed_warnings_produce_the_exact_envelope_codes() {
    let result = structured(&warning_dependencies(), "json");
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
    let result = run_with_dependencies(
        ["flea", "tori", "auth", "login", "--format", "toon"],
        &invalid_auth_dependencies(),
    );

    assert_eq!(result.exit_code, 40);
    assert_eq!(result.presentation, Presentation::Structured);
    assert_eq!(
        result.document,
        "ok: false\ncontext:\n  marketplace: tori\n  portal: fi\nerror:\n  code: output.serialization_failed\n  message: authentication login output has an invalid status\n  upstream_transient: false\n  safe_to_retry: false"
    );
}
