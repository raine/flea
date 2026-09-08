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
                PublicationCategorySuggestion, VintedComposer, VintedComposerReadiness,
                VintedPublicationComposer, publication_attribute_definitions,
                publication_attribute_options, selection_command,
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
        /// Add best-effort marketplace category evidence.
        #[arg(long)]
        marketplace_evidence: bool,
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
        #[arg(long, conflicts_with_all = ["readiness", "field"])]
        full: bool,
        #[arg(long, hide = true, conflicts_with = "full")]
        readiness: bool,
        /// Inspect only this field and its selectable options, e.g. attribute.size.
        #[arg(long, conflicts_with_all = ["full", "readiness"])]
        field: Option<String>,
        /// Maximum options per focused page (1-100, default 20).
        #[arg(long, requires = "field", value_parser = parse_limit)]
        option_limit: Option<usize>,
        /// Number of selectable options to skip.
        #[arg(long, requires = "field")]
        option_offset: Option<usize>,
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
        field,
        option_limit,
        option_offset,
    } = command
    {
        let supplied = input.as_ref().map(read_json).transpose()?;
        let composer = VintedPublicationComposer::new(session, api)
            .compose(portal, category_id, supplied)
            .await?;
        if let Some(field) = field {
            return focused_field_outcome(
                &composer,
                &field,
                input.as_ref(),
                option_limit.unwrap_or(20),
                option_offset.unwrap_or(0),
            );
        }
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
            marketplace_evidence,
            title,
            description,
            parent,
            limit,
            offset,
        } => {
            let options = SearchOptions {
                query: keyword,
                marketplace_evidence: *marketplace_evidence,
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
                            *marketplace_evidence,
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
                        *marketplace_evidence,
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

fn focused_field_outcome(
    composer: &VintedComposer,
    key: &str,
    input: Option<&PathBuf>,
    limit: usize,
    offset: usize,
) -> Result<CommandOutcome, AppError> {
    parse_limit(&limit.to_string()).map_err(AppError::usage)?;
    let mut field = composer
        .form
        .fields
        .iter()
        .find(|field| field.key == key)
        .cloned()
        .ok_or_else(|| {
            AppError::usage(format!(
                "Unknown composer field `{key}`. Available fields: {}",
                composer
                    .form
                    .fields
                    .iter()
                    .map(|field| field.key.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ))
        })?;
    field.raw = None;
    let source_options_truncated = field.options_truncated;
    let matching = composer
        .form
        .options
        .iter()
        .filter(|option| option.field == key)
        .collect::<Vec<_>>();
    let total = matching.len();
    let options = matching
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|option| {
            let mut option = option.clone();
            option.raw = None;
            option
        })
        .collect::<Vec<_>>();
    let returned = options.len();
    let truncated = offset.saturating_add(returned) < total;
    field.options_returned = returned;
    field.options_truncated = source_options_truncated || truncated;
    let issues = composer
        .form
        .issues
        .iter()
        .filter(|issue| issue.field == key)
        .map(|issue| {
            let mut issue = issue.clone();
            issue.raw = None;
            issue
        })
        .collect::<Vec<_>>();
    let stdin_input = input.is_some_and(|path| path.as_os_str() == "-");
    let mut guidance = "Apply the selected option value to your ListingInput and rerun compose with --input to validate readiness.".to_owned();
    if stdin_input {
        guidance.push_str(" Save the original stdin ListingInput to a file and replace <saved-listing.json> in the continuation before running it.");
    }
    if source_options_truncated {
        guidance.push_str(" The discovered option catalog is incomplete; use the field-specific discovery command (for brands: category brands CATEGORY_ID SEARCH_TEXT).");
    }
    let mut actions = Vec::new();
    if truncated {
        let input_arg = input
            .map(|path| {
                let path = if stdin_input {
                    "<saved-listing.json>".into()
                } else {
                    path.to_string_lossy()
                };
                format!(" --input={}", shell_quote(&path))
            })
            .unwrap_or_default();
        actions.push(NextAction { command: invocation::vinted_fi(format!(
            "category compose {} --field={} --option-limit {limit} --option-offset {}{input_arg}",
            composer.category.id, shell_quote(key), offset.saturating_add(returned)
        )) });
    }
    Ok(CommandOutcome::new(CommandData::Raw(json!({
        "scope": composer.scope,
        "category": composer.category,
        "field": field,
        "issues": issues,
        "options": options,
        "attribute_selection_payload": composer.attribute_selection_payload,
        "limit": limit,
        "offset": offset,
        "total": total,
        "returned": returned,
        "truncated": truncated,
        "source_options_truncated": source_options_truncated,
        "guidance": guidance,
    })))
    .with_next_actions(actions))
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
    marketplace_evidence: bool,
    title: Option<&str>,
    description: Option<&str>,
    parent: Option<u64>,
    limit: usize,
    offset: usize,
) -> String {
    let mut command = "category search".to_owned();
    if marketplace_evidence {
        command.push_str(" --marketplace-evidence");
    }
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

    fn focused_fixture() -> VintedComposer {
        use crate::domain::{
            field::{Field, FieldOption, FieldType, Requirement},
            publication_form::PublicationForm,
        };
        let definition = json!({"groups":[{"id":99,"title":"Heading","options":[
            {"id":100,"title":"Separator","type":"heading"},
            {"id":591,"title":"22"}, {"id":592,"title":"23"}
        ]}]});
        let mut form = PublicationForm::default();
        for key in ["attribute.size", "attribute.condition", "brand"] {
            let mut field = Field::new(
                key,
                key,
                FieldType::MultiSelect,
                Requirement::Required,
                None,
                "attributes",
            );
            field.raw = Some(json!({"bulky":"raw schema"}));
            form.fields.push(field);
        }
        for option in publication_attribute_options(definition.as_object().unwrap()) {
            form.options.push(FieldOption {
                field: "attribute.size".into(),
                value: option["id"].clone(),
                label: option["title"].as_str().unwrap().into(),
                raw: Some(option.clone()),
            });
        }
        form.options.push(FieldOption {
            field: "brand".into(),
            value: json!(1),
            label: "Bulky brand".into(),
            raw: Some(json!({"large":true})),
        });
        form.values.insert("attribute.size".into(), json!([591]));
        form.validate();
        VintedComposer {
            scope: DiscoveryScope::Selection,
            category: crate::marketplace::vinted::composer::PublicationCategory {
                id: 2749,
                title: "Shoes".into(),
                path: vec![],
                leaf: true,
            },
            attribute_selection_payload: json!([{"code":"category","value":[2749]},{"code":"size","value":[591]}]),
            form,
            brand_validation: None,
            suggestions: vec![],
            issue_actions: vec![],
            semantic_resolutions: vec![],
            listing_input: None,
            normalized_input: None,
        }
    }

    #[test]
    fn focused_options_are_bounded_and_exclude_other_fields_and_raw_data() {
        let composer = focused_fixture();
        let before = serde_json::to_value(&composer).unwrap();
        let path = PathBuf::from("seller's listing.json");
        let outcome =
            focused_field_outcome(&composer, "attribute.size", Some(&path), 1, 0).unwrap();
        let data = serde_json::to_value(outcome.data).unwrap();
        assert_eq!(
            data["options"],
            json!([{"field":"attribute.size","value":591,"label":"22"}])
        );
        assert_eq!(data["field"]["value"], json!([591]));
        assert_eq!(data["total"], 2);
        assert_eq!(data["returned"], 1);
        assert_eq!(data["truncated"], true);
        assert_eq!(data["issues"], json!([]));
        assert!(data["field"].get("raw").is_none());
        assert!(data.get("form").is_none());
        assert_eq!(
            data["attribute_selection_payload"],
            composer.attribute_selection_payload
        );
        assert_eq!(outcome.next_actions.len(), 1);
        assert!(
            outcome.next_actions[0]
                .command
                .contains("--input='seller'\\''s listing.json'")
        );
        assert!(
            outcome.next_actions[0]
                .command
                .contains("--option-offset 1")
        );
        assert_eq!(serde_json::to_value(&composer).unwrap(), before);
        for offset in [2, usize::MAX] {
            let outcome =
                focused_field_outcome(&composer, "attribute.size", None, 1, offset).unwrap();
            let data = serde_json::to_value(outcome.data).unwrap();
            assert_eq!(data["options"], json!([]));
            assert_eq!(data["truncated"], false);
            assert!(outcome.next_actions.is_empty());
        }
    }

    #[test]
    fn focused_stdin_continuation_requires_saved_input_and_unknown_fields_fail() {
        let composer = focused_fixture();
        let outcome =
            focused_field_outcome(&composer, "attribute.size", Some(&PathBuf::from("-")), 1, 0)
                .unwrap();
        let data = serde_json::to_value(outcome.data).unwrap();
        assert!(
            data["guidance"]
                .as_str()
                .unwrap()
                .contains("Save the original stdin")
        );
        assert!(
            outcome.next_actions[0]
                .command
                .contains("--input='<saved-listing.json>'")
        );
        let error = focused_field_outcome(&composer, "size", None, 20, 0).unwrap_err();
        assert!(error.to_string().contains("Unknown composer field `size`"));
        assert!(error.to_string().contains("attribute.size"));
        assert!(focused_field_outcome(&composer, "attribute.size", None, 0, 0).is_err());
        assert!(focused_field_outcome(&composer, "attribute.size", None, 101, 0).is_err());
    }

    #[test]
    fn generated_search_accepts_leading_hyphen_values() {
        use clap::Parser;
        let command = search_command(
            "- shoes",
            true,
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
                marketplace_evidence: false,
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
        assert!(search_api.requests.lock().unwrap().is_empty());
        assert_eq!(
            serde_json::to_value(&result.stages).unwrap()["marketplace"],
            "not_requested"
        );
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
                marketplace_evidence: false,
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
                marketplace_evidence: true,
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
                    marketplace_evidence: true,
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
                marketplace_evidence: false,
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
                        marketplace_evidence: false,
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
