pub(crate) mod auth;
pub(crate) mod binding;
pub(crate) mod interactive;
pub(crate) mod item;
pub(crate) mod publication;
pub(crate) mod publication_discovery;
pub(crate) mod search;
pub(crate) mod session;

use super::{
    AuthRequirement, CapabilityDescriptor, CapabilityId, MarketplaceDescriptor, MarketplaceId,
    PortalId,
};

const PORTALS: &[PortalId] = &[PortalId::Fi];

const CAPABILITIES: &[CapabilityDescriptor] = &[
    CapabilityDescriptor::validated(CapabilityId::AuthLogin, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::AuthStatus, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::AuthLogout, AuthRequirement::None),
    CapabilityDescriptor::source_derived(CapabilityId::AuthRefresh, AuthRequirement::Internal),
    CapabilityDescriptor::source_derived(CapabilityId::Search, AuthRequirement::Required),
    CapabilityDescriptor::validated(CapabilityId::ItemShow, AuthRequirement::Required),
    CapabilityDescriptor::unavailable(CapabilityId::LocationSearch),
    CapabilityDescriptor::source_derived(CapabilityId::Category, AuthRequirement::Required),
    CapabilityDescriptor::unavailable(CapabilityId::Favorite),
    CapabilityDescriptor::unavailable(CapabilityId::SavedSearch),
    CapabilityDescriptor::source_derived(CapabilityId::Draft, AuthRequirement::Required),
    CapabilityDescriptor::unavailable(CapabilityId::Listing),
];

pub(super) const MANIFEST: MarketplaceDescriptor = MarketplaceDescriptor {
    marketplace: MarketplaceId::Vinted,
    portals: PORTALS,
    capabilities: CAPABILITIES,
};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::marketplace::CapabilityMaturity;

    #[test]
    fn manifest_exposes_source_derived_operations() {
        for id in [
            CapabilityId::AuthLogin,
            CapabilityId::AuthStatus,
            CapabilityId::AuthLogout,
            CapabilityId::ItemShow,
        ] {
            assert_eq!(
                MANIFEST
                    .capabilities
                    .iter()
                    .find(|capability| capability.id == id)
                    .unwrap()
                    .maturity,
                CapabilityMaturity::Validated
            );
        }
        for id in [
            CapabilityId::AuthRefresh,
            CapabilityId::Search,
            CapabilityId::Category,
            CapabilityId::Draft,
        ] {
            assert_eq!(
                MANIFEST
                    .capabilities
                    .iter()
                    .find(|capability| capability.id == id)
                    .unwrap()
                    .maturity,
                CapabilityMaturity::SourceDerived
            );
        }
    }
}
