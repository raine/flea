use std::{collections::BTreeMap, sync::Mutex};

use clap::Parser;
use flea::{
    api::{
        item::{PublicItemApi, PublicItemApiError},
        search::{
            PublicSearch, PublicSearchApi, SEARCH_FACET_OPTION_LIMIT, SearchApiError,
            UpstreamSearchRequest,
        },
    },
    cli::{Cli, Command, search},
};
use serde_json::{Value, json};

struct FixtureApi {
    search_response: Value,
    location_response: Value,
    requests: Mutex<Vec<UpstreamSearchRequest>>,
}

impl FixtureApi {
    fn new(search_response: Value) -> Self {
        Self {
            location_response: location_fixture(),
            search_response,
            requests: Mutex::new(Vec::new()),
        }
    }
}

impl PublicSearchApi for FixtureApi {
    fn search(&self, request: &UpstreamSearchRequest) -> Result<Value, SearchApiError> {
        self.requests.lock().unwrap().push(request.clone());
        Ok(self.search_response.clone())
    }

    fn location_metadata(&self) -> Result<Value, SearchApiError> {
        Ok(self.location_response.clone())
    }
}

struct ExplainItemApi {
    responses: BTreeMap<String, Result<Value, PublicItemApiError>>,
    requests: Mutex<Vec<String>>,
}

impl PublicItemApi for ExplainItemApi {
    fn item(&self, listing_id: &str) -> Result<Value, PublicItemApiError> {
        self.requests.lock().unwrap().push(listing_id.to_owned());
        self.responses
            .get(listing_id)
            .cloned()
            .unwrap_or(Err(PublicItemApiError::NotFound))
    }
}

#[test]
fn saved_search_creation_reuses_public_search_argument_mapping() {
    let api = FixtureApi::new(empty_fixture());
    let cli = Cli::parse_from([
        "flea",
        "saved-search",
        "create",
        "--name",
        "Chair alert",
        "--email",
        "chair",
        "--category",
        "2.93.3215.8368",
        "--price-from",
        "10",
        "--trade-type",
        "sell",
        "--shipping",
        "--facet",
        "brand=42",
    ]);
    let Command::SavedSearch(args) = cli.command else {
        panic!("saved search command")
    };
    let flea::cli::saved_search::SavedSearchCommand::Create { search: args, .. } = args.command
    else {
        panic!("create command")
    };

    let parameters = search::saved_search_parameters(args, &api).unwrap();

    assert_eq!(parameters["q"], ["chair"]);
    assert_eq!(parameters["product_category"], ["2.93.3215.8368"]);
    assert_eq!(parameters["price_from"], ["10"]);
    assert_eq!(parameters["trade_type"], ["1"]);
    assert_eq!(parameters["shipping_exists"], ["true"]);
    assert_eq!(parameters["brand"], ["42"]);
    assert!(!parameters.contains_key("dealer_segment"));
}

#[test]
fn canonical_leaf_category_value_is_accepted_by_search() {
    let api = FixtureApi::new(empty_fixture());
    let cli = Cli::parse_from([
        "flea",
        "search",
        "kävelymatto & tuoli",
        "--category",
        "2.93.3215.8368",
        "--location",
        "Helsinki",
        "--price-from",
        "10",
        "--price-to",
        "100",
        "--shipping",
        "--page",
        "2",
        "--limit",
        "20",
    ]);
    let Command::Search(args) = cli.command else {
        panic!("search command");
    };
    search::dispatch_with_api(*args, &api).unwrap();
    let request = api.requests.lock().unwrap()[0].clone();

    assert_eq!(
        request.path_and_query(),
        "/search/SEARCH_ID_BAP_COMMON?client=android&location=1.100018.110091&page=2&price_from=10&price_to=100&product_category=2.93.3215.8368&q=k%C3%A4velymatto+%26+tuoli&rows=20&shipping_exists=true"
    );
}

#[test]
fn encodes_helsinki_twenty_kilometer_radius_as_tori_meter_parameters() {
    let api = FixtureApi::new(empty_fixture());
    let cli = Cli::parse_from([
        "flea",
        "search",
        "tuoli",
        "--latitude",
        "60.1699",
        "--longitude",
        "24.9384",
        "--radius-km",
        "20",
        "--limit",
        "1",
    ]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    search::dispatch_with_api(*args, &api).unwrap();
    assert_eq!(
        api.requests.lock().unwrap()[0].path_and_query(),
        "/search/SEARCH_ID_BAP_COMMON?client=android&lat=60.1699&lon=24.9384&page=1&q=tuoli&radius=20000&rows=1"
    );
}

#[test]
fn normalizes_source_observed_docs_metadata_facets_and_privacy_fields() {
    let api = FixtureApi::new(full_fixture());
    let request = UpstreamSearchRequest {
        query: "tuoli".to_owned(),
        page: 1,
        limit: 20,
        include_filters: true,
        facet_option_limit: None,
        parameters: Default::default(),
    };
    let (result, _) = PublicSearch::new(&api)
        .execute(
            &request,
            Some(flea::domain::search::SearchLocation {
                id: "1.100018.110091".to_owned(),
                name: "Helsinki".to_owned(),
                parent: Some("Uusimaa".to_owned()),
                depth: 1,
            }),
        )
        .unwrap();

    let listing = &result.results[0];
    assert_eq!(listing.listing_id, "42346404");
    assert_eq!(listing.price.as_ref().unwrap().amount, 37);
    assert_eq!(
        listing.price.as_ref().unwrap().currency.as_deref(),
        Some("EUR")
    );
    assert_eq!(listing.image_count, Some(2));
    assert_eq!(listing.distance, Some(1200.0));
    assert_eq!(
        listing.published_at.as_deref(),
        Some("2026-08-22T10:23:36Z")
    );
    assert_eq!(listing.condition.as_deref(), Some("Hyvä"));
    assert_eq!(listing.shipping, Some(true));
    assert_eq!(listing.seller.as_deref(), Some("private"));
    assert_eq!(listing.category_id.as_deref(), Some("3215"));
    assert_eq!(
        listing.category_path.as_deref(),
        Some("Koti ja asuminen > Huonekalut")
    );
    assert_eq!(result.results[1].price.as_ref().unwrap().amount, 0);
    assert_eq!(
        result.results[1].category_id.as_deref(),
        Some("2.93.3215.46")
    );
    assert_eq!(
        result.results[1].category_path.as_deref(),
        Some("Koti ja asuminen > Huonekalut > Tuolit")
    );
    assert_eq!(result.results[1].image_count, None);
    assert_eq!(result.pagination.total, 1_200);
    assert!(result.pagination.has_next);
    assert_eq!(result.pagination.next_page, Some(2));
    assert!(result.pagination.capped);
    assert_eq!(
        result.facets[0].options[1].parent_value.as_deref(),
        Some("0.93")
    );
    assert_eq!(
        result.facets[1]
            .range
            .as_ref()
            .unwrap()
            .from_name
            .as_deref(),
        Some("price_from")
    );

    let serialized = serde_json::to_string(&result).unwrap();
    assert_eq!(result.location.as_ref().unwrap().name, "Helsinki");
    assert!(!serialized.contains("applied_filters"));
    for private in [
        "coordinates",
        "tracking",
        "guided_search",
        "quest_time",
        "uuid-fixture",
        "image_urls",
        "published_at_ms",
        "listing_type",
        "display",
        "total_pages",
        "accessible_pages",
        "upstream_page_limit",
        "has_previous",
        "previous_page",
        "resolved_location",
    ] {
        assert!(
            !serialized.contains(private),
            "normalized output leaked {private}"
        );
    }
}

#[test]
fn resolves_unambiguous_location_names_case_insensitively() {
    let api = FixtureApi::new(empty_fixture());
    let search = PublicSearch::new(&api);
    let location = search.resolve_location("  HELSINKI ").unwrap();
    assert_eq!(location.id, "1.100018.110091");
    assert_eq!(location.parent.as_deref(), Some("Uusimaa"));

    let matches = search.locations("sink").unwrap();
    assert_eq!(matches.returned, 1);
    assert_eq!(matches.total, 1);
    assert_eq!(matches.locations[0].name, "Helsinki");
}

#[test]
fn searches_an_explicit_helsinki_area_and_exposes_resolved_locations() {
    let api = FixtureApi::new(full_fixture());
    let cli = Cli::parse_from([
        "flea",
        "search",
        "tuoli",
        "--area",
        "Helsinki,Espoo,Vantaa",
        "--limit",
        "20",
    ]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let output = search::dispatch_with_api(*args, &api).unwrap();

    assert_eq!(
        api.requests.lock().unwrap()[0].path_and_query(),
        "/search/SEARCH_ID_BAP_COMMON?client=android&location=1.100018.110091&location=1.100018.110049&location=1.100018.110092&page=1&q=tuoli&rows=20"
    );
    assert_eq!(output["resolved_area"]["locations"][0]["name"], "Helsinki");
    assert_eq!(output["resolved_area"]["locations"][2]["name"], "Vantaa");
    assert_eq!(
        output["_next_actions"][0]["command"],
        "flea search 'tuoli' --area '1.100018.110091,1.100018.110049,1.100018.110092' --page 2 --limit 20"
    );
}

#[test]
fn unknown_and_ambiguous_place_names_return_actionable_structured_errors() {
    let mut api = FixtureApi::new(empty_fixture());
    api.location_response["filters"][0]["filter_items"][0]["filter_items"][3]["filter_items"] = json!([{
        "display_name": "Kauniainen", "name": "location", "value": "2.100018.110235.202700",
        "filter_items": []
    }]);

    let ambiguous = PublicSearch::new(&api)
        .resolve_location("Kauniainen")
        .unwrap_err();
    assert_eq!(ambiguous.code, "search.location_ambiguous");
    assert_eq!(
        ambiguous.details.as_ref().unwrap()["matches"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(ambiguous.details.as_ref().unwrap()["suggestion"].is_string());

    let unknown = PublicSearch::new(&api)
        .resolve_location("Atlantis")
        .unwrap_err();
    assert_eq!(unknown.code, "search.location_not_found");
    assert!(unknown.details.as_ref().unwrap()["suggestion"].is_string());
}

#[test]
fn omits_upstream_placeholder_zero_distance() {
    let api = FixtureApi::new(json!({
        "docs": [{"id": "1", "heading": "Nearby chair", "distance": 0.0}],
        "metadata": {
            "result_size": {"match_count": 1},
            "paging": {"current": 1, "last": 1}
        }
    }));
    let request = UpstreamSearchRequest {
        page: 1,
        limit: 1,
        ..Default::default()
    };
    let (result, _) = PublicSearch::new(&api).execute(&request, None).unwrap();

    assert_eq!(result.results[0].distance, None);
}

#[test]
fn bounds_and_prioritizes_large_category_and_location_facets() {
    let taxonomy_options = |name: &str| {
        (0..SEARCH_FACET_OPTION_LIMIT + 3)
            .map(|index| {
                json!({
                    "display_name": format!("{name} {index}"),
                    "name": name,
                    "value": index.to_string(),
                    "hits": if index == SEARCH_FACET_OPTION_LIMIT + 1 { 7 } else { 0 },
                    "selected": index == SEARCH_FACET_OPTION_LIMIT + 2,
                    "filter_items": []
                })
            })
            .collect::<Vec<_>>()
    };
    let api = FixtureApi::new(json!({
        "docs": [],
        "filters": [
            {
                "display_name": "Kategoria",
                "name": "category",
                "type": "STANDARD_FILTER",
                "filter_items": taxonomy_options("category")
            },
            {
                "display_name": "Sijainti",
                "name": "location",
                "type": "STANDARD_FILTER",
                "filter_items": taxonomy_options("location")
            }
        ],
        "metadata": {"result_size":{"match_count":0},"paging":{"current":1,"last":1}}
    }));
    let cli = Cli::parse_from(["flea", "search", "GPU", "--include-facets"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let output = search::dispatch_with_api(*args, &api).unwrap();

    for facet in output["facets"].as_array().unwrap() {
        assert_eq!(
            facet["options"].as_array().unwrap().len(),
            SEARCH_FACET_OPTION_LIMIT
        );
        assert_eq!(facet["option_count"], SEARCH_FACET_OPTION_LIMIT + 3);
        assert_eq!(facet["returned_option_count"], SEARCH_FACET_OPTION_LIMIT);
        assert_eq!(facet["truncated"], true);
        assert_eq!(facet["options"][0]["selected"], true);
        assert_eq!(facet["options"][1]["hits"], 7);
    }
    assert_eq!(
        output["_next_actions"][0],
        json!({
            "command": "flea search 'GPU' --include-facets --facet-option-limit 103 --page 1 --limit 20",
            "reason": "facet_options_truncated"
        })
    );

    let cli = Cli::parse_from([
        "flea",
        "search",
        "GPU",
        "--include-facets",
        "--facet-option-limit",
        "103",
    ]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let broader = search::dispatch_with_api(*args, &api).unwrap();
    for facet in broader["facets"].as_array().unwrap() {
        assert_eq!(
            facet["returned_option_count"],
            SEARCH_FACET_OPTION_LIMIT + 3
        );
        assert_eq!(facet["truncated"], false);
    }
    assert!(broader.get("_next_actions").is_none());
}

#[test]
fn validates_coordinates_radius_pagination_and_duplicate_json_inputs_locally() {
    let api = FixtureApi::new(empty_fixture());
    for arguments in [
        vec![
            "flea",
            "search",
            "x",
            "--latitude",
            "91",
            "--longitude",
            "24",
            "--radius-km",
            "20",
        ],
        vec![
            "flea",
            "search",
            "x",
            "--latitude",
            "60",
            "--longitude",
            "24",
        ],
        vec![
            "flea",
            "search",
            "x",
            "--latitude",
            "60",
            "--longitude",
            "24",
            "--radius-km",
            "0",
        ],
        vec!["flea", "search", "x", "--page", "51"],
        vec!["flea", "search", "x", "--limit", "301"],
        vec!["flea", "search", "x", "--condition", ""],
        vec![
            "flea",
            "search",
            "x",
            "--price-from",
            "20",
            "--price-to",
            "10",
        ],
    ] {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let Command::Search(args) = cli.command else {
            unreachable!()
        };
        assert!(search::dispatch_with_api(*args, &api).is_err());
    }
    assert!(api.requests.lock().unwrap().is_empty());

    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("search.json");
    std::fs::write(&path, r#"{"page":2}"#).unwrap();
    let cli = Cli::parse_from([
        "flea",
        "search",
        "x",
        "--page",
        "3",
        "--input",
        path.to_str().unwrap(),
    ]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    assert!(
        search::dispatch_with_api(*args, &api)
            .unwrap_err()
            .to_string()
            .contains("both --input and command flags")
    );
}

#[test]
fn upstream_read_failures_are_transient_safe_bounded_and_redacted() {
    struct ErrorApi;
    impl PublicSearchApi for ErrorApi {
        fn search(&self, _request: &UpstreamSearchRequest) -> Result<Value, SearchApiError> {
            Err(SearchApiError::Transport(
                "private query cookie -> secret stack trace".to_owned(),
            ))
        }
        fn location_metadata(&self) -> Result<Value, SearchApiError> {
            unreachable!()
        }
    }
    let cli = Cli::parse_from(["flea", "search", "private query"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let error = search::dispatch_with_api(*args, &ErrorApi).unwrap_err();
    assert_eq!(error.code, "upstream.request_failed");
    assert!(error.upstream_transient);
    assert!(error.safe_to_retry);
    let rendered = format!("{error:?}");
    assert!(!rendered.contains("private query"));
    assert!(!rendered.contains("stack trace"));
    assert!(!rendered.contains("secret"));
}

#[test]
fn unicode_query_limit_counts_characters_instead_of_bytes() {
    let api = FixtureApi::new(empty_fixture());
    let query = "ä".repeat(500);
    let cli = Cli::parse_from(["flea", "search", &query]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };

    search::dispatch_with_api(*args, &api).unwrap();
    assert_eq!(api.requests.lock().unwrap()[0].query, query);
}

#[test]
fn page_cap_action_requests_facets_for_executable_refinement() {
    let api = FixtureApi::new(full_fixture());
    let cli = Cli::parse_from(["flea", "search", "tuoli", "--page", "50"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let output = search::dispatch_with_api(*args, &api).unwrap();

    assert_eq!(
        output["_next_actions"][0]["command"],
        "flea search 'tuoli' --include-facets --page 1 --limit 20"
    );
}

#[test]
fn default_output_is_compact_and_omits_empty_or_protocol_fields() {
    let api = FixtureApi::new(full_fixture());
    let item_api = ExplainItemApi {
        responses: BTreeMap::new(),
        requests: Mutex::default(),
    };
    let cli = Cli::parse_from(["flea", "search", "tuoli"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let output = search::dispatch_with_apis(*args, &api, Some(&item_api)).unwrap();
    let listing = output["results"][0].as_object().unwrap();

    assert!(item_api.requests.lock().unwrap().is_empty());

    assert_eq!(
        listing.keys().cloned().collect::<Vec<_>>(),
        [
            "listing_id",
            "title",
            "price",
            "location",
            "category_id",
            "category_path",
            "url",
            "published_at",
            "image_count",
            "distance",
            "condition",
            "shipping",
            "seller",
        ]
    );
    assert_eq!(output["pagination"]["page"], 1);
    assert_eq!(output["pagination"]["returned"], 2);
    assert_eq!(output["pagination"]["total"], 1_200);
    assert_eq!(output["pagination"]["has_next"], true);
    assert_eq!(output["pagination"]["next_page"], 2);
    assert!(output.get("applied_filters").is_none());
    assert!(output.get("facets").is_none());
}

#[test]
fn explains_a_generic_title_from_bounded_public_description_evidence() {
    let search_api = FixtureApi::new(json!({
        "docs": [
            {"id": "45917182", "heading": "Potkulauta"},
            {"id": "2", "heading": "Micro Mini potkulauta"}
        ],
        "metadata": {"result_size":{"match_count":2},"paging":{"current":1,"last":1}}
    }));
    let description = format!(
        "{}\nMicro Mini lasten potkulauta.\u{0} {}",
        "Siisti ja hyväkuntoinen. ".repeat(5),
        "Kaukaisen kuvauksen loppu. ".repeat(5)
    );
    let item_api = ExplainItemApi {
        responses: BTreeMap::from([(
            "45917182".to_owned(),
            Ok(json!({
                "ad": {
                    "title": "Potkulauta",
                    "description": description
                },
                "meta": {"adId": 45917182}
            })),
        )]),
        requests: Mutex::default(),
    };
    let cli = Cli::parse_from(["flea", "search", "micro mini potkulauta", "--explain", "1"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let output = search::dispatch_with_apis(*args, &search_api, Some(&item_api)).unwrap();

    assert_eq!(item_api.requests.lock().unwrap().as_slice(), ["45917182"]);
    let explanation = &output["results"][0]["match_explanation"];
    assert_eq!(explanation["source_field"], "description");
    assert_eq!(explanation["evidence_origin"], "public_item");
    assert_eq!(explanation["match_method"], "cli_derived_token_match");
    assert_eq!(explanation["matched_terms"], json!(["micro", "mini"]));
    let excerpt = explanation["excerpt"].as_str().unwrap();
    assert!(excerpt.contains("Micro Mini lasten potkulauta"));
    assert!(excerpt.chars().count() <= 160);
    assert!(!excerpt.contains(['\n', '\0']));
    assert_ne!(excerpt, description);
    assert!(output["results"][1].get("match_explanation").is_none());
    assert_eq!(output["explain"]["request_limit"], 1);
    assert_eq!(output["explain"]["requested"], 1);
    assert_eq!(output["explain"]["hydrated"], 1);
    assert_eq!(output["explain"]["explained"], 1);
    assert_eq!(output["explain"]["truncated"], false);
}

#[test]
fn explain_enforces_its_request_bound_and_reports_partial_failures() {
    let search_api = FixtureApi::new(json!({
        "docs": [
            {"id": "1", "heading": "Potkulauta"},
            {"id": "2", "heading": "Potkulauta"},
            {"id": "3", "heading": "Potkulauta"}
        ],
        "metadata": {"result_size":{"match_count":3},"paging":{"current":1,"last":1}}
    }));
    let item_api = ExplainItemApi {
        responses: BTreeMap::from([
            (
                "1".to_owned(),
                Ok(json!({
                    "ad": {"title":"Potkulauta", "description":"Micro Mini potkulauta"},
                    "meta": {"adId": 1}
                })),
            ),
            ("2".to_owned(), Err(PublicItemApiError::Upstream(503))),
        ]),
        requests: Mutex::default(),
    };
    let cli = Cli::parse_from(["flea", "search", "micro mini potkulauta", "--explain", "2"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let output = search::dispatch_with_apis(*args, &search_api, Some(&item_api)).unwrap();

    assert_eq!(item_api.requests.lock().unwrap().as_slice(), ["1", "2"]);
    assert_eq!(output["results"].as_array().unwrap().len(), 3);
    assert_eq!(output["explain"]["requested"], 2);
    assert_eq!(output["explain"]["hydrated"], 1);
    assert_eq!(output["explain"]["explained"], 1);
    assert_eq!(output["explain"]["truncated"], true);
    assert_eq!(output["explain"]["failures"][0]["listing_id"], "2");
    assert_eq!(
        output["explain"]["failures"][0]["code"],
        "upstream.request_failed"
    );
    assert_eq!(output["explain"]["failures"][0]["upstream_transient"], true);
    assert_eq!(output["explain"]["failures"][0]["safe_to_retry"], true);
}

#[test]
fn explain_bounds_and_mode_combinations_fail_before_search_requests() {
    let api = FixtureApi::new(empty_fixture());
    for arguments in [
        vec!["flea", "search", "query", "--explain", "0"],
        vec!["flea", "search", "query", "--explain", "21"],
        vec!["flea", "search", "--explain", "1"],
        vec!["flea", "search", "query", "--explain", "1", "--raw"],
    ] {
        let cli = Cli::parse_from(arguments);
        let Command::Search(args) = cli.command else {
            unreachable!()
        };
        assert!(search::dispatch_with_api(*args, &api).is_err());
    }
    assert!(api.requests.lock().unwrap().is_empty());
}

#[test]
fn raw_mode_preserves_the_upstream_document() {
    let raw = full_fixture();
    let api = FixtureApi::new(raw.clone());
    let cli = Cli::parse_from(["flea", "search", "tuoli", "--raw"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    assert_eq!(search::dispatch_with_api(*args, &api).unwrap(), raw);
}

fn empty_fixture() -> Value {
    json!({
        "docs": [],
        "metadata": {
            "params": {},
            "result_size": {"match_count": 0},
            "paging": {"param":"page","current":1,"last":1},
            "is_end_of_paging": true
        }
    })
}

fn location_fixture() -> Value {
    json!({
        "docs": [],
        "filters": [{
            "display_name": "Sijainti",
            "name": "location",
            "type": "STANDARD_FILTER",
            "filter_items": [{
                "display_name": "Uusimaa", "name": "location", "value": "0.100018",
                "filter_items": [
                    {
                        "display_name": "Helsinki", "name": "location", "value": "1.100018.110091",
                        "filter_items": []
                    },
                    {
                        "display_name": "Espoo", "name": "location", "value": "1.100018.110049",
                        "filter_items": []
                    },
                    {
                        "display_name": "Vantaa", "name": "location", "value": "1.100018.110092",
                        "filter_items": []
                    },
                    {
                        "display_name": "Kauniainen", "name": "location", "value": "1.100018.110235",
                        "filter_items": []
                    }
                ]
            }]
        }],
        "metadata": {"result_size":{"match_count":0},"paging":{"current":1,"last":1}}
    })
}

fn full_fixture() -> Value {
    json!({
        "docs": [
            {
                "type":"bap", "id":"42346404", "ad_id":42346404, "heading":"Baden tuoli",
                "location":"Helsinki, Siltamäki, Uusimaa",
                "image":{"url":"https://img.tori.net/item/one","path":"item/one","height":4624,"width":3468},
                "image_urls":["https://img.tori.net/item/one","https://img.tori.net/item/two"],
                "flags":["private","unknown-future-flag"], "timestamp":1787394216000_i64,
                "coordinates":{"lat":60.1699,"lon":24.9384,"accuracy":5},
                "labels":[{"id":"private","text":"Yksityinen","type":"SECONDARY"}],
                "canonical_url":"https://www.tori.fi/recommerce/forsale/item/42346404",
                "extras":[], "price":{"amount":37,"currency_code":"EUR","price_unit":"€"},
                "distance":1200.0, "trade_type":"Myydään", "condition":"Hyvä",
                "shipping_available":true, "seller_type":"private",
                "category":{"id":3215,"value":"Huonekalut","parent":{"id":93,"value":"Koti ja asuminen"}}
            },
            {
                "type":"bap", "id":"42346405", "heading":"Ilmainen tuoli",
                "price":{"amount":0,"value":"Annetaan"}, "extras":[],
                "categoryId":"2.93.3215.46",
                "categoryPath":["Koti ja asuminen","Huonekalut","Tuolit"]
            }
        ],
        "filters": [
            {"display_name":"Osasto","name":"category","type":"STANDARD_FILTER","filter_items":[
                {"display_name":"Koti ja asuminen","name":"category","value":"0.93","hits":30,"selected":false,"filter_items":[
                    {"display_name":"Huonekalut","name":"sub_category","value":"1.93.3215","hits":20,"selected":false,"filter_items":[]}
                ]}
            ]},
            {"display_name":"Hinta","name":"price","type":"RANGE_FILTER","filter_items":[],"min_value":0,"max_value":3000,"step":10,"unit":"€","name_from":"price_from","name_to":"price_to"},
            {"display_name":"Kartta","name":"radius","type":"MAP_RADIUS_FILTER","filter_items":[]}
        ],
        "metadata": {
            "params":{"q":["tuoli"]}, "num_results":2,
            "result_size":{"match_count":1200,"group_count":1200},
            "paging":{"param":"page","current":1,"last":50}, "sort":"RELEVANCE",
            "is_end_of_paging":false, "quest_time":12, "uuid":"uuid-fixture",
            "tracking":{"experiment":"secret-interest"}, "guided_search":{"anything":true}
        }
    })
}
