use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use reqwest::{StatusCode, header::HeaderMap};
use serde_json::{Value, json};
use tori::{
    Presentation,
    api::{
        adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
        auth::{
            AuthCredentials, AuthenticatedAccount, AuthenticationApi, OAuthFlow, SchibstedTokens,
            SecretString, ToriSession,
        },
        client::{HttpError, HttpResponse, RequestSpec, ToriClient},
        listings::HttpListingsApi,
        search::HttpPublicSearchApi,
    },
    cli::{
        Command, CommandRuntime,
        auth::{AuthCommandHandler, AuthStore},
        category, draft, listing, location, search,
    },
    error::AppError,
    run_with_runtime,
};

#[derive(Clone, Default)]
struct MockClient {
    responses: Arc<Mutex<VecDeque<HttpResponse>>>,
    requests: Arc<Mutex<Vec<RequestSpec>>>,
}

impl MockClient {
    fn with_responses(responses: impl IntoIterator<Item = HttpResponse>) -> Self {
        Self {
            responses: Arc::new(Mutex::new(responses.into_iter().collect())),
            requests: Arc::default(),
        }
    }
}

impl ToriClient for MockClient {
    fn execute(
        &self,
        request: RequestSpec,
    ) -> Pin<Box<dyn Future<Output = Result<HttpResponse, HttpError>> + Send + '_>> {
        self.requests.lock().unwrap().push(request);
        let response = self
            .responses
            .lock()
            .unwrap()
            .pop_front()
            .expect("mock response");
        Box::pin(async move { Ok(response) })
    }
}

struct TestRuntime {
    client: MockClient,
}

impl CommandRuntime for TestRuntime {
    fn execute(&self, command: Command) -> Result<Value, AppError> {
        match command {
            Command::Auth(args) => match args.command {
                tori::cli::auth::AuthCommand::Login => {
                    Ok(json!({ "authenticated": true, "user_id": "user-1" }))
                }
                command => block_on(
                    AuthCommandHandler::new(FakeAuthApi, MemoryAuthStore::default())
                        .dispatch(command),
                ),
            },
            Command::Category(args) => {
                let api = HttpListingsApi::new(Arc::new(self.client.clone()));
                category::dispatch_with_api(args, &api)
            }
            Command::Draft(args) => block_on(draft::execute(
                args.command,
                HttpAdInputApi::new(ClientTransport::new(self.client.clone())),
                WorkflowConfig::default(),
            )),
            Command::Listing(args) => {
                let api = HttpListingsApi::new(Arc::new(self.client.clone()));
                listing::dispatch_with_api(args, &api)
            }
            Command::Search(args) => {
                let api = HttpPublicSearchApi::new(Arc::new(self.client.clone()));
                search::dispatch_with_api(*args, &api)
            }
            Command::Location(args) => {
                let api = HttpPublicSearchApi::new(Arc::new(self.client.clone()));
                location::dispatch_with_api(args, &api)
            }
            Command::Skill(args) => tori::cli::skill::dispatch(args),
        }
    }
}

#[derive(Default)]
struct MemoryAuthStore {
    flow: Mutex<Option<OAuthFlow>>,
    credentials: Mutex<Option<AuthCredentials>>,
}

impl AuthStore for MemoryAuthStore {
    fn save_flow(&self, flow: &OAuthFlow) -> Result<(), AppError> {
        *self.flow.lock().unwrap() = Some(flow.clone());
        Ok(())
    }

    fn load_flow(&self, flow_id: &str) -> Result<Option<OAuthFlow>, AppError> {
        Ok(self
            .flow
            .lock()
            .unwrap()
            .clone()
            .filter(|flow| flow.flow_id == flow_id))
    }

    fn delete_flow(&self, _flow_id: &str) -> Result<(), AppError> {
        *self.flow.lock().unwrap() = None;
        Ok(())
    }

    fn load_credentials(&self) -> Result<Option<AuthCredentials>, AppError> {
        Ok(self.credentials.lock().unwrap().clone())
    }

    fn commit_credentials(
        &self,
        flow_id: &str,
        credentials: &AuthCredentials,
    ) -> Result<(), AppError> {
        *self.credentials.lock().unwrap() = Some(credentials.clone());
        self.delete_flow(flow_id)
    }

    fn clear_auth(&self) -> Result<(), AppError> {
        *self.flow.lock().unwrap() = None;
        *self.credentials.lock().unwrap() = None;
        Ok(())
    }
}

struct FakeAuthApi;

impl AuthenticationApi for FakeAuthApi {
    async fn exchange_authorization_code(
        &self,
        _code: &str,
        _pkce_verifier: &str,
    ) -> Result<SchibstedTokens, AppError> {
        Ok(SchibstedTokens::new_for_adapter(
            "access".to_owned(),
            "refresh".to_owned(),
            "id".to_owned(),
        ))
    }

    async fn exchange_spid_code(&self, _access_token: &str) -> Result<SecretString, AppError> {
        Ok(SecretString::new_for_adapter("spid".to_owned()))
    }

    async fn login_to_tori(
        &self,
        _spid_code: &str,
        _id_token: &str,
        _device_id: &str,
        _installation_id: &str,
        _ab_test_device_id: &str,
    ) -> Result<ToriSession, AppError> {
        let account = AuthenticatedAccount {
            user_id: "user-1".to_owned(),
        };
        Ok(ToriSession::new_for_adapter(
            account.user_id,
            "bearer".to_owned(),
        ))
    }
}

#[test]
fn auth_login_flows_from_parser_to_one_envelope() {
    let value = invoke(
        &TestRuntime {
            client: MockClient::default(),
        },
        ["tori", "--format", "json", "auth", "login"],
    );

    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["authenticated"], true);
    assert!(value.get("warnings").is_none());
    assert!(value.get("next_actions").is_none());
}

#[test]
fn draft_create_flows_through_the_http_adapter() {
    let client = MockClient::with_responses([response(StatusCode::CREATED, draft_state("one"))]);
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        ["tori", "--format", "json", "draft", "create"],
    );

    assert_eq!(value["data"]["draft"]["draft_id"], "draft-1");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path_and_query, "/drafts");
}

#[test]
fn partial_draft_failure_preserves_recovery_envelope_and_exit_code() {
    let client = MockClient::with_responses([
        response(StatusCode::CREATED, draft_state("one")),
        response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "message": "category service unavailable" }),
        ),
    ]);
    let result = run_with_runtime(
        [
            "tori",
            "--format",
            "json",
            "draft",
            "create",
            "--category",
            "chairs",
        ],
        &TestRuntime { client },
    );
    let value: Value = serde_json::from_str(&result.document).expect("one JSON envelope");

    assert_eq!(result.exit_code, 50);
    assert_eq!(result.presentation, Presentation::Structured);
    assert_eq!(value["ok"], false);
    assert_eq!(value["partial"]["draft_id"], "draft-1");
    assert_eq!(
        value["next_actions"][0]["command"],
        "tori draft show draft-1"
    );
}

#[test]
fn image_add_flows_through_upload_and_ordering() {
    let directory = tempfile::tempdir().unwrap();
    let image_path = directory.path().join("chair.png");
    image::DynamicImage::new_rgb8(4, 6)
        .save(&image_path)
        .unwrap();
    let client = MockClient::with_responses([
        response(StatusCode::OK, draft_state("one")),
        response(
            StatusCode::CREATED,
            json!({ "image_id": "image-1", "state": "processing" }),
        ),
        response(StatusCode::OK, draft_with_images("two", "processing")),
    ]);
    let value = invoke_vec(
        &TestRuntime {
            client: client.clone(),
        },
        vec![
            "tori".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
            "draft".to_owned(),
            "image".to_owned(),
            "add".to_owned(),
            "draft-1".to_owned(),
            image_path.to_string_lossy().into_owned(),
        ],
    );

    assert_eq!(value["data"]["images"][0]["image_id"], "image-1");
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}

#[test]
fn publish_flows_through_every_http_step() {
    let valid = json!({
        "draft_id": "draft-1",
        "etag": "one",
        "values": { "title": "Chair", "delivery": ["pickup"] },
        "required_fields": ["title", "delivery"],
        "images": []
    });
    let submitted = json!({
        "draft_id": "draft-1",
        "etag": "three",
        "values": { "title": "Chair", "delivery": ["pickup"], "revision": "revision-1" }
    });
    let client = MockClient::with_responses([
        response(StatusCode::OK, valid.clone()),
        response(StatusCode::OK, valid.clone()),
        response(StatusCode::OK, valid),
        response(StatusCode::OK, submitted),
        response(StatusCode::NO_CONTENT, Value::Null),
        response(
            StatusCode::OK,
            json!({ "revision": "revision-1", "context": {} }),
        ),
        response(
            StatusCode::CREATED,
            json!({ "listing_id": "listing-1", "revision": "revision-1", "state": "pending" }),
        ),
        response(StatusCode::OK, json!({ "order_id": "order-1" })),
        response(StatusCode::NO_CONTENT, Value::Null),
        response(StatusCode::OK, json!({ "state": "pending" })),
    ]);
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        ["tori", "--format", "json", "draft", "publish", "draft-1"],
    );

    assert_eq!(value["data"]["listing_id"], "listing-1");
    assert_eq!(client.requests.lock().unwrap().len(), 10);
}

#[test]
fn public_search_flows_without_authentication_through_one_envelope() {
    let client = MockClient::with_responses([response(
        StatusCode::OK,
        json!({
            "docs": [{
                "id": "42346404",
                "heading": "Baden tuoli",
                "location": "Helsinki, Uusimaa",
                "canonical_url": "https://www.tori.fi/recommerce/forsale/item/42346404",
                "price": {"amount": 37, "currency_code": "EUR", "price_unit": "€"}
            }],
            "metadata": {
                "params": {"q": ["tuoli"]},
                "result_size": {"match_count": 100},
                "paging": {"current": 1, "last": 5},
                "is_end_of_paging": false
            }
        }),
    )]);
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        ["tori", "--format", "json", "search", "tuoli"],
    );

    assert_eq!(value["data"]["results"][0]["listing_id"], "42346404");
    assert_eq!(value["data"]["pagination"]["limit"], 20);
    assert_eq!(
        value["next_actions"][0]["command"],
        "tori search 'tuoli' --page 2 --limit 20"
    );
    assert!(value["data"].get("_next_actions").is_none());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].service, "SEARCH-QUEST");
    assert!(requests[0].path_and_query.contains("client=android"));
    assert!(!requests[0].path_and_query.contains("include_filters"));
}

#[test]
fn category_and_listing_commands_flow_through_http_normalization() {
    let categories = MockClient::with_responses([response(
        StatusCode::OK,
        json!([{ "id": "100", "label": "Furniture", "selectable": true }]),
    )]);
    let category = invoke(
        &TestRuntime {
            client: categories.clone(),
        },
        ["tori", "--format", "json", "category", "list"],
    );
    assert_eq!(category["data"]["categories"][0]["category_id"], "100");

    let listings = MockClient::with_responses([response(
        StatusCode::OK,
        json!({
            "id": "listing-1",
            "etag": "v1",
            "state": { "type": "active" },
            "fields": { "title": "Chair" }
        }),
    )]);
    let listing = invoke(
        &TestRuntime {
            client: listings.clone(),
        },
        ["tori", "--format", "json", "listing", "show", "listing-1"],
    );
    assert_eq!(listing["data"]["listing_id"], "listing-1");
    assert_eq!(listing["data"]["fields"]["title"], "Chair");
}

fn invoke<const N: usize>(runtime: &TestRuntime, args: [&str; N]) -> Value {
    let result = run_with_runtime(args, runtime);
    assert_eq!(result.exit_code, 0, "{}", result.document);
    assert_eq!(result.presentation, Presentation::Structured);
    serde_json::from_str(&result.document).expect("one JSON envelope")
}

fn invoke_vec(runtime: &TestRuntime, args: Vec<String>) -> Value {
    let result = run_with_runtime(args, runtime);
    assert_eq!(result.exit_code, 0, "{}", result.document);
    assert_eq!(result.presentation, Presentation::Structured);
    serde_json::from_str(&result.document).expect("one JSON envelope")
}

fn response(status: StatusCode, body: Value) -> HttpResponse {
    HttpResponse {
        status,
        headers: HeaderMap::new(),
        body: serde_json::to_vec(&body).unwrap(),
    }
}

fn draft_state(etag: &str) -> Value {
    json!({
        "draft_id": "draft-1",
        "etag": etag,
        "values": {},
        "images": []
    })
}

fn draft_with_images(etag: &str, state: &str) -> Value {
    json!({
        "draft_id": "draft-1",
        "etag": etag,
        "values": {},
        "images": [{ "image_id": "image-1", "position": 0, "state": state }]
    })
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
