use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Category {
    pub category_id: String,
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub path: String,
    pub selectable: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategoryList {
    pub categories: Vec<Category>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategorySearchContext {
    pub category_id: String,
    pub label: String,
    pub path: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CategorySearchResult {
    pub categories: Vec<Category>,
    pub query: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<CategorySearchContext>,
    pub offset: usize,
    pub limit: usize,
    pub returned: usize,
    pub total: usize,
    pub truncated: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListingState {
    All,
    Active,
    Pending,
    Expired,
    Disposed,
    Draft,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ListingActionName {
    Edit,
    Dispose,
    Delete,
    Republish,
    Undispose,
    View,
    Unknown,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListingAction {
    pub name: ListingActionName,
    pub label: String,
    pub method: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListingStatistics {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub views: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub favorites: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListingSummary {
    pub listing_id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub price: Option<String>,
    pub state: ListingState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_url: Option<String>,
    pub public_url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub days_until_expires: Option<u64>,
    pub statistics: ListingStatistics,
    pub actions: Vec<ListingAction>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListingFacet {
    pub state: ListingState,
    pub label: String,
    pub total: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListingCollection {
    pub listings: Vec<ListingSummary>,
    pub total: u64,
    pub facets: Vec<ListingFacet>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ListingDetail {
    pub listing_id: String,
    pub state: ListingState,
    pub fields: BTreeMap<String, Value>,
    pub statistics: ListingStatistics,
    pub actions: Vec<ListingAction>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ListingSnapshot {
    pub detail: ListingDetail,
    pub etag: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ListingCopySource {
    pub listing_id: String,
    pub fields: BTreeMap<String, Value>,
    pub image_urls: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListingMutation {
    pub listing_id: String,
    pub state: ListingState,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ListingRef {
    pub listing_id: String,
}
