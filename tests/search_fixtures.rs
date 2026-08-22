use std::sync::Mutex;

use clap::Parser;
use serde_json::{Value, json};
use tori::{
    api::search::{
        PublicSearch, PublicSearchApi, SEARCH_FACET_OPTION_LIMIT, SearchApiError,
        UpstreamSearchRequest,
    },
    cli::{Cli, Command, search},
};

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

#[test]
fn exact_search_target_is_stable_encoded_and_uses_taxonomy_depth() {
    let api = FixtureApi::new(empty_fixture());
    let cli = Cli::parse_from([
        "tori",
        "search",
        "kävelymatto & tuoli",
        "--category",
        "2.93.3215.46",
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
        "/search/SEARCH_ID_BAP_COMMON?client=android&location=1.100018.110091&page=2&price_from=10&price_to=100&product_category=2.93.3215.46&q=k%C3%A4velymatto+%26+tuoli&rows=20&shipping_exists=true"
    );
}

#[test]
fn encodes_helsinki_twenty_kilometer_radius_as_tori_meter_parameters() {
    let api = FixtureApi::new(empty_fixture());
    let cli = Cli::parse_from([
        "tori",
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
        parameters: Default::default(),
    };
    let (result, _) = PublicSearch::new(&api).execute(&request, None).unwrap();

    assert_eq!(result.results[0].listing_id, "42346404");
    assert_eq!(
        result.results[0].price.as_ref().unwrap().display.as_deref(),
        Some("37 €")
    );
    assert_eq!(result.results[0].image_urls.len(), 2);
    assert_eq!(
        result.results[1].price.as_ref().unwrap().display.as_deref(),
        Some("Annetaan")
    );
    assert!(result.results[1].image_urls.is_empty());
    assert_eq!(result.pagination.total, 1_200);
    assert_eq!(result.pagination.total_pages, 60);
    assert_eq!(result.pagination.accessible_pages, 50);
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
    for private in [
        "coordinates",
        "tracking",
        "guided_search",
        "quest_time",
        "uuid-fixture",
    ] {
        assert!(
            !serialized.contains(private),
            "normalized output leaked {private}"
        );
    }
}

#[test]
fn resolves_location_names_case_insensitively_and_deterministically() {
    let api = FixtureApi::new(empty_fixture());
    let search = PublicSearch::new(&api);
    let location = search.resolve_location("  HELSINKI ").unwrap();
    assert_eq!(location.id, "1.100018.110091");
    assert_eq!(location.parent.as_deref(), Some("Uusimaa"));

    let matches = search.locations("sink").unwrap();
    assert_eq!(matches.returned, 1);
    assert_eq!(matches.locations[0].name, "Helsinki");
}

#[test]
fn bounds_large_dynamic_facet_output_transparently() {
    let options: Vec<Value> = (0..SEARCH_FACET_OPTION_LIMIT + 3)
        .map(|index| {
            json!({
                "display_name": format!("Brand {index}"),
                "name": "brand",
                "value": index.to_string(),
                "hits": 1,
                "selected": false,
                "filter_items": []
            })
        })
        .collect();
    let api = FixtureApi::new(json!({
        "docs": [],
        "filters": [{"display_name":"Merkki","name":"brand","type":"STANDARD_FILTER","filter_items":options}],
        "metadata": {"result_size":{"match_count":0},"paging":{"current":1,"last":1}}
    }));
    let (result, _) = PublicSearch::new(&api)
        .execute(
            &UpstreamSearchRequest {
                page: 1,
                limit: 20,
                include_filters: true,
                ..Default::default()
            },
            None,
        )
        .unwrap();
    assert_eq!(result.facets[0].options.len(), SEARCH_FACET_OPTION_LIMIT);
    assert_eq!(result.facets[0].option_count, SEARCH_FACET_OPTION_LIMIT + 3);
    assert!(result.facets[0].truncated);
}

#[test]
fn validates_coordinates_radius_pagination_and_duplicate_json_inputs_locally() {
    let api = FixtureApi::new(empty_fixture());
    for arguments in [
        vec![
            "tori",
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
            "tori",
            "search",
            "x",
            "--latitude",
            "60",
            "--longitude",
            "24",
        ],
        vec![
            "tori",
            "search",
            "x",
            "--latitude",
            "60",
            "--longitude",
            "24",
            "--radius-km",
            "0",
        ],
        vec!["tori", "search", "x", "--page", "51"],
        vec!["tori", "search", "x", "--limit", "301"],
        vec![
            "tori",
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
        "tori",
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
fn upstream_failures_are_retryable_bounded_and_redacted() {
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
    let cli = Cli::parse_from(["tori", "search", "private query"]);
    let Command::Search(args) = cli.command else {
        unreachable!()
    };
    let error = search::dispatch_with_api(*args, &ErrorApi).unwrap_err();
    assert_eq!(error.code, "upstream.request_failed");
    assert!(error.retryable);
    let rendered = format!("{error:?}");
    assert!(!rendered.contains("private query"));
    assert!(!rendered.contains("stack trace"));
    assert!(!rendered.contains("secret"));
}

#[test]
fn raw_mode_preserves_the_upstream_document() {
    let raw = full_fixture();
    let api = FixtureApi::new(raw.clone());
    let cli = Cli::parse_from(["tori", "search", "tuoli", "--raw"]);
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
                "filter_items": [{
                    "display_name": "Helsinki", "name": "location", "value": "1.100018.110091",
                    "filter_items": []
                }]
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
                "distance":1200.0, "trade_type":"Myydään"
            },
            {
                "type":"bap", "id":"42346405", "heading":"Ilmainen tuoli",
                "price":{"amount":0,"value":"Annetaan"}, "extras":[]
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
