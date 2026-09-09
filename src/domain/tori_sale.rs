use serde::{Deserialize, Serialize};

/// An account ad summary, not a ToriDiili transaction.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToriSale {
    pub listing_id: String,
    pub title: Option<String>,
    /// Upstream machine state, preserved even when unknown to Flea.
    pub state: String,
    /// Opaque display text, not a transaction price or net proceeds.
    pub subtitle: Option<String>,
    pub image: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToriSalesCollection {
    pub sales: Vec<ToriSale>,
    pub count: usize,
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    pub next_offset: Option<usize>,
}
