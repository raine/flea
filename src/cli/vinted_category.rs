use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};
use serde_json::{Value, json};

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    domain::envelope::NextAction,
    error::AppError,
    invocation,
    marketplace::{
        PortalId,
        vinted::{
            binding::VINTED_FI_BINDING,
            composer::{
                PublicationCategoryCollection, PublicationCategorySuggestion,
                VintedComposerReadiness, VintedPublicationComposer, categories_for_search,
                category_suggestions_from_search, selection_command,
            },
            publication_discovery::{
                DiscoveryRequest, DiscoveryScope, PublicationDiscoveryOutput,
                VintedPublicationDiscoveryApi, validate_request,
            },
            search::VintedSearchSession,
        },
    },
};

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
        after_help = "Examples:\n  CATEGORY_ID=$(flea --format json vinted category search SEARCH_TEXT | jq -er '.data.categories[] | select(.leaf) | .id' | head -n1)\n  flea vinted category compose \"$CATEGORY_ID\" --input listing.json\n  flea vinted category compose \"$CATEGORY_ID\" --input listing.json --readiness"
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
        let categories = categories_for_search(&response, &catalogs);
        let suggestions = category_suggestions_from_search(&response);
        let count = categories.len();
        let guidance = (count == 0).then(|| {
            format!(
                "Vinted's localized category service returned no matches for this query on portal {portal} with locale {}. Browse the localized catalog or try a Vinted-provided suggestion when available.",
                VINTED_FI_BINDING.iso_locale
            )
        });
        let mut next_actions = category_search_next_actions(&query, count, &suggestions);
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
            },
        ))
        .with_next_actions(next_actions))
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
    let Some(attributes) = response
        .get("attributes")
        .and_then(Value::as_array)
        .or_else(|| {
            response
                .pointer("/data/attributes")
                .and_then(Value::as_array)
        })
    else {
        return Vec::new();
    };
    let mut actions = Vec::new();
    for attribute in attributes {
        let Some(code) = attribute.get("code").and_then(Value::as_str) else {
            continue;
        };
        let Some(values) = ["values", "options", "items"]
            .iter()
            .find_map(|key| attribute.get(*key).and_then(Value::as_array))
        else {
            continue;
        };
        for value in values
            .iter()
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
    use std::{future::Future, pin::Pin, sync::Mutex};

    use serde_json::json;

    use super::*;
    use crate::marketplace::vinted::auth::VintedCredentialRecord;

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
        let outcome = execute(
            PortalId::Fi,
            VintedCategoryCommand::Search {
                keyword: "reppu".into(),
            },
            &session,
            &api,
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
        let outcome = execute(
            PortalId::Fi,
            VintedCategoryCommand::Search {
                keyword: "backpack".into(),
            },
            &session,
            &api,
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

    #[test]
    fn attribute_actions_carry_the_exact_selection_payload_forward() {
        let request = DiscoveryRequest::Attributes {
            selections: json!([{"code":"category","value":[4380]}]),
        };
        let actions = attribute_next_actions(
            &request,
            &json!({"attributes":[{
                "code":"condition",
                "values":[{"id":6,"title":"Good"},{"id":7,"title":"New"}]
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
