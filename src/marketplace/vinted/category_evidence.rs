use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;

use crate::{
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            composer::PublicationCategory,
            search::{
                FilterContextInput, FilterRequest, SearchResult, VintedSearch, VintedSearchApi,
                VintedSearchSession,
            },
        },
    },
};

const MAX_FACET_REQUESTS: usize = 16;
const MAX_CANDIDATES: usize = 8;
const MIN_BRANCH_HITS: i64 = 5;

#[derive(Clone, Copy, Debug)]
pub struct RecommendationContext<'a> {
    pub keyword: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MarketplaceCategoryEvidence {
    pub source: &'static str,
    pub requests: usize,
    pub truncated: bool,
    pub selection_required: bool,
    pub context_fields: Vec<&'static str>,
    pub recommendations: Vec<MarketplaceCategoryRecommendation>,
    pub counts: Vec<MarketplaceCategoryCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguous_roots: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MarketplaceCategoryRecommendation {
    pub category_id: u64,
    pub score: u8,
    pub evidence: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct MarketplaceCategoryCount {
    pub category_id: u64,
    pub listings: i64,
}

pub struct MarketplaceCategoryEvidenceResult {
    pub categories: Vec<PublicationCategory>,
    pub evidence: MarketplaceCategoryEvidence,
}

pub async fn discover(
    portal: PortalId,
    context: RecommendationContext<'_>,
    categories: &[PublicationCategory],
    session: &dyn VintedSearchSession,
    api: &dyn VintedSearchApi,
) -> Result<MarketplaceCategoryEvidenceResult, AppError> {
    let (query, context_fields) = recommendation_query(context);
    let by_id = categories
        .iter()
        .cloned()
        .map(|category| (category.id, category))
        .collect::<BTreeMap<_, _>>();
    let search = VintedSearch::new(session, api);
    let mut frontier = VecDeque::<Option<u64>>::from([None]);
    let mut queued = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut leaf_hits = BTreeMap::<u64, i64>::new();
    let mut requests = 0;

    while requests < MAX_FACET_REQUESTS {
        let Some(parent_id) = frontier.pop_front() else {
            break;
        };
        if let Some(parent_id) = parent_id
            && !visited.insert(parent_id)
        {
            continue;
        }
        requests += 1;
        let result = search
            .execute_filter(
                portal,
                FilterRequest::Facets {
                    code: "catalog".to_owned(),
                    context: FilterContextInput {
                        query: Some(query.clone()),
                        catalog: parent_id.map(|id| vec![id.to_string()]).unwrap_or_default(),
                        ..FilterContextInput::default()
                    },
                    option_limit: Some(500),
                    raw: false,
                },
            )
            .await?;
        let SearchResult::Filters(collection) = result else {
            return Err(AppError::unexpected(
                "Vinted category facets returned an incompatible result",
            ));
        };
        let mut options = collection
            .filters
            .into_iter()
            .find(|filter| filter.name == "catalog")
            .map(|filter| filter.options)
            .unwrap_or_default();
        options.sort_by(|left, right| {
            right
                .hits
                .unwrap_or_default()
                .cmp(&left.hits.unwrap_or_default())
                .then_with(|| left.value.cmp(&right.value))
        });

        for option in options {
            let hits = option.hits.unwrap_or_default();
            if hits <= 0 {
                continue;
            }
            let Ok(id) = option.value.parse::<u64>() else {
                continue;
            };
            let Some(category) = by_id.get(&id) else {
                continue;
            };
            if category.leaf {
                leaf_hits
                    .entry(id)
                    .and_modify(|current| *current = (*current).max(hits))
                    .or_insert(hits);
            } else if hits >= MIN_BRANCH_HITS && queued.insert(id) {
                frontier.push_back(Some(id));
            }
        }
    }

    let truncated = !frontier.is_empty();
    let mut ranked = leaf_hits
        .into_iter()
        .filter_map(|(id, listings)| by_id.get(&id).cloned().map(|category| (category, listings)))
        .collect::<Vec<_>>();
    ranked.sort_by(|(left, left_hits), (right, right_hits)| {
        right_hits
            .cmp(left_hits)
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.id.cmp(&right.id))
    });
    ranked.truncate(MAX_CANDIDATES);

    let mut roots = ranked
        .iter()
        .filter_map(|(category, _)| category.path.first().cloned())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    if roots.len() < 2 {
        roots.clear();
    }
    let strongest = ranked
        .first()
        .map(|(_, listings)| *listings)
        .unwrap_or_default();
    let recommendations = ranked
        .iter()
        .map(|(category, listings)| {
            let score = relative_score(*listings, strongest);
            MarketplaceCategoryRecommendation {
                category_id: category.id,
                score,
                evidence: format!(
                    "{listings} matching listings; {score}% of strongest live category signal"
                ),
            }
        })
        .collect();
    let counts = ranked
        .iter()
        .map(|(category, listings)| MarketplaceCategoryCount {
            category_id: category.id,
            listings: *listings,
        })
        .collect();
    let categories = ranked.into_iter().map(|(category, _)| category).collect();

    Ok(MarketplaceCategoryEvidenceResult {
        categories,
        evidence: MarketplaceCategoryEvidence {
            source: "vinted_category_facets",
            requests,
            truncated,
            selection_required: true,
            context_fields,
            recommendations,
            counts,
            ambiguous_roots: roots,
        },
    })
}

fn recommendation_query(context: RecommendationContext<'_>) -> (String, Vec<&'static str>) {
    let mut fields = Vec::new();
    let mut parts = Vec::new();
    for (field, value) in [
        ("keyword", Some(context.keyword)),
        ("title", context.title),
        ("description", context.description),
    ] {
        let Some(value) = value else {
            continue;
        };
        let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
        if normalized.is_empty()
            || parts
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&normalized))
        {
            continue;
        }
        fields.push(field);
        parts.push(normalized);
    }
    if parts.iter().map(String::len).sum::<usize>() + parts.len().saturating_sub(1) > 256 {
        let budget = (256 - parts.len().saturating_sub(1)) / parts.len();
        for part in &mut parts {
            truncate_utf8(part, budget);
        }
    }
    (parts.join(" "), fields)
}

fn truncate_utf8(value: &mut String, max_bytes: usize) {
    if value.len() <= max_bytes {
        return;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
}

fn relative_score(listings: i64, strongest: i64) -> u8 {
    if listings <= 0 || strongest <= 0 {
        return 0;
    }
    ((listings.saturating_mul(100) + strongest / 2) / strongest).clamp(1, 100) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recommendation_query_uses_all_nonempty_listing_text() {
        let (query, fields) = recommendation_query(RecommendationContext {
            keyword: "Vibram FiveFingers",
            title: Some("Men's trail running shoes"),
            description: Some("Barefoot shoes with rugged soles"),
        });

        assert_eq!(
            query,
            "Vibram FiveFingers Men's trail running shoes Barefoot shoes with rugged soles"
        );
        assert_eq!(fields, ["keyword", "title", "description"]);
    }

    #[test]
    fn recommendation_query_bounds_each_utf8_context_field() {
        let keyword = "Vibram ".repeat(40);
        let title = "juoksukengät ".repeat(40);
        let description = "maastojuoksuun ".repeat(40);
        let (query, _) = recommendation_query(RecommendationContext {
            keyword: &keyword,
            title: Some(&title),
            description: Some(&description),
        });

        assert!(query.len() <= 256);
        assert!(query.starts_with("Vibram"));
        assert!(query.contains("juoksukengät"));
        assert!(query.contains("maastojuoksuun"));
    }

    #[test]
    fn scores_explain_relative_live_support() {
        assert_eq!(relative_score(60, 60), 100);
        assert_eq!(relative_score(40, 60), 67);
        assert_eq!(relative_score(1, 1000), 1);
        assert_eq!(relative_score(0, 60), 0);
    }
}
