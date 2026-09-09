use serde::{Deserialize, Serialize};

use super::search::SearchPrice;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VintedSalesStatus {
    All,
    InProgress,
    #[default]
    Completed,
    Canceled,
}

impl VintedSalesStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::InProgress => "in_progress",
            Self::Completed => "completed",
            Self::Canceled => "canceled",
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VintedSale {
    pub transaction_id: Option<String>,
    pub conversation_id: Option<String>,
    pub title: Option<String>,
    pub price: Option<SearchPrice>,
    pub status_text: Option<String>,
    pub transaction_user_status: Option<String>,
    pub photo_url: Option<String>,
    /// Upstream order date, not necessarily a sale completion timestamp.
    pub date: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct VintedSalesPagination {
    pub current_page: usize,
    pub total_entries: usize,
    pub total_pages: usize,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct VintedSalesCollection {
    pub sales: Vec<VintedSale>,
    pub count: usize,
    pub status: VintedSalesStatus,
    pub limit: usize,
    pub pagination: VintedSalesPagination,
    pub next_page: Option<usize>,
}
