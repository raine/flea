use std::{future::Future, pin::Pin, sync::Arc};

use flea::{
    AppError, PortalId,
    dependencies::{
        ApplicationDependencies, DiscoveryRequest, VintedCredentialRecord,
        VintedPublicationDiscoveryApi,
    },
};
use serde_json::{Value, json};

fn run_json(args: &[&str]) -> (u8, Value) {
    let arguments = std::iter::once("flea")
        .chain(["--format", "json"])
        .chain(args.iter().copied());
    let result = flea::run(arguments);
    let document = serde_json::from_str(&result.document).expect("JSON envelope");
    (result.exit_code, document)
}

struct DiscoveryFixture;

impl VintedPublicationDiscoveryApi for DiscoveryFixture {
    fn execute<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        request: &'a DiscoveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(match request {
                DiscoveryRequest::SearchCatalog { .. } => json!({"catalog_ids":[4380]}),
                DiscoveryRequest::Catalogs => json!({"catalogs":[{
                    "id":10,"title":"Cycling","catalogs":[{
                        "id":4380,"title":"Locks","catalogs":[]
                    }]
                }]}),
                DiscoveryRequest::Attributes { .. } => json!({"attributes":[{
                    "code":"condition","title":"Condition",
                    "required":true,"values":[{"id":6,"title":"Good"}]
                }]}),
                DiscoveryRequest::Brands { keyword, .. } if keyword == "Vibram Fivefingers" => {
                    json!({"brands":[{"id":123456,"title":"Vibram Fivefingers"}]})
                }
                DiscoveryRequest::Brands { .. } => json!({"brands":[{"id":22,"title":"Abus"}]}),
                DiscoveryRequest::Colors => json!({"colors":[{"id":3,"title":"Black"}]}),
                DiscoveryRequest::Configuration => json!({
                    "currencies":["EUR"],"minimum_price":"1.00","maximum_price":"10000.00"
                }),
                DiscoveryRequest::PackageSizes { .. } => {
                    json!({"package_sizes":[{"id":1,"title":"Small"}]})
                }
            })
        })
    }
}

fn discovery_dependencies() -> ApplicationDependencies {
    ApplicationDependencies::production()
        .with_vinted_credentials_provider(|_| {
            Ok(VintedCredentialRecord::new_for_adapter(
                PortalId::Fi,
                "fixture-user".into(),
                None,
                "fixture-access".into(),
                "fixture-refresh".into(),
                u64::MAX,
                "fixture-device".into(),
                "fixture-anonymous".into(),
                None,
            ))
        })
        .with_vinted_publication_discovery_api(Arc::new(DiscoveryFixture))
}

fn run_discovery_json(args: &[&str]) -> Value {
    let arguments = std::iter::once("flea")
        .chain(["--format", "json"])
        .chain(args.iter().copied());
    let result = flea::run_with_dependencies(arguments, &discovery_dependencies());
    assert_eq!(result.exit_code, 0, "{}", result.document);
    serde_json::from_str(&result.document).expect("JSON envelope")
}

#[test]
fn vinted_publication_discovery_guides_the_category_and_attribute_chain() {
    let search = run_discovery_json(&["vinted", "category", "search", "lukot"]);
    assert_eq!(search["data"]["scope"], "portal");
    assert_eq!(search["data"]["categories"][0]["id"], 4380);
    assert_eq!(
        search["next_actions"][0]["command"],
        "flea vinted --portal fi category compose 4380"
    );

    let compose = run_discovery_json(&["vinted", "category", "compose", "4380"]);
    assert_eq!(compose["data"]["scope"], "category");
    assert_eq!(
        compose["data"]["attribute_selection_payload"],
        json!([{"code":"category","value":[4380]}])
    );
    assert!(compose["data"]["form"]["options"].is_array());
    assert!(
        compose["data"]["issue_actions"]
            .as_array()
            .is_some_and(|actions| {
                actions.iter().any(|action| {
                    action["field"] == "attribute.condition"
                        && action["command"].as_str().is_some_and(|command| {
                            command.contains(r#"[{"code":"category","value":[4380]}]"#)
                        })
                })
            })
    );

    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), r#"[{"code":"category","value":[4380]}]"#).unwrap();
    let attributes = run_discovery_json(&[
        "vinted",
        "category",
        "attributes",
        "--input",
        file.path().to_str().unwrap(),
    ]);
    assert_eq!(attributes["data"]["scope"], "selection");
    assert_eq!(
        attributes["data"]["selection_payload"],
        json!([{"code":"category","value":[4380]}])
    );
    assert!(
        attributes["next_actions"][0]["command"]
            .as_str()
            .unwrap()
            .contains(r#"{"code":"condition","value":[6]}"#)
    );
}

#[test]
fn composer_readiness_omits_discovery_catalogs_and_reports_validation() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        serde_json::to_vec(&json!({
            "title":"Lock", "description":"Steel lock", "catalog_id":4380,
            "price":"10.00", "currency":"EUR", "package_size_id":1,
            "brand_id":22, "brand":"Abus", "color_ids":[3],
            "item_attributes":[{"code":"condition","ids":[6]}]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run_discovery_json(&[
        "vinted",
        "category",
        "compose",
        "4380",
        "--input",
        file.path().to_str().unwrap(),
        "--readiness",
    ]);

    assert_eq!(output["data"]["ready"], true);
    assert_eq!(output["data"]["selected_values"]["brand"]["brand_id"], 22);
    assert_eq!(output["data"]["brand_validation"]["valid"], true);
    assert_eq!(output["data"]["issues"], json!([]));
    assert!(output["data"].get("form").is_none());
    assert!(output["data"].get("attribute_selection_payload").is_none());
    assert!(output["data"].get("listing_input").is_none());

    let incomplete = run_discovery_json(&["vinted", "category", "compose", "4380", "--readiness"]);
    assert_eq!(incomplete["data"]["ready"], false);
    assert!(incomplete["data"]["issues"].as_array().unwrap().len() > 1);
    assert!(
        incomplete["data"]["next_actions"]
            .as_array()
            .is_some_and(|actions| !actions.is_empty())
    );
    assert!(
        incomplete["next_actions"]
            .as_array()
            .is_some_and(|actions| !actions.is_empty())
    );
}

#[test]
fn composer_links_supplied_brand_to_focused_category_discovery() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), r#"{"brand":"Vibram Fivefingers"}"#).unwrap();
    let compose = run_discovery_json(&[
        "vinted",
        "category",
        "compose",
        "4380",
        "--input",
        file.path().to_str().unwrap(),
    ]);
    assert_eq!(
        compose["next_actions"][0]["command"],
        "flea vinted category brands 4380 'Vibram Fivefingers'"
    );

    let brands =
        run_discovery_json(&["vinted", "category", "brands", "4380", "Vibram Fivefingers"]);
    assert_eq!(brands["data"]["response"]["brands"][0]["id"], 123456);
    assert_eq!(
        brands["data"]["response"]["brands"][0]["title"],
        "Vibram Fivefingers"
    );

    std::fs::write(
        file.path(),
        r#"{
            "title":"Shoes","description":"Minimal running shoes",
            "catalog_id":4380,"price":"25.00","currency":"EUR",
            "package_size_id":1,"brand_id":123456,
            "brand":"Vibram Fivefingers","color_ids":[3],
            "item_attributes":[{"code":"condition","ids":[6]}]
        }"#,
    )
    .unwrap();
    let resolved = run_discovery_json(&[
        "vinted",
        "category",
        "compose",
        "4380",
        "--input",
        file.path().to_str().unwrap(),
    ]);
    assert_eq!(resolved["data"]["brand_validation"]["status"], "searched");
    assert!(
        resolved["data"]["brand_validation"]["valid"]
            .as_bool()
            .unwrap()
    );
    assert_eq!(resolved["data"]["listing_input"]["brand_id"], 123456);
    assert_eq!(
        resolved["data"]["listing_input"]["brand"],
        "Vibram Fivefingers"
    );
}

#[test]
fn discovery_surfaces_report_their_authoritative_scopes() {
    for (args, scope) in [
        (vec!["vinted", "category", "brands", "4380"], "category"),
        (
            vec!["vinted", "category", "package-sizes", "4380"],
            "category",
        ),
        (vec!["vinted", "category", "colors"], "portal"),
        (vec!["vinted", "category", "configuration"], "account"),
    ] {
        let output = run_discovery_json(&args);
        assert_eq!(output["data"]["scope"], scope);
    }
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
        capability["name"] == "search"
            && capability["auth"] == "required"
            && capability["maturity"] == "source_derived"
    }));
    assert!(capabilities.iter().any(|capability| {
        capability["name"] == "item.show"
            && capability["auth"] == "required"
            && capability["maturity"] == "validated"
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

    let (exit_code, invalid_item) = run_json(&["vinted", "item", "show", "../123"]);
    assert_eq!(exit_code, 20);
    assert_eq!(invalid_item["context"]["marketplace"], "vinted");
    assert_eq!(invalid_item["error"]["code"], "vinted_item.invalid_id");
    assert_eq!(
        invalid_item["next_actions"][0]["command"],
        "flea vinted --portal fi search"
    );
}
