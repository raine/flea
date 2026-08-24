use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    domain::envelope::{NextAction, Warning},
    error::AppError,
    invocation,
    marketplace::{
        PortalId,
        vinted::{
            binding::VINTED_FI_BINDING,
            category_evidence,
            composer::{
                PublicationCategoryCollection, PublicationCategorySuggestion,
                VintedComposerReadiness, VintedPublicationComposer, categories_for_search,
                categories_from_response, category_suggestions_from_search,
                publication_attribute_definitions, publication_attribute_options,
                selection_command,
            },
            publication_discovery::{
                DiscoveryRequest, DiscoveryScope, PublicationDiscoveryOutput,
                VintedPublicationDiscoveryApi, validate_request,
            },
            search::{VintedSearchApi, VintedSearchSession},
        },
    },
};

const MAX_DIRECT_CATEGORY_RESULTS: usize = 8;

#[derive(Debug, Args)]
pub struct VintedCategoryArgs {
    #[command(subcommand)]
    pub command: VintedCategoryCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedCategoryCommand {
    #[command(
        about = "List the Vinted publication catalog tree",
        long_about = "Fetch the authenticated minimized Vinted catalog tree used by the publication form."
    )]
    List,
    #[command(
        about = "Search Vinted publication categories",
        long_about = "Send a keyword to Vinted's authenticated, portal-localized publication category service. Output reports the active portal and request locale. If no category matches, browse the localized catalog or follow any Vinted-provided suggestions."
    )]
    Search {
        /// Portal-localized category search text.
        keyword: String,
    },
    #[command(
        about = "Compose a complete Vinted publication form",
        long_about = "Primary guided entry point for Vinted publication. Combine a category-scoped runtime ID with selection-scoped attributes, category-scoped brands and package sizes, portal-scoped colors, and account-scoped configuration. Optional partial or complete ListingInput JSON confirms seller facts and enables payload validation. Add --readiness for selected values and validation results without the discovery option catalog.",
        after_help = "Examples:\n  flea --format json vinted category search SEARCH_TEXT\n  flea vinted category compose CATEGORY_ID --input listing.json\n  flea vinted category compose CATEGORY_ID --input listing.json --readiness"
    )]
    Compose {
        /// Runtime leaf category ID.
        category_id: u64,
        /// Partial or complete ListingInput JSON, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: Option<PathBuf>,
        /// Return concise readiness without fields or unselected options.
        #[arg(long)]
        readiness: bool,
    },
    #[command(
        about = "Discover layered Vinted category attributes",
        long_about = "Selection-scoped discovery. Post a JSON array of selected attributes and receive the next exact selection commands. Include the category selection emitted by compose, then repeat after choosing each parent value.",
        after_help = "Example:\n  flea --format json vinted category compose \"$CATEGORY_ID\" | jq '[.data.form.options[] | select(.field == \"category\") | .raw]' > selections.json\n  flea vinted category attributes --input selections.json"
    )]
    Attributes {
        /// JSON selection array, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
    },
    #[command(
        about = "Search brands valid for a Vinted category",
        long_about = "Category-scoped discovery. Fetch minimized brand choices for a runtime Vinted category ID and optional search text.",
        after_help = "Example:\n  flea vinted category brands \"$CATEGORY_ID\" BRAND_TEXT"
    )]
    Brands {
        /// Runtime category ID.
        category_id: u64,
        /// Optional brand search text.
        #[arg(default_value = "")]
        keyword: String,
    },
    #[command(
        about = "List Vinted publication colors",
        long_about = "Portal-scoped discovery. Fetch authenticated color choices exposed to the Vinted publication form. No category ID is accepted.",
        after_help = "Example:\n  flea vinted category colors"
    )]
    Colors,
    #[command(
        about = "Show Vinted publication configuration",
        long_about = "Account-scoped discovery. Fetch upload session, price limits, image limits, measurements, and other runtime publication configuration. No category ID is accepted.",
        after_help = "Example:\n  flea vinted category configuration"
    )]
    Configuration,
    #[command(
        about = "List package sizes for a Vinted category",
        long_about = "Category-scoped discovery. Fetch shipping package sizes and optional parcel measurement configuration for a runtime category ID.",
        after_help = "Example:\n  flea vinted category package-sizes \"$CATEGORY_ID\""
    )]
    PackageSizes {
        /// Runtime category ID.
        category_id: u64,
    },
}

impl VintedCategoryCommand {
    pub const fn telemetry_name(&self) -> &'static str {
        match self {
            Self::List => "category list",
            Self::Search { .. } => "category search",
            Self::Compose { .. } => "category compose",
            Self::Attributes { .. } => "category attributes",
            Self::Brands { .. } => "category brands",
            Self::Colors => "category colors",
            Self::Configuration => "category configuration",
            Self::PackageSizes { .. } => "category package-sizes",
        }
    }
}

pub async fn execute(
    portal: PortalId,
    command: VintedCategoryCommand,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationDiscoveryApi,
    search_api: &dyn VintedSearchApi,
) -> Result<CommandOutcome, AppError> {
    if let VintedCategoryCommand::Compose {
        category_id,
        input,
        readiness,
    } = command
    {
        let supplied = input.as_ref().map(read_json).transpose()?;
        let composer = VintedPublicationComposer::new(session, api)
            .compose(portal, category_id, supplied)
            .await?;
        let next_actions = composer
            .issue_actions
            .iter()
            .map(|action| crate::domain::envelope::NextAction {
                command: action.command.clone(),
            })
            .collect();
        let data = if readiness {
            CommandData::VintedComposerReadiness(VintedComposerReadiness::from(&composer))
        } else {
            CommandData::VintedComposer(composer)
        };
        return Ok(CommandOutcome::new(data).with_next_actions(next_actions));
    }

    let search_query = match &command {
        VintedCategoryCommand::Search { keyword } => Some(keyword.clone()),
        _ => None,
    };
    let request = match command {
        VintedCategoryCommand::List => DiscoveryRequest::Catalogs,
        VintedCategoryCommand::Search { keyword } => DiscoveryRequest::SearchCatalog { keyword },
        VintedCategoryCommand::Compose { .. } => unreachable!("handled above"),
        VintedCategoryCommand::Attributes { input } => DiscoveryRequest::Attributes {
            selections: read_json(&input)?,
        },
        VintedCategoryCommand::Brands {
            category_id,
            keyword,
        } => DiscoveryRequest::Brands {
            category_id,
            keyword,
        },
        VintedCategoryCommand::Colors => DiscoveryRequest::Colors,
        VintedCategoryCommand::Configuration => DiscoveryRequest::Configuration,
        VintedCategoryCommand::PackageSizes { category_id } => {
            DiscoveryRequest::PackageSizes { category_id }
        }
    };
    validate_request(&request)?;
    let credentials = session.credentials(portal).await?;
    let response = api.execute(&credentials, &request).await?;
    if let Some(query) = search_query {
        let catalogs = api
            .execute(&credentials, &DiscoveryRequest::Catalogs)
            .await?;
        let mut categories = categories_for_search(&response, &catalogs);
        let direct_count = categories.len();
        let needs_marketplace_evidence = needs_marketplace_evidence(direct_count);
        let mut marketplace_evidence = None;
        let mut warnings = Vec::new();
        if needs_marketplace_evidence {
            categories.clear();
            let runtime_categories = categories_from_response(&catalogs);
            match category_evidence::discover(
                portal,
                &query,
                &runtime_categories,
                session,
                search_api,
            )
            .await
            {
                Ok(result) if !result.categories.is_empty() => {
                    categories = result.categories;
                    marketplace_evidence = Some(result.evidence);
                }
                Ok(_) => {}
                Err(error) => warnings.push(Warning {
                    code: "vinted.category_marketplace_evidence_failed".to_owned(),
                    message: format!(
                        "Marketplace category evidence was unavailable: {}",
                        error.message
                    ),
                }),
            }
        }
        let suggestions = category_suggestions_from_search(&response);
        let count = categories.len();
        let guidance = if marketplace_evidence.is_some() {
            Some(format!(
                "Vinted's localized category service returned no focused direct match on portal {portal} with locale {}. These publishable leaf categories are ranked by category counts from current Vinted listings matching this text; choose the category that matches the item.",
                VINTED_FI_BINDING.iso_locale
            ))
        } else if count == 0 {
            Some(format!(
                "Vinted's localized category service returned no matches for this query on portal {portal} with locale {}. Browse the localized catalog or try a Vinted-provided suggestion when available.",
                VINTED_FI_BINDING.iso_locale
            ))
        } else {
            None
        };
        let actionable_direct_count = if needs_marketplace_evidence { 0 } else { count };
        let mut next_actions =
            category_search_next_actions(&query, actionable_direct_count, &suggestions);
        next_actions.extend(
            categories
                .iter()
                .filter(|category| category.leaf)
                .map(|category| NextAction {
                    command: invocation::vinted_fi(format!("category compose {}", category.id)),
                }),
        );
        Ok(CommandOutcome::new(CommandData::VintedCategories(
            PublicationCategoryCollection {
                scope: DiscoveryScope::Portal,
                portal,
                request_locale: VINTED_FI_BINDING.iso_locale.to_owned(),
                query,
                categories,
                count,
                guidance,
                suggestions,
                marketplace_evidence,
            },
        ))
        .with_next_actions(next_actions)
        .with_warnings(warnings))
    } else {
        let next_actions = attribute_next_actions(&request, &response);
        let output = PublicationDiscoveryOutput {
            scope: request.scope(),
            category_id: request.category_id(),
            selection_payload: request.selection_payload(),
            response,
        };
        Ok(
            CommandOutcome::new(CommandData::VintedPublicationDiscovery(output))
                .with_next_actions(next_actions),
        )
    }
}

fn needs_marketplace_evidence(direct_count: usize) -> bool {
    direct_count == 0 || direct_count > MAX_DIRECT_CATEGORY_RESULTS
}

fn category_search_next_actions(
    query: &str,
    count: usize,
    suggestions: &[PublicationCategorySuggestion],
) -> Vec<NextAction> {
    let mut actions = Vec::new();
    if count == 0 {
        actions.push(NextAction {
            command: invocation::vinted_fi("category list"),
        });
    }
    actions.extend(
        suggestions
            .iter()
            .filter(|suggestion| suggestion.keyword != query)
            .map(|suggestion| NextAction {
                command: invocation::vinted_fi(format!(
                    "category search {}",
                    shell_quote(&suggestion.keyword)
                )),
            }),
    );
    actions
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn attribute_next_actions(request: &DiscoveryRequest, response: &Value) -> Vec<NextAction> {
    let DiscoveryRequest::Attributes { selections } = request else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    for (code, definition) in publication_attribute_definitions(response) {
        for value in publication_attribute_options(definition)
            .into_iter()
            .filter_map(|option| option.get("id").or_else(|| option.get("value")))
        {
            let mut payload = selections.as_array().cloned().unwrap_or_default();
            payload.retain(|selection| selection.get("code") != Some(&json!(code)));
            payload.push(json!({"code": code, "value": [value]}));
            actions.push(NextAction {
                command: selection_command(&Value::Array(payload)),
            });
        }
    }
    actions
}

fn read_json(path: &PathBuf) -> Result<Value, AppError> {
    let bytes = if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| AppError::usage(format!("Failed to read JSON input: {error}")))?;
        bytes
    } else {
        fs::read(path).map_err(|error| {
            AppError::usage(format!(
                "Failed to read JSON input `{}`: {error}",
                path.display()
            ))
        })?
    };
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::usage("JSON input exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::usage(format!("Input must be valid JSON: {error}")))
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Mutex};

    use serde_json::json;

    use super::*;
    use crate::marketplace::vinted::{
        auth::VintedCredentialRecord,
        search::{CatalogueRequest, VintedSearchApi},
    };

    struct FixtureApi {
        search: Value,
        catalogs: Value,
        requests: Mutex<Vec<DiscoveryRequest>>,
    }

    impl VintedPublicationDiscoveryApi for FixtureApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            request: &'a DiscoveryRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.requests.lock().unwrap().push(request.clone());
            let response = match request {
                DiscoveryRequest::SearchCatalog { .. } => self.search.clone(),
                DiscoveryRequest::Catalogs => self.catalogs.clone(),
                _ => unreachable!("fixture only supports category search"),
            };
            Box::pin(std::future::ready(Ok(response)))
        }
    }

    struct FixtureSearchApi {
        responses: BTreeMap<Option<String>, Value>,
        requests: Mutex<Vec<CatalogueRequest>>,
    }

    impl VintedSearchApi for FixtureSearchApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            request: &'a CatalogueRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.requests.lock().unwrap().push(request.clone());
            let parent = request
                .context
                .attributes
                .get("catalog")
                .and_then(|ids| ids.first())
                .cloned();
            Box::pin(std::future::ready(Ok(self
                .responses
                .get(&parent)
                .cloned()
                .unwrap_or_else(|| json!({"categories":[]})))))
        }
    }

    fn empty_search_api() -> FixtureSearchApi {
        FixtureSearchApi {
            responses: BTreeMap::new(),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn credentials() -> VintedCredentialRecord {
        VintedCredentialRecord::new_for_adapter(
            PortalId::Fi,
            "user-1".into(),
            Some("fixture".into()),
            "access-token".into(),
            "refresh-token".into(),
            4_000_000_000,
            "device-1".into(),
            "anonymous-1".into(),
            None,
        )
    }

    fn api(search: Value) -> FixtureApi {
        FixtureApi {
            search,
            catalogs: json!({"catalogs":[
                {"id":10,"title":"Asusteet","catalogs":[
                    {"id":4380,"title":"Reput","catalogs":[]}
                ]}
            ]}),
            requests: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn localized_success_reports_portal_locale_and_resolved_catalog() {
        let api = api(json!({"catalog_ids":[4380]}));
        let session = |_| Ok(credentials());
        let search_api = empty_search_api();
        let outcome = execute(
            PortalId::Fi,
            VintedCategoryCommand::Search {
                keyword: "reppu".into(),
            },
            &session,
            &api,
            &search_api,
        )
        .await
        .unwrap();

        let CommandData::VintedCategories(result) = outcome.data else {
            panic!("expected normalized category search output");
        };
        assert_eq!(result.portal, PortalId::Fi);
        assert_eq!(result.request_locale, "fi-FI");
        assert_eq!(result.query, "reppu");
        assert_eq!(result.categories[0].path, ["Asusteet", "Reput"]);
        assert!(result.guidance.is_none());
        assert_eq!(
            outcome.next_actions[0].command,
            "flea vinted --portal fi category compose 4380"
        );
        assert_eq!(
            api.requests.lock().unwrap().as_slice(),
            [
                DiscoveryRequest::SearchCatalog {
                    keyword: "reppu".into()
                },
                DiscoveryRequest::Catalogs
            ]
        );
    }

    #[tokio::test]
    async fn zero_results_explain_localization_and_offer_upstream_suggestions() {
        let api = api(json!({"catalog_ids":[], "suggestions":["reppu"]}));
        let session = |_| Ok(credentials());
        let search_api = empty_search_api();
        let outcome = execute(
            PortalId::Fi,
            VintedCategoryCommand::Search {
                keyword: "backpack".into(),
            },
            &session,
            &api,
            &search_api,
        )
        .await
        .unwrap();

        let CommandData::VintedCategories(result) = outcome.data else {
            panic!("expected normalized category search output");
        };
        assert_eq!(result.count, 0);
        assert!(
            result
                .guidance
                .as_deref()
                .unwrap()
                .contains("portal fi with locale fi-FI")
        );
        assert_eq!(result.suggestions[0].keyword, "reppu");
        assert_eq!(
            outcome
                .next_actions
                .iter()
                .map(|action| action.command.as_str())
                .collect::<Vec<_>>(),
            [
                "flea vinted --portal fi category list",
                "flea vinted --portal fi category search 'reppu'"
            ]
        );
    }

    #[tokio::test]
    async fn zero_upstream_results_use_marketplace_category_facets() {
        let api = FixtureApi {
            search: json!({"catalog_ids":[]}),
            catalogs: json!({"catalogs":[
                {"id":100,"title":"Miehet","catalogs":[
                    {"id":110,"title":"Kengät","catalogs":[
                        {"id":1453,"title":"Juoksukengät","catalogs":[]},
                        {"id":2678,"title":"Vaelluskengät","catalogs":[]}
                    ]}
                ]}
            ]}),
            requests: Mutex::new(Vec::new()),
        };
        let search_api = FixtureSearchApi {
            responses: BTreeMap::from([
                (
                    None,
                    json!({"categories":[{"id":100,"title":"Miehet","item_count":100}]}),
                ),
                (
                    Some("100".into()),
                    json!({"categories":[{"id":110,"title":"Kengät","item_count":100}]}),
                ),
                (
                    Some("110".into()),
                    json!({"categories":[
                        {"id":1453,"title":"Juoksukengät","item_count":40},
                        {"id":2678,"title":"Vaelluskengät","item_count":60}
                    ]}),
                ),
            ]),
            requests: Mutex::new(Vec::new()),
        };
        let session = |_| Ok(credentials());
        let outcome = execute(
            PortalId::Fi,
            VintedCategoryCommand::Search {
                keyword: "paljasjalkakengät".into(),
            },
            &session,
            &api,
            &search_api,
        )
        .await
        .unwrap();

        let CommandData::VintedCategories(result) = outcome.data else {
            panic!("expected normalized category search output");
        };
        assert_eq!(result.categories[0].id, 2678);
        assert_eq!(result.categories[1].id, 1453);
        assert!(
            result
                .guidance
                .as_deref()
                .unwrap()
                .contains("current Vinted listings")
        );
        let evidence = result.marketplace_evidence.unwrap();
        assert_eq!(evidence.requests, 3);
        assert_eq!(evidence.counts[0].listings, 60);
        assert_eq!(search_api.requests.lock().unwrap().len(), 3);
        assert_eq!(
            outcome.next_actions[0].command,
            "flea vinted --portal fi category list"
        );
        assert_eq!(
            outcome.next_actions[1].command,
            "flea vinted --portal fi category compose 2678"
        );
    }

    #[test]
    fn empty_and_broad_direct_results_use_marketplace_evidence() {
        assert!(needs_marketplace_evidence(0));
        assert!(!needs_marketplace_evidence(MAX_DIRECT_CATEGORY_RESULTS));
        assert!(needs_marketplace_evidence(MAX_DIRECT_CATEGORY_RESULTS + 1));
    }

    #[test]
    fn attribute_actions_carry_nested_options_and_exact_selections_forward() {
        let request = DiscoveryRequest::Attributes {
            selections: json!([{"code":"category","value":[4380]}]),
        };
        let actions = attribute_next_actions(
            &request,
            &json!({"attributes":[{
                "code":"condition",
                "configuration":{
                    "title":"Condition",
                    "required":true,
                    "groups":[{
                        "id":1,
                        "title":"Condition",
                        "options":[{"id":6,"title":"Good"},{"id":7,"title":"New"}]
                    }]
                }
            }]}),
        );

        assert_eq!(actions.len(), 2);
        assert!(
            actions[0].command.contains(
                r#"[{"code":"category","value":[4380]},{"code":"condition","value":[6]}]"#
            )
        );
        assert!(
            actions[0]
                .command
                .ends_with("flea vinted category attributes --input -")
        );
    }
}
