use std::{future::Future, pin::Pin, sync::Arc};

use flea::{
    AppError, PortalId,
    dependencies::{
        ApplicationDependencies, CatalogueRequest, DiscoveryRequest, VintedCredentialRecord,
        VintedPublicationDiscoveryApi, VintedSearchApi,
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
                DiscoveryRequest::SearchCatalog { keyword } if keyword == "cycling item" => {
                    json!({"catalog_ids":[4380,4381]})
                }
                DiscoveryRequest::SearchCatalog { .. } => json!({"catalog_ids":[4380]}),
                DiscoveryRequest::Catalogs => json!({"catalogs":[{
                    "id":10,"title":"Cycling","catalogs":[
                        {"id":4380,"title":"Locks","catalogs":[]},
                        {"id":4381,"title":"Lights","catalogs":[]}
                    ]
                }]}),
                DiscoveryRequest::Attributes { .. } => {
                    let size_options = (0..1_500)
                        .map(|index| {
                            json!({
                                "id": index + 42,
                                "title": format!("Size {index}"),
                                "value": format!("eu_{}", index + 42)
                            })
                        })
                        .collect::<Vec<_>>();
                    json!({"attributes":[
                        {
                            "code":"condition",
                            "configuration":{
                                "title":"Condition","required":true,
                                "options":[{"id":6,"title":"Good","code":"good"}]
                            }
                        },
                        {
                            "code":"size",
                            "configuration":{
                                "title":"Size","required":true,
                                "options":size_options
                            }
                        }
                    ]})
                }
                DiscoveryRequest::Brands { keyword, .. } if keyword == "Vibram FiveFingers" => {
                    json!({
                        "brands":[{"id":123456,"title":"Vibram Fivefingers"}],
                        "disable_custom_brands":false
                    })
                }
                DiscoveryRequest::Brands { keyword, .. } if keyword == "ac me" => json!({
                    "brands":[
                        {"id":700,"title":"AC-ME"},
                        {"id":701,"title":"Acme"}
                    ],
                    "disable_custom_brands":false
                }),
                DiscoveryRequest::Brands { keyword, .. } if keyword == "Restricted label" => {
                    json!({"brands":[],"disable_custom_brands":true})
                }
                DiscoveryRequest::Brands { .. } => json!({
                    "brands":[{"id":22,"title":"Abus"}],
                    "disable_custom_brands":false
                }),
                DiscoveryRequest::Colors => {
                    json!({"colors":[{"id":3,"title":"Black","code":"black"}]})
                }
                DiscoveryRequest::Configuration => json!({
                    "currencies":["EUR"],"minimum_price":"1.00","maximum_price":"10000.00"
                }),
                DiscoveryRequest::PackageSizes { .. } => {
                    json!({"package_sizes":[{"id":1,"title":"Small","code":"small"}]})
                }
            })
        })
    }
}

struct UnavailableSearchFixture;

impl VintedSearchApi for UnavailableSearchFixture {
    fn execute<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        _request: &'a CatalogueRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(async { Err(AppError::unexpected("Fixture marketplace unavailable")) })
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
        .with_vinted_search_api(Arc::new(UnavailableSearchFixture))
}

fn run_discovery_result(args: &[&str]) -> flea::RunResult {
    let arguments = std::iter::once("flea")
        .chain(["--format", "json"])
        .chain(args.iter().copied());
    flea::run_with_dependencies(arguments, &discovery_dependencies())
}

fn run_discovery_document(format: &str, args: &[&str]) -> String {
    let arguments = std::iter::once("flea")
        .chain(["--format", format])
        .chain(args.iter().copied());
    let result = flea::run_with_dependencies(arguments, &discovery_dependencies());
    if result.exit_code != 0 {
        eprintln!("{}", result.document);
    }
    assert_eq!(result.exit_code, 0, "{}", result.document);
    result.document
}

fn run_discovery_json(args: &[&str]) -> Value {
    serde_json::from_str(&run_discovery_document("json", args)).expect("JSON envelope")
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

    let compose = run_discovery_json(&["vinted", "category", "compose", "4380", "--full"]);
    assert_eq!(compose["data"]["scope"], "category");
    assert_eq!(
        compose["data"]["attribute_selection_payload"],
        json!([{"code":"category","value":[4380]}])
    );
    for (field, option) in [("attribute.condition", 6), ("attribute.size", 42)] {
        assert!(
            compose["data"]["form"]["fields"]
                .as_array()
                .is_some_and(|fields| fields.iter().any(|candidate| {
                    candidate["key"] == field && candidate["requirement"] == "required"
                }))
        );
        assert!(
            compose["data"]["form"]["options"]
                .as_array()
                .is_some_and(|options| options.iter().any(|candidate| {
                    candidate["field"] == field && candidate["value"] == option
                }))
        );
    }
    assert!(
        compose["data"]["issue_actions"]
            .as_array()
            .is_some_and(|actions| {
                actions.iter().any(|action| {
                    action["field"] == "attribute.condition"
                        && action["command"].as_str().is_some_and(|command| {
                            command.contains(
                                r#"[{"code":"category","value":[4380]},{"code":"condition","value":[6]}]"#,
                            )
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
fn guided_sell_resolves_exact_runtime_values_into_a_non_mutating_proposal() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        serde_json::to_vec(&json!({
            "title":"Steel lock", "description":"Used steel bicycle lock",
            "price":"10.00", "category":"lock", "brand":"Abus",
            "colors":["Black"], "package_size":"Small",
            "condition":"Good", "size":"Size 0", "images":["front.jpg"]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run_discovery_json(&[
        "vinted",
        "sell",
        "--input",
        file.path().to_str().unwrap(),
        "--select",
        "category=4380",
    ]);

    assert_eq!(output["data"]["status"], "ready");
    assert_eq!(output["data"]["mutated"], false);
    assert_eq!(output["data"]["safe_to_retry"], true);
    assert_eq!(
        output["data"]["proposed_mutation"]["listing_input"]["catalog_id"],
        4380
    );
    assert_eq!(
        output["data"]["proposed_mutation"]["listing_input"]["item_attributes"],
        json!([
            {"code":"condition","ids":[6]},
            {"code":"size","ids":[42]}
        ])
    );
    assert_eq!(
        output["next_actions"][0]["command"],
        "flea vinted --portal fi publish --input listing.json --image 'front.jpg'"
    );
}

#[test]
fn guided_sell_requires_explicit_category_selection_for_multiple_runtime_leaves() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        serde_json::to_vec(&json!({
            "title":"Cycling item", "description":"Seller description",
            "price":"10.00", "category":"cycling item", "images":["front.jpg"]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run_discovery_json(&["vinted", "sell", "--input", file.path().to_str().unwrap()]);

    assert_eq!(output["data"]["status"], "needs_input");
    assert_eq!(output["data"]["ambiguities"][0]["field"], "category");
    assert_eq!(
        output["data"]["ambiguities"][0]["choices"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        output["data"]["ambiguities"][0]["choices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| {
                action["command"]
                    .as_str()
                    .unwrap()
                    .contains("--select 'category=")
            })
    );
}

#[test]
fn guided_sell_returns_scoped_resumable_choices_instead_of_guessing() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        serde_json::to_vec(&json!({
            "title":"Steel lock", "description":"Used steel bicycle lock",
            "price":10, "category":"lock", "brand":"Abus",
            "colors":["Black"], "package_size":"Small",
            "condition":"Used", "size":"Size 0", "images":["front.jpg"]
        }))
        .unwrap(),
    )
    .unwrap();

    let output = run_discovery_json(&[
        "vinted",
        "sell",
        "--input",
        file.path().to_str().unwrap(),
        "--select",
        "category=4380",
    ]);

    assert_eq!(output["data"]["status"], "needs_input");
    assert!(output["data"].get("proposed_mutation").is_none());
    let ambiguity = output["data"]["ambiguities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|ambiguity| ambiguity["field"] == "attribute.condition")
        .unwrap();
    assert_eq!(ambiguity["semantic_value"], "Used");
    assert_eq!(ambiguity["code"], "unmatched_semantic_value");
    assert!(
        output
            .get("next_actions")
            .is_none_or(|actions| actions.as_array().is_some_and(Vec::is_empty))
    );
    assert_eq!(ambiguity["choices"][0]["id"], 6);
    assert!(
        ambiguity["choices"][0]["command"]
            .as_str()
            .unwrap()
            .contains("--select 'attribute.condition=6'")
    );
    assert_eq!(output["data"]["mutated"], false);
    assert_eq!(output["data"]["safe_to_retry"], true);
}

#[test]
fn composer_defaults_to_concise_readiness_and_reports_validation() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        serde_json::to_vec(&json!({
            "title":"Lock", "description":"Steel lock", "catalog_id":4380,
            "price":"10.00", "currency":"EUR", "package_size_id":1,
            "brand_id":22, "brand":"Abus", "color_ids":[3],
            "item_attributes":[
                {"code":"condition","ids":[6]},
                {"code":"size","ids":[42]}
            ]
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
    ]);

    assert_eq!(output["data"]["ready"], true);
    assert_eq!(output["data"]["selected_values"]["brand"]["brand_id"], 22);
    assert_eq!(output["data"]["brand_validation"]["valid"], true);
    assert_eq!(output["data"]["issues"], json!([]));
    assert!(output["data"].get("form").is_none());
    assert!(output["data"].get("attribute_selection_payload").is_none());
    assert!(output["data"].get("listing_input").is_none());

    let incomplete = run_discovery_json(&["vinted", "category", "compose", "4380"]);
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
fn composer_resolves_semantic_values_from_their_live_scoped_catalogs() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        serde_json::to_vec(&json!({
            "title":"Lock", "description":"Steel lock", "catalog_id":4380,
            "price":"10.00", "currency":"EUR",
            "package_size":"small", "brand_id":22, "brand":"Abus",
            "colors":["black"], "condition":"good", "size":"eu_42"
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
        "--full",
    ]);

    assert_eq!(output["data"]["form"]["ready"], true);
    assert_eq!(output["data"]["listing_input"]["package_size_id"], 1);
    assert_eq!(output["data"]["listing_input"]["color_ids"], json!([3]));
    assert_eq!(
        output["data"]["listing_input"]["item_attributes"],
        json!([
            {"code":"size","ids":[42]},
            {"code":"condition","ids":[6]}
        ])
    );
    assert_eq!(
        output["data"]["semantic_resolutions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|resolution| resolution["scope"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["selection", "selection", "portal", "category"]
    );
    assert!(
        output["data"]["semantic_resolutions"]
            .as_array()
            .unwrap()
            .iter()
            .all(|resolution| resolution.get("id").is_none())
    );
}

#[test]
fn composer_returns_correction_actions_for_unavailable_semantic_values() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), r#"{"colors":["violet"]}"#).unwrap();
    let output = run_discovery_json(&[
        "vinted",
        "category",
        "compose",
        "4380",
        "--input",
        file.path().to_str().unwrap(),
    ]);

    assert_eq!(output["data"]["ready"], false);
    let issue = output["data"]["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["field"] == "color")
        .unwrap();
    assert_eq!(issue["code"], "semantic_unavailable");
    assert_eq!(issue["raw"]["scope"], "portal");
    assert!(
        output["data"]["next_actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|action| action["field"] == "color"
                && action["command"] == "flea vinted category colors")
    );
}

#[test]
fn direct_publication_stops_before_mutation_for_semantic_corrections() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        r#"{
            "title":"Lock","description":"Steel lock","catalog_id":4380,
            "price":"10.00","currency":"EUR","package_size":"small",
            "brand_id":22,"brand":"Abus","colors":["violet"],
            "condition":"good","size":"eu_42"
        }"#,
    )
    .unwrap();
    let result = run_discovery_result(&[
        "vinted",
        "publish",
        "--input",
        file.path().to_str().unwrap(),
        "--image",
        "/path/not/read-before-resolution.jpg",
    ]);
    let output: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 20);
    assert_eq!(
        output["error"]["code"],
        "vinted.listing_input_correction_required"
    );
    assert_eq!(output["error"]["safe_to_retry"], false);
    assert_eq!(output["error"]["details"]["issues"][0]["field"], "color");
    assert_eq!(
        output["next_actions"][0]["command"],
        "flea vinted category colors"
    );
}

#[test]
fn composer_keeps_large_catalogs_explicit_and_default_formats_equivalent() {
    let args = ["vinted", "category", "compose", "4380"];
    let json_document = run_discovery_document("json", &args);
    let json_output: Value = serde_json::from_str(&json_document).unwrap();

    assert!(
        json_document.len() < 20_000,
        "{len}",
        len = json_document.len()
    );
    assert_eq!(json_output["data"]["ready"], false);
    assert!(json_output["data"].get("form").is_none());
    assert!(
        json_output["data"]["issues"]
            .as_array()
            .is_some_and(|issues| issues
                .iter()
                .any(|issue| issue["field"] == "attribute.size"))
    );
    assert!(
        json_output["data"]["next_actions"]
            .as_array()
            .is_some_and(|actions| actions
                .iter()
                .any(|action| action["field"] == "attribute.size"))
    );
    assert!(
        json_output["next_actions"]
            .as_array()
            .is_some_and(|actions| !actions.is_empty())
    );

    let toon_document = run_discovery_document("toon", &args);
    let toon_output: Value = toon_format::decode_default(&toon_document).unwrap();
    assert_eq!(toon_output, json_output);

    let full_document =
        run_discovery_document("json", &["vinted", "category", "compose", "4380", "--full"]);
    let full_output: Value = serde_json::from_str(&full_document).unwrap();
    assert!(full_document.len() > 50_000);
    assert!(
        full_output["data"]["form"]["options"]
            .as_array()
            .is_some_and(|options| options
                .iter()
                .filter(|option| option["field"] == "attribute.size")
                .count()
                == 1_500)
    );
    assert!(
        full_output["data"]["issue_actions"]
            .as_array()
            .is_some_and(|actions| actions
                .iter()
                .any(|action| action["field"] == "attribute.size"))
    );
}

#[test]
fn composer_resolves_a_unique_normalized_brand_automatically() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        r#"{
            "title":"Shoes","description":"Minimal running shoes",
            "catalog_id":4380,"price":"25.00","currency":"EUR",
            "package_size_id":1,"brand":"Vibram FiveFingers","color_ids":[3],
            "item_attributes":[
                {"code":"condition","ids":[6]},
                {"code":"size","ids":[42]}
            ]
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
        "--full",
    ]);

    assert_eq!(resolved["data"]["brand_validation"]["status"], "resolved");
    assert_eq!(
        resolved["data"]["brand_validation"]["match_kind"],
        "normalized"
    );
    assert_eq!(resolved["data"]["listing_input"]["brand_id"], 123456);
    assert_eq!(
        resolved["data"]["listing_input"]["brand"],
        "Vibram Fivefingers"
    );
    assert!(resolved.get("next_actions").is_none());
}

#[test]
fn composer_preserves_ambiguous_and_custom_brand_policy_results() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), r#"{"brand":"ac me"}"#).unwrap();
    let ambiguous = run_discovery_json(&[
        "vinted",
        "category",
        "compose",
        "4380",
        "--input",
        file.path().to_str().unwrap(),
    ]);
    assert_eq!(ambiguous["data"]["brand_validation"]["status"], "ambiguous");
    assert_eq!(
        ambiguous["data"]["brand_validation"]["options"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let brand_action = ambiguous["data"]["next_actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|action| action["field"] == "brand")
        .unwrap();
    assert_eq!(
        brand_action["command"],
        "flea vinted category compose 4380 --input listing.json"
    );

    std::fs::write(file.path(), r#"{"brand":"Independent label"}"#).unwrap();
    let custom = run_discovery_json(&[
        "vinted",
        "category",
        "compose",
        "4380",
        "--input",
        file.path().to_str().unwrap(),
    ]);
    assert_eq!(custom["data"]["brand_validation"]["status"], "custom");
    assert_eq!(
        custom["data"]["selected_values"]["brand"],
        json!({"brand_id":null,"brand":"Independent label"})
    );

    std::fs::write(file.path(), r#"{"brand":"Restricted label"}"#).unwrap();
    let restricted = run_discovery_json(&[
        "vinted",
        "category",
        "compose",
        "4380",
        "--input",
        file.path().to_str().unwrap(),
    ]);
    assert_eq!(
        restricted["data"]["brand_validation"]["status"],
        "custom_disabled"
    );
    assert!(!restricted["data"]["ready"].as_bool().unwrap());
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

#[test]
fn guided_sell_singleton_hint_requires_selection_and_retains_warning_and_full_path() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(
        file.path(),
        serde_json::to_vec(&json!({
            "title":"Children shoes", "category":"unmatched phrase"
        }))
        .unwrap(),
    )
    .unwrap();
    let output = run_discovery_json(&[
        "vinted",
        "sell",
        "--marketplace-evidence",
        "--input",
        file.path().to_str().unwrap(),
    ]);
    assert_eq!(output["data"]["status"], "needs_input");
    assert_eq!(output["data"]["mutated"], false);
    assert_eq!(output["data"]["safe_to_retry"], true);
    assert!(output["data"].get("proposed_mutation").is_none());
    assert_eq!(
        output["data"]["ambiguities"][0]["choices"][0]["label"],
        "Cycling > Locks"
    );
    assert_eq!(
        output["data"]["category_discovery"]["selection_required"],
        true
    );
    assert_eq!(output["data"]["category_discovery"]["returned"], 1);
    assert_eq!(
        output["data"]["category_discovery"]["stages"]["marketplace"],
        "unavailable"
    );
    assert!(!output["warnings"].as_array().unwrap().is_empty());
}

#[test]
fn guided_sell_rejects_absent_and_nonleaf_selections() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"{}").unwrap();
    for selection in ["category=10", "category=99999"] {
        let output = run_discovery_result(&[
            "vinted",
            "sell",
            "--input",
            file.path().to_str().unwrap(),
            "--select",
            selection,
        ]);
        assert_ne!(output.exit_code, 0);
        let document: Value = serde_json::from_str(&output.document).unwrap();
        assert_eq!(
            document["error"]["code"],
            "vinted.guided_sell.category_unavailable"
        );
    }
}

#[test]
fn guided_sell_missing_category_offers_compact_roots() {
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"{}").unwrap();
    let output = run_discovery_json(&["vinted", "sell", "--input", file.path().to_str().unwrap()]);
    assert_eq!(output["data"]["status"], "needs_input");
    assert_eq!(
        output["next_actions"][0]["command"],
        "flea vinted --portal fi category list --roots"
    );
}
