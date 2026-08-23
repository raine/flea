pub mod tori;
pub(crate) mod vinted;

use std::fmt;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MarketplaceId {
    Tori,
    Vinted,
}

impl fmt::Display for MarketplaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Tori => "tori",
            Self::Vinted => "vinted",
        })
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum PortalId {
    #[default]
    Fi,
}

impl fmt::Display for PortalId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Fi => "fi",
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MarketplaceContext {
    pub marketplace: MarketplaceId,
    pub portal: PortalId,
}

impl MarketplaceContext {
    pub const TORI_FI: Self = Self {
        marketplace: MarketplaceId::Tori,
        portal: PortalId::Fi,
    };

    pub const VINTED_FI: Self = Self {
        marketplace: MarketplaceId::Vinted,
        portal: PortalId::Fi,
    };
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityId {
    #[serde(rename = "auth.login")]
    AuthLogin,
    #[serde(rename = "auth.status")]
    AuthStatus,
    #[serde(rename = "auth.logout")]
    AuthLogout,
    #[serde(rename = "auth.refresh")]
    AuthRefresh,
    Search,
    #[serde(rename = "item.show")]
    ItemShow,
    #[serde(rename = "location.search")]
    LocationSearch,
    Category,
    Favorite,
    SavedSearch,
    Draft,
    Listing,
}

impl CapabilityId {
    pub const ALL: [Self; 12] = [
        Self::AuthLogin,
        Self::AuthStatus,
        Self::AuthLogout,
        Self::AuthRefresh,
        Self::Search,
        Self::ItemShow,
        Self::LocationSearch,
        Self::Category,
        Self::Favorite,
        Self::SavedSearch,
        Self::Draft,
        Self::Listing,
    ];
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityMaturity {
    Validated,
    SourceDerived,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthRequirement {
    None,
    Required,
    Internal,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct CapabilityDescriptor {
    #[serde(rename = "name")]
    pub id: CapabilityId,
    pub auth: AuthRequirement,
    pub maturity: CapabilityMaturity,
}

impl CapabilityDescriptor {
    pub const fn validated(id: CapabilityId, auth: AuthRequirement) -> Self {
        Self {
            id,
            auth,
            maturity: CapabilityMaturity::Validated,
        }
    }

    pub const fn source_derived(id: CapabilityId, auth: AuthRequirement) -> Self {
        Self {
            id,
            auth,
            maturity: CapabilityMaturity::SourceDerived,
        }
    }

    pub const fn unavailable(id: CapabilityId) -> Self {
        Self {
            id,
            auth: AuthRequirement::Unknown,
            maturity: CapabilityMaturity::Unavailable,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct MarketplaceDescriptor {
    pub marketplace: MarketplaceId,
    pub portals: &'static [PortalId],
    pub capabilities: &'static [CapabilityDescriptor],
}

const MARKETPLACES: &[MarketplaceDescriptor] = &[tori::MANIFEST, vinted::MANIFEST];

pub fn marketplaces() -> &'static [MarketplaceDescriptor] {
    MARKETPLACES
}

pub fn marketplace(id: MarketplaceId) -> &'static MarketplaceDescriptor {
    MARKETPLACES
        .iter()
        .find(|descriptor| descriptor.marketplace == id)
        .expect("every marketplace ID has a descriptor")
}

pub use vinted::binding::{VINTED_FI_BINDING, VintedPortalBinding};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_manifest_describes_each_capability_once() {
        for manifest in marketplaces() {
            for id in CapabilityId::ALL {
                assert_eq!(
                    manifest
                        .capabilities
                        .iter()
                        .filter(|capability| capability.id == id)
                        .count(),
                    1,
                    "{id:?} entries in {:?}",
                    manifest.marketplace
                );
            }
            assert_eq!(manifest.capabilities.len(), CapabilityId::ALL.len());
        }
    }

    #[test]
    fn capability_identifiers_preserve_wire_names() {
        let cases = [
            (CapabilityId::AuthLogin, "auth.login"),
            (CapabilityId::AuthStatus, "auth.status"),
            (CapabilityId::AuthLogout, "auth.logout"),
            (CapabilityId::AuthRefresh, "auth.refresh"),
            (CapabilityId::Search, "search"),
            (CapabilityId::ItemShow, "item.show"),
            (CapabilityId::LocationSearch, "location.search"),
            (CapabilityId::Category, "category"),
            (CapabilityId::Favorite, "favorite"),
            (CapabilityId::SavedSearch, "saved_search"),
            (CapabilityId::Draft, "draft"),
            (CapabilityId::Listing, "listing"),
        ];

        for (id, expected) in cases {
            assert_eq!(serde_json::to_value(id).unwrap(), expected);
        }
    }
}
