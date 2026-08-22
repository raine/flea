use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchCollection {
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<SearchLocationContext>,
    pub results: Vec<SearchListing>,
    pub pagination: SearchPagination,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub applied_filters: Vec<AppliedFilter>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub facets: Vec<SearchFacet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_area: Option<SearchAreaContext>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explain: Option<SearchExplainSummary>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchListing {
    pub listing_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<SearchPrice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shipping: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seller: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_explanation: Option<SearchMatchExplanation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchMatchExplanation {
    pub source_field: String,
    pub evidence_origin: String,
    pub match_method: String,
    pub matched_terms: Vec<String>,
    pub excerpt: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchExplainSummary {
    pub request_limit: usize,
    pub requested: usize,
    pub hydrated: usize,
    pub explained: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub failures: Vec<SearchExplainFailure>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchExplainFailure {
    pub listing_id: String,
    pub code: String,
    pub retryable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchPrice {
    pub amount: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedFilter {
    pub name: String,
    pub values: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchFacet {
    pub name: String,
    pub label: String,
    pub facet_type: String,
    pub options: Vec<SearchFacetOption>,
    pub option_count: usize,
    pub truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<SearchFacetRange>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchFacetOption {
    pub value: String,
    pub label: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_value: Option<String>,
    pub depth: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hits: Option<i64>,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchFacetRange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minimum: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maximum: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub to_name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchPagination {
    pub page: usize,
    pub limit: usize,
    pub returned: usize,
    pub total: usize,
    pub has_next: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<usize>,
    #[serde(skip)]
    pub capped: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchLocationContext {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchLocation {
    pub id: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    pub depth: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchArea {
    pub locations: Vec<SearchLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SearchAreaContext {
    pub locations: Vec<SearchLocationContext>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LocationCollection {
    pub locations: Vec<SearchLocation>,
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
}
