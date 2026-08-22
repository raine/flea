use serde_json::{Value, json};
use tori::{Presentation, cli::CommandRuntime, error::AppError};

struct AuthLoginRuntime;

impl CommandRuntime for AuthLoginRuntime {
    fn execute(&self, _command: tori::cli::Command) -> Result<Value, AppError> {
        Ok(json!({ "authenticated": true, "user_id": "42" }))
    }
}

#[test]
fn default_auth_login_reports_human_success() {
    let result = tori::run_with_runtime(["tori", "auth", "login"], &AuthLoginRuntime);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::PlainStdout);
    assert_eq!(result.document, "Signed in to Tori.\n");
}

#[test]
fn explicit_json_auth_login_keeps_the_structured_envelope() {
    let result = tori::run_with_runtime(
        ["tori", "auth", "login", "--format", "json"],
        &AuthLoginRuntime,
    );

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::Structured);
    let envelope: Value = serde_json::from_str(&result.document).expect("valid JSON envelope");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["authenticated"], true);
    assert!(envelope.get("warnings").is_none());
    assert!(envelope.get("next_actions").is_none());
}
