use std::collections::BTreeMap;

use flea::{
    marketplace::tori::{
        auth::SecretString, favorites::FavoritesApiError, item::PublicItemApiError,
        listings::ListingsApiError, saved_searches::SavedSearchApiError, search::SearchApiError,
    },
    storage::auth_flow::AuthFlow,
};

#[test]
fn tori_api_errors_preserve_capability_specific_variants() {
    assert_eq!(
        PublicItemApiError::Expired.to_string(),
        "listing has expired"
    );
    assert_eq!(
        ListingsApiError::Conflict.to_string(),
        "the resource changed remotely"
    );
    assert_eq!(
        ListingsApiError::Validation {
            message: "title rejected".to_owned(),
            fields: BTreeMap::from([("title".to_owned(), "private reason".to_owned())]),
        }
        .to_string(),
        "listing validation failed: title rejected"
    );
    assert_eq!(
        SearchApiError::Rejected.to_string(),
        "search request was rejected"
    );
    assert_eq!(
        FavoritesApiError::Invalid.to_string(),
        "favorite request was rejected"
    );
    assert_eq!(
        SavedSearchApiError::Rejected.to_string(),
        "saved search request was rejected"
    );
}

#[test]
fn tori_api_error_debug_output_redacts_private_context() {
    let errors = [
        format!(
            "{:?}",
            SearchApiError::Transport("secret search target".to_owned())
        ),
        format!(
            "{:?}",
            PublicItemApiError::Unexpected("secret item response".to_owned())
        ),
        format!(
            "{:?}",
            ListingsApiError::UnexpectedResponse("secret listing response".to_owned())
        ),
        format!(
            "{:?}",
            ListingsApiError::Validation {
                message: "secret validation message".to_owned(),
                fields: BTreeMap::from([("title".to_owned(), "secret field reason".to_owned(),)]),
            }
        ),
    ];

    for rendered in errors {
        assert!(
            !rendered.contains("secret"),
            "leaked debug output: {rendered}"
        );
    }
}

#[test]
fn secret_string_serializes_transparently_and_redacts_debug_output() {
    let secret = SecretString::new_for_adapter("protocol-secret".to_owned());

    assert_eq!(
        serde_json::to_string(&secret).unwrap(),
        "\"protocol-secret\""
    );
    assert_eq!(format!("{secret:?}"), "<redacted>");
}

#[test]
fn stored_auth_flow_serialization_and_debug_have_distinct_boundaries() {
    let flow = AuthFlow {
        flow_id: "flow-123".to_owned(),
        expires_at_unix: 200,
        state: "secret-state".to_owned(),
        nonce: "secret-nonce".to_owned(),
        pkce_verifier: "secret-verifier".to_owned(),
        device_id: "secret-device".to_owned(),
        installation_id: "secret-installation".to_owned(),
        ab_test_device_id: "secret-ab-device".to_owned(),
    };

    let serialized = serde_json::to_value(&flow).unwrap();
    assert_eq!(serialized["state"], "secret-state");
    assert_eq!(serialized["device_id"], "secret-device");

    let rendered = format!("{flow:?}");
    for secret in [
        "secret-state",
        "secret-nonce",
        "secret-verifier",
        "secret-device",
        "secret-installation",
        "secret-ab-device",
    ] {
        assert!(!rendered.contains(secret));
    }
}
