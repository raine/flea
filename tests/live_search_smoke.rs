use std::process::Command;

use serde_json::Value;

const LIVE_OPT_IN: &str = "TORI_LIVE_SEARCH_SMOKE";
const LIVE_QUERY: &str = "TORI_LIVE_SEARCH_QUERY";

#[test]
#[ignore = "requires TORI_LIVE_SEARCH_SMOKE=1 and explicit read-only query configuration"]
fn public_search_is_read_only_and_does_not_require_authentication() {
    assert_eq!(
        std::env::var(LIVE_OPT_IN).as_deref(),
        Ok("1"),
        "set {LIVE_OPT_IN}=1 to acknowledge a read-only public request"
    );
    let query = std::env::var(LIVE_QUERY)
        .expect("TORI_LIVE_SEARCH_QUERY must contain a harmless marketplace query");
    let output = Command::new(env!("CARGO_BIN_EXE_tori"))
        .args(["search", &query, "--limit", "1", "--format", "json"])
        .env("XDG_STATE_HOME", tempfile::tempdir().unwrap().path())
        .output()
        .expect("tori executable should run");
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "tori returned invalid JSON: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });

    assert_eq!(envelope["ok"], true, "public search failed: {envelope}");
    assert_eq!(envelope["data"]["pagination"]["limit"], 1);
    assert!(envelope["data"]["results"].is_array());
}
