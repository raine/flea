use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;
use unicode_normalization::UnicodeNormalization;

use super::{
    category_evidence::{self, MarketplaceCategoryEvidence, RecommendationContext},
    composer::{
        PublicationCategory, PublicationCategorySuggestion, category_ids_from_search,
        category_suggestions_from_search,
    },
    publication_discovery::{DiscoveryRequest, VintedPublicationDiscoveryApi},
    search::{VintedSearchApi, VintedSearchSession},
};
use crate::{domain::envelope::Warning, error::AppError, marketplace::PortalId};

pub const DEFAULT_LIMIT: usize = 20;
pub const MAX_LIMIT: usize = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CategoryNode {
    #[serde(flatten)]
    pub category: PublicationCategory,
    pub parent_id: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchSource {
    CatalogExact,
    CatalogPathTokens,
    PublicationSearch,
    Marketplace,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CategoryCandidate {
    #[serde(flatten)]
    pub node: CategoryNode,
    pub match_sources: Vec<MatchSource>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CategoryPage {
    pub categories: Vec<CategoryCandidate>,
    pub parent: Option<CategoryNode>,
    pub limit: usize,
    pub offset: usize,
    pub total: usize,
    pub returned: usize,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StageStatus {
    Complete,
    Ok,
    Empty,
    Unavailable,
    NotRequested,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DiscoveryStages {
    pub local_catalog: StageStatus,
    pub publication_search: StageStatus,
    pub marketplace: StageStatus,
}

#[derive(Debug, Serialize)]
pub struct CategoryDiscovery {
    pub page: CategoryPage,
    pub suggestions: Vec<PublicationCategorySuggestion>,
    pub marketplace_evidence: Option<MarketplaceCategoryEvidence>,
    pub stages: DiscoveryStages,
    pub warnings: Vec<Warning>,
    pub selection_required: bool,
}

#[derive(Clone, Copy)]
pub struct SearchOptions<'a> {
    pub query: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
    pub parent_id: Option<u64>,
    pub limit: usize,
    pub offset: usize,
}

impl SearchOptions<'_> {
    pub fn validate(&self) -> Result<(), AppError> {
        validate_limit(self.limit)?;
        if self.query.len() > 256 || tokens(self.query).is_empty() {
            return Err(AppError::usage(
                "Category query must contain letters or numbers and be at most 256 UTF-8 bytes",
            ));
        }
        Ok(())
    }
}

pub fn validate_limit(limit: usize) -> Result<(), AppError> {
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(AppError::usage("Category limit must be between 1 and 100"));
    }
    Ok(())
}

pub struct CatalogTree {
    nodes: BTreeMap<u64, CategoryNode>,
}

impl CatalogTree {
    pub fn from_response(response: &Value) -> Result<Self, AppError> {
        let roots = response
            .get("catalogs")
            .and_then(Value::as_array)
            .ok_or_else(invalid_tree)?;
        if roots.is_empty() {
            return Err(invalid_tree());
        }
        let mut nodes = BTreeMap::new();
        for root in roots {
            normalize_node(root, None, &[], &mut nodes)?;
        }
        Ok(Self { nodes })
    }

    pub fn get(&self, id: u64) -> Option<&CategoryNode> {
        self.nodes.get(&id)
    }

    pub fn publication_categories(&self) -> Vec<PublicationCategory> {
        self.nodes
            .values()
            .map(|node| node.category.clone())
            .collect()
    }

    fn parent(&self, parent_id: Option<u64>) -> Result<Option<CategoryNode>, AppError> {
        parent_id
            .map(|id| {
                self.get(id).cloned().ok_or_else(|| {
                    AppError::validation(
                        "vinted.category_not_found",
                        "The parent category is absent from the current publication catalog",
                    )
                })
            })
            .transpose()
    }

    fn in_scope(&self, id: u64, parent_id: Option<u64>) -> bool {
        let Some(parent) = parent_id else {
            return true;
        };
        let mut current = Some(id);
        while let Some(id) = current {
            if id == parent {
                return true;
            }
            current = self.get(id).and_then(|node| node.parent_id);
        }
        false
    }

    pub fn browse(
        &self,
        parent_id: Option<u64>,
        limit: usize,
        offset: usize,
    ) -> Result<CategoryPage, AppError> {
        validate_limit(limit)?;
        let parent = self.parent(parent_id)?;
        let mut candidates = self
            .nodes
            .values()
            .filter(|node| node.parent_id == parent_id)
            .map(|node| CategoryCandidate {
                node: node.clone(),
                match_sources: vec![],
            })
            .collect::<Vec<_>>();
        sort_candidates(&mut candidates);
        Ok(page(candidates, parent, limit, offset))
    }

    #[cfg(test)]
    pub fn search(&self, options: SearchOptions<'_>) -> Result<CategoryPage, AppError> {
        options.validate()?;
        let parent = self.parent(options.parent_id)?;
        Ok(page(
            self.local_matches(options),
            parent,
            options.limit,
            options.offset,
        ))
    }

    fn local_matches(&self, options: SearchOptions<'_>) -> Vec<CategoryCandidate> {
        let query = normalize(options.query);
        let query_tokens = tokens(options.query);
        let mut candidates = self
            .nodes
            .values()
            .filter_map(|node| {
                if !self.in_scope(node.category.id, options.parent_id) {
                    return None;
                }
                let path = node.category.path.join(" > ");
                let source =
                    if normalize(&node.category.title) == query || normalize(&path) == query {
                        MatchSource::CatalogExact
                    } else if query_tokens.is_subset(&tokens(&path)) {
                        MatchSource::CatalogPathTokens
                    } else {
                        return None;
                    };
                Some(CategoryCandidate {
                    node: node.clone(),
                    match_sources: vec![source],
                })
            })
            .collect::<Vec<_>>();
        sort_candidates(&mut candidates);
        candidates
    }
}

fn invalid_tree() -> AppError {
    AppError::upstream(
        "vinted.invalid_catalog_tree",
        "Vinted returned an empty, malformed, or conflicting publication catalog tree",
    )
}

fn normalize_node(
    value: &Value,
    parent_id: Option<u64>,
    parents: &[String],
    nodes: &mut BTreeMap<u64, CategoryNode>,
) -> Result<(), AppError> {
    let object = value.as_object().ok_or_else(invalid_tree)?;
    let id = object
        .get("id")
        .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()))
        .filter(|id| *id > 0)
        .ok_or_else(invalid_tree)?;
    let title = object
        .get("title")
        .and_then(Value::as_str)
        .filter(|title| !title.trim().is_empty())
        .ok_or_else(invalid_tree)?
        .to_owned();
    let mut children = None;
    for key in ["catalogs", "children", "subcategories", "subcatalogs"] {
        if let Some(value) = object.get(key) {
            if children.is_some() {
                return Err(invalid_tree());
            }
            children = Some(value.as_array().ok_or_else(invalid_tree)?);
        }
    }
    // An omitted child collection represents a minimized terminal node.
    let children = children.map(Vec::as_slice).unwrap_or(&[]);
    let mut explicit_nonleaf = false;
    for key in ["leaf", "is_leaf", "is_leaf_catalog"] {
        if let Some(value) = object.get(key) {
            explicit_nonleaf |= !value.as_bool().ok_or_else(invalid_tree)?;
        }
    }
    let mut path = parents.to_vec();
    path.push(title.clone());
    let node = CategoryNode {
        category: PublicationCategory {
            id,
            title,
            path: path.clone(),
            leaf: children.is_empty() && !explicit_nonleaf,
        },
        parent_id,
    };
    if nodes.insert(id, node).is_some() {
        return Err(invalid_tree());
    }
    for child in children {
        normalize_node(child, Some(id), &path, nodes)?;
    }
    Ok(())
}

fn normalize(value: &str) -> String {
    value
        .nfkc()
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
fn tokens(value: &str) -> BTreeSet<String> {
    normalize(value)
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}
fn sort_candidates(candidates: &mut [CategoryCandidate]) {
    candidates.sort_by(|a, b| {
        a.match_sources
            .first()
            .cmp(&b.match_sources.first())
            .then_with(|| a.node.category.path.len().cmp(&b.node.category.path.len()))
            .then_with(|| a.node.category.path.cmp(&b.node.category.path))
            .then_with(|| a.node.category.id.cmp(&b.node.category.id))
    });
}
fn page(
    candidates: Vec<CategoryCandidate>,
    parent: Option<CategoryNode>,
    limit: usize,
    offset: usize,
) -> CategoryPage {
    let total = candidates.len();
    let categories = candidates
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let returned = categories.len();
    CategoryPage {
        categories,
        parent,
        limit,
        offset,
        total,
        returned,
        truncated: offset.saturating_add(returned) < total,
    }
}

pub async fn discover(
    portal: PortalId,
    options: SearchOptions<'_>,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationDiscoveryApi,
    search_api: &dyn VintedSearchApi,
) -> Result<CategoryDiscovery, AppError> {
    options.validate()?;
    let credentials = session.credentials(portal).await?;
    let response = api
        .execute(&credentials, &DiscoveryRequest::Catalogs)
        .await?;
    let tree = CatalogTree::from_response(&response)?;
    discover_in_tree(portal, options, &tree, session, api, search_api).await
}

pub async fn discover_in_tree(
    portal: PortalId,
    options: SearchOptions<'_>,
    tree: &CatalogTree,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationDiscoveryApi,
    search_api: &dyn VintedSearchApi,
) -> Result<CategoryDiscovery, AppError> {
    options.validate()?;
    let parent = tree.parent(options.parent_id)?;
    let mut candidates = tree.local_matches(options);
    let mut warnings = Vec::new();
    let mut suggestions = Vec::new();
    let mut stages = DiscoveryStages {
        local_catalog: StageStatus::Complete,
        publication_search: StageStatus::NotRequested,
        marketplace: StageStatus::NotRequested,
    };
    let keyword_result = async {
        let credentials = session.credentials(portal).await?;
        api.execute(
            &credentials,
            &DiscoveryRequest::SearchCatalog {
                keyword: options.query.to_owned(),
            },
        )
        .await
    }
    .await;
    match keyword_result {
        Ok(response) => {
            suggestions = category_suggestions_from_search(&response);
            let ids = category_ids_from_search(&response);
            let mut matched = false;
            for id in ids {
                matched |= merge_candidate(
                    tree,
                    &mut candidates,
                    id,
                    options.parent_id,
                    MatchSource::PublicationSearch,
                );
            }
            stages.publication_search = if matched {
                StageStatus::Ok
            } else {
                StageStatus::Empty
            };
        }
        Err(error) => {
            stages.publication_search = StageStatus::Unavailable;
            warnings.push(stage_warning(
                "vinted.category_publication_search_failed",
                "Publication keyword search",
                error,
            ));
        }
    }
    let mut marketplace_evidence = None;
    let has_context = [options.title, options.description]
        .into_iter()
        .flatten()
        .any(|text| !text.trim().is_empty());
    if candidates.is_empty() || candidates.len() > 8 || has_context {
        match category_evidence::discover(
            portal,
            RecommendationContext {
                keyword: options.query,
                title: options.title,
                description: options.description,
            },
            &tree.publication_categories(),
            session,
            search_api,
        )
        .await
        {
            Ok(mut result) => {
                result
                    .categories
                    .retain(|category| tree.in_scope(category.id, options.parent_id));
                let roots = result
                    .categories
                    .iter()
                    .filter_map(|category| category.path.first().cloned())
                    .collect::<BTreeSet<_>>();
                result.evidence.ambiguous_roots = if roots.len() > 1 {
                    roots.into_iter().collect()
                } else {
                    vec![]
                };
                let ids = result
                    .categories
                    .iter()
                    .map(|category| category.id)
                    .collect::<BTreeSet<_>>();
                result
                    .evidence
                    .recommendations
                    .retain(|item| ids.contains(&item.category_id));
                result
                    .evidence
                    .counts
                    .retain(|item| ids.contains(&item.category_id));
                stages.marketplace = if ids.is_empty() {
                    StageStatus::Empty
                } else {
                    StageStatus::Ok
                };
                for id in ids {
                    merge_candidate(
                        tree,
                        &mut candidates,
                        id,
                        options.parent_id,
                        MatchSource::Marketplace,
                    );
                }
                marketplace_evidence = Some(result.evidence);
            }
            Err(error) => {
                stages.marketplace = StageStatus::Unavailable;
                warnings.push(stage_warning(
                    "vinted.category_marketplace_evidence_failed",
                    "Marketplace category evidence",
                    error,
                ));
            }
        }
    }
    sort_candidates(&mut candidates);
    Ok(CategoryDiscovery {
        page: page(candidates, parent, options.limit, options.offset),
        suggestions,
        marketplace_evidence,
        stages,
        warnings,
        selection_required: true,
    })
}

fn merge_candidate(
    tree: &CatalogTree,
    candidates: &mut Vec<CategoryCandidate>,
    id: u64,
    parent: Option<u64>,
    source: MatchSource,
) -> bool {
    let Some(node) = tree.get(id).filter(|_| tree.in_scope(id, parent)) else {
        return false;
    };
    if let Some(candidate) = candidates
        .iter_mut()
        .find(|candidate| candidate.node.category.id == id)
    {
        if !candidate.match_sources.contains(&source) {
            candidate.match_sources.push(source);
        }
    } else {
        candidates.push(CategoryCandidate {
            node: node.clone(),
            match_sources: vec![source],
        });
    }
    true
}
fn stage_warning(code: &str, stage: &str, error: AppError) -> Warning {
    Warning {
        code: code.to_owned(),
        message: format!(
            "{stage} was unavailable ({}): {}",
            error.code, error.message
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{auth::VintedCredentialRecord, search::CatalogueRequest};
    use super::*;
    use serde_json::json;
    use std::{future::Future, pin::Pin, sync::Mutex};

    fn fixture() -> Value {
        json!({"catalogs":[{"id":1,"title":"Lapset","path":"","catalogs":[
            {"id":2,"title":"Poikien","path":"Lapset","catalogs":[{"id":3,"title":"Kengät","catalogs":[{"id":4,"title":"Tarralenkkitossut","catalogs":[]}]}]},
            {"id":5,"title":"Tyttöjen","catalogs":[{"id":6,"title":"Kengät","catalogs":[{"id":7,"title":"Tarralenkkitossut","catalogs":[]}]}]}
        ]}]})
    }
    fn options(query: &str) -> SearchOptions<'_> {
        SearchOptions {
            query,
            title: None,
            description: None,
            parent_id: None,
            limit: DEFAULT_LIMIT,
            offset: 0,
        }
    }
    fn ids(page: &CategoryPage) -> Vec<u64> {
        page.categories.iter().map(|c| c.node.category.id).collect()
    }

    #[test]
    fn canonical_paths_ignore_presentation_paths_and_preserve_parents() {
        let mut raw = fixture();
        raw["catalogs"][0]["catalogs"][0]["path"] = json!("Lapset > Poikien");
        let tree = CatalogTree::from_response(&raw).unwrap();
        assert_eq!(tree.get(1).unwrap().category.path, ["Lapset"]);
        assert_eq!(tree.get(2).unwrap().category.path, ["Lapset", "Poikien"]);
        assert_eq!(tree.get(4).unwrap().parent_id, Some(3));
        assert_eq!(ids(&tree.browse(Some(1), 20, 0).unwrap()), [2, 5]);
        assert!(tree.browse(Some(4), 20, 0).unwrap().categories.is_empty());
    }

    #[test]
    fn malformed_trees_fail_and_nonleaf_metadata_is_conservative() {
        for raw in [
            json!({}),
            json!({"catalogs":[]}),
            json!({"catalogs":[{"id":1,"title":"A","catalogs":null}]}),
            json!({"catalogs":[{"id":1,"title":"A"},{"id":1,"title":"B"}]}),
            json!({"catalogs":[{"id":1,"title":" "}]}),
            json!({"catalogs":[{"id":0,"title":"A"}]}),
        ] {
            assert_eq!(
                CatalogTree::from_response(&raw).err().unwrap().code,
                "vinted.invalid_catalog_tree"
            );
        }
        let tree = CatalogTree::from_response(&json!({"catalogs":[{"id":1,"title":"A","leaf":true,"catalogs":[{"id":2,"title":"B","leaf":false}]}]})).unwrap();
        assert!(!tree.get(1).unwrap().category.leaf);
        assert!(!tree.get(2).unwrap().category.leaf);
    }

    #[test]
    fn local_tokens_are_unicode_normalized_scoped_and_not_translated() {
        let tree = CatalogTree::from_response(&fixture()).unwrap();
        assert_eq!(
            ids(&tree.search(options("  LAPSET kenga\u{308}t ")).unwrap()),
            [3, 6, 4, 7]
        );
        assert_eq!(
            ids(&tree.search(options("poikien tarralenkkitossut")).unwrap()),
            [4]
        );
        for query in ["lasten kengät", "children shoes"] {
            assert_eq!(tree.search(options(query)).unwrap().total, 0);
        }
        let mut scoped = options("kengät");
        scoped.parent_id = Some(2);
        assert_eq!(ids(&tree.search(scoped).unwrap()), [3, 4]);
        scoped.parent_id = Some(999);
        assert!(tree.search(scoped).is_err());
        let mut bounded = options("lapset");
        bounded.limit = 2;
        bounded.offset = 2;
        let result = tree.search(bounded).unwrap();
        assert_eq!(result.total, 7);
        assert_eq!(result.returned, 2);
        assert!(result.truncated);
        assert_eq!(ids(&result), ids(&tree.search(bounded).unwrap()));
    }

    #[test]
    fn invalid_queries_and_limits_fail_before_discovery() {
        for query in ["", "  ", "-- >", &"ä".repeat(129)] {
            assert!(options(query).validate().is_err());
        }
        for limit in [0, 101, usize::MAX] {
            let mut o = options("shoes");
            o.limit = limit;
            assert!(o.validate().is_err());
        }
    }

    struct Api {
        keyword: Option<Value>,
        requests: Mutex<Vec<DiscoveryRequest>>,
    }
    impl VintedPublicationDiscoveryApi for Api {
        fn execute<'a>(
            &'a self,
            _: &'a VintedCredentialRecord,
            request: &'a DiscoveryRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            self.requests.lock().unwrap().push(request.clone());
            Box::pin(std::future::ready(match request {
                DiscoveryRequest::Catalogs => Ok(fixture()),
                DiscoveryRequest::SearchCatalog { .. } => self
                    .keyword
                    .clone()
                    .ok_or_else(|| AppError::upstream("test.keyword_down", "HTTP 404")),
                _ => panic!("unexpected discovery"),
            }))
        }
    }
    struct NoMarketplace;
    impl VintedSearchApi for NoMarketplace {
        fn execute<'a>(
            &'a self,
            _: &'a VintedCredentialRecord,
            _: &'a CatalogueRequest,
        ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
            Box::pin(std::future::ready(Err(AppError::upstream(
                "test.marketplace_down",
                "HTTP 404",
            ))))
        }
    }
    fn credentials(_: PortalId) -> Result<VintedCredentialRecord, AppError> {
        Ok(VintedCredentialRecord::new_for_adapter(
            PortalId::Fi,
            "user".into(),
            None,
            "access".into(),
            "refresh".into(),
            u64::MAX,
            "device".into(),
            "anonymous".into(),
            None,
        ))
    }

    #[tokio::test]
    async fn optional_failures_preserve_local_and_direct_candidates() {
        for keyword in [
            None,
            Some(json!({"catalog_ids":[7,999],"suggestions":[{"id":6,"title":"ignored"}]})),
        ] {
            let api = Api {
                keyword,
                requests: Mutex::new(vec![]),
            };
            let mut o = options("poikien tarralenkkitossut");
            o.title = Some("seller title");
            let result = discover(PortalId::Fi, o, &credentials, &api, &NoMarketplace)
                .await
                .unwrap();
            assert_eq!(result.page.categories[0].node.category.id, 4);
            assert!(result.selection_required);
            assert_eq!(result.stages.marketplace, StageStatus::Unavailable);
            assert!(!ids(&result.page).contains(&999));
            assert!(!ids(&result.page).contains(&6));
            assert_eq!(
                api.requests
                    .lock()
                    .unwrap()
                    .iter()
                    .filter(|r| matches!(r, DiscoveryRequest::Catalogs))
                    .count(),
                1
            );
            if api.keyword.is_some() {
                assert_eq!(ids(&result.page), [4, 7]);
            } else {
                assert_eq!(result.warnings.len(), 2);
            }
        }
    }

    #[tokio::test]
    async fn upstream_candidates_obey_parent_scope() {
        let api = Api {
            keyword: Some(json!({"catalog_ids":[4,7]})),
            requests: Mutex::new(vec![]),
        };
        let mut o = options("unknown words");
        o.parent_id = Some(2);
        let result = discover(PortalId::Fi, o, &credentials, &api, &NoMarketplace)
            .await
            .unwrap();
        assert_eq!(ids(&result.page), [4]);
        assert_eq!(
            result.page.categories[0].match_sources,
            [MatchSource::PublicationSearch]
        );
        assert_eq!(result.stages.marketplace, StageStatus::NotRequested);
    }
}
