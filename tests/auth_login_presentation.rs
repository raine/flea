use flea::{Presentation, cli::runtime::ApplicationDependencies};
use serde_json::{Value, json};

fn dependencies() -> ApplicationDependencies {
    ApplicationDependencies::production().with_tori_auth_handler(|_| async {
        Ok(json!({ "authenticated": true, "user_id": "42" }).into())
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
