use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::search::SearchPrice;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VintedListingState {
    Public,
    Hidden,
    Sold,
    Deleted,
    Moderated,
    Missing,
    Draft,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VintedListingValue {
    pub id: Option<String>,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VintedListingShipping {
    pub package_size_id: Option<String>,
    pub shipment_prices: Option<Value>,
    pub parcel: Option<Value>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VintedListingPhoto {
    pub order: usize,
    pub id: Option<String>,
    pub url: Option<String>,
    pub width: Option<u64>,
    pub height: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VintedListingDetail {
    pub listing_id: String,
    pub state: VintedListingState,
    pub title: Option<String>,
    pub description: Option<String>,
    pub price: Option<SearchPrice>,
    pub condition: Option<VintedListingValue>,
    pub category: Option<VintedListingValue>,
    pub brand: Option<VintedListingValue>,
    pub colors: Vec<VintedListingValue>,
    pub shipping: Option<VintedListingShipping>,
    pub photos: Vec<VintedListingPhoto>,
    pub canonical_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VintedListingSummary {
    pub listing_id: String,
    pub state: VintedListingState,
    pub title: Option<String>,
    pub price: Option<SearchPrice>,
    pub canonical_url: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VintedListingCollection {
    pub listings: Vec<VintedListingSummary>,
    pub count: usize,
    pub active_count: usize,
    pub draft_count: usize,
    pub truncated: bool,
}
