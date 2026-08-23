use serde::{Deserialize, Serialize};

use super::{item::ItemImage, search::SearchPrice};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct VintedItemDetail {
    pub listing_id: String,
    pub title: String,
    pub description: String,
    pub price: Option<SearchPrice>,
    pub canonical_url: String,
    pub images: Vec<ItemImage>,
    pub seller: VintedItemSeller,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VintedItemSeller {
    pub display_name: Option<String>,
    pub business: Option<bool>,
    pub seller_disclosed_location: Option<VintedSellerDisclosedLocation>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct VintedSellerDisclosedLocation {
    pub name: String,
    pub source: VintedSellerLocationSource,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VintedSellerLocationSource {
    City,
    Country,
    BusinessProfile,
}
