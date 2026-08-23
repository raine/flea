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
    unavailable("search"),
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VintedPortalBinding {
    pub context: MarketplaceContext,
    pub host: &'static str,
    pub locale: &'static str,
    pub iso_locale: &'static str,
    pub client_profile: &'static str,
    pub client_id: &'static str,
    pub callback_scheme: &'static str,
    pub redirect_uri: &'static str,
    pub portal_header: &'static str,
}

pub const VINTED_FI_BINDING: VintedPortalBinding = VintedPortalBinding {
    context: MarketplaceContext::VINTED_FI,
    host: "https://www.vinted.fi",
    locale: "fi",
    iso_locale: "fi-FI",
    client_profile: "android-fr",
    client_id: "android",
    callback_scheme: "vintedfr",
    redirect_uri: "vintedfr://auth",
    portal_header: "fr",
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_matrix_exposes_only_validated_vinted_auth_operations() {
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
            CapabilityMaturity::Unavailable
        );
    }

    #[test]
    fn validated_vinted_binding_keeps_host_and_client_profile_distinct() {
        assert_eq!(VINTED_FI_BINDING.host, "https://www.vinted.fi");
        assert_eq!(VINTED_FI_BINDING.client_profile, "android-fr");
        assert_eq!(VINTED_FI_BINDING.portal_header, "fr");
        assert_eq!(VINTED_FI_BINDING.callback_scheme, "vintedfr");
    }
}
