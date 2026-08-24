use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use flea::{
    AppError, PortalId,
    dependencies::{
        DiscoveryRequest, ListingLookup, VintedCredentialRecord, VintedListingApi,
        VintedListingRequest, VintedListingResult, VintedListings, VintedPublicationDiscoveryApi,
    },
    domain::vinted_listing::{VintedConditionIdentityStatus, VintedListingState},
    run_with_dependencies,
};
use serde_json::Value;

struct FixtureApi {
    calls: Mutex<Vec<String>>,
}

impl FixtureApi {
    fn new() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl VintedListingApi for FixtureApi {
    fn wardrobe_item<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ListingLookup, AppError>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("wardrobe:{item_id}"));
        let result = match item_id {
            "9001" => Ok(ListingLookup::Found(fixture("published-wardrobe"))),
            "9002" | "9004" | "9005" | "9006" => Ok(ListingLookup::Missing),
            "9003" => Ok(ListingLookup::Deleted),
            _ => panic!("unexpected item ID"),
        };
        Box::pin(async move { result })
    }

    fn item_for_edit<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        self.calls.lock().unwrap().push(format!("edit:{item_id}"));
        let result = Ok(fixture("published-edit"));
        Box::pin(async move { result })
    }

    fn wardrobe_items<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        condition: &'a str,
        page: usize,
        per_page: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("list:{condition}:{page}:{per_page}"));
        let result = Ok(fixture(match condition {
            "active" => "active-list",
            "drafts" => "draft-list",
            _ => panic!("unexpected condition"),
        }));
        Box::pin(async move { result })
    }
}

struct FixtureDiscoveryApi;

impl VintedPublicationDiscoveryApi for FixtureDiscoveryApi {
    fn execute<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        request: &'a DiscoveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        let DiscoveryRequest::Attributes { selections } = request else {
            panic!("unexpected discovery request")
        };
        assert_eq!(
            selections,
            &serde_json::json!([{"code": "category", "value": [3412]}])
        );
        Box::pin(async {
            Ok(serde_json::json!({
                "attributes": [{
                    "code": "condition",
                    "configuration": {
                        "title": "Kunto",
                        "groups": [{
                            "options": [
                                {"id": 6, "title": "Tyydyttävä"},
                                {"id": 7, "title": "Hyvä"}
                            ]
                        }]
                    }
                }]
            }))
        })
    }
}

struct FailingApi;

impl VintedListingApi for FailingApi {
    fn wardrobe_item<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ListingLookup, AppError>> + Send + 'a>> {
        Box::pin(async move {
            if item_id == "9010" {
                Err(AppError::upstream(
                    "fixture.detail_failed",
                    "detail endpoint failed",
                ))
            } else {
                Ok(ListingLookup::Missing)
            }
        })
    }

    fn item_for_edit<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        _item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        panic!("editable detail should not be requested")
    }

    fn wardrobe_items<'a>(
        &'a self,
        _credentials: &'a VintedCredentialRecord,
        _condition: &'a str,
        _page: usize,
        _per_page: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(async {
            Err(AppError::upstream(
                "fixture.collection_failed",
                "collection endpoint failed",
            ))
        })
    }
}

fn fixture(name: &str) -> Value {
    let path = format!("tests/fixtures/vinted/listings/{name}.json");
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

#[tokio::test]
async fn publication_condition_round_trips_through_listing_inspection() {
    let publication_item_id = "9001";
    let api = FixtureApi::new();
    let session = |_| Ok(credentials());
    let result = VintedListings::new(&session, &api)
        .with_discovery(&FixtureDiscoveryApi)
        .execute(
            PortalId::Fi,
            VintedListingRequest::Show {
                item_id: publication_item_id.to_owned(),
            },
        )
        .await
        .unwrap();
    let VintedListingResult::Detail(detail) = result else {
        panic!("expected listing detail")
    };

    assert_eq!(detail.listing_id, publication_item_id);
    assert_eq!(detail.state, VintedListingState::Public);
    assert_eq!(detail.title.as_deref(), Some("Bicycle lock"));
    assert_eq!(
        detail.description.as_deref(),
        Some("Clean cable lock with two keys.")
    );
    assert_eq!(
        detail.price.as_ref().unwrap().currency.as_deref(),
        Some("EUR")
    );
    let condition = detail.condition.as_ref().unwrap();
    assert_eq!(condition.name.as_deref(), Some("Tyydyttävä"));
    assert_eq!(
        condition.identity.status,
        VintedConditionIdentityStatus::ComposerMatched
    );
    assert_eq!(condition.identity.upstream_id, None);
    assert_eq!(condition.identity.composer_id.as_deref(), Some("6"));
    assert_eq!(
        detail.category.as_ref().unwrap().id.as_deref(),
        Some("3412")
    );
    assert_eq!(
        detail.brand.as_ref().unwrap().name.as_deref(),
        Some("Kryptonite")
    );
    assert_eq!(detail.colors.len(), 2);
    assert_eq!(
        detail.shipping.as_ref().unwrap().package_size_id.as_deref(),
        Some("2")
    );
    assert_eq!(detail.photos[0].id.as_deref(), Some("41"));
    assert_eq!(detail.photos[1].order, 1);
    assert_eq!(
        detail.canonical_url.as_deref(),
        Some("https://www.vinted.fi/items/9001-bicycle-lock")
    );
    assert_eq!(
        api.calls.lock().unwrap().as_slice(),
        ["wardrobe:9001", "edit:9001"]
    );
}

#[tokio::test]
async fn moderated_listing_falls_back_to_account_summary() {
    let api = FixtureApi::new();
    let session = |_| Ok(credentials());
    let result = VintedListings::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedListingRequest::Show {
                item_id: "9004".to_owned(),
            },
        )
        .await
        .unwrap();
    let VintedListingResult::Detail(detail) = result else {
        panic!("expected listing detail")
    };

    assert_eq!(detail.state, VintedListingState::Moderated);
    assert_eq!(detail.title.as_deref(), Some("Bicycle lock pending review"));
    assert_eq!(
        detail.canonical_url.as_deref(),
        Some("https://www.vinted.fi/items/9004-bicycle-lock")
    );
    assert_eq!(detail.photos[0].id.as_deref(), Some("51"));
    assert!(detail.description.is_none());
    assert!(detail.shipping.is_none());
    assert_eq!(
        api.calls.lock().unwrap().as_slice(),
        ["wardrobe:9004", "list:active:1:100", "list:drafts:1:100"]
    );
}

#[tokio::test]
async fn active_account_state_wins_for_duplicate_ids() {
    let api = FixtureApi::new();
    let session = |_| Ok(credentials());
    let result = VintedListings::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedListingRequest::Show {
                item_id: "9006".to_owned(),
            },
        )
        .await
        .unwrap();
    let VintedListingResult::Detail(detail) = result else {
        panic!("expected listing detail")
    };

    assert_eq!(detail.state, VintedListingState::Moderated);
    assert_eq!(detail.title.as_deref(), Some("Duplicate pending listing"));
    assert_eq!(
        api.calls.lock().unwrap().as_slice(),
        ["wardrobe:9006", "list:active:1:100", "list:drafts:1:100"]
    );
}

#[tokio::test]
async fn missing_and_deleted_states_do_not_request_editable_details() {
    let api = FixtureApi::new();
    let session = |_| Ok(credentials());
    for (id, expected) in [
        ("9002", VintedListingState::Missing),
        ("9003", VintedListingState::Deleted),
    ] {
        let result = VintedListings::new(&session, &api)
            .execute(
                PortalId::Fi,
                VintedListingRequest::Show {
                    item_id: id.to_owned(),
                },
            )
            .await
            .unwrap();
        let VintedListingResult::Detail(detail) = result else {
            panic!("expected detail")
        };
        assert_eq!(detail.state, expected);
        assert!(detail.title.is_none());
    }
    assert!(
        api.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| !call.starts_with("edit:"))
    );
}

#[tokio::test]
async fn item_absent_from_detail_and_account_collections_is_missing() {
    let api = FixtureApi::new();
    let session = |_| Ok(credentials());
    let result = VintedListings::new(&session, &api)
        .execute(
            PortalId::Fi,
            VintedListingRequest::Show {
                item_id: "9005".to_owned(),
            },
        )
        .await
        .unwrap();
    let VintedListingResult::Detail(detail) = result else {
        panic!("expected listing detail")
    };

    assert_eq!(detail.state, VintedListingState::Missing);
    assert_eq!(
        api.calls.lock().unwrap().as_slice(),
        ["wardrobe:9005", "list:active:1:100", "list:drafts:1:100"]
    );
}

#[tokio::test]
async fn endpoint_errors_are_not_misclassified_as_absence() {
    let session = |_| Ok(credentials());
    for (item_id, expected_code) in [
        ("9010", "fixture.detail_failed"),
        ("9011", "fixture.collection_failed"),
    ] {
        let error = VintedListings::new(&session, &FailingApi)
            .execute(
                PortalId::Fi,
                VintedListingRequest::Show {
                    item_id: item_id.to_owned(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, expected_code);
    }
}

#[tokio::test]
async fn list_combines_active_and_draft_associated_items() {
    let api = FixtureApi::new();
    let session = |_| Ok(credentials());
    let result = VintedListings::new(&session, &api)
        .execute(PortalId::Fi, VintedListingRequest::List)
        .await
        .unwrap();
    let VintedListingResult::Collection(collection) = result else {
        panic!("expected collection")
    };
    assert_eq!(collection.count, 4);
    assert_eq!(collection.active_count, 3);
    assert_eq!(collection.draft_count, 1);
    assert_eq!(collection.listings[3].state, VintedListingState::Draft);
    assert_eq!(
        api.calls.lock().unwrap().as_slice(),
        ["list:active:1:100", "list:drafts:1:100"]
    );
}

#[test]
fn cli_moderated_listing_succeeds_with_review_guidance() {
    let api = Arc::new(FixtureApi::new());
    let dependencies = flea::dependencies::ApplicationDependencies::production()
        .with_vinted_credentials_provider(|_| Ok(credentials()))
        .with_vinted_listing_api(api)
        .with_vinted_publication_discovery_api(Arc::new(FixtureDiscoveryApi));
    let result = run_with_dependencies(
        [
            "flea", "--format", "json", "vinted", "listing", "show", "9004",
        ],
        &dependencies,
    );
    let envelope: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 0);
    assert_eq!(envelope["ok"], true);
    assert_eq!(envelope["data"]["state"], "moderated");
    assert_eq!(
        envelope["warnings"][0]["code"],
        "vinted_listing.under_review"
    );
    assert_eq!(
        envelope["next_actions"][0]["command"],
        "flea vinted --portal fi listing show 9004"
    );
}

#[test]
fn cli_listing_output_excludes_account_and_session_data() {
    let api = Arc::new(FixtureApi::new());
    let dependencies = flea::dependencies::ApplicationDependencies::production()
        .with_vinted_credentials_provider(|_| Ok(credentials()))
        .with_vinted_listing_api(api)
        .with_vinted_publication_discovery_api(Arc::new(FixtureDiscoveryApi));
    let result = run_with_dependencies(
        [
            "flea", "--format", "json", "vinted", "listing", "show", "9001",
        ],
        &dependencies,
    );
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    let serialized = envelope["data"].to_string();

    assert_eq!(result.exit_code, 0);
    assert_eq!(
        envelope["data"]["condition"],
        serde_json::json!({
            "name": "Tyydyttävä",
            "identity": {
                "status": "composer_matched",
                "upstream_id": null,
                "composer_id": "6"
            }
        })
    );
    assert!(!serialized.contains("fixture-user"));
    assert!(!serialized.contains("fixture-login"));
    assert!(!serialized.contains("access"));
    assert!(!serialized.contains("refresh"));
    assert!(!serialized.contains("device"));
}
