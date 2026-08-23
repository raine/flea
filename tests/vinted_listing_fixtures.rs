use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use flea::{
    AppError, PortalId,
    dependencies::{
        ListingLookup, VintedCredentialRecord, VintedListingApi, VintedListingRequest,
        VintedListingResult, VintedListings,
    },
    domain::vinted_listing::VintedListingState,
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
            "9002" => Ok(ListingLookup::Missing),
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
async fn publication_item_id_resolves_immediately_without_search_indexing() {
    let publication_item_id = "9001";
    let api = FixtureApi::new();
    let session = |_| Ok(credentials());
    let result = VintedListings::new(&session, &api)
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
    assert_eq!(
        detail.condition.as_ref().unwrap().name.as_deref(),
        Some("Very good")
    );
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
    assert_eq!(collection.count, 2);
    assert_eq!(collection.active_count, 1);
    assert_eq!(collection.draft_count, 1);
    assert_eq!(collection.listings[1].state, VintedListingState::Draft);
    assert_eq!(
        api.calls.lock().unwrap().as_slice(),
        ["list:active:1:100", "list:drafts:1:100"]
    );
}

#[test]
fn cli_listing_output_excludes_account_and_session_data() {
    let api = Arc::new(FixtureApi::new());
    let dependencies = flea::dependencies::ApplicationDependencies::production()
        .with_vinted_credentials_provider(|_| Ok(credentials()))
        .with_vinted_listing_api(api);
    let result = run_with_dependencies(
        [
            "flea", "--format", "json", "vinted", "listing", "show", "9001",
        ],
        &dependencies,
    );
    let envelope: Value = serde_json::from_str(&result.document).unwrap();
    let serialized = envelope["data"].to_string();

    assert_eq!(result.exit_code, 0);
    assert!(!serialized.contains("fixture-user"));
    assert!(!serialized.contains("fixture-login"));
    assert!(!serialized.contains("access"));
    assert!(!serialized.contains("refresh"));
    assert!(!serialized.contains("device"));
}
