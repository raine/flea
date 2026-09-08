use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};
use serde::Serialize;
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
            categories::{self, CatalogTree, CategoryPage, DiscoveryStages, SearchOptions},
            category_evidence::MarketplaceCategoryEvidence,
            composer::{
                PublicationCategorySuggestion, VintedComposerReadiness, VintedPublicationComposer,
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

#[derive(Debug, Serialize)]
pub struct CategoryOutput {
    pub scope: DiscoveryScope,
    pub portal: PortalId,
    pub request_locale: String,
    pub mode: &'static str,
    pub query: Option<String>,
    #[serde(flatten)]
    pub page: CategoryPage,
    pub count: usize,
    pub selection_required: bool,
    pub guidance: Option<String>,
    pub suggestions: Vec<PublicationCategorySuggestion>,
    pub marketplace_evidence: Option<MarketplaceCategoryEvidence>,
    pub stages: Option<DiscoveryStages>,
}

fn parse_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "limit must be an integer".to_owned())?;
    if !(1..=categories::MAX_LIMIT).contains(&limit) {
        return Err(format!(
            "limit must be between 1 and {}",
            categories::MAX_LIMIT
        ));
    }
    Ok(limit)
}

#[derive(Debug, Args)]
pub struct VintedCategoryArgs {
    #[command(subcommand)]
    pub command: VintedCategoryCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedCategoryCommand {
    #[command(
        about = "List the Vinted publication catalog tree",
        long_about = "Fetch the authenticated minimized Vinted catalog tree used by the publication form. With no flags, preserve the complete raw export. Use --roots or --parent ID for compact pages (default 20, maximum 100); compact browsing never calls search or ranking services."
    )]
    List {
        /// Return compact roots instead of the raw tree.
        #[arg(long, conflicts_with = "parent", group = "compact")]
        roots: bool,
        /// Return compact direct children of this runtime category ID.
        #[arg(long, group = "compact")]
        parent: Option<u64>,
        /// Maximum categories per compact page (1-100, default 20).
        #[arg(long, requires = "compact", value_parser = parse_limit)]
        limit: Option<usize>,
        /// Number of categories to skip in a compact page.
        #[arg(long, requires = "compact")]
        offset: Option<usize>,
    },
    #[command(
        about = "Search and rank Vinted publication categories",
        long_about = "Search the current publication catalog by localized full-path tokens, with best-effort publication search and marketplace hints. Selection is always required. Browse roots or children when wording does not match observed labels. Results default to 20; --limit accepts 1 through 100.",
        after_help = "Example:\n  flea vinted category search 'Vibram FiveFingers' --title 'Men’s trail running shoes' --description 'Barefoot shoes with rugged soles'"
    )]
    Search {
        /// Portal-localized category search text.
        keyword: String,
        /// Listing title used as recommendation context.
        #[arg(long)]
        title: Option<String>,
        /// Listing description used as recommendation context.
        #[arg(long)]
        description: Option<String>,
        /// Restrict results to this category subtree.
        #[arg(long)]
        parent: Option<u64>,
        /// Maximum category candidates per page (1-100).
        #[arg(long, default_value = "20", value_parser = parse_limit)]
        limit: usize,
        /// Number of matching candidates to skip.
        #[arg(long, default_value = "0")]
        offset: usize,
    },
    #[command(
        about = "Compose and validate a Vinted publication form",
        long_about = "Primary guided entry point for Vinted publication. Combine a category-scoped runtime ID with selection-scoped attributes, category-scoped brands and package sizes, portal-scoped colors, and account-scoped configuration. The default response contains readiness, selected values, issues, and next actions. Optional partial or complete ListingInput JSON accepts semantic size, condition, color, and package values as well as explicit opaque IDs. Add --full to include the complete field and runtime option catalogs.",
        after_help = "Examples:\n  flea --format json vinted category search SEARCH_TEXT\n  flea vinted category compose CATEGORY_ID --input listing.json\n  flea vinted category compose CATEGORY_ID --full"
    )]
    Compose {
        /// Runtime leaf category ID.
        category_id: u64,
        /// Partial or complete ListingInput JSON, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: Option<PathBuf>,
        /// Include complete fields and runtime option catalogs.
        #[arg(long, conflicts_with = "readiness")]
        full: bool,
        #[arg(long, hide = true, conflicts_with = "full")]
        readiness: bool,
    },
    #[command(
        about = "Discover layered Vinted category attributes",
        long_about = "Selection-scoped discovery. Post a JSON array of selected attributes and receive the next exact selection commands. Include the category selection emitted by compose, then repeat after choosing each parent value.",
        after_help = "Example:\n  flea --format json vinted category compose \"$CATEGORY_ID\" --full | jq '[.data.form.options[] | select(.field == \"category\") | .raw]' > selections.json\n  flea vinted category attributes --input selections.json"
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
            Self::List { .. } => "category list",
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
        full,
        readiness: _,
    } = command
    {
        let supplied = input.as_ref().map(read_json).transpose()?;
        let composer = VintedPublicationComposer::new(session, api)
            .compose(portal, category_id, supplied)
            .await?;
        let readiness = VintedComposerReadiness::from(&composer);
        let next_actions = if full {
            &composer.issue_actions
        } else {
            &readiness.next_actions
        }
        .iter()
        .map(|action| crate::domain::envelope::NextAction {
            command: action.command.clone(),
        })
        .collect();
        let data = if full {
            CommandData::VintedComposer(Box::new(composer))
        } else {
            CommandData::VintedComposerReadiness(readiness)
        };
        return Ok(CommandOutcome::new(data).with_next_actions(next_actions));
    }

    match &command {
        VintedCategoryCommand::Search {
            keyword,
            title,
            description,
            parent,
            limit,
            offset,
        } => {
            let options = SearchOptions {
                query: keyword,
                title: title.as_deref(),
                description: description.as_deref(),
                parent_id: *parent,
                limit: *limit,
                offset: *offset,
            };
            options.validate()?;
            let result = categories::discover(portal, options, session, api, search_api).await?;
            let mut actions = page_actions(&result.page);
            actions.push(NextAction {
                command: browse_command(*parent, categories::DEFAULT_LIMIT, 0),
            });
            for suggestion in result.suggestions.iter().take(categories::DEFAULT_LIMIT) {
                if suggestion.keyword != *keyword {
                    actions.push(NextAction {
                        command: search_command(
                            &suggestion.keyword,
                            title.as_deref(),
                            description.as_deref(),
                            *parent,
                            *limit,
                            0,
                        ),
                    });
                }
            }
            if result.page.truncated {
                actions.push(NextAction {
                    command: search_command(
                        keyword,
                        title.as_deref(),
                        description.as_deref(),
                        *parent,
                        *limit,
                        offset.saturating_add(result.page.returned),
                    ),
                });
            }
            let mut seen_actions = std::collections::HashSet::new();
            actions.retain(|action| seen_actions.insert(action.command.clone()));
            let guidance = Some("Candidates are current taxonomy nodes, not semantic certainty. Choose a leaf intentionally; browse nonleaves. Marketplace counts are relative listing support, not classification confidence, and zero listings do not exclude a category. Optional service failures do not mean a category is absent.".to_owned());
            return Ok(
                CommandOutcome::new(CommandData::VintedCategories(CategoryOutput {
                    scope: DiscoveryScope::Portal,
                    portal,
                    request_locale: VINTED_FI_BINDING.iso_locale.to_owned(),
                    mode: "search",
                    query: Some(keyword.clone()),
                    count: result.page.returned,
                    page: result.page,
                    selection_required: true,
                    guidance,
                    suggestions: result.suggestions,
                    marketplace_evidence: result.marketplace_evidence,
                    stages: Some(result.stages),
                }))
                .with_next_actions(actions)
                .with_warnings(result.warnings),
            );
        }
        VintedCategoryCommand::List {
            roots,
            parent,
            limit,
            offset,
        } if *roots || parent.is_some() => {
            let limit = limit.unwrap_or(categories::DEFAULT_LIMIT);
            let offset = offset.unwrap_or(0);
            parse_limit(&limit.to_string()).map_err(AppError::usage)?;
            let credentials = session.credentials(portal).await?;
            let response = api
                .execute(&credentials, &DiscoveryRequest::Catalogs)
                .await?;
            let page = CatalogTree::from_response(&response)?.browse(*parent, limit, offset)?;
            let mut actions = page_actions(&page);
            if page.truncated {
                actions.push(NextAction {
                    command: browse_command(*parent, limit, offset.saturating_add(page.returned)),
                });
            }
            return Ok(
                CommandOutcome::new(CommandData::VintedCategories(CategoryOutput {
                    scope: DiscoveryScope::Portal,
                    portal,
                    request_locale: VINTED_FI_BINDING.iso_locale.to_owned(),
                    mode: if *roots { "roots" } else { "children" },
                    query: None,
                    count: page.returned,
                    page,
                    selection_required: true,
                    guidance: None,
                    suggestions: Vec::new(),
                    marketplace_evidence: None,
                    stages: None,
                }))
                .with_next_actions(actions),
            );
        }
        VintedCategoryCommand::List { limit, offset, .. }
            if limit.is_some() || offset.is_some() =>
        {
            return Err(AppError::usage("Pagination requires --roots or --parent"));
        }
        _ => {}
    }
    let request = match command {
        VintedCategoryCommand::List { .. } => DiscoveryRequest::Catalogs,
        VintedCategoryCommand::Search { keyword, .. } => {
            DiscoveryRequest::SearchCatalog { keyword }
        }
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

fn page_actions(page: &CategoryPage) -> Vec<NextAction> {
    let mut nodes = page
        .categories
        .iter()
        .map(|candidate| &candidate.node)
        .collect::<Vec<_>>();
    if page.categories.is_empty()
        && let Some(parent) = &page.parent
        && parent.category.leaf
    {
        nodes.push(parent);
    }
    nodes
        .into_iter()
        .map(|node| NextAction {
            command: if node.category.leaf {
                invocation::vinted_fi(format!("category compose {}", node.category.id))
            } else {
                browse_command(Some(node.category.id), categories::DEFAULT_LIMIT, 0)
            },
        })
        .collect()
}

fn browse_command(parent: Option<u64>, limit: usize, offset: usize) -> String {
    let scope = parent.map_or_else(|| "--roots".to_owned(), |id| format!("--parent {id}"));
    invocation::vinted_fi(format!(
        "category list {scope} --limit {limit} --offset {offset}"
    ))
}

fn search_command(
    query: &str,
    title: Option<&str>,
    description: Option<&str>,
    parent: Option<u64>,
    limit: usize,
    offset: usize,
) -> String {
    let mut command = "category search".to_owned();
    for (flag, value) in [("title", title), ("description", description)] {
        if let Some(value) = value {
            command.push_str(&format!(" --{flag}={}", shell_quote(value)));
        }
    }
    if let Some(parent) = parent {
        command.push_str(&format!(" --parent {parent}"));
    }
    command.push_str(&format!(
        " --limit {limit} --offset {offset} -- {}",
        shell_quote(query)
    ));
    invocation::vinted_fi(command)
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

    #[test]
    fn generated_search_accepts_leading_hyphen_values() {
        use clap::Parser;
        let command = search_command(
            "- shoes",
            Some("- title"),
            Some("- lightly used"),
            None,
            2,
            2,
        );
        let script = format!("set -- {command}; printf '%s\\0' \"$@\"");
        let output = std::process::Command::new("sh")
            .args(["-c", &script])
            .output()
            .unwrap();
        assert!(output.status.success());
        let words = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|word| !word.is_empty())
            .map(|word| std::str::from_utf8(word).unwrap());
        assert!(crate::cli::Cli::try_parse_from(words).is_ok());
    }

    struct FixtureApi {
        search: Value,
        catalogs: Value,
        fail_keyword: bool,
        requests: Mutex<Vec<DiscoveryRequest>>,
    }

    impl VintedPublicationDiscoveryApi for FixtureApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            request: &'a DiscoveryRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.requests.lock().unwrap().push(request.clone());
            if self.fail_keyword && matches!(request, DiscoveryRequest::SearchCatalog { .. }) {
                return Box::pin(std::future::ready(Err(AppError::usage(
                    "Publication search returned HTTP 404",
                ))));
            }
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
        unavailable: bool,
        requests: Mutex<Vec<CatalogueRequest>>,
    }

    impl VintedSearchApi for FixtureSearchApi {
        fn execute<'a>(
            &'a self,
            _credentials: &'a VintedCredentialRecord,
            request: &'a CatalogueRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.requests.lock().unwrap().push(request.clone());
            if self.unavailable {
                return Box::pin(std::future::ready(Err(AppError::usage(
                    "Vinted search returned HTTP 404",
                ))));
            }
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
            unavailable: false,
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
            fail_keyword: false,
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
                title: None,
                description: None,
                parent: None,
                limit: 20,
                offset: 0,
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
        assert_eq!(result.query.as_deref(), Some("reppu"));
        assert_eq!(
            result.page.categories[0].node.category.path,
            ["Asusteet", "Reput"]
        );
        assert!(result.selection_required);
        assert_eq!(
            outcome.next_actions[0].command,
            "flea vinted --portal fi category compose 4380"
        );
        assert_eq!(
            api.requests.lock().unwrap().as_slice(),
            [
                DiscoveryRequest::Catalogs,
                DiscoveryRequest::SearchCatalog {
                    keyword: "reppu".into()
                }
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
                title: None,
                description: None,
                parent: None,
                limit: 20,
                offset: 0,
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
                .contains("Optional service failures")
        );
        assert_eq!(result.suggestions[0].keyword, "reppu");
        assert_eq!(
            outcome
                .next_actions
                .iter()
                .map(|action| action.command.as_str())
                .collect::<Vec<_>>(),
            [
                "flea vinted --portal fi category list --roots --limit 20 --offset 0",
                "flea vinted --portal fi category search --limit 20 --offset 0 -- 'reppu'"
            ]
        );
    }

    #[tokio::test]
    async fn listing_context_ranks_products_that_fit_nearby_categories() {
        let api = FixtureApi {
            search: json!({"catalog_ids":[1453,2678,3001]}),
            fail_keyword: false,
            catalogs: json!({"catalogs":[
                {"id":100,"title":"Miehet","catalogs":[
                    {"id":110,"title":"Kengät","catalogs":[
                        {"id":1453,"title":"Juoksukengät","catalogs":[]},
                        {"id":2678,"title":"Vaelluskengät","catalogs":[]},
                        {"id":3001,"title":"Vapaa-ajan kengät","catalogs":[]}
                    ]}
                ]}
            ]}),
            requests: Mutex::new(Vec::new()),
        };
        let search_api = FixtureSearchApi {
            unavailable: false,
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
                        {"id":1453,"title":"Juoksukengät","item_count":60},
                        {"id":2678,"title":"Vaelluskengät","item_count":40},
                        {"id":3001,"title":"Vapaa-ajan kengät","item_count":10}
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
                title: Some("Vibram FiveFingers miesten juoksukengät".into()),
                description: Some("Kevyet paljasjalkakengät maastojuoksuun".into()),
                parent: None,
                limit: 20,
                offset: 0,
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
        let mut ids = result
            .page
            .categories
            .iter()
            .map(|candidate| candidate.node.category.id)
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, [1453, 2678, 3001]);
        assert!(result.selection_required);
        let evidence = result.marketplace_evidence.unwrap();
        assert_eq!(evidence.requests, 3);
        assert!(evidence.selection_required);
        assert_eq!(evidence.context_fields, ["keyword", "title", "description"]);
        assert_eq!(evidence.recommendations[0].category_id, 1453);
        assert_eq!(evidence.recommendations[0].score, 100);
        assert_eq!(evidence.recommendations[1].score, 67);
        assert!(
            evidence.recommendations[1]
                .evidence
                .contains("40 matching listings")
        );
        assert_eq!(evidence.counts[0].listings, 60);
        let requests = search_api.requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert!(
            requests[0]
                .context
                .query
                .contains("Vibram FiveFingers miesten juoksukengät")
        );
        assert!(requests[0].context.query.contains("maastojuoksuun"));
        assert_eq!(outcome.next_actions.len(), 4);
        assert!(
            outcome
                .next_actions
                .iter()
                .all(|action| action.command.contains("category compose")
                    || action.command.contains("category list --roots"))
        );
    }

    #[tokio::test]
    async fn optional_failures_preserve_candidates_and_stage_warnings() {
        for fail_keyword in [false, true] {
            let mut api = api(json!({"catalog_ids":[4380]}));
            api.fail_keyword = fail_keyword;
            let session = |_| Ok(credentials());
            let mut search_api = empty_search_api();
            search_api.unavailable = true;
            let outcome = execute(
                PortalId::Fi,
                VintedCategoryCommand::Search {
                    keyword: "Reput".into(),
                    title: Some("Seller's bag".into()),
                    description: None,
                    parent: None,
                    limit: 20,
                    offset: 0,
                },
                &session,
                &api,
                &search_api,
            )
            .await
            .unwrap();
            let CommandData::VintedCategories(result) = outcome.data else {
                panic!("expected candidates")
            };
            assert_eq!(result.page.categories.len(), 1);
            assert_eq!(result.page.categories[0].node.category.id, 4380);
            assert!(result.selection_required);
            assert_eq!(outcome.warnings.len(), if fail_keyword { 2 } else { 1 });
            assert!(
                outcome
                    .warnings
                    .iter()
                    .any(|warning| warning.code == "vinted.category_marketplace_evidence_failed")
            );
            let stages = serde_json::to_value(result.stages).unwrap();
            assert_eq!(stages["marketplace"], "unavailable");
            if fail_keyword {
                assert_eq!(stages["publication_search"], "unavailable");
            }
            assert_eq!(
                outcome.next_actions[0].command,
                "flea vinted --portal fi category compose 4380"
            );
        }
    }

    #[tokio::test]
    async fn compact_browse_fetches_only_catalogs_and_preserves_raw_export() {
        let api = api(json!({"catalog_ids":[]}));
        let session = |_| Ok(credentials());
        let search_api = empty_search_api();
        for (parent, roots, expected_action) in [
            (
                None,
                true,
                "category list --parent 10 --limit 20 --offset 0",
            ),
            (Some(10), false, "category compose 4380"),
            (Some(4380), false, "category compose 4380"),
        ] {
            let outcome = execute(
                PortalId::Fi,
                VintedCategoryCommand::List {
                    roots,
                    parent,
                    limit: Some(1),
                    offset: None,
                },
                &session,
                &api,
                &search_api,
            )
            .await
            .unwrap();
            let CommandData::VintedCategories(result) = outcome.data else {
                panic!("expected compact page")
            };
            assert!(result.page.returned <= 1);
            assert_eq!(
                outcome.next_actions[0].command,
                invocation::vinted_fi(expected_action)
            );
            let value = serde_json::to_value(&result).unwrap();
            assert!(value.get("response").is_none());
            assert!(
                value["categories"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .all(|node| node.get("catalogs").is_none())
            );
            if parent == Some(4380) {
                assert_eq!(result.page.total, 0);
                assert!(result.page.parent.unwrap().category.leaf);
            }
        }
        let raw = execute(
            PortalId::Fi,
            VintedCategoryCommand::List {
                roots: false,
                parent: None,
                limit: None,
                offset: None,
            },
            &session,
            &api,
            &search_api,
        )
        .await
        .unwrap();
        let CommandData::VintedPublicationDiscovery(raw) = raw.data else {
            panic!("expected raw tree")
        };
        assert_eq!(raw.response, api.catalogs);
        assert!(
            api.requests
                .lock()
                .unwrap()
                .iter()
                .all(|request| matches!(request, DiscoveryRequest::Catalogs))
        );
        assert!(search_api.requests.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn search_pages_preserve_context_and_nonleaves_only_browse() {
        let api = api(json!({"catalog_ids":[]}));
        let session = |_| Ok(credentials());
        let search_api = empty_search_api();
        let outcome = execute(
            PortalId::Fi,
            VintedCategoryCommand::Search {
                keyword: "Asusteet".into(),
                title: Some("Seller's bag".into()),
                description: Some("Line one\nLine two".into()),
                parent: Some(10),
                limit: 1,
                offset: 0,
            },
            &session,
            &api,
            &search_api,
        )
        .await
        .unwrap();
        let CommandData::VintedCategories(result) = outcome.data else {
            panic!("expected page")
        };
        assert_eq!(result.page.total, 2);
        assert!(result.page.truncated);
        assert!(!result.page.categories[0].node.category.leaf);
        assert_eq!(outcome.next_actions.len(), 2);
        assert!(
            outcome.next_actions[0]
                .command
                .contains("category list --parent 10")
        );
        assert_eq!(
            outcome.next_actions.last().unwrap().command,
            "flea vinted --portal fi category search --title='Seller'\\''s bag' --description='Line one\nLine two' --parent 10 --limit 1 --offset 1 -- 'Asusteet'"
        );
        assert!(
            !outcome
                .next_actions
                .iter()
                .any(|action| action.command.contains("compose 10"))
        );
    }

    #[tokio::test]
    async fn invalid_search_is_rejected_before_credentials_or_requests() {
        let api = api(json!({"catalog_ids":[]}));
        let session = |_| -> Result<VintedCredentialRecord, AppError> {
            panic!("must validate before credentials")
        };
        let search_api = empty_search_api();
        for keyword in ["   ".to_owned(), "!!!".to_owned(), "ä".repeat(129)] {
            assert!(
                execute(
                    PortalId::Fi,
                    VintedCategoryCommand::Search {
                        keyword,
                        title: None,
                        description: None,
                        parent: None,
                        limit: 20,
                        offset: 0,
                    },
                    &session,
                    &api,
                    &search_api
                )
                .await
                .is_err()
            );
        }
        assert!(api.requests.lock().unwrap().is_empty());
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
