use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchCollection {
    pub query: String,
    pub results: Vec<SearchListing>,
    pub pagination: SearchPagination,
    pub applied_filters: Vec<AppliedFilter>,
    pub facets: Vec<SearchFacet>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_location: Option<SearchLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchListing {
    pub listing_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<SearchPrice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distance: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub category: Option<MachineLabel>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trade_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listing_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub image_urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published_at_ms: Option<i64>,
    pub labels: Vec<MachineLabel>,
    pub flags: Vec<String>,
    pub extras: Vec<MachineLabel>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SearchPrice {
    pub amount: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct MachineLabel {
    pub value: String,
    pub label: String,
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
    pub total_pages: usize,
    pub accessible_pages: usize,
    pub upstream_page_limit: usize,
    pub capped: bool,
    pub has_previous: bool,
    pub has_next: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub previous_page: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_page: Option<usize>,
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
pub struct LocationCollection {
    pub locations: Vec<SearchLocation>,
    pub returned: usize,
    pub truncated: bool,
}
