use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;

use super::{ListingsApiError, resource_not_found, upstream_error};
use crate::{
    domain::listing::{Category, CategoryList, CategorySearchContext, CategorySearchResult},
    domain::observation::{Observation, ObservationOperation},
    error::AppError,
    retry::{OperationMethod, RetryContext},
};

pub const CATEGORY_SEARCH_LIMIT_DEFAULT: usize = 20;
pub const CATEGORY_SEARCH_LIMIT_MAX: usize = 100;

pub trait TaxonomyApi: Send + Sync {
    fn categories(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<UpstreamCategory>, ListingsApiError>> + Send + '_>>;
}

pub struct Taxonomy<'a> {
    api: &'a dyn TaxonomyApi,
}

impl<'a> Taxonomy<'a> {
    pub fn new(api: &'a dyn TaxonomyApi) -> Self {
        Self { api }
    }

    pub async fn categories(&self, parent: Option<&str>) -> Result<CategoryList, AppError> {
        let categories = self.api.categories().await.map_err(category_error)?;
        let flattened = flatten_categories(&categories).map_err(category_protocol_error)?;

        if let Some(parent_id) = parent
            && !flattened
                .iter()
                .any(|category| category.category_id == parent_id)
        {
            return Err(resource_not_found(
                "category.not_found",
                "category",
                parent_id,
            ));
        }

        Ok(CategoryList {
            categories: flattened
                .into_iter()
                .filter(|category| category.parent_id.as_deref() == parent)
                .collect(),
        })
    }

    pub async fn search_categories(&self, query: &str) -> Result<CategorySearchResult, AppError> {
        self.search_categories_with_options(query, CategorySearchOptions::default())
            .await
    }

    pub async fn search_categories_with_options(
        &self,
        query: &str,
        options: CategorySearchOptions<'_>,
    ) -> Result<CategorySearchResult, AppError> {
        let query = query.trim();
        let normalized_query = normalize_category_text(query);
        let query_tokens = category_tokens(&normalized_query);
        if normalized_query.is_empty() || query_tokens.is_empty() {
            return Err(AppError::usage(
                "category search query must contain letters or numbers",
            ));
        }
        if !(1..=CATEGORY_SEARCH_LIMIT_MAX).contains(&options.limit) {
            return Err(AppError::usage(format!(
                "--limit must be between 1 and {CATEGORY_SEARCH_LIMIT_MAX}"
            )));
        }
        if options.parent.is_some() && options.path.is_some() {
            return Err(AppError::usage(
                "--parent and --path cannot be used together",
            ));
        }

        let categories = self.api.categories().await.map_err(category_error)?;
        let flattened = flatten_categories(&categories).map_err(category_protocol_error)?;
        let context = resolve_category_context(&flattened, options.parent, options.path)?;
        let parents = flattened
            .iter()
            .map(|category| (category.category_id.clone(), category.parent_id.clone()))
            .collect::<HashMap<_, _>>();
        let mut scored = flattened
            .into_iter()
            .filter(|category| {
                context.as_ref().is_none_or(|context| {
                    is_category_descendant(&category.category_id, &context.category_id, &parents)
                })
            })
            .filter_map(|category| {
                category_rank(&category, &normalized_query, &query_tokens).map(|rank| {
                    let normalized_path = normalize_category_text(&category.path);
                    (rank, normalized_path, category)
                })
            })
            .collect::<Vec<_>>();
        scored.sort_by(
            |(left_rank, left_path, left), (right_rank, right_path, right)| {
                left_rank
                    .cmp(right_rank)
                    .then_with(|| left_path.cmp(right_path))
                    .then_with(|| left.path.cmp(&right.path))
                    .then_with(|| left.category_id.cmp(&right.category_id))
            },
        );

        let total = scored.len();
        let categories = scored
            .into_iter()
            .skip(options.offset)
            .take(options.limit)
            .map(|(_, _, category)| category)
            .collect::<Vec<_>>();
        let returned = categories.len();
        let truncated = options.offset.saturating_add(returned) < total;

        Ok(CategorySearchResult {
            categories,
            query: query.to_owned(),
            context,
            offset: options.offset,
            limit: options.limit,
            returned,
            total,
            truncated,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamCategoryTaxonomy {
    pub categories: Vec<UpstreamCategory>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamCategory {
    #[serde(alias = "category_id", deserialize_with = "deserialize_category_id")]
    pub id: String,
    pub label: String,
    #[serde(default, alias = "parent_id")]
    pub parent_id: Option<String>,
    #[serde(default, alias = "isSelectable")]
    pub selectable: Option<bool>,
    #[serde(default)]
    pub children: Vec<UpstreamCategory>,
}

fn deserialize_category_id<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    match Value::deserialize(deserializer)? {
        Value::String(id) => Ok(id),
        Value::Number(id) if id.is_u64() => Ok(id.to_string()),
        _ => Err(serde::de::Error::custom(
            "category ID must be a string or unsigned integer",
        )),
    }
}

#[derive(Clone, Copy, Debug)]
pub struct CategorySearchOptions<'a> {
    pub parent: Option<&'a str>,
    pub path: Option<&'a str>,
    pub offset: usize,
    pub limit: usize,
}

impl Default for CategorySearchOptions<'_> {
    fn default() -> Self {
        Self {
            parent: None,
            path: None,
            offset: 0,
            limit: CATEGORY_SEARCH_LIMIT_DEFAULT,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct CategoryRank {
    class: u8,
    distance: usize,
}

fn resolve_category_context(
    categories: &[Category],
    parent: Option<&str>,
    path: Option<&str>,
) -> Result<Option<CategorySearchContext>, AppError> {
    if let Some(parent_id) = parent {
        return categories
            .iter()
            .find(|category| category.category_id == parent_id)
            .map(category_search_context)
            .map(Some)
            .ok_or_else(|| resource_not_found("category.not_found", "category", parent_id));
    }

    let Some(path) = path else {
        return Ok(None);
    };
    let normalized_path = normalize_category_text(path);
    let context_segments = category_path_segments(&normalized_path);
    if context_segments.is_empty() {
        return Err(AppError::usage("--path must not be empty"));
    }
    let matches = categories
        .iter()
        .filter(|category| {
            let candidate = normalize_category_text(&category.path);
            category_path_segments(&candidate).ends_with(&context_segments)
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [] => Err(AppError::validation(
            "category.path_not_found",
            "category path context was not found",
        )
        .with_details(json!({ "path": path }))),
        [category] => Ok(Some(category_search_context(category))),
        _ => Err(AppError::validation(
            "category.path_ambiguous",
            "category path context is ambiguous",
        )
        .with_details(json!({
            "path": path,
            "matches": matches
                .into_iter()
                .map(category_search_context)
                .collect::<Vec<_>>()
        }))),
    }
}

fn category_search_context(category: &Category) -> CategorySearchContext {
    CategorySearchContext {
        category_id: category.category_id.clone(),
        taxonomy_value: category.taxonomy_value.clone(),
        label: category.label.clone(),
        path: category.path.clone(),
    }
}

fn is_category_descendant(
    category_id: &str,
    ancestor_id: &str,
    parents: &HashMap<String, Option<String>>,
) -> bool {
    let mut parent = parents.get(category_id).and_then(Option::as_deref);
    while let Some(parent_id) = parent {
        if parent_id == ancestor_id {
            return true;
        }
        parent = parents.get(parent_id).and_then(Option::as_deref);
    }
    false
}

fn category_rank(
    category: &Category,
    query: &str,
    query_tokens: &[String],
) -> Option<CategoryRank> {
    if category.category_id == query {
        return Some(CategoryRank {
            class: 0,
            distance: 0,
        });
    }

    let label = normalize_category_text(&category.label);
    let path = normalize_category_text(&category.path);
    let label_tokens = category_tokens(&label);
    let path_segments = category_path_segments(&path);
    if label == query {
        return Some(CategoryRank {
            class: 1,
            distance: 0,
        });
    }
    if path == query {
        return Some(CategoryRank {
            class: 2,
            distance: 0,
        });
    }
    if label.starts_with(query) {
        return Some(CategoryRank {
            class: 3,
            distance: 0,
        });
    }
    if tokens_contain_all(&label_tokens, query_tokens) {
        return Some(CategoryRank {
            class: 4,
            distance: 0,
        });
    }

    let ancestors = path_segments
        .get(..path_segments.len().saturating_sub(1))
        .unwrap_or_default();
    if let Some(distance) = closest_segment_match(ancestors, |segment| segment == query) {
        return Some(CategoryRank { class: 5, distance });
    }
    if let Some(distance) = closest_segment_match(ancestors, |segment| segment.starts_with(query)) {
        return Some(CategoryRank { class: 6, distance });
    }
    if let Some(distance) = path_token_distance(&path_segments, query_tokens) {
        return Some(CategoryRank { class: 7, distance });
    }
    if label.contains(query) {
        return Some(CategoryRank {
            class: 8,
            distance: 0,
        });
    }
    if equivalent_category_terms(query_tokens)
        .iter()
        .any(|term| label.contains(term))
    {
        return Some(CategoryRank {
            class: 9,
            distance: 0,
        });
    }
    if path.contains(query) {
        return Some(CategoryRank {
            class: 10,
            distance: 0,
        });
    }
    query_tokens
        .iter()
        .all(|token| path.contains(token))
        .then_some(CategoryRank {
            class: 11,
            distance: 0,
        })
}

fn normalize_category_text(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn category_tokens(value: &str) -> Vec<String> {
    value
        .split(|character: char| !character.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(str::to_owned)
        .collect()
}

fn category_path_segments(path: &str) -> Vec<&str> {
    path.split('>')
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect()
}

fn tokens_contain_all(haystack: &[String], needles: &[String]) -> bool {
    !needles.is_empty() && needles.iter().all(|needle| haystack.contains(needle))
}

fn closest_segment_match(ancestors: &[&str], predicate: impl Fn(&str) -> bool) -> Option<usize> {
    ancestors
        .iter()
        .rev()
        .position(|segment| predicate(segment))
        .map(|index| index + 1)
}

fn path_token_distance(segments: &[&str], query_tokens: &[String]) -> Option<usize> {
    if query_tokens.is_empty() {
        return None;
    }
    query_tokens
        .iter()
        .map(|query_token| {
            segments
                .iter()
                .rev()
                .position(|segment| category_tokens(segment).contains(query_token))
        })
        .collect::<Option<Vec<_>>>()
        .map(|distances| distances.into_iter().max().unwrap_or_default())
}

fn equivalent_category_terms(query_tokens: &[String]) -> &'static [&'static str] {
    match query_tokens {
        [term] if term == "tarvike" => &["varuste"],
        [term] if term == "tarvikkeet" => &["varusteet"],
        _ => &[],
    }
}

fn flatten_categories(roots: &[UpstreamCategory]) -> Result<Vec<Category>, String> {
    fn visit(
        nodes: &[UpstreamCategory],
        inherited_parent: Option<&str>,
        parent_path: &str,
        ancestor_ids: &[String],
        seen: &mut HashSet<String>,
        output: &mut Vec<Category>,
    ) -> Result<(), String> {
        for node in nodes {
            if node.id.trim().is_empty() || !node.id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err("category taxonomy contains an invalid ID".to_owned());
            }
            if node.label.trim().is_empty() {
                return Err("category taxonomy contains an empty label".to_owned());
            }
            if !seen.insert(node.id.clone()) {
                return Err("category taxonomy contains a duplicate ID".to_owned());
            }
            if let Some(parent_id) = node.parent_id.as_deref()
                && Some(parent_id) != inherited_parent
            {
                return Err("category taxonomy contains an inconsistent parent ID".to_owned());
            }
            let path = if parent_path.is_empty() {
                node.label.clone()
            } else {
                format!("{parent_path} > {}", node.label)
            };
            let mut taxonomy_ids = ancestor_ids.to_vec();
            taxonomy_ids.push(node.id.clone());
            let taxonomy_value = format!("{}.{}", taxonomy_ids.len() - 1, taxonomy_ids.join("."));
            output.push(Category {
                category_id: node.id.clone(),
                taxonomy_value,
                label: node.label.clone(),
                parent_id: inherited_parent.map(ToOwned::to_owned),
                path: path.clone(),
                selectable: node.selectable.unwrap_or(node.children.is_empty()),
            });
            visit(
                &node.children,
                Some(&node.id),
                &path,
                &taxonomy_ids,
                seen,
                output,
            )?;
        }
        Ok(())
    }

    if roots.is_empty() {
        return Err("category taxonomy is empty".to_owned());
    }
    let mut output = Vec::new();
    visit(roots, None, "", &[], &mut HashSet::new(), &mut output)?;
    Ok(output)
}

fn category_error(error: ListingsApiError) -> AppError {
    let read = RetryContext::read(OperationMethod::Get);
    match error {
        ListingsApiError::Authentication => AppError::authentication(
            "category.authentication_failed",
            "Tori rejected authentication for category discovery",
        ),
        ListingsApiError::NotFound => AppError::upstream(
            "category.endpoint_unavailable",
            "Tori's category taxonomy endpoint is unavailable",
        )
        .with_observation(
            Observation::unrecognized_response("category_taxonomy", Some(404)),
            ObservationOperation::Read,
        ),
        ListingsApiError::UnexpectedResponse(message) => category_protocol_error(message),
        other => upstream_error(other, read),
    }
}

fn category_protocol_error(_message: String) -> AppError {
    AppError::upstream(
        "category.protocol_drift",
        "Tori returned an unexpected category taxonomy response",
    )
    .with_observation(
        Observation::unrecognized_response("category_taxonomy", Some(200)),
        ObservationOperation::Read,
    )
}
