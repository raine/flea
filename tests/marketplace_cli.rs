use serde_json::Value;

fn run_json(args: &[&str]) -> (u8, Value) {
    let arguments = std::iter::once("flea")
        .chain(["--format", "json"])
        .chain(args.iter().copied());
    let result = flea::run(arguments);
    let document = serde_json::from_str(&result.document).expect("JSON envelope");
    (result.exit_code, document)
}

#[test]
fn capability_discovery_is_offline_and_marketplace_scoped() {
    let (exit_code, all) = run_json(&["capabilities"]);
    assert_eq!(exit_code, 0);
    assert!(all.get("context").is_none());
    assert_eq!(all["data"]["marketplaces"][0]["marketplace"], "tori");
    assert_eq!(all["data"]["marketplaces"][1]["marketplace"], "vinted");

    let (exit_code, vinted) = run_json(&["vinted", "--portal", "fi", "capabilities"]);
    assert_eq!(exit_code, 0);
    assert_eq!(vinted["context"]["marketplace"], "vinted");
    assert_eq!(vinted["context"]["portal"], "fi");
    let capabilities = vinted["data"]["capabilities"]
        .as_array()
        .expect("capability list");
    assert!(capabilities.iter().any(|capability| {
        capability["name"] == "auth.login" && capability["maturity"] == "validated"
    }));
    assert!(capabilities.iter().any(|capability| {
        capability["name"] == "search" && capability["maturity"] == "unavailable"
    }));
}

#[test]
fn unsupported_commands_return_structured_marketplace_errors() {
    let (exit_code, missing_marketplace) = run_json(&["search", "chair"]);
    assert_eq!(exit_code, 2);
    assert_eq!(missing_marketplace["error"]["code"], "marketplace.required");
    assert!(missing_marketplace.get("context").is_none());
    assert_eq!(
        missing_marketplace["next_actions"][0]["command"],
        "flea marketplaces"
    );

    let (exit_code, unavailable) = run_json(&["vinted", "search", "chair"]);
    assert_eq!(exit_code, 2);
    assert_eq!(unavailable["context"]["marketplace"], "vinted");
    assert_eq!(
        unavailable["error"]["code"],
        "marketplace.capability_unavailable"
    );
    assert_eq!(unavailable["error"]["details"]["command"], "search");
    assert_eq!(
        unavailable["next_actions"][0]["command"],
        "flea vinted --portal fi capabilities"
    );
}
