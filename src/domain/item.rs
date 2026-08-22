use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PublicItemDetail {
    pub listing_id: String,
    pub title: String,
    pub description: String,
    pub price: Option<ItemPrice>,
    pub location: Option<ItemLocation>,
    pub condition: Option<ItemAttribute>,
    pub seller: ItemSeller,
    pub shipping: ItemShipping,
    pub images: Vec<ItemImage>,
    pub published_at: Option<String>,
    pub published_at_ms: Option<i64>,
    pub canonical_url: Option<String>,
    pub trade_type: Option<String>,
    pub category: Vec<ItemAttribute>,
    pub attributes: Vec<ItemAttribute>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ItemPrice {
    pub amount: Value,
    pub currency: Option<String>,
    pub display: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemLocation {
    pub name: Option<String>,
    pub postal_code: Option<String>,
    pub country_code: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemAttribute {
    pub value: String,
    pub label: Option<String>,
    pub machine_id: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemSeller {
    pub seller_type: Option<String>,
    pub display_name: Option<String>,
    pub profile_url: Option<String>,
    pub verified: Option<bool>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct ItemShipping {
    pub available: Option<bool>,
    pub eligible: Option<bool>,
    pub seller_pays: Option<bool>,
    pub buy_now: Option<bool>,
    pub method: Option<String>,
    pub price: Option<Value>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ItemImage {
    pub url: String,
    pub width: Option<u64>,
    pub height: Option<u64>,
    pub description: Option<String>,
}
