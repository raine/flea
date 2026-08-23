use std::{
    collections::{BTreeMap, VecDeque},
    future::Future,
    pin::Pin,
    sync::Mutex,
};

use clap::Parser;
use flea::{
    api::listings::{
        CATEGORY_SEARCH_LIMIT_DEFAULT, CATEGORY_SEARCH_LIMIT_MAX, CategorySearchOptions,
        LISTING_PAGE_SIZE, Listings, ListingsApi, ListingsApiError, UpstreamCategory,
        UpstreamListing, UpstreamListingPage,
    },
    cli::{
        Cli, Command,
        listing::{ListingCommand, listing_changes},
    },
    domain::listing::{ListingActionName, ListingState},
};
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

type UpdateCall = (String, String, BTreeMap<String, Value>);

struct MockListingsApi {
    categories: Result<Vec<UpstreamCategory>, ListingsApiError>,
    pages: Mutex<VecDeque<UpstreamListingPage>>,
    listings: Mutex<VecDeque<UpstreamListing>>,
    listing_errors: Mutex<VecDeque<ListingsApiError>>,
    updates: Mutex<VecDeque<Result<UpstreamListing, ListingsApiError>>>,
    page_calls: Mutex<Vec<(usize, usize)>>,
    update_calls: Mutex<Vec<UpdateCall>>,
    disposed: Mutex<Vec<String>>,
    deleted: Mutex<Vec<String>>,
}

impl MockListingsApi {
    fn fixtures() -> Self {
        Self {
            categories: Ok(fixture("categories.json")),
            pages: Mutex::new(VecDeque::from([
                fixture("page-1.json"),
                fixture("page-2.json"),
            ])),
            listings: Mutex::new(VecDeque::from([fixture("detail.json")])),
            listing_errors: Mutex::new(VecDeque::new()),
            updates: Mutex::new(VecDeque::new()),
            page_calls: Mutex::new(Vec::new()),
            update_calls: Mutex::new(Vec::new()),
            disposed: Mutex::new(Vec::new()),
            deleted: Mutex::new(Vec::new()),
        }
    }
}

impl ListingsApi for MockListingsApi {
    fn categories(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<UpstreamCategory>, ListingsApiError>> + Send + '_>>
    {
        let categories = self.categories.clone();
        Box::pin(async move { categories })
    }

    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListingPage, ListingsApiError>> + Send + '_>>
    {
        self.page_calls.lock().unwrap().push((offset, limit));
        let page =
            self.pages.lock().unwrap().pop_front().ok_or_else(|| {
                ListingsApiError::UnexpectedResponse("missing mock page".to_owned())
            });
        Box::pin(async move { page })
    }

    fn listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>> {
        let result = if let Some(error) = self.listing_errors.lock().unwrap().pop_front() {
            Err(error)
        } else if let Some(index) = listing_id
            .strip_prefix("10")
            .and_then(|value| value.parse::<u64>().ok())
        {
            Ok(serde_json::from_value(json!({
                "id": listing_id,
                "etag": format!("listing-{listing_id}"),
                "fields": {"trade_type": "sell", "price": index + 10}
            }))
            .unwrap())
        } else {
            self.listings.lock().unwrap().pop_front().ok_or_else(|| {
                ListingsApiError::UnexpectedResponse("missing mock listing".to_owned())
            })
        };
        Box::pin(async move { result })
    }

    fn update_listing<'a>(
        &'a self,
        listing_id: &'a str,
        etag: &'a str,
        fields: &'a BTreeMap<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>> {
        self.update_calls.lock().unwrap().push((
            listing_id.to_owned(),
            etag.to_owned(),
            fields.clone(),
        ));
        let result = self
            .updates
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok(fixture("detail-updated.json")));
        Box::pin(async move { result })
    }

    fn dispose_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>> {
        self.disposed.lock().unwrap().push(listing_id.to_owned());
        Box::pin(async { Ok(()) })
    }

    fn delete_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>> {
        self.deleted.lock().unwrap().push(listing_id.to_owned());
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn discovers_category_roots_children_and_search_paths() {
    let api = MockListingsApi::fixtures();
    let listings = Listings::new(&api);

    let roots = listings.categories(None).await.unwrap();
    assert_eq!(roots.categories.len(), 2);
    assert_eq!(roots.categories[0].category_id, "100");
    assert!(!roots.categories[0].selectable);

    let children = listings.categories(Some("100")).await.unwrap();
    assert_eq!(children.categories.len(), 1);
    assert_eq!(children.categories[0].category_id, "110");

    let matches = listings.search_categories("työtuolit").await.unwrap();
    assert_eq!(matches.categories.len(), 1);
    assert_eq!(
        matches.categories[0].path,
        "Koti ja sisustus > Huonekalut > Työtuolit"
    );
}

#[tokio::test]
async fn live_taxonomy_fixture_flattens_and_matches_finnish_queries() {
    let taxonomy: flea::api::listings::UpstreamCategoryTaxonomy = serde_json::from_str(
        include_str!("fixtures/listings/category-taxonomy-live.json"),
    )
    .unwrap();
    let mut api = MockListingsApi::fixtures();
    api.categories = Ok(taxonomy.categories);
    let listings = Listings::new(&api);

    let cycling = listings.search_categories("polkupyörä").await.unwrap();
    assert_eq!(cycling.categories.len(), 2);
    assert_eq!(cycling.categories[0].category_id, "257");
    assert_eq!(cycling.categories[1].category_id, "8375");
    assert_eq!(
        cycling.categories[0].path,
        "Urheilu ja ulkoilu > Pyöräily > Polkupyörät"
    );

    let children = listings.categories(Some("3963")).await.unwrap();
    assert_eq!(children.categories.len(), 6);
    assert!(children.categories.iter().all(|category| {
        category.parent_id.as_deref() == Some("3963")
            && category
                .path
                .starts_with("Urheilu ja ulkoilu > Pyöräily > ")
    }));

    let components = listings
        .search_categories("tietokonekomponentit")
        .await
        .unwrap();
    assert_eq!(components.categories.len(), 1);
    assert_eq!(components.categories[0].category_id, "8368");
    assert_eq!(components.categories[0].taxonomy_value, "2.93.3215.8368");
    assert_eq!(
        serde_json::to_value(&components).unwrap()["categories"][0]["taxonomy_value"],
        "2.93.3215.8368"
    );

    let no_matches = listings.search_categories("lukko").await.unwrap();
    assert!(no_matches.categories.is_empty());
    assert_eq!(no_matches.returned, 0);
    assert_eq!(no_matches.total, 0);
    assert!(!no_matches.truncated);
}

#[tokio::test]
async fn category_search_bounds_broad_results_and_reports_pagination() {
    let taxonomy: flea::api::listings::UpstreamCategoryTaxonomy = serde_json::from_str(
        include_str!("fixtures/listings/category-taxonomy-live.json"),
    )
    .unwrap();
    let mut api = MockListingsApi::fixtures();
    api.categories = Ok(taxonomy.categories);
    let listings = Listings::new(&api);

    let first = listings.search_categories("tarvikkeet").await.unwrap();
    assert_eq!(first.limit, CATEGORY_SEARCH_LIMIT_DEFAULT);
    assert_eq!(first.offset, 0);
    assert_eq!(first.returned, CATEGORY_SEARCH_LIMIT_DEFAULT);
    assert!(first.total > first.returned);
    assert!(first.truncated);

    let second = listings
        .search_categories_with_options(
            "tarvikkeet",
            CategorySearchOptions {
                offset: first.returned,
                limit: 7,
                ..CategorySearchOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(second.offset, CATEGORY_SEARCH_LIMIT_DEFAULT);
    assert_eq!(second.limit, 7);
    assert_eq!(second.returned, 7);
    assert_eq!(second.total, first.total);
    assert_ne!(second.categories[0], first.categories[0]);
}

#[tokio::test]
async fn category_search_resolves_ids_labels_and_finnish_unicode_deterministically() {
    let taxonomy: flea::api::listings::UpstreamCategoryTaxonomy = serde_json::from_str(
        include_str!("fixtures/listings/category-taxonomy-live.json"),
    )
    .unwrap();
    let mut api = MockListingsApi::fixtures();
    api.categories = Ok(taxonomy.categories);
    let listings = Listings::new(&api);

    let id = listings.search_categories("258").await.unwrap();
    assert_eq!(id.categories.len(), 1);
    assert_eq!(id.categories[0].label, "Pyöräilyvarusteet");

    let label = listings
        .search_categories("PYÖRÄILYVARUSTEET")
        .await
        .unwrap();
    assert_eq!(label.categories[0].category_id, "258");

    let decomposed = listings
        .search_categories("PYO\u{308}RA\u{308}ILYVARUSTEET")
        .await
        .unwrap();
    assert_eq!(decomposed.categories[0].category_id, "258");

    let finnish = listings
        .search_categories("ÖLJYVÄRIMAALAUKSET")
        .await
        .unwrap();
    assert_eq!(finnish.categories[0].category_id, "390");
}

#[tokio::test]
async fn category_search_filters_bicycle_accessories_by_parent_or_path() {
    let taxonomy: flea::api::listings::UpstreamCategoryTaxonomy = serde_json::from_str(
        include_str!("fixtures/listings/category-taxonomy-live.json"),
    )
    .unwrap();
    let mut api = MockListingsApi::fixtures();
    api.categories = Ok(taxonomy.categories);
    let listings = Listings::new(&api);

    let by_parent = listings
        .search_categories_with_options(
            "tarvikkeet",
            CategorySearchOptions {
                parent: Some("3963"),
                ..CategorySearchOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(by_parent.context.as_ref().unwrap().category_id, "3963");
    assert_eq!(by_parent.categories[0].category_id, "258");
    assert_eq!(by_parent.categories[0].label, "Pyöräilyvarusteet");

    let by_path = listings
        .search_categories_with_options(
            "TARVIKKEET",
            CategorySearchOptions {
                path: Some("Urheilu ja ulkoilu > PYÖRÄILY"),
                ..CategorySearchOptions::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(by_path.context.as_ref().unwrap().category_id, "3963");
    assert_eq!(by_path.categories, by_parent.categories);
}

#[tokio::test]
async fn category_ranking_orders_exact_prefix_token_context_and_substring_matches() {
    let mut api = MockListingsApi::fixtures();
    api.categories = Ok(vec![
        UpstreamCategory {
            id: "10".to_owned(),
            label: "Pyöräily".to_owned(),
            parent_id: None,
            selectable: Some(true),
            children: vec![
                UpstreamCategory {
                    id: "11".to_owned(),
                    label: "Pyöräilyvarusteet".to_owned(),
                    parent_id: None,
                    selectable: Some(true),
                    children: Vec::new(),
                },
                UpstreamCategory {
                    id: "12".to_owned(),
                    label: "Hauska pyöräily opas".to_owned(),
                    parent_id: None,
                    selectable: Some(true),
                    children: Vec::new(),
                },
                UpstreamCategory {
                    id: "13".to_owned(),
                    label: "Kengät".to_owned(),
                    parent_id: None,
                    selectable: Some(true),
                    children: Vec::new(),
                },
            ],
        },
        UpstreamCategory {
            id: "20".to_owned(),
            label: "Retkipyöräilyoppaat".to_owned(),
            parent_id: None,
            selectable: Some(true),
            children: Vec::new(),
        },
    ]);
    let results = Listings::new(&api)
        .search_categories("pyöräily")
        .await
        .unwrap();
    let ids = results
        .categories
        .iter()
        .map(|category| category.category_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, ["10", "11", "12", "13", "20"]);
}

#[tokio::test]
async fn category_search_breaks_equal_relevance_ties_by_path_and_id() {
    let mut api = MockListingsApi::fixtures();
    api.categories = Ok(vec![
        UpstreamCategory {
            id: "20".to_owned(),
            label: "B".to_owned(),
            parent_id: None,
            selectable: Some(false),
            children: vec![UpstreamCategory {
                id: "22".to_owned(),
                label: "Tarvikkeet".to_owned(),
                parent_id: None,
                selectable: Some(true),
                children: Vec::new(),
            }],
        },
        UpstreamCategory {
            id: "10".to_owned(),
            label: "A".to_owned(),
            parent_id: None,
            selectable: Some(false),
            children: vec![
                UpstreamCategory {
                    id: "12".to_owned(),
                    label: "Tarvikkeet".to_owned(),
                    parent_id: None,
                    selectable: Some(true),
                    children: Vec::new(),
                },
                UpstreamCategory {
                    id: "11".to_owned(),
                    label: "Tarvikkeet".to_owned(),
                    parent_id: None,
                    selectable: Some(true),
                    children: Vec::new(),
                },
            ],
        },
    ]);
    let results = Listings::new(&api)
        .search_categories("tarvikkeet")
        .await
        .unwrap();
    let ids = results
        .categories
        .iter()
        .map(|category| category.category_id.as_str())
        .collect::<Vec<_>>();

    assert_eq!(ids, ["11", "12", "22"]);
}

#[tokio::test]
async fn category_search_rejects_limits_outside_the_documented_bound() {
    let api = MockListingsApi::fixtures();
    let listings = Listings::new(&api);
    for limit in [0, CATEGORY_SEARCH_LIMIT_MAX + 1] {
        let error = listings
            .search_categories_with_options(
                "tuolit",
                CategorySearchOptions {
                    limit,
                    ..CategorySearchOptions::default()
                },
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, "cli.invalid_usage");
        assert!(error.message.contains("between 1 and 100"));
    }
}

#[tokio::test]
async fn category_failures_distinguish_collection_parent_and_protocol_errors() {
    let mut api = MockListingsApi::fixtures();
    api.categories = Err(ListingsApiError::NotFound);
    let endpoint = Listings::new(&api)
        .search_categories("pyöräily")
        .await
        .unwrap_err();
    assert_eq!(endpoint.code, "category.endpoint_unavailable");
    assert_eq!(endpoint.exit_class.code(), 40);

    let api = MockListingsApi::fixtures();
    let parent = Listings::new(&api)
        .categories(Some("missing"))
        .await
        .unwrap_err();
    assert_eq!(parent.code, "category.not_found");
    assert_eq!(parent.details.unwrap()["category_id"], "missing");

    let mut api = MockListingsApi::fixtures();
    api.categories = Ok(vec![UpstreamCategory {
        id: "100".to_owned(),
        label: String::new(),
        parent_id: None,
        selectable: Some(true),
        children: Vec::new(),
    }]);
    let malformed = Listings::new(&api).categories(None).await.unwrap_err();
    assert_eq!(malformed.code, "category.protocol_drift");
}

#[tokio::test]
async fn transparently_paginates_the_fifty_item_cap_and_normalizes_results() {
    let api = MockListingsApi::fixtures();
    let collection = Listings::new(&api).list().await.unwrap();

    assert_eq!(collection.total, 52);
    assert_eq!(collection.listings.len(), 52);
    assert_eq!(collection.facets.len(), 3);
    assert_eq!(collection.facets[1].state, ListingState::Active);
    assert_eq!(collection.listings[0].listing_id, "1000");
    assert_eq!(collection.listings[0].statistics.views, Some(100));
    assert_eq!(
        collection.listings[0].trade_type,
        flea::domain::commerce::TradeType::Sell
    );
    assert_eq!(collection.listings[0].price.amount, Some(json!(10)));
    assert_eq!(
        collection.listings[0].price.currency.as_deref(),
        Some("EUR")
    );
    assert_eq!(
        collection.listings[0].actions[0].name,
        ListingActionName::Edit
    );
    assert_eq!(
        *api.page_calls.lock().unwrap(),
        [(0, LISTING_PAGE_SIZE), (50, LISTING_PAGE_SIZE)]
    );
}

#[tokio::test]
async fn show_normalizes_complete_fields_statistics_and_actions() {
    let api = MockListingsApi::fixtures();
    let detail = Listings::new(&api).show("36443414").await.unwrap();

    assert_eq!(detail.state, ListingState::Active);
    assert_eq!(detail.fields["material"], "10");
    assert_eq!(detail.trade_type, flea::domain::commerce::TradeType::Sell);
    assert_eq!(detail.price.amount, Some(json!(45)));
    assert!(!detail.fields.contains_key("price"));
    assert!(!detail.fields.contains_key("trade_type"));
    assert_eq!(detail.statistics.views, Some(1234));
    assert_eq!(detail.statistics.favorites, Some(17));
    assert_eq!(detail.actions[1].name, ListingActionName::Delete);
    assert_eq!(detail.actions[1].method, "DELETE");
}

#[tokio::test]
async fn show_merges_normalized_summary_values_into_partial_detail_models() {
    let mut api = MockListingsApi::fixtures();
    api.listings = Mutex::new(VecDeque::from([serde_json::from_value(json!({
        "id": "46031010",
        "state": { "type": "ACTIVE" },
        "fields": { "description": "Lock cable" },
        "data": {
            "title": "Bicycle lock cable",
            "subtitle": "Tori myydään 5 €",
            "image": "https://img.example/lock.jpg"
        }
    }))
    .unwrap()]));

    let detail = Listings::new(&api).show("46031010").await.unwrap();

    assert_eq!(detail.fields["title"], "Bicycle lock cable");
    assert_eq!(detail.trade_type, flea::domain::commerce::TradeType::Sell);
    assert_eq!(detail.price.kind, flea::domain::commerce::PriceKind::Fixed);
    assert_eq!(detail.price.amount, Some(json!(5)));
    assert_eq!(detail.price.currency.as_deref(), Some("EUR"));
    assert_eq!(detail.price.display.as_deref(), Some("Tori myydään 5 €"));
    assert_eq!(detail.fields["image"], "https://img.example/lock.jpg");
    assert_eq!(
        detail.fields["public_url"],
        "https://www.tori.fi/recommerce/forsale/item/46031010"
    );
}

#[tokio::test]
async fn show_reconciles_detail_not_found_with_the_matching_active_collection_item() {
    let mut api = MockListingsApi::fixtures();
    api.listing_errors = Mutex::new(VecDeque::from([ListingsApiError::NotFound]));
    api.pages = Mutex::new(VecDeque::from([serde_json::from_value(json!({
        "summaries": [{
            "id": "46031010",
            "state": { "type": "ACTIVE" },
            "data": {
                "title": "Bicycle lock cable",
                "subtitle": "5 €",
                "location": "Helsinki",
                "image": "https://img.example/lock.jpg"
            }
        }],
        "total": 1
    }))
    .unwrap()]));

    let detail = Listings::new(&api).show("46031010").await.unwrap();

    assert_eq!(detail.listing_id, "46031010");
    assert_eq!(detail.state, ListingState::Active);
    assert_eq!(detail.fields["title"], "Bicycle lock cable");
    assert_eq!(
        detail.trade_type,
        flea::domain::commerce::TradeType::Unknown
    );
    assert_eq!(
        detail.price.kind,
        flea::domain::commerce::PriceKind::Unavailable
    );
    assert_eq!(detail.price.display.as_deref(), Some("5 €"));
    assert_eq!(detail.fields["location"], "Helsinki");
    assert_eq!(detail.fields["image"], "https://img.example/lock.jpg");
    assert_eq!(
        detail.fields["public_url"],
        "https://www.tori.fi/recommerce/forsale/item/46031010"
    );
}

#[tokio::test]
async fn show_preserves_definitive_not_found_after_detail_and_collection_agree() {
    let mut api = MockListingsApi::fixtures();
    api.listing_errors = Mutex::new(VecDeque::from([ListingsApiError::NotFound]));
    api.pages = Mutex::new(VecDeque::from([serde_json::from_value(json!({
        "summaries": [],
        "total": 0
    }))
    .unwrap()]));

    let error = Listings::new(&api).show("46031010").await.unwrap_err();

    assert_eq!(error.code, "listing.not_found");
    assert!(!error.upstream_transient);
    assert!(!error.safe_to_retry);
}

#[tokio::test]
async fn show_reports_observation_delay_when_collection_cannot_reconcile_not_found() {
    let mut api = MockListingsApi::fixtures();
    api.listing_errors = Mutex::new(VecDeque::from([ListingsApiError::NotFound]));
    api.pages = Mutex::new(VecDeque::from([serde_json::from_value(json!({
        "summaries": [],
        "total": 1
    }))
    .unwrap()]));

    let error = Listings::new(&api).show("46031010").await.unwrap_err();

    assert_eq!(error.code, "listing.observation_delayed");
    assert!(error.safe_to_retry);
    assert_eq!(
        error.details.as_ref().unwrap()["detail_status"],
        "not_found"
    );
    assert_eq!(error.details.as_ref().unwrap()["observation_attempts"], 2);
    assert_eq!(
        error.next_actions[0].command,
        "flea tori listing show 46031010"
    );
}

#[tokio::test]
async fn show_uses_collection_when_detail_model_is_unexpected() {
    let mut api = MockListingsApi::fixtures();
    api.listing_errors = Mutex::new(VecDeque::from([ListingsApiError::UnexpectedResponse(
        "fixture model".to_owned(),
    )]));
    api.pages = Mutex::new(VecDeque::from([serde_json::from_value(json!({
        "summaries": [{
            "id": 46031010,
            "state": { "display": "published" },
            "data": { "title": "Bicycle lock cable" }
        }],
        "total": 1
    }))
    .unwrap()]));

    let detail = Listings::new(&api).show("46031010").await.unwrap();

    assert_eq!(detail.listing_id, "46031010");
    assert_eq!(detail.state, ListingState::Active);
}

#[tokio::test]
async fn show_preserves_safe_model_diagnostics_when_no_fallback_matches() {
    let mut api = MockListingsApi::fixtures();
    api.listing_errors = Mutex::new(VecDeque::from([ListingsApiError::UnexpectedResponse(
        "private fixture body".to_owned(),
    )]));
    api.pages = Mutex::new(VecDeque::from([serde_json::from_value(json!({
        "summaries": [],
        "total": 0
    }))
    .unwrap()]));

    let error = Listings::new(&api).show("46031010").await.unwrap_err();

    assert_eq!(error.code, "upstream.unexpected_response");
    assert_eq!(
        error.details.as_ref().unwrap()["response_model"],
        "unrecognized"
    );
    assert_eq!(
        error.details.as_ref().unwrap()["response_status_class"],
        "success"
    );
    assert!(!format!("{error:?}").contains("private fixture body"));
}

#[tokio::test]
async fn update_preserves_unmentioned_fields_and_uses_the_fetched_etag() {
    let api = MockListingsApi::fixtures();
    let detail = Listings::new(&api)
        .update(
            "36443414",
            BTreeMap::from([("price".to_owned(), json!(50))]),
        )
        .await
        .unwrap();

    assert_eq!(detail.price.amount, Some(json!(50)));
    let calls = api.update_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "36443414");
    assert_eq!(calls[0].1, "listing-v7");
    assert_eq!(calls[0].2["price"], 50);
    assert_eq!(calls[0].2["condition"], "3");
    assert_eq!(calls[0].2["material"], "10");
    assert_eq!(calls[0].2["description"], "Solid birch");
}

#[tokio::test]
async fn etag_conflict_returns_fresh_state_without_retrying_the_mutation() {
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
        .await
        .unwrap_err();

    assert_eq!(error.code, "listing.conflict");
    assert_eq!(error.exit_class.code(), 30);
    assert!(!error.upstream_transient);
    assert!(error.safe_to_retry);
    assert_eq!(
        error.next_actions[0].command,
        "flea tori listing show 36443414"
    );
    assert_eq!(error.details.unwrap()["current"]["price"]["amount"], 50);
    assert_eq!(api.update_calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn etag_conflict_without_fresh_observation_requires_authoritative_show() {
    let mut api = MockListingsApi::fixtures();
    api.updates = Mutex::new(VecDeque::from([Err(ListingsApiError::Conflict)]));

    let error = Listings::new(&api)
        .update(
            "36443414",
            BTreeMap::from([("price".to_owned(), json!(60))]),
        )
        .await
        .unwrap_err();

    assert!(!error.upstream_transient);
    assert!(!error.safe_to_retry);
    assert_eq!(error.details.unwrap()["current"], Value::Null);
    assert_eq!(
        error.next_actions[0].command,
        "flea tori listing show 36443414"
    );
}

#[tokio::test]
async fn dispose_delete_and_copy_hooks_are_immediate_and_deterministic() {
    let mut api = MockListingsApi::fixtures();
    api.listings = Mutex::new(VecDeque::from([fixture("detail.json")]));
    let listings = Listings::new(&api);

    let disposed = listings.dispose("36443414").await.unwrap();
    assert_eq!(disposed.state, ListingState::Disposed);
    let deleted = listings.delete("36443415").await.unwrap();
    assert_eq!(deleted.listing_id, "36443415");
    let source = listings.copy_source("36443414").await.unwrap();

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

#[tokio::test]
async fn json_and_flag_duplicates_are_a_structured_usage_error() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("listing.json");
    std::fs::write(&input, r#"{"title":"JSON title","condition":"3"}"#).unwrap();
    let cli = Cli::parse_from([
        "flea",
        "tori",
        "listing",
        "update",
        "36443414",
        "--title",
        "Flag title",
        "--input",
        input.to_str().unwrap(),
    ]);
    let Command::Tori(tori) = cli.command else {
        panic!("expected Tori command");
    };
    let flea::cli::ToriCommand::Listing(args) = tori.command else {
        panic!("expected listing command");
    };
    let ListingCommand::Update { values, .. } = args.command else {
        panic!("expected listing update");
    };

    let error = listing_changes(*values).unwrap_err();
    assert_eq!(error.code, "cli.invalid_usage");
    assert_eq!(error.exit_class.code(), 2);
}

#[tokio::test]
async fn ambiguous_mutation_failures_include_listing_recovery_context() {
    let mut api = MockListingsApi::fixtures();
    api.updates = Mutex::new(VecDeque::from([Err(ListingsApiError::Upstream(502))]));

    let error = Listings::new(&api)
        .update(
            "36443414",
            BTreeMap::from([("price".to_owned(), json!(60))]),
        )
        .await
        .unwrap_err();

    assert_eq!(error.exit_class.code(), 50);
    assert_eq!(error.code, "mutation.uncertain");
    assert!(error.upstream_transient);
    assert!(!error.safe_to_retry);
    assert_eq!(error.partial.as_ref().unwrap()["listing_id"], "36443414");
    assert_eq!(error.partial.as_ref().unwrap()["operation"], "update");
    assert_eq!(
        error.next_actions[0].command,
        "flea tori listing show 36443414"
    );
    assert!(!error.message.contains("upstream-secret"));
}

#[tokio::test]
async fn rejects_listing_ids_that_can_change_request_paths() {
    let api = MockListingsApi::fixtures();

    let error = Listings::new(&api)
        .show("../credentials")
        .await
        .unwrap_err();

    assert_eq!(error.exit_class.code(), 2);
    assert!(api.listings.lock().unwrap().len() == 1);
}

#[tokio::test]
async fn semantic_values_and_failures_use_structured_errors() {
    let api = MockListingsApi::fixtures();
    let error = Listings::new(&api)
        .update(
            "36443414",
            BTreeMap::from([("trade_type".to_owned(), json!("Give away"))]),
        )
        .await
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
