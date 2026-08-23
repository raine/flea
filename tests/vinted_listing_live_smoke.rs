use std::process::Command;

use serde_json::Value;

#[test]
#[ignore = "requires an authenticated Vinted account and explicit publication expectations"]
fn publication_item_id_resolves_to_matching_remote_fields() {
    assert_eq!(
        std::env::var("FLEA_LIVE_VINTED_LISTING_SMOKE").as_deref(),
        Ok("1"),
        "set FLEA_LIVE_VINTED_LISTING_SMOKE=1 to acknowledge a read-only account request"
    );
    let item_id = required("FLEA_LIVE_VINTED_ITEM_ID");
    let expected_title = required("FLEA_LIVE_VINTED_TITLE");
    let expected_description = required("FLEA_LIVE_VINTED_DESCRIPTION");
    let expected_price = required("FLEA_LIVE_VINTED_PRICE");
    let expected_currency = required("FLEA_LIVE_VINTED_CURRENCY");

    let output = Command::new(env!("CARGO_BIN_EXE_flea"))
        .args(["--format", "json", "vinted", "listing", "show", &item_id])
        .output()
        .expect("flea executable should run");
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "flea returned invalid JSON: {error}; stderr: {}",
            String::from_utf8_lossy(&output.stderr)
        )
    });

    assert_eq!(envelope["ok"], true, "listing inspection failed");
    assert_eq!(envelope["data"]["listing_id"], item_id);
    assert_eq!(envelope["data"]["title"], expected_title);
    assert_eq!(envelope["data"]["description"], expected_description);
    let expected_price: Value =
        serde_json::from_str(&expected_price).expect("price must be a JSON number");
    assert_eq!(envelope["data"]["price"]["amount"], expected_price);
    assert_eq!(envelope["data"]["price"]["currency"], expected_currency);
    assert!(envelope["data"]["canonical_url"].is_string());
    assert!(envelope["data"]["photos"].is_array());
}

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must match the published listing"))
}
