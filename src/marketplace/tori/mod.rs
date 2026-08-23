pub mod adinput;
pub mod auth;
pub mod client;
pub mod favorites;
pub(crate) mod interactive;
pub mod item;
pub mod listings;
pub(crate) mod login;
pub mod saved_searches;
pub mod search;
pub(crate) mod session;
pub mod signing;

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
