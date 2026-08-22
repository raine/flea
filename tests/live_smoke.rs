use std::process::Command;

use serde_json::{Value, json};

const LIVE_OPT_IN: &str = "TORI_LIVE_SMOKE";

#[test]
#[ignore = "requires an authenticated Tori account and TORI_LIVE_SMOKE=1"]
fn authenticated_draft_lifecycle_never_publishes() {
    assert_eq!(
        std::env::var(LIVE_OPT_IN).as_deref(),
        Ok("1"),
        "set {LIVE_OPT_IN}=1 to acknowledge remote draft mutations"
    );
    let category = std::env::var("TORI_LIVE_CATEGORY")
        .expect("TORI_LIVE_CATEGORY must contain a selectable category machine value");

    let auth = invoke(&["auth", "status"]);
    assert_eq!(auth["ok"], true, "auth status failed: {auth}");
    assert_eq!(
        auth["data"]["authenticated"], true,
        "live smoke requires a configured account: {auth}"
    );

    let categories = invoke(&["category", "list"]);
    assert_eq!(
        categories["ok"], true,
        "category discovery failed: {categories}"
    );
    assert!(
        categories["data"]["categories"]
            .as_array()
            .is_some_and(|values| !values.is_empty()),
        "category discovery returned no roots: {categories}"
    );

    let created = invoke(&[
        "draft",
        "create",
        "--category",
        &category,
        "--title",
        "tori-cli live smoke draft",
    ]);
    assert_eq!(created["ok"], true, "draft create failed: {created}");
    let draft_id = created["data"]["draft"]["draft_id"]
        .as_str()
        .or_else(|| created["data"]["draft_id"].as_str())
        .expect("draft create envelope must identify the remote draft")
        .to_owned();
    let cleanup = DraftCleanup::new(draft_id.clone());

    let shown = invoke(&["draft", "show", &draft_id]);
    assert_eq!(shown["ok"], true, "draft show failed: {shown}");

    let updated = invoke(&[
        "draft",
        "update",
        &draft_id,
        "--title",
        "tori-cli live smoke draft updated",
    ]);
    assert_eq!(updated["ok"], true, "draft update failed: {updated}");
    assert!(
        updated
            .to_string()
            .contains("tori-cli live smoke draft updated"),
        "updated draft did not return the replacement title: {updated}"
    );

    cleanup.delete();
}

fn invoke(arguments: &[&str]) -> Value {
    assert!(
        !arguments.contains(&"publish"),
        "the automated live harness must never publish"
    );
    let output = Command::new(env!("CARGO_BIN_EXE_tori"))
        .args(arguments)
        .args(["--format", "json"])
        .output()
        .expect("tori executable should run");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "tori returned invalid JSON: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

struct DraftCleanup {
    draft_id: String,
    deleted: bool,
}

impl DraftCleanup {
    fn new(draft_id: String) -> Self {
        Self {
            draft_id,
            deleted: false,
        }
    }

    fn delete(mut self) {
        let result = invoke(&["draft", "delete", &self.draft_id]);
        assert_eq!(result["ok"], true, "draft delete failed: {result}");
        self.deleted = true;
    }
}

impl Drop for DraftCleanup {
    fn drop(&mut self) {
        if self.deleted {
            return;
        }
        let result = invoke(&["draft", "delete", &self.draft_id]);
        if result["ok"] != json!(true) {
            eprintln!(
                "live smoke cleanup failed for draft {}: {result}",
                self.draft_id
            );
        }
    }
}
