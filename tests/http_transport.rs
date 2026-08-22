use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};

use reqwest::{
    Method, StatusCode,
    header::{AUTHORIZATION, CONTENT_TYPE, ETAG, HeaderMap, HeaderValue, IF_MATCH},
};
use tori::api::client::{
    ClientConfig, DeviceIdentity, HttpClient, MultipartPart, RequestBody, RequestSpec, Transport,
    TransportError, TransportErrorKind, TransportFuture, TransportRequest, TransportResponse,
    compatibility,
};

#[derive(Clone)]
struct FixtureTransport {
    requests: Arc<Mutex<Vec<TransportRequest>>>,
    responses: Arc<Mutex<VecDeque<Result<TransportResponse, TransportError>>>>,
}

impl FixtureTransport {
    fn new(responses: impl IntoIterator<Item = Result<TransportResponse, TransportError>>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
        }
    }

    fn requests(&self) -> Vec<TransportRequest> {
        self.requests.lock().unwrap().clone()
    }
}

impl Transport for FixtureTransport {
    fn execute(&self, request: TransportRequest) -> TransportFuture<'_> {
        self.requests.lock().unwrap().push(request);
        let response = self.responses.lock().unwrap().pop_front().unwrap();
        Box::pin(async move { response })
    }
}

fn fixture_response(status: StatusCode) -> Result<TransportResponse, TransportError> {
    Ok(TransportResponse {
        status,
        headers: HeaderMap::new(),
        body: b"fixture".to_vec(),
    })
}

fn client(transport: FixtureTransport) -> HttpClient<FixtureTransport> {
    let config = ClientConfig {
        request_timeout: Duration::from_secs(7),
        max_response_bytes: 32,
        max_get_retries: 2,
        retry_base_delay: Duration::ZERO,
        ..ClientConfig::default()
    };
    HttpClient::with_transport(
        config,
        DeviceIdentity {
            installation_id: "installation-fixture".to_owned(),
            ab_test_device_id: "ab-fixture".to_owned(),
        },
        Some("bearer-fixture-secret".to_owned()),
        transport,
    )
}

#[tokio::test]
async fn fixture_observes_exact_signed_target_and_compatibility_headers() {
    let transport = FixtureTransport::new([fixture_response(StatusCode::OK)]);
    let client = client(transport.clone());

    client
        .send(RequestSpec::new(
            Method::GET,
            "/search?foo=one%20two&x=1",
            "SEARCH-QUEST",
        ))
        .await
        .unwrap();

    let requests = transport.requests();
    let request = &requests[0];
    assert_eq!(request.path_and_query, "/search?foo=one%20two&x=1");
    assert_eq!(request.deadline, Duration::from_secs(7));
    assert_eq!(request.max_response_bytes, 32);
    assert_eq!(
        request.headers["finn-gw-key"],
        "rARwXhpkwuDVfRL8MgsujE4ytirzxT/D8+CXWtp/nxKg8qaTA+9VQAMgnZI/5iIzZk+Yln1ls7I7/Dfw6XeolA=="
    );
    assert_eq!(request.headers["finn-gw-service"], "SEARCH-QUEST");
    assert_eq!(
        request.headers["finn-device-info"],
        compatibility::DEVICE_INFO
    );
    assert_eq!(
        request.headers["x-nmp-app-version-name"],
        compatibility::APP_VERSION
    );
    assert_eq!(
        request.headers["finn-app-installation-id"],
        "installation-fixture"
    );
    assert_eq!(request.headers["ab-test-device-id"], "ab-fixture");
    assert_eq!(
        request.headers[AUTHORIZATION],
        "Bearer bearer-fixture-secret"
    );
}

#[tokio::test]
async fn public_search_uses_android_service_without_identity_or_bearer_headers() {
    let transport = FixtureTransport::new([fixture_response(StatusCode::OK)]);
    let config = ClientConfig {
        include_device_identity: false,
        retry_base_delay: Duration::ZERO,
        ..ClientConfig::default()
    };
    let client = HttpClient::with_transport(
        config,
        DeviceIdentity {
            installation_id: String::new(),
            ab_test_device_id: String::new(),
        },
        None,
        transport.clone(),
    );
    let target = "/search/SEARCH_ID_BAP_COMMON?client=android&location=1.100018.110091&page=2&price_from=10&price_to=100&product_category=2.93.3215.46&q=k%C3%A4velymatto+%26+tuoli&rows=20&shipping_exists=true";

    client
        .send(RequestSpec::new(
            Method::GET,
            target,
            compatibility::SERVICE_SEARCH,
        ))
        .await
        .unwrap();

    let request = &transport.requests()[0];
    assert_eq!(request.path_and_query, target);
    assert_eq!(request.headers["finn-gw-service"], "SEARCH-QUEST");
    assert_eq!(
        request.headers["finn-gw-key"],
        "BenSRb92e+OeS4F5KliUWD76rJhmcpnZY/UOSXBhc9wl5b+aDv7eGbhCXCFaDrK01BR3vvt6XyIbMx0rmrdAuA=="
    );
    assert!(!request.headers.contains_key(AUTHORIZATION));
    assert!(!request.headers.contains_key("finn-app-installation-id"));
    assert!(!request.headers.contains_key("ab-test-device-id"));
}

#[tokio::test]
async fn carries_if_match_and_extracts_etag() {
    let mut headers = HeaderMap::new();
    headers.insert(ETAG, HeaderValue::from_static("\"revision-2\""));
    let transport = FixtureTransport::new([Ok(TransportResponse {
        status: StatusCode::OK,
        headers,
        body: Vec::new(),
    })]);
    let client = client(transport.clone());

    let response = client
        .send(
            RequestSpec::new(
                Method::PUT,
                "/items/42",
                compatibility::SERVICE_ITEM_CREATION,
            )
            .body(
                br#"{"title":"Tuoli"}"#,
                HeaderValue::from_static("application/json"),
            )
            .if_match(HeaderValue::from_static("\"revision-1\"")),
        )
        .await
        .unwrap();

    assert_eq!(response.etag().unwrap(), "\"revision-2\"");
    let request = &transport.requests()[0];
    assert_eq!(request.headers[IF_MATCH], "\"revision-1\"");
    assert_eq!(request.headers[CONTENT_TYPE], "application/json");
    assert_eq!(
        request.headers["finn-gw-key"],
        "Z/0xcvBWEKSpCbv+YmW1leV/8S7LcV8562o9OGTWzNZjhQCYkCwbKCAxrtJKXpmpYahBhI16CBhJ18eCpWRmFA=="
    );
}

#[tokio::test]
async fn retries_only_bounded_safe_methods() {
    let get_transport = FixtureTransport::new([
        fixture_response(StatusCode::SERVICE_UNAVAILABLE),
        Err(TransportError {
            kind: TransportErrorKind::Connection,
        }),
        fixture_response(StatusCode::OK),
    ]);
    let get_client = client(get_transport.clone());
    let response = get_client
        .send(RequestSpec::new(Method::GET, "/items/42", "ITEMS"))
        .await
        .unwrap();
    assert_eq!(response.status, StatusCode::OK);
    assert_eq!(get_transport.requests().len(), 3);

    let post_transport = FixtureTransport::new([
        fixture_response(StatusCode::SERVICE_UNAVAILABLE),
        fixture_response(StatusCode::OK),
    ]);
    let post_client = client(post_transport.clone());
    let response = post_client
        .send(RequestSpec::new(Method::POST, "/items", "ITEMS"))
        .await
        .unwrap();
    assert_eq!(response.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(post_transport.requests().len(), 1);
}

#[tokio::test]
async fn empty_post_sends_an_explicit_zero_content_length_once() {
    let transport = FixtureTransport::new([fixture_response(StatusCode::OK)]);
    let client = client(transport.clone());

    client
        .send(RequestSpec::new(Method::POST, "/create", "ITEMS").empty_body())
        .await
        .unwrap();

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].headers["content-length"], "0");
}

#[tokio::test]
async fn multipart_request_is_inspectable_and_signs_an_empty_body() {
    let transport = FixtureTransport::new([fixture_response(StatusCode::CREATED)]);
    let client = client(transport.clone());
    let part = MultipartPart::bytes("file", b"image-fixture".to_vec())
        .file_name("chair.jpg")
        .mime_type("image/jpeg");

    client
        .send(
            RequestSpec::new(Method::POST, "/adinput/ad/recommerce/42/upload", "")
                .adinput()
                .multipart(vec![part]),
        )
        .await
        .unwrap();

    let request = &transport.requests()[0];
    let RequestBody::Multipart(parts) = &request.body else {
        panic!("expected multipart body");
    };
    assert_eq!(parts[0].name, "file");
    assert_eq!(parts[0].file_name.as_deref(), Some("chair.jpg"));
    assert_eq!(parts[0].mime_type.as_deref(), Some("image/jpeg"));
    assert_eq!(parts[0].len(), 13);
    assert!(request.headers.get("finn-gw-service").is_none());
    assert_eq!(
        request.headers["finn-gw-key"],
        "QyQXHYg+awmHpe06A/R2UNCDbuVYzw4Govp68c/nuoqUK6E4zdK60dTRuXy54rMGLDUvhxIBOc4ucMkc753R/Q=="
    );
}

#[tokio::test]
async fn rejects_oversized_fixture_responses() {
    let transport = FixtureTransport::new([Ok(TransportResponse {
        status: StatusCode::OK,
        headers: HeaderMap::new(),
        body: vec![0; 33],
    })]);
    let error = client(transport)
        .send(RequestSpec::new(Method::POST, "/items", "ITEMS"))
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "HTTP response exceeded the configured size bound"
    );
}

#[tokio::test]
async fn debug_and_errors_do_not_expose_credentials_or_signatures() {
    let transport = FixtureTransport::new([fixture_response(StatusCode::OK)]);
    let client = client(transport.clone());
    let mut spec = RequestSpec::new(Method::GET, "/oauth?code=authorization-material", "AUTH");
    spec.headers.insert(
        AUTHORIZATION,
        HeaderValue::from_static("raw-authorization-secret"),
    );
    let spec_debug = format!("{spec:?}");
    assert!(!spec_debug.contains("authorization-material"));
    assert!(!spec_debug.contains("raw-authorization-secret"));

    let error = client.send(spec).await.unwrap_err();
    assert_eq!(error.to_string(), "invalid HTTP request");

    client
        .send(RequestSpec::new(Method::GET, "/safe", "AUTH"))
        .await
        .unwrap();
    let request_debug = format!("{:?}", transport.requests()[0]);
    assert!(!request_debug.contains("bearer-fixture-secret"));
    assert!(!request_debug.contains("authorization-material"));
    assert!(
        !request_debug.contains(
            transport.requests()[0].headers["finn-gw-key"]
                .to_str()
                .unwrap()
        )
    );
}

#[tokio::test]
async fn rejects_targets_that_url_parsing_would_rewrite() {
    for target in [
        "/drafts/../credentials",
        "/drafts\\@attacker.example/secret",
        "/query with spaces",
        "//attacker.example/secret",
    ] {
        let transport = FixtureTransport::new([fixture_response(StatusCode::OK)]);
        let error = client(transport.clone())
            .send(RequestSpec::new(Method::GET, target, "ITEMS"))
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "invalid HTTP request",
            "target: {target}"
        );
        assert!(transport.requests().is_empty());
    }
}

#[tokio::test]
async fn caps_adversarial_retry_and_timeout_configuration() {
    let responses = (0..=8).map(|_| fixture_response(StatusCode::SERVICE_UNAVAILABLE));
    let transport = FixtureTransport::new(responses);
    let config = ClientConfig {
        request_timeout: Duration::MAX,
        max_get_retries: usize::MAX,
        retry_base_delay: Duration::ZERO,
        ..ClientConfig::default()
    };
    let client = HttpClient::with_transport(
        config,
        DeviceIdentity {
            installation_id: "installation".to_owned(),
            ab_test_device_id: "ab".to_owned(),
        },
        None,
        transport.clone(),
    );

    client
        .send(RequestSpec::new(Method::GET, "/safe", "ITEMS"))
        .await
        .unwrap();

    let requests = transport.requests();
    assert_eq!(requests.len(), 9);
    assert!(
        requests
            .iter()
            .all(|request| request.deadline == Duration::from_secs(120))
    );
}
