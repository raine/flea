use std::{fs, future::Future, path::Path, pin::Pin, sync::Arc};

use flea::{
    AppError, PortalId,
    dependencies::{
        ApplicationDependencies, CatalogueRequest, DiscoveryRequest, VintedCredentialRecord,
        VintedPublicationDiscoveryApi, VintedSearchApi,
    },
};
use serde_json::{Value, json};

struct Discovery;

impl VintedPublicationDiscoveryApi for Discovery {
    fn execute<'a>(
        &'a self,
        _: &'a VintedCredentialRecord,
        request: &'a DiscoveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(match request {
                DiscoveryRequest::SearchCatalog { .. } => json!({"catalog_ids":[4380]}),
                DiscoveryRequest::Catalogs => {
                    json!({"catalogs":[{"id":4380,"title":"Locks","catalogs":[]}]})
                }
                DiscoveryRequest::Attributes { .. } => json!({"attributes":[{
                    "code":"condition", "configuration":{"title":"Condition","required":true,
                    "options":[{"id":6,"title":"Good","code":"good"}]}
                }]}),
                DiscoveryRequest::Brands { .. } => {
                    json!({"brands":[{"id":22,"title":"Abus"}],"disable_custom_brands":false})
                }
                DiscoveryRequest::Colors => {
                    json!({"colors":[{"id":3,"title":"Black","code":"black"}]})
                }
                DiscoveryRequest::Configuration => {
                    json!({"currencies":["EUR"],"minimum_price":"1.00","maximum_price":"10000.00"})
                }
                DiscoveryRequest::PackageSizes { .. } => {
                    json!({"package_sizes":[{"id":1,"title":"Small","code":"small"}]})
                }
            })
        })
    }
}

struct NoSearch;
impl VintedSearchApi for NoSearch {
    fn execute<'a>(
        &'a self,
        _: &'a VintedCredentialRecord,
        _: &'a CatalogueRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(async { panic!("export must not search marketplace listings") })
    }
}

fn run(input: &Path, output: Option<&Path>) -> (u8, Value) {
    let dependencies = ApplicationDependencies::production()
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
        .with_vinted_publication_discovery_api(Arc::new(Discovery))
        .with_vinted_search_api(Arc::new(NoSearch));
    let mut args = vec![
        "flea",
        "--format",
        "json",
        "vinted",
        "sell",
        "--input",
        input.to_str().unwrap(),
        "--select",
        "category=4380",
        "--image",
        "front 'one'.heic",
        "--image",
        "back two.png",
    ];
    if let Some(output) = output {
        args.extend(["--output", output.to_str().unwrap()]);
    }
    let result = flea::run_with_dependencies(args, &dependencies);
    (
        result.exit_code,
        serde_json::from_str(&result.document).unwrap(),
    )
}

fn facts(directory: &Path) -> std::path::PathBuf {
    let path = directory.join("facts.json");
    fs::write(
        &path,
        serde_json::to_vec(&json!({
            "title":"Steel lock", "description":"Seller's original description: hyvä",
            "price":"10.00", "category":"lock", "brand":"Abus",
            "colors":["Black"], "package_size":"Small", "condition":"Good",
            "is_unisex":true, "images":["original image.heic"]
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

#[test]
fn ready_export_matches_validated_json_and_quotes_ordered_images() {
    let directory = tempfile::tempdir().unwrap();
    let input = facts(directory.path());
    let output = directory.path().join("seller's ready listing.json");
    let original = fs::read(&input).unwrap();
    let (code, document) = run(&input, Some(&output));
    assert_eq!(code, 0, "{document}");
    assert_eq!(document["data"]["status"], "ready");
    assert_eq!(document["data"]["mutated"], false);
    let proposal = &document["data"]["proposed_mutation"];
    let saved: Value = serde_json::from_slice(&fs::read(&output).unwrap()).unwrap();
    assert_eq!(saved, proposal["listing_input"]);
    let command = format!(
        "flea vinted --portal fi publish --input='{}' --image='original image.heic' --image='front '\\''one'\\''.heic' --image='back two.png'",
        output.to_str().unwrap().replace('\'', "'\\''")
    );
    assert_eq!(proposal["command"], command);
    assert_eq!(document["next_actions"], json!([{"command":command}]));
    assert_eq!(
        proposal["image_paths"],
        json!(["original image.heic", "front 'one'.heic", "back two.png"])
    );
    assert_eq!(fs::read(&input).unwrap(), original);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[test]
fn existing_output_and_facts_input_are_never_overwritten() {
    let directory = tempfile::tempdir().unwrap();
    let input = facts(directory.path());
    let output = directory.path().join("ready.json");
    fs::write(&output, b"existing output").unwrap();
    for destination in [&output, &input] {
        let original = fs::read(destination).unwrap();
        let (code, document) = run(&input, Some(destination));
        assert_ne!(code, 0, "{document}");
        assert_eq!(document["ok"], false);
        assert!(
            document["error"]["message"]
                .as_str()
                .unwrap()
                .contains("Failed to save")
        );
        assert_eq!(fs::read(destination).unwrap(), original);
    }
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 2);
}

#[cfg(unix)]
#[test]
fn symlink_outputs_including_dangling_links_are_never_followed() {
    let directory = tempfile::tempdir().unwrap();
    let input = facts(directory.path());
    let original = fs::read(&input).unwrap();
    let missing = directory.path().join("missing.json");
    for (name, target) in [("existing-link", &input), ("dangling-link", &missing)] {
        let output = directory.path().join(name);
        std::os::unix::fs::symlink(target, &output).unwrap();
        let (code, document) = run(&input, Some(&output));
        assert_ne!(code, 0, "{document}");
        assert_eq!(fs::read_link(&output).unwrap(), *target);
    }
    assert!(!missing.exists());
    assert_eq!(fs::read(&input).unwrap(), original);
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 3);
}

#[test]
fn needs_input_does_not_create_output_and_preserves_export_in_continuation() {
    let directory = tempfile::tempdir().unwrap();
    let input = facts(directory.path());
    let mut incomplete: Value = serde_json::from_slice(&fs::read(&input).unwrap()).unwrap();
    incomplete.as_object_mut().unwrap().remove("condition");
    fs::write(&input, serde_json::to_vec(&incomplete).unwrap()).unwrap();
    let output = directory.path().join("ready listing.json");
    let (code, document) = run(&input, Some(&output));
    assert_eq!(code, 0, "{document}");
    assert_eq!(document["data"]["status"], "needs_input");
    assert!(!output.exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    let command = document["data"]["ambiguities"][0]["choices"][0]["command"]
        .as_str()
        .unwrap();
    assert!(command.contains(&format!("--output='{}'", output.display())));
}

#[test]
fn filesystem_failures_return_errors_without_partial_exports() {
    let directory = tempfile::tempdir().unwrap();
    let input = facts(directory.path());
    let output = directory.path().join("missing-parent").join("ready.json");
    let (code, document) = run(&input, Some(&output));
    assert_ne!(code, 0, "{document}");
    assert_eq!(document["ok"], false);
    assert!(!output.exists());
    let (code, document) = run(&input, Some(directory.path()));
    assert_ne!(code, 0, "{document}");
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn default_proposal_does_not_save_a_file() {
    let directory = tempfile::tempdir().unwrap();
    let input = facts(directory.path());
    let (code, document) = run(&input, None);
    assert_eq!(code, 0, "{document}");
    assert!(
        document["data"]["proposed_mutation"]["command"]
            .as_str()
            .unwrap()
            .contains("--input listing.json")
    );
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[test]
fn invalid_facts_do_not_create_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("facts.json");
    fs::write(&input, b"not valid JSON").unwrap();
    let output = directory.path().join("ready.json");
    let (code, document) = run(&input, Some(&output));
    assert_ne!(code, 0, "{document}");
    assert!(!output.exists());
    assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
}
