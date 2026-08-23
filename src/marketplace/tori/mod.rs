pub(crate) mod adinput;
pub(crate) mod auth;
pub(crate) mod client;
pub(crate) mod discovery;
pub(crate) mod favorites;
pub(crate) mod interactive;
pub(crate) mod item;
pub(crate) mod listings;
pub(crate) mod login;
pub(crate) mod saved_searches;
pub(crate) mod search;
pub(crate) mod session;
pub(crate) mod signing;

use super::{
    AuthRequirement, CapabilityDescriptor, CapabilityId, MarketplaceDescriptor, MarketplaceId,
    PortalId,
};

const PORTALS: &[PortalId] = &[PortalId::Fi];

const CAPABILITIES: &[CapabilityDescriptor] = &[
    CapabilityDescriptor::validated(CapabilityId::AuthLogin, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::AuthStatus, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::AuthLogout, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::Search, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::ItemShow, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::LocationSearch, AuthRequirement::None),
    CapabilityDescriptor::validated(CapabilityId::Category, AuthRequirement::Required),
    CapabilityDescriptor::validated(CapabilityId::Favorite, AuthRequirement::Required),
    CapabilityDescriptor::validated(CapabilityId::SavedSearch, AuthRequirement::Required),
    CapabilityDescriptor::validated(CapabilityId::Draft, AuthRequirement::Required),
    CapabilityDescriptor::validated(CapabilityId::Listing, AuthRequirement::Required),
    CapabilityDescriptor::validated(CapabilityId::AuthRefresh, AuthRequirement::Internal),
];

pub(super) const MANIFEST: MarketplaceDescriptor = MarketplaceDescriptor {
    marketplace: MarketplaceId::Tori,
    portals: PORTALS,
    capabilities: CAPABILITIES,
};
