//! Compatibility facade for the crate's established `flea::api` library paths.
//!
//! Tori implementations have canonical paths under [`crate::marketplace::tori`].

/// Compatibility path for the Tori authentication protocol.
pub mod auth {
    pub use crate::marketplace::tori::auth::*;
}
/// Compatibility path for the Tori HTTP client and transport seams.
pub mod client {
    pub use crate::marketplace::tori::client::*;
}
/// Compatibility path for Tori favorite capabilities.
pub mod favorites {
    pub use crate::marketplace::tori::favorites::*;
}
/// Compatibility path for Tori item capabilities.
pub mod item {
    pub use crate::marketplace::tori::item::*;
}
/// Compatibility path for Tori listing capabilities.
pub mod listings {
    pub use crate::marketplace::tori::listings::*;
}
/// Compatibility path for Tori saved-search capabilities.
pub mod saved_searches {
    pub use crate::marketplace::tori::saved_searches::*;
}
/// Compatibility path for Tori search capabilities.
pub mod search {
    pub use crate::marketplace::tori::search::*;
}
/// Compatibility path for Tori gateway signing primitives.
pub mod signing {
    pub use crate::marketplace::tori::signing::*;
}
