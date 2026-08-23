use clap::{Args, Subcommand};
use serde::Serialize;

use crate::{
    cli::outcome::CommandOutcome,
    domain::envelope::NextAction,
    error::AppError,
    marketplace::tori::listings::{
        CATEGORY_SEARCH_LIMIT_DEFAULT, CATEGORY_SEARCH_LIMIT_MAX, CategorySearchOptions, Listings,
        ListingsApi,
    },
};

#[derive(Debug, Args)]
pub struct CategoryArgs {
    #[command(subcommand)]
    pub command: CategoryCommand,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum CategoryCommand {
    #[command(
        about = "Search categories (authentication required)",
        long_about = "Search and rank Tori categories by ID, label, and hierarchy context. Authentication is required. If you are not signed in, run `flea tori auth login` before searching. Each result contains a category_id for listing workflows and a canonical taxonomy_value accepted by `flea tori search --category`. Results default to 20 and --limit accepts 1 through 100.",
        after_long_help = "Search-filter example:\n  flea tori category search tietokonekomponentit\n  flea tori search --category 2.93.3215.8368\n\nBroad-query refinement:\n  flea tori category search tarvikkeet\n  flea tori category search tarvikkeet --path 'Urheilu ja ulkoilu > Pyöräily'\n\nUse --parent with a category_id or --path with a returned category path to search its descendants. Follow next_actions to continue a truncated search."
    )]
    Search {
        /// Category ID or text used to match category labels and paths.
        query: String,

        /// Restrict matches to descendants of this category machine value.
        #[arg(long, conflicts_with = "path")]
        parent: Option<String>,

        /// Restrict matches to descendants of this exact label or category path.
        #[arg(long, conflicts_with = "parent")]
        path: Option<String>,

        /// Results to return, from 1 through 100.
        #[arg(
            long,
            default_value_t = CATEGORY_SEARCH_LIMIT_DEFAULT,
            value_parser = parse_category_search_limit
        )]
        limit: usize,

        /// Zero-based result offset. Follow next_actions to preserve search context.
        #[arg(long, default_value_t = 0)]
        offset: usize,
    },
    #[command(
        about = "List categories (authentication required)",
        long_about = "List root categories or the direct children of a parent category machine value. Authentication is required. If you are not signed in, run `flea tori auth login` before listing. Each result contains a category_id for listing workflows and a canonical taxonomy_value accepted by `flea tori search --category`."
    )]
    List {
        /// Parent category machine value whose children to list.
        #[arg(long)]
        parent: Option<String>,
    },
}

pub async fn dispatch_with_api(
    command: CategoryArgs,
    api: &dyn ListingsApi,
) -> Result<CommandOutcome, AppError> {
    let listings = Listings::new(api);
    match command.command {
        CategoryCommand::Search {
            query,
            parent,
            path,
            limit,
            offset,
        } => {
            let result = listings
                .search_categories_with_options(
                    &query,
                    CategorySearchOptions {
                        parent: parent.as_deref(),
                        path: path.as_deref(),
                        offset,
                        limit,
                    },
                )
                .await?;
            let value = serde_json::to_value(&result)
                .map_err(|error| AppError::output(error.to_string()))?;
            let next_actions = result
                .truncated
                .then(|| NextAction {
                    command: next_page_command(
                        &query,
                        parent.as_deref(),
                        path.as_deref(),
                        offset.saturating_add(result.returned),
                        limit,
                    ),
                })
                .into_iter()
                .collect();
            Ok(CommandOutcome::new(value).with_next_actions(next_actions))
        }
        CategoryCommand::List { parent } => {
            serde_json::to_value(listings.categories(parent.as_deref()).await?)
                .map(CommandOutcome::new)
                .map_err(|error| AppError::output(error.to_string()))
        }
    }
}

fn parse_category_search_limit(value: &str) -> Result<usize, String> {
    let limit = value
        .parse::<usize>()
        .map_err(|_| "limit must be a whole number".to_owned())?;
    (1..=CATEGORY_SEARCH_LIMIT_MAX)
        .contains(&limit)
        .then_some(limit)
        .ok_or_else(|| format!("limit must be between 1 and {CATEGORY_SEARCH_LIMIT_MAX}"))
}

fn next_page_command(
    query: &str,
    parent: Option<&str>,
    path: Option<&str>,
    offset: usize,
    limit: usize,
) -> String {
    let mut parts = vec!["flea tori category search".to_owned(), shell_quote(query)];
    if let Some(parent) = parent {
        parts.push(format!("--parent {}", shell_quote(parent)));
    }
    if let Some(path) = path {
        parts.push(format!("--path {}", shell_quote(path)));
    }
    parts.push(format!("--offset {offset}"));
    parts.push(format!("--limit {limit}"));
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}
