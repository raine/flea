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

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MarketplaceCategoryEvidence {
    pub source: &'static str,
    pub requests: usize,
    pub truncated: bool,
    pub selection_required: bool,
    pub counts: Vec<MarketplaceCategoryCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ambiguous_roots: Vec<String>,
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
    query: &str,
    categories: &[PublicationCategory],
    session: &dyn VintedSearchSession,
    api: &dyn VintedSearchApi,
) -> Result<MarketplaceCategoryEvidenceResult, AppError> {
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
                        query: Some(query.to_owned()),
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
    ranked = diversify_roots(ranked, MAX_CANDIDATES);

    let mut roots = ranked
        .iter()
        .filter_map(|(category, _)| category.path.first().cloned())
        .collect::<Vec<_>>();
    roots.sort();
    roots.dedup();
    if roots.len() < 2 {
        roots.clear();
    }
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
            counts,
            ambiguous_roots: roots,
        },
    })
}

fn diversify_roots(
    ranked: Vec<(PublicationCategory, i64)>,
    limit: usize,
) -> Vec<(PublicationCategory, i64)> {
    let mut root_order = Vec::new();
    let mut groups = BTreeMap::<String, VecDeque<(PublicationCategory, i64)>>::new();
    for (category, hits) in ranked {
        let root = category.path.first().cloned().unwrap_or_default();
        if !groups.contains_key(&root) {
            root_order.push(root.clone());
        }
        groups.entry(root).or_default().push_back((category, hits));
    }

    let mut selected = Vec::new();
    while selected.len() < limit {
        let mut added = false;
        for root in &root_order {
            if let Some(candidate) = groups.get_mut(root).and_then(VecDeque::pop_front) {
                selected.push(candidate);
                added = true;
                if selected.len() == limit {
                    break;
                }
            }
        }
        if !added {
            break;
        }
    }
    selected
}

#[cfg(test)]
mod tests {
    use super::*;

    fn category(id: u64, root: &str) -> PublicationCategory {
        PublicationCategory {
            id,
            title: format!("Category {id}"),
            path: vec![root.to_owned()],
            leaf: true,
        }
    }

    #[test]
    fn candidate_limit_preserves_each_audience_root() {
        let ranked = vec![
            (category(1, "Naiset"), 100),
            (category(2, "Naiset"), 90),
            (category(3, "Naiset"), 80),
            (category(4, "Miehet"), 20),
            (category(5, "Lapset"), 10),
        ];

        let selected = diversify_roots(ranked, 3);

        assert_eq!(
            selected
                .iter()
                .map(|(category, _)| category.id)
                .collect::<Vec<_>>(),
            [1, 4, 5]
        );
    }
}
