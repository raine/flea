pub(crate) mod tori;
pub(crate) mod vinted;

pub(crate) use crate::domain::metadata::{
    AuthRequirement, CapabilityDescriptor, CapabilityId, CapabilityMaturity, MarketplaceContext,
    MarketplaceDescriptor, MarketplaceId, PortalId,
};

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
