use crate::marketplace::MarketplaceContext;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VintedPortalBinding {
    pub context: MarketplaceContext,
    pub host: &'static str,
    pub api_host: &'static str,
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
    api_host: "https://api.vinted.com",
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
    fn validated_binding_keeps_host_and_client_profile_distinct() {
        assert_eq!(VINTED_FI_BINDING.host, "https://www.vinted.fi");
        assert_eq!(VINTED_FI_BINDING.api_host, "https://api.vinted.com");
        assert_eq!(VINTED_FI_BINDING.client_profile, "android-fr");
        assert_eq!(VINTED_FI_BINDING.portal_header, "fr");
        assert_eq!(VINTED_FI_BINDING.callback_scheme, "vintedfr");
    }
}
