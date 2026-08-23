use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretString(String);

impl SecretString {
    pub(crate) fn new(value: String) -> Self {
        Self(value)
    }

    pub(crate) fn expose(&self) -> &str {
        &self.0
    }

    #[doc(hidden)]
    pub fn new_for_adapter(value: String) -> Self {
        Self::new(value)
    }
}

impl std::fmt::Debug for SecretString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

pub(crate) fn random_secret(bytes: usize) -> SecretString {
    let mut random = Vec::with_capacity(bytes);
    while random.len() < bytes {
        random.extend_from_slice(Uuid::new_v4().as_bytes());
    }
    random.truncate(bytes);
    SecretString::new(URL_SAFE_NO_PAD.encode(random))
}

pub(crate) fn random_uuid_secret() -> SecretString {
    SecretString::new(Uuid::new_v4().to_string())
}

pub(crate) fn pkce_challenge(verifier: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

pub(crate) fn states_equal(actual: &str, expected: &str, comparison_key: &[u8]) -> bool {
    let mut expected_mac = Hmac::<Sha256>::new_from_slice(comparison_key)
        .expect("the static comparison key has a valid length");
    expected_mac.update(expected.as_bytes());
    let expected_tag = expected_mac.finalize().into_bytes();

    let mut actual_mac = Hmac::<Sha256>::new_from_slice(comparison_key)
        .expect("the static comparison key has a valid length");
    actual_mac.update(actual.as_bytes());
    actual_mac.verify_slice(&expected_tag).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_rfc_7636_vector() {
        assert_eq!(
            pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn secret_debug_output_is_redacted() {
        let secret = SecretString::new("sensitive".to_owned());
        assert_eq!(format!("{secret:?}"), "<redacted>");
    }

    #[test]
    fn state_comparison_is_domain_separated() {
        assert!(states_equal("state", "state", b"provider-one"));
        assert!(!states_equal("other", "state", b"provider-one"));
    }
}
