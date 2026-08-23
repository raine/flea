use flea::{
    Presentation,
    cli::{
        outcome::{CommandData, CommandOutcome},
        runtime::ApplicationDependencies,
    },
};
use serde_json::{Value, json};

fn dependencies() -> ApplicationDependencies {
    ApplicationDependencies::production()
        .with_tori_auth_handler(|_| async {
            Ok(CommandOutcome::new(CommandData::Raw(
                json!({ "authenticated": true, "user_id": "42" }),
            )))
        })
        .with_vinted_auth_handler(|_, _| async {
            Ok(CommandOutcome::new(CommandData::Raw(json!({
                "authenticated": true,
                "user_id": "84"
            }))))
        })
}

#[test]
fn default_auth_login_reports_human_success() {
    let result = flea::run_with_dependencies(["flea", "tori", "auth", "login"], &dependencies());

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::PlainStdout);
    assert_eq!(result.document, "Signed in to Tori.\n");
}

#[test]
fn default_vinted_auth_login_reports_marketplace_specific_human_success() {
    let result = flea::run_with_dependencies(["flea", "vinted", "auth", "login"], &dependencies());

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::PlainStdout);
    assert_eq!(result.document, "Signed in to Vinted.\n");
}

#[test]
fn default_auth_status_keeps_the_structured_envelope() {
    let result = flea::run_with_dependencies(["flea", "tori", "auth", "status"], &dependencies());

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::Structured);
    assert_eq!(
        toon_format::decode_default::<Value>(&result.document).unwrap()["data"],
        json!({ "authenticated": true, "user_id": "42" })
    );
}

#[test]
fn explicit_json_auth_login_keeps_the_structured_envelope() {
    let result = flea::run_with_dependencies(
        ["flea", "tori", "auth", "login", "--format", "json"],
        &dependencies(),
    );

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::Structured);
    let envelope: Value = serde_json::from_str(&result.document).expect("valid JSON envelope");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["authenticated"], true);
    assert!(envelope.get("warnings").is_none());
    assert!(envelope.get("next_actions").is_none());
}
