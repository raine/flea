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
