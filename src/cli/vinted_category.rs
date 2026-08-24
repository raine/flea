use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};
use serde_json::Value;

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
                VintedPublicationComposer, categories_for_search, category_suggestions_from_search,
            },
            publication_discovery::{
                DiscoveryRequest, VintedPublicationDiscoveryApi, validate_request,
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
        long_about = "Combine the selected category with runtime attributes, brands, colors, price configuration, and package sizes. Optional partial or complete ListingInput JSON confirms seller facts and enables payload validation."
    )]
    Compose {
        /// Runtime leaf category ID.
        category_id: u64,
        /// Partial or complete ListingInput JSON, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: Option<PathBuf>,
    },
    #[command(
        about = "Discover layered Vinted category attributes",
        long_about = "Post a JSON array of selected category attributes and return the next layered attribute configuration. Include the category selection and repeat after each parent selection."
    )]
    Attributes {
        /// JSON selection array, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
    },
    #[command(
        about = "Search brands valid for a Vinted category",
        long_about = "Fetch minimized brand choices scoped to a runtime Vinted category ID and optional search text."
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
        long_about = "Fetch the authenticated color choices exposed to the Vinted publication form."
    )]
    Colors,
    #[command(
        about = "Show Vinted publication configuration",
        long_about = "Fetch upload session, price limits, image limits, measurements, and other runtime publication configuration."
    )]
    Configuration,
    #[command(
        about = "List package sizes for a Vinted category",
        long_about = "Fetch shipping package sizes and optional parcel measurement configuration for a runtime category ID."
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
    if let VintedCategoryCommand::Compose { category_id, input } = command {
        let supplied = input.as_ref().map(read_json).transpose()?;
        let composer = VintedPublicationComposer::new(session, api)
            .compose(portal, category_id, supplied)
            .await?;
        return Ok(CommandOutcome::new(CommandData::VintedComposer(composer)));
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
        let next_actions = category_search_next_actions(&query, count, &suggestions);
        Ok(CommandOutcome::new(CommandData::VintedCategories(
            PublicationCategoryCollection {
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
        Ok(CommandOutcome::new(CommandData::Raw(response)))
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
        assert!(outcome.next_actions.is_empty());
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
}
