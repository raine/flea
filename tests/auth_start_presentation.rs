use std::{fs, process::Command};

use serde_json::{Value, json};
use tempfile::tempdir;
use tori::{Presentation, cli::CommandRuntime, error::AppError};

const LOGIN_URL: &str = "https://login.vend.fi/oauth/authorize?client_id=client&state=oauth-state";
const COMPLETION_COMMAND: &str = "tori auth complete flow-1";

struct AuthStartRuntime;

impl CommandRuntime for AuthStartRuntime {
    fn execute(&self, _command: tori::cli::Command) -> Result<Value, AppError> {
        Ok(json!({
            "flow_id": "flow-1",
            "login_url": LOGIN_URL,
            "expires_at_unix": 1_700_000_600_u64,
            "completion_command": COMPLETION_COMMAND
        }))
    }
}

#[test]
fn default_auth_start_matches_plain_text_snapshot() {
    let result = tori::run_with_runtime(["tori", "auth", "start"], &AuthStartRuntime);

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::PlainStdout);
    assert_eq!(result.document, include_str!("snapshots/auth-start.txt"));
    for structured_syntax in ["ok:", "data:", "warnings", "next_actions", "[0]{"] {
        assert!(
            !result.document.contains(structured_syntax),
            "plain auth output contains {structured_syntax:?}"
        );
    }
}

#[test]
fn explicit_json_auth_start_keeps_the_structured_envelope() {
    let result = tori::run_with_runtime(
        ["tori", "auth", "start", "--format", "json"],
        &AuthStartRuntime,
    );

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.presentation, Presentation::Structured);
    let envelope: Value = serde_json::from_str(&result.document).expect("valid JSON envelope");
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["login_url"], LOGIN_URL);
    assert_eq!(envelope["data"]["completion_command"], COMPLETION_COMMAND);
}

#[test]
fn binary_default_stdout_is_plain_and_json_is_machine_readable() {
    let state = tempdir().expect("temporary state directory");
    let plain = invoke_auth_start(state.path(), &[]);
    assert!(plain.status.success());
    assert!(plain.stderr.is_empty());
    let plain_stdout = String::from_utf8(plain.stdout).expect("UTF-8 stdout");
    assert!(plain_stdout.contains("Sign in to Tori"));
    assert!(plain_stdout.contains("https://login.vend.fi/oauth/authorize?"));
    assert!(plain_stdout.contains("Open ToriAuthHelper.app"));
    assert!(plain_stdout.contains("tori auth complete"));
    assert!(!plain_stdout.contains("CALLBACK_URL"));
    assert!(!plain_stdout.contains("ok:"));
    assert!(!plain_stdout.contains("next_actions"));
    assert!(!plain_stdout.contains("[0]{"));

    let json_output = invoke_auth_start(state.path(), &["--format", "json"]);
    assert!(json_output.status.success());
    assert!(json_output.stderr.is_empty());
    let envelope: Value =
        serde_json::from_slice(&json_output.stdout).expect("machine-readable JSON stdout");
    assert_eq!(envelope["ok"], true);
    assert!(
        envelope["data"]["login_url"]
            .as_str()
            .is_some_and(|url| url.starts_with("https://login.vend.fi/oauth/authorize?"))
    );

    let logs = fs::read_to_string(state.path().join("tori-cli/logs/tori-cli.jsonl"))
        .expect("diagnostic log");
    assert!(!logs.contains("login.vend.fi/oauth/authorize"));
}

fn invoke_auth_start(state: &std::path::Path, extra_args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tori"))
        .args(["auth", "start"])
        .args(extra_args)
        .env("XDG_STATE_HOME", state)
        .output()
        .expect("tori auth start should run")
}
