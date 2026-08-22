use std::{
    collections::{BTreeMap, VecDeque},
    sync::Mutex,
};

use clap::Parser;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tori::{
    api::listings::{
        LISTING_PAGE_SIZE, Listings, ListingsApi, ListingsApiError, UpstreamCategory,
        UpstreamListing, UpstreamListingPage,
    },
    cli::{
        Cli, Command,
        listing::{ListingCommand, listing_changes},
    },
    domain::listing::{ListingActionName, ListingState},
};

type UpdateCall = (String, String, BTreeMap<String, Value>);

struct MockListingsApi {
    categories: Vec<UpstreamCategory>,
    pages: Mutex<VecDeque<UpstreamListingPage>>,
    listings: Mutex<VecDeque<UpstreamListing>>,
    updates: Mutex<VecDeque<Result<UpstreamListing, ListingsApiError>>>,
    page_calls: Mutex<Vec<(usize, usize)>>,
    update_calls: Mutex<Vec<UpdateCall>>,
    disposed: Mutex<Vec<String>>,
    deleted: Mutex<Vec<String>>,
}

impl MockListingsApi {
    fn fixtures() -> Self {
        Self {
            categories: fixture("categories.json"),
            pages: Mutex::new(VecDeque::from([
                fixture("page-1.json"),
                fixture("page-2.json"),
            ])),
            listings: Mutex::new(VecDeque::from([fixture("detail.json")])),
            updates: Mutex::new(VecDeque::new()),
            page_calls: Mutex::new(Vec::new()),
            update_calls: Mutex::new(Vec::new()),
            disposed: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
        }
    }
}

impl ListingsApi for MockListingsApi {
    fn categories(&self) -> Result<Vec<UpstreamCategory>, ListingsApiError> {
        Ok(self.categories.clone())
    }

    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Result<UpstreamListingPage, ListingsApiError> {
        self.page_calls.lock().unwrap().push((offset, limit));
        self.pages
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ListingsApiError::UnexpectedResponse("missing mock page".to_owned()))
    }

    fn listing(&self, _listing_id: &str) -> Result<UpstreamListing, ListingsApiError> {
        self.listings
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| ListingsApiError::UnexpectedResponse("missing mock listing".to_owned()))
    }

    fn update_listing(
        &self,
        listing_id: &str,
        etag: &str,
        fields: &BTreeMap<String, Value>,
    ) -> Result<UpstreamListing, ListingsApiError> {
        self.update_calls.lock().unwrap().push((
            listing_id.to_owned(),
            etag.to_owned(),
            fields.clone(),
        ));
        self.updates
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(fixture("detail-updated.json")))
    }

    fn dispose_listing(&self, listing_id: &str) -> Result<(), ListingsApiError> {
        self.disposed.lock().unwrap().push(listing_id.to_owned());
        Ok(())
    }

    fn delete_listing(&self, listing_id: &str) -> Result<(), ListingsApiError> {
        self.deleted.lock().unwrap().push(listing_id.to_owned());
        Ok(())
    }
}

#[test]
fn discovers_category_roots_children_and_search_paths() {
    let api = MockListingsApi::fixtures();
    let listings = Listings::new(&api);

    let roots = listings.categories(None).unwrap();
    assert_eq!(roots.categories.len(), 2);
    assert_eq!(roots.categories[0].category_id, "100");
    assert!(!roots.categories[0].selectable);

    let children = listings.categories(Some("100")).unwrap();
    assert_eq!(children.categories.len(), 1);
    assert_eq!(children.categories[0].category_id, "110");

    let matches = listings.search_categories("työtuolit").unwrap();
    assert_eq!(matches.categories.len(), 1);
    assert_eq!(
        matches.categories[0].path,
        "Koti ja sisustus > Huonekalut > Työtuolit"
    );
}

#[test]
fn transparently_paginates_the_fifty_item_cap_and_normalizes_results() {
    let api = MockListingsApi::fixtures();
    let collection = Listings::new(&api).list().unwrap();

    assert_eq!(collection.total, 52);
    assert_eq!(collection.listings.len(), 52);
    assert_eq!(collection.facets.len(), 3);
    assert_eq!(collection.facets[1].state, ListingState::Active);
    assert_eq!(collection.listings[0].listing_id, "1000");
    assert_eq!(collection.listings[0].statistics.views, Some(100));
    assert_eq!(
        collection.listings[0].actions[0].name,
        ListingActionName::Edit
    );
    assert_eq!(
        *api.page_calls.lock().unwrap(),
        [(0, LISTING_PAGE_SIZE), (50, LISTING_PAGE_SIZE)]
    );
}

#[test]
fn show_normalizes_complete_fields_statistics_and_actions() {
    let api = MockListingsApi::fixtures();
    let detail = Listings::new(&api).show("36443414").unwrap();

    assert_eq!(detail.state, ListingState::Active);
    assert_eq!(detail.fields["material"], "10");
    assert_eq!(detail.statistics.views, Some(1234));
    assert_eq!(detail.statistics.favorites, Some(17));
    assert_eq!(detail.actions[1].name, ListingActionName::Delete);
    assert_eq!(detail.actions[1].method, "DELETE");
}

#[test]
fn update_preserves_unmentioned_fields_and_uses_the_fetched_etag() {
    let api = MockListingsApi::fixtures();
    let detail = Listings::new(&api)
        .update(
            "36443414",
            BTreeMap::from([("price".to_owned(), json!(50))]),
        )
        .unwrap();

    assert_eq!(detail.fields["price"], 50);
    let calls = api.update_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "36443414");
    assert_eq!(calls[0].1, "listing-v7");
    assert_eq!(calls[0].2["price"], 50);
    assert_eq!(calls[0].2["condition"], "3");
    assert_eq!(calls[0].2["material"], "10");
    assert_eq!(calls[0].2["description"], "Solid birch");
}

#[test]
fn etag_conflict_returns_fresh_state_without_retrying_the_mutation() {
    let mut api = MockListingsApi::fixtures();
    api.listings = Mutex::new(VecDeque::from([
        fixture("detail.json"),
        fixture("detail-updated.json"),
    ]));
    api.updates = Mutex::new(VecDeque::from([Err(ListingsApiError::Conflict)]));

    let error = Listings::new(&api)
        .update(
            "36443414",
            BTreeMap::from([("price".to_owned(), json!(60))]),
        )
        .unwrap_err();

    assert_eq!(error.code, "listing.conflict");
    assert_eq!(error.exit_class.code(), 30);
    assert!(error.retryable);
    assert_eq!(error.details.unwrap()["current"]["fields"]["price"], 50);
    assert_eq!(api.update_calls.lock().unwrap().len(), 1);
}

#[test]
fn dispose_delete_and_copy_hooks_are_immediate_and_deterministic() {
    let mut api = MockListingsApi::fixtures();
    api.listings = Mutex::new(VecDeque::from([fixture("detail.json")]));
    let listings = Listings::new(&api);

    let disposed = listings.dispose("36443414").unwrap();
    assert_eq!(disposed.state, ListingState::Disposed);
    let deleted = listings.delete("36443415").unwrap();
    assert_eq!(deleted.listing_id, "36443415");
    let source = listings.copy_source("36443414").unwrap();

    assert_eq!(*api.disposed.lock().unwrap(), ["36443414"]);
    assert_eq!(*api.deleted.lock().unwrap(), ["36443415"]);
    assert!(!source.fields.contains_key("images"));
    assert_eq!(
        source.image_urls,
        [
            "https://img.example/full-1.jpg",
            "https://img.example/full-2.jpg"
        ]
    );
}

#[test]
fn json_and_flag_duplicates_are_a_structured_usage_error() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("listing.json");
    std::fs::write(&input, r#"{"title":"JSON title","condition":"3"}"#).unwrap();
    let cli = Cli::parse_from([
        "tori",
        "listing",
        "update",
        "36443414",
        "--title",
        "Flag title",
        "--input",
        input.to_str().unwrap(),
    ]);
    let Command::Listing(args) = cli.command else {
        panic!("expected listing command");
    };
    let ListingCommand::Update { values, .. } = args.command else {
        panic!("expected listing update");
    };

    let error = listing_changes(*values).unwrap_err();
    assert_eq!(error.code, "cli.invalid_usage");
    assert_eq!(error.exit_class.code(), 2);
}

#[test]
fn semantic_values_and_failures_use_structured_errors() {
    let api = MockListingsApi::fixtures();
    let error = Listings::new(&api)
        .update(
            "36443414",
            BTreeMap::from([("trade_type".to_owned(), json!("Give away"))]),
        )
        .unwrap_err();

    assert_eq!(error.code, "listing.validation_failed");
    assert_eq!(error.exit_class.code(), 20);
    assert_eq!(
        error.details.unwrap()["fields"]["trade_type"],
        "expected one of: sell, give_away, wanted"
    );
    assert!(api.update_calls.lock().unwrap().is_empty());
}

fn fixture<T: DeserializeOwned>(name: &str) -> T {
    let document = match name {
        "categories.json" => include_str!("fixtures/listings/categories.json"),
        "page-1.json" => include_str!("fixtures/listings/page-1.json"),
        "page-2.json" => include_str!("fixtures/listings/page-2.json"),
        "detail.json" => include_str!("fixtures/listings/detail.json"),
        "detail-updated.json" => include_str!("fixtures/listings/detail-updated.json"),
        _ => panic!("unknown fixture {name}"),
    };
    serde_json::from_str(document).unwrap_or_else(|error| panic!("invalid fixture {name}: {error}"))
}
