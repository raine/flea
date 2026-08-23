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
    pub name: &'static str,
    pub auth: AuthRequirement,
    pub maturity: CapabilityMaturity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct MarketplaceDescriptor {
    pub marketplace: MarketplaceId,
    pub portals: &'static [PortalId],
    pub capabilities: &'static [CapabilityDescriptor],
}

const FI_PORTAL: &[PortalId] = &[PortalId::Fi];

const TORI_CAPABILITIES: &[CapabilityDescriptor] = &[
    validated("auth.login", AuthRequirement::None),
    validated("auth.status", AuthRequirement::None),
    validated("auth.logout", AuthRequirement::None),
    validated("search", AuthRequirement::None),
    validated("item.show", AuthRequirement::None),
    validated("location.search", AuthRequirement::None),
    validated("category", AuthRequirement::Required),
    validated("favorite", AuthRequirement::Required),
    validated("saved_search", AuthRequirement::Required),
    validated("draft", AuthRequirement::Required),
    validated("listing", AuthRequirement::Required),
    validated("auth.refresh", AuthRequirement::Internal),
];

const VINTED_CAPABILITIES: &[CapabilityDescriptor] = &[
    validated("auth.login", AuthRequirement::None),
    validated("auth.status", AuthRequirement::None),
    validated("auth.logout", AuthRequirement::None),
    CapabilityDescriptor {
        name: "auth.refresh",
        auth: AuthRequirement::Internal,
        maturity: CapabilityMaturity::SourceDerived,
    },
    CapabilityDescriptor {
        name: "search",
        auth: AuthRequirement::Required,
        maturity: CapabilityMaturity::SourceDerived,
    },
    unavailable("item.show"),
    unavailable("location.search"),
    unavailable("category"),
    unavailable("favorite"),
    unavailable("saved_search"),
    unavailable("draft"),
    unavailable("listing"),
];

const MARKETPLACES: &[MarketplaceDescriptor] = &[
    MarketplaceDescriptor {
        marketplace: MarketplaceId::Tori,
        portals: FI_PORTAL,
        capabilities: TORI_CAPABILITIES,
    },
    MarketplaceDescriptor {
        marketplace: MarketplaceId::Vinted,
        portals: FI_PORTAL,
        capabilities: VINTED_CAPABILITIES,
    },
];

const fn validated(name: &'static str, auth: AuthRequirement) -> CapabilityDescriptor {
    CapabilityDescriptor {
        name,
        auth,
        maturity: CapabilityMaturity::Validated,
    }
}

const fn unavailable(name: &'static str) -> CapabilityDescriptor {
    CapabilityDescriptor {
        name,
        auth: AuthRequirement::Unknown,
        maturity: CapabilityMaturity::Unavailable,
    }
}

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
    fn capability_matrix_exposes_source_derived_vinted_operations() {
        let vinted = marketplace(MarketplaceId::Vinted);

        for name in ["auth.login", "auth.status", "auth.logout"] {
            assert_eq!(
                vinted
                    .capabilities
                    .iter()
                    .find(|capability| capability.name == name)
                    .unwrap()
                    .maturity,
                CapabilityMaturity::Validated
            );
        }
        assert_eq!(
            vinted
                .capabilities
                .iter()
                .find(|capability| capability.name == "auth.refresh")
                .unwrap()
                .maturity,
            CapabilityMaturity::SourceDerived
        );
        assert_eq!(
            vinted
                .capabilities
                .iter()
                .find(|capability| capability.name == "search")
                .unwrap()
                .maturity,
            CapabilityMaturity::SourceDerived
        );
    }
}
