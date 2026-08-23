use std::{future::Future, pin::Pin, sync::Mutex};

use flea::{
    AppError, ExitClass, PortalId,
    dependencies::{
        VintedCredentialRecord, VintedItemApi, VintedItemRequest, VintedItemResult, VintedItems,
    },
    domain::vinted_item::VintedSellerLocationSource,
};
use serde_json::Value;

enum FixtureResponse {
    Value(Value),
    Error {
        code: &'static str,
        message: &'static str,
        exit_class: ExitClass,
    },
}

struct FixtureApi {
    response: FixtureResponse,
    calls: Mutex<Vec<String>>,
}

impl FixtureApi {
    fn response(response: Value) -> Self {
        Self {
            response: FixtureResponse::Value(response),
            calls: Mutex::default(),
        }
    }

    fn error(code: &'static str, message: &'static str, exit_class: ExitClass) -> Self {
        Self {
            response: FixtureResponse::Error {
                code,
                message,
                exit_class,
            },
            calls: Mutex::default(),
        }
    }
}

impl VintedItemApi for FixtureApi {
    fn item<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        self.calls.lock().unwrap().push(item_id.to_owned());
        let response = match &self.response {
            FixtureResponse::Value(value) => Ok(value.clone()),
            FixtureResponse::Error {
                code,
                message,
                exit_class,
            } => Err(AppError::new(*code, *message, *exit_class)),
        };
        Box::pin(async move { response })
    }
}

fn fixture(name: &str) -> Value {
    let path = format!("tests/fixtures/vinted/items/{name}.json");
    serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn credentials() -> VintedCredentialRecord {
    VintedCredentialRecord::new_for_adapter(
        PortalId::Fi,
        "fixture-user".to_owned(),
        Some("fixture-login".to_owned()),
        "access".to_owned(),
        "refresh".to_owned(),
        u64::MAX,
        "device".to_owned(),
        "anonymous".to_owned(),
        None,
    )
}

async fn detail(name: &str, id: &str) -> flea::domain::vinted_item::VintedItemDetail {
    let api = FixtureApi::response(fixture(name));
    let session = |_| Ok(credentials());
    let result = VintedItems::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedItemRequest {
                item_id: id.to_owned(),
                raw: false,
            },
        )
        .await
        .unwrap();
    let VintedItemResult::Detail(detail) = result else {
        panic!("expected normalized item detail")
    };
    *detail
}

#[tokio::test]
async fn direct_and_wrapped_items_expose_only_permitted_seller_location_fields() {
    let city = detail("disclosed-city-direct", "101").await;
    let city_location = city.seller.seller_disclosed_location.unwrap();
    assert_eq!(city_location.name, "Helsinki");
    assert_eq!(city_location.source, VintedSellerLocationSource::City);
    assert_eq!(city.images.len(), 1);

    let country = detail("disclosed-country-wrapped", "102").await;
    let country_location = country.seller.seller_disclosed_location.unwrap();
    assert_eq!(country_location.name, "Viro");
    assert_eq!(country_location.source, VintedSellerLocationSource::Country);
}

#[tokio::test]
async fn hidden_and_missing_locations_do_not_use_presentation_text() {
    for (name, id) in [("hidden-location", "103"), ("missing-location", "104")] {
        let item = detail(name, id).await;
        assert!(item.seller.seller_disclosed_location.is_none());
        let serialized = serde_json::to_string(&item.seller).unwrap();
        for presentation_location in ["Espoo", "Vantaa", "Turku", "Tampere", "Oulu"] {
            assert!(!serialized.contains(presentation_location));
        }
    }
}

#[tokio::test]
async fn business_plugin_location_is_identified_as_seller_profile_information() {
    let item = detail("business-seller-location", "105").await;
    let location = item.seller.seller_disclosed_location.unwrap();
    assert_eq!(location.name, "Helsinki, Suomi");
    assert_eq!(location.source, VintedSellerLocationSource::BusinessProfile);
}

#[tokio::test]
async fn malformed_payloads_return_a_structured_upstream_error() {
    let api = FixtureApi::response(fixture("malformed-response"));
    let session = |_| Ok(credentials());
    let error = VintedItems::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedItemRequest {
                item_id: "106".to_owned(),
                raw: false,
            },
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "vinted_item.unexpected_response");
    assert_eq!(error.exit_class, ExitClass::Upstream);
}

#[tokio::test]
async fn raw_mode_returns_the_exact_direct_or_wrapped_upstream_document() {
    let raw = fixture("raw-output");
    let api = FixtureApi::response(raw.clone());
    let session = |_| Ok(credentials());
    let result = VintedItems::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedItemRequest {
                item_id: "107".to_owned(),
                raw: true,
            },
        )
        .await
        .unwrap();

    assert_eq!(result, VintedItemResult::Raw(raw));
}

#[tokio::test]
async fn invalid_ids_fail_before_credentials_or_network_access() {
    let api = FixtureApi::response(fixture("raw-output"));
    let credential_calls = Mutex::new(0);
    let session = |_: PortalId| {
        *credential_calls.lock().unwrap() += 1;
        Ok(credentials())
    };
    let error = VintedItems::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedItemRequest {
                item_id: "../107".to_owned(),
                raw: false,
            },
        )
        .await
        .unwrap_err();

    assert_eq!(error.code, "vinted_item.invalid_id");
    assert_eq!(*credential_calls.lock().unwrap(), 0);
    assert!(api.calls.lock().unwrap().is_empty());
}

#[test]
fn live_verification_record_is_sanitized_and_identifies_the_active_route() {
    let record = fixture("sanitized-live-verification");
    assert_eq!(record["item_detail"]["http_status"], 200);
    assert_eq!(
        record["item_detail"]["route"],
        "GET https://api.vinted.com/item-details/item/<item-id>"
    );
    assert_eq!(record["search"]["selected_item_id"], "<redacted>");
    for field in [
        "credentials",
        "account_identifiers",
        "device_identifiers",
        "request_identifiers",
    ] {
        assert_eq!(record["sanitization"][field], "omitted");
    }
}

#[tokio::test]
async fn removed_listing_fixture_maps_to_the_not_found_contract() {
    assert_eq!(fixture("removed-listing")["http_status"], 404);
    let api = FixtureApi::error(
        "vinted_item.not_found",
        "Vinted item was not found; it may have been removed or sold",
        ExitClass::Validation,
    );
    let session = |_| Ok(credentials());
    let error = VintedItems::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedItemRequest {
                item_id: "108".to_owned(),
                raw: false,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, "vinted_item.not_found");
    assert_eq!(error.exit_class, ExitClass::Validation);
}
