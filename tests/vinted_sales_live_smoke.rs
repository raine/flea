use std::process::Command;

use serde_json::Value;

#[test]
#[ignore = "requires an authenticated Vinted account and explicit read-only consent"]
fn authenticated_sales_history_has_a_bounded_page_and_safe_identity() {
    assert_eq!(std::env::var("FLEA_LIVE_VINTED_SALES_SMOKE").as_deref(), Ok("1"), "set FLEA_LIVE_VINTED_SALES_SMOKE=1 to acknowledge a read-only account request");
    let output = Command::new(env!("CARGO_BIN_EXE_flea"))
        .args(["--format", "json", "vinted", "sales", "list", "--limit", "1"])
        .output().expect("flea executable should run");
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("valid CLI JSON");
    assert!(output.status.success(), "sales request must succeed; account output withheld");
    assert_eq!(envelope["ok"], true);
    let data = &envelope["data"];
    assert_eq!(data["status"], "completed");
    assert_eq!(data["pagination"]["current_page"], 1);
    let sales = data["sales"].as_array().expect("sales array");
    assert!(sales.len() <= 1);
    for sale in sales {
        assert!(sale["transaction_id"].is_string() || sale["transaction_id"].is_null());
        for excluded in ["listing_id", "canonical_url", "buyer", "address", "session"] {
            assert!(sale.get(excluded).is_none());
        }
    }
}
