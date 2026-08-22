use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use hmac::{Hmac, Mac};
use sha2::Sha512;

pub(crate) const GATEWAY_SIGNING_KEY: &[u8] = b"3b535f36-79be-424b-a6fd-116c6e69f137";

pub struct SigningContext<'a> {
    pub method: &'a str,
    pub path_and_query: &'a str,
    pub service: &'a str,
    pub body: &'a [u8],
}

/// A gateway signature whose formatting implementations never reveal its value.
#[derive(Clone, Eq, PartialEq)]
pub struct GatewaySignature(String);

impl GatewaySignature {
    pub fn as_header_value(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for GatewaySignature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GatewaySignature([REDACTED])")
    }
}

/// Signs the exact method, path and raw query, service, and body bytes.
///
/// A root path is represented by an empty path in the gateway message. Query
/// ordering and percent encoding are preserved exactly as supplied.
pub fn sign(context: SigningContext<'_>) -> GatewaySignature {
    let path_and_query = match context.path_and_query {
        "/" => "",
        value => value,
    };
    let prefix = format!(
        "{};{};{};",
        context.method.to_ascii_uppercase(),
        path_and_query,
        context.service
    );

    let mut mac = Hmac::<Sha512>::new_from_slice(GATEWAY_SIGNING_KEY)
        .expect("HMAC accepts keys of any length");
    mac.update(prefix.as_bytes());
    mac.update(context.body);
    GatewaySignature(STANDARD.encode(mac.finalize().into_bytes()))
}

#[cfg(test)]
mod tests {
    use super::{SigningContext, sign};

    #[test]
    fn matches_known_gateway_vector() {
        let signature = sign(SigningContext {
            method: "GET",
            path_and_query: "/public/users/697554341/unreadmessagecount",
            service: "MESSAGING-API",
            body: b"",
        });

        assert_eq!(
            signature.as_header_value(),
            "bbAqA7PQNmE6YbhPHwTmhasqW/n2rXnHl+f2UTJjxQWcIDynRvYR2sDCBxDpWgJkTfVfPOkbjzVR78rnn/1ojg=="
        );
    }

    #[test]
    fn preserves_exact_query_and_body_bytes() {
        let query_signature = sign(SigningContext {
            method: "get",
            path_and_query: "/search?foo=one%20two&x=1",
            service: "SEARCH-QUEST",
            body: b"",
        });
        assert_eq!(
            query_signature.as_header_value(),
            "rARwXhpkwuDVfRL8MgsujE4ytirzxT/D8+CXWtp/nxKg8qaTA+9VQAMgnZI/5iIzZk+Yln1ls7I7/Dfw6XeolA=="
        );

        let body_signature = sign(SigningContext {
            method: "POST",
            path_and_query: "/items/42",
            service: "RC-ITEM-CREATION-FLOW-API",
            body: br#"{"title":"Tuoli"}"#,
        });
        assert_eq!(
            body_signature.as_header_value(),
            "Xff7efJCPUIBUKKZJbzR6YNxD7WyyhynrKOHbmhzGWmlHNDbmfo8SK3YCJ2/uK2mpaCQkG2f06aS4oIac3jz6A=="
        );
    }

    #[test]
    fn normalizes_only_the_root_path() {
        let root = sign(SigningContext {
            method: "HEAD",
            path_and_query: "/",
            service: "HEALTH",
            body: b"",
        });
        let empty = sign(SigningContext {
            method: "HEAD",
            path_and_query: "",
            service: "HEALTH",
            body: b"",
        });

        assert_eq!(root, empty);
    }

    #[test]
    fn debug_redacts_the_signature() {
        let signature = sign(SigningContext {
            method: "GET",
            path_and_query: "/secret",
            service: "SERVICE",
            body: b"",
        });
        let rendered = format!("{signature:?}");

        assert_eq!(rendered, "GatewaySignature([REDACTED])");
        assert!(!rendered.contains(signature.as_header_value()));
    }
}
