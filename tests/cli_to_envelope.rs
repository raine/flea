use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

use flea::{
    Presentation,
    api::{
        auth::{
            AuthCredentials, AuthenticatedAccount, AuthenticationApi, OAuthFlow, SchibstedTokens,
            SecretString, ToriSession,
        },
        client::{HttpError, HttpResponse, RequestSpec, ToriClient},
        favorites::{FavoritesApi, HttpFavoritesApi},
        item::HttpPublicItemApi,
        listings::HttpListingsApi,
        saved_searches::HttpSavedSearchesApi,
        search::HttpPublicSearchApi,
    },
    cli::{
        Command, CommandFuture, CommandRuntime, ToriCommand,
        auth::{AuthCommandHandler, AuthStore},
        category, draft, favorite, item, listing, location, saved_search, search,
    },
    error::AppError,
    marketplace::tori::adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
    run_with_runtime,
};
use reqwest::{StatusCode, header::HeaderMap};
use serde_json::{Value, json};

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
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        Box::pin(async move {
            let result = match command {
                Command::Tori(args) => match args.command {
                    ToriCommand::Auth(args) => match args.command {
                        flea::cli::auth::AuthCommand::Login => {
                            Ok(json!({ "authenticated": true, "user_id": "user-1" }))
                        }
                        command => {
                            AuthCommandHandler::new(FakeAuthApi, MemoryAuthStore::default())
                                .dispatch(command)
                                .await
                        }
                    },
                    ToriCommand::Capabilities => {
                        unreachable!("capabilities use the production runtime")
                    }
                    ToriCommand::Category(args) => {
                        let api = HttpListingsApi::new(Arc::new(self.client.clone()));
                        category::dispatch_with_api(args, &api).await
                    }
                    ToriCommand::Draft(args) => match args.command {
                        command @ flea::cli::draft::DraftCommand::Preview {
                            verify_category: false,
                            ..
                        } => draft::execute_preview(command, None).await,
                        command @ flea::cli::draft::DraftCommand::Preview {
                            verify_category: true,
                            ..
                        } => {
                            let api = HttpListingsApi::new(Arc::new(self.client.clone()));
                            draft::execute_preview(command, Some(&api)).await
                        }
                        command => {
                            draft::execute(
                                command,
                                HttpAdInputApi::new(ClientTransport::new(self.client.clone())),
                                WorkflowConfig::default(),
                            )
                            .await
                        }
                    },
                    ToriCommand::Favorite(args) => {
                        let api = HttpFavoritesApi::new(Arc::new(self.client.clone()));
                        favorite::dispatch_with_api(args, &api).await
                    }
                    ToriCommand::Item(args) => {
                        let api = HttpPublicItemApi::new(Arc::new(self.client.clone()));
                        item::dispatch_with_api(args, &api).await
                    }
                    ToriCommand::Listing(args) => {
                        let api = HttpListingsApi::new(Arc::new(self.client.clone()));
                        listing::dispatch_with_api(args, &api).await
                    }
                    ToriCommand::Search(args) => {
                        let api = HttpPublicSearchApi::new(Arc::new(self.client.clone()));
                        search::dispatch_with_api(*args, &api).await
                    }
                    ToriCommand::SavedSearch(args) => {
                        let api = HttpSavedSearchesApi::new(Arc::new(self.client.clone()));
                        let search_api = HttpPublicSearchApi::new(Arc::new(self.client.clone()));
                        saved_search::dispatch_with_apis(*args, &api, &search_api).await
                    }
                    ToriCommand::Location(args) => {
                        let api = HttpPublicSearchApi::new(Arc::new(self.client.clone()));
                        location::dispatch_with_api(args, &api).await
                    }
                },
                Command::Skill(args) => flea::cli::skill::dispatch(args),
                Command::Capabilities
                | Command::Marketplaces
                | Command::Vinted(_)
                | Command::Unsupported(_) => unreachable!("command uses the production runtime"),
            };
            result.map(flea::cli::outcome::CommandOutcome::from_legacy_value)
        })
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
        _id_token: Option<&str>,
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
        ["flea", "--format", "json", "tori", "auth", "login"],
    );

    assert_eq!(value["ok"], true);
    assert_eq!(value["data"]["authenticated"], true);
    assert!(value.get("warnings").is_none());
    assert!(value.get("next_actions").is_none());
}

#[test]
fn draft_preview_is_offline_and_performs_zero_transport_requests() {
    let client = MockClient::default();
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        [
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "preview",
            "--category",
            "258",
            "--title",
            "Koivutuoli",
            "--description",
            "Hyväkuntoinen tuoli noudettavaksi Helsingistä.",
            "--price",
            "45.50",
            "--trade-type",
            "sell",
            "--postal-code",
            "00100",
            "--delivery",
            "pickup",
        ],
    );

    assert_eq!(value["data"]["remote_mutation"], "none");
    assert_eq!(value["data"]["local_validation"]["status"], "passed");
    assert_eq!(
        value["data"]["remote_verification"]["status"],
        "not_requested"
    );
    assert!(client.requests.lock().unwrap().is_empty());
}

#[test]
fn draft_preview_enrichment_uses_only_the_read_only_taxonomy_request() {
    let client = MockClient::with_responses([response(
        StatusCode::OK,
        json!({
            "categories": [{
                "id": 258,
                "label": "Tuolit",
                "isSelectable": true
            }]
        }),
    )]);
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        [
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "preview",
            "--category",
            "258",
            "--verify-category",
        ],
    );

    assert_eq!(value["data"]["remote_verification"]["status"], "verified");
    assert_eq!(
        value["data"]["remote_verification"]["verified_constraints"],
        json!(["category_exists", "category_selectable"])
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, reqwest::Method::GET);
    assert_eq!(requests[0].path_and_query, "/categories/taxonomy");
}

#[test]
fn invalid_create_and_update_inputs_fail_before_transport_access() {
    let client = MockClient::default();
    let runtime = TestRuntime {
        client: client.clone(),
    };

    for arguments in [
        vec![
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "create",
            "--postal-code",
            "Helsinki",
        ],
        vec![
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "update",
            "draft-1",
            "--delivery",
            "pickup",
            "--delivery",
            "pickup",
        ],
    ] {
        let result = run_with_runtime(arguments, &runtime);
        let value: Value = serde_json::from_str(&result.document).unwrap();
        assert_eq!(result.exit_code, 20, "{}", result.document);
        assert_eq!(value["error"]["code"], "draft.input_invalid");
    }

    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn favorite_mutations_send_an_explicit_empty_body() {
    let client = MockClient::with_responses([response(StatusCode::NO_CONTENT, Value::Null)]);
    let api = HttpFavoritesApi::new(Arc::new(client.clone()));

    api.add(1131149, 25085448).await.unwrap();

    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].path_and_query, "/favorites/1131149/Ad/25085448");
    assert!(requests[0].content_length_zero);
    assert!(matches!(
        &requests[0].body,
        flea::api::client::RequestBody::Bytes(bytes) if bytes.is_empty()
    ));
}

#[test]
fn draft_create_flows_through_the_http_adapter() {
    let client = MockClient::with_responses([response(StatusCode::CREATED, draft_state("one"))]);
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        ["flea", "--format", "json", "tori", "draft", "create"],
    );

    assert_eq!(value["data"]["draft"]["draft_id"], "draft-1");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].path_and_query,
        "/adinput/ad/withModel/recommerce"
    );
    assert_eq!(requests[0].service, "APPS-ADINPUT");
    assert_eq!(requests[0].host, flea::api::client::ApiHost::Adinput);
    assert!(requests[0].content_length_zero);
    assert!(matches!(
        &requests[0].body,
        flea::api::client::RequestBody::Bytes(bytes) if bytes.is_empty()
    ));
}

#[test]
fn draft_show_is_compact_by_default_and_expands_deterministically() {
    let draft = || {
        let options = (0..593)
            .map(|index| {
                json!({
                    "field": "category",
                    "value": index.to_string(),
                    "label": format!("Category {index}")
                })
            })
            .collect::<Vec<_>>();
        json!({
            "draft_id": "draft-1",
            "etag": "etag-7",
            "revision": "revision-7",
            "values": {
                "category": "258",
                "title": "Chair",
                "description": "Solid birch chair",
                "trade_type": "sell",
                "price": 45,
                "postal_code": "00100"
            },
            "fields": [{
                "key": "category",
                "label": "Category",
                "type": "select",
                "requirement": "required",
                "status": "set",
                "value": "258",
                "section": "details",
                "option_count": 593,
                "options_returned": 593,
                "options_truncated": false
            }],
            "options": options,
            "required_fields": [],
            "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
        })
    };
    let client = || {
        MockClient::with_responses([
            response(StatusCode::OK, draft()),
            response(StatusCode::OK, delivery_page(true)),
            response(
                StatusCode::OK,
                json!({
                    "categories": [{
                        "id": "258",
                        "label": "Bicycle accessories",
                        "isSelectable": true
                    }]
                }),
            ),
        ])
    };

    let compact = invoke(
        &TestRuntime { client: client() },
        [
            "flea", "--format", "json", "tori", "draft", "show", "draft-1",
        ],
    );
    assert_eq!(compact["data"]["revision"], "revision-7");
    assert_eq!(compact["data"]["category"]["label"], "Bicycle accessories");
    assert_eq!(compact["data"]["delivery"]["selected"][0], "pickup");
    assert_eq!(compact["data"]["images"][0]["state"], "ready");
    assert_eq!(compact["data"]["option_sets"][0]["option_count"], 593);
    assert!(compact["data"].get("fields").is_none());
    assert!(compact["data"].get("options").is_none());
    let compact_text = compact.to_string();
    assert!(!compact_text.contains("Category 257"));
    assert!(!compact_text.contains("Category 259"));

    let expanded = invoke(
        &TestRuntime { client: client() },
        [
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "show",
            "draft-1",
            "--include-fields",
            "--include-options",
            "category",
        ],
    );
    assert_eq!(expanded["data"]["fields"].as_array().unwrap().len(), 2);
    assert_eq!(expanded["data"]["options"].as_array().unwrap().len(), 593);
    assert_eq!(expanded["data"]["options"][258]["label"], "Category 258");

    let toon = run_with_runtime(
        ["flea", "tori", "draft", "show", "draft-1"],
        &TestRuntime { client: client() },
    );
    let toon_again = run_with_runtime(
        ["flea", "tori", "draft", "show", "draft-1"],
        &TestRuntime { client: client() },
    );
    assert_eq!(toon.exit_code, 0, "{}", toon.document);
    assert_eq!(toon.document, toon_again.document);
    let decoded: Value = toon_format::decode_default(&toon.document).unwrap();
    assert_eq!(decoded["data"]["revision"], "revision-7");
    assert!(decoded["data"].get("options").is_none());

    let expanded_toon = run_with_runtime(
        [
            "flea",
            "tori",
            "draft",
            "show",
            "draft-1",
            "--include-fields",
            "--include-options",
            "category",
        ],
        &TestRuntime { client: client() },
    );
    assert_eq!(expanded_toon.exit_code, 0, "{}", expanded_toon.document);
    let decoded: Value = toon_format::decode_default(&expanded_toon.document).unwrap();
    assert_eq!(decoded["data"]["options"].as_array().unwrap().len(), 593);
    assert_eq!(decoded["data"]["fields"].as_array().unwrap().len(), 2);
}

#[test]
fn draft_show_compact_and_expanded_output_matches_json_and_toon_snapshots() {
    let run = |format: &str, expanded: bool| {
        let mut args = vec![
            "flea", "--format", format, "tori", "draft", "show", "draft-1",
        ];
        if expanded {
            args.extend(["--include-fields", "--include-options", "category"]);
        }
        let result = run_with_runtime(
            args,
            &TestRuntime {
                client: draft_show_snapshot_client(),
            },
        );
        assert_eq!(result.exit_code, 0, "{}", result.document);
        normalize_observation_timestamp(result.document)
    };

    let compact_json = run("json", false);
    let compact_toon = run("toon", false);
    let expanded_json = run("json", true);
    let expanded_toon = run("toon", true);
    assert_eq!(
        compact_json,
        include_str!("snapshots/draft-show-compact.json").trim_end()
    );
    assert_eq!(
        compact_toon,
        include_str!("snapshots/draft-show-compact.toon").trim_end()
    );
    assert_eq!(
        expanded_json,
        include_str!("snapshots/draft-show-expanded.json").trim_end()
    );
    assert_eq!(
        expanded_toon,
        include_str!("snapshots/draft-show-expanded.toon").trim_end()
    );
}

#[test]
fn draft_validate_is_compact_deterministic_and_read_only_in_json_and_toon() {
    let client = || {
        MockClient::with_responses([
            response(
                StatusCode::OK,
                json!({
                    "draft_id": "draft-1",
                    "etag": "one",
                    "values": {
                        "category": "furniture/chairs",
                        "title": "Chair",
                        "description": "Solid birch chair",
                        "trade_type": "sell",
                        "price": 45,
                        "postal_code": "00100"
                    },
                    "fields": [],
                    "options": [],
                    "required_fields": [],
                    "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
                }),
            ),
            response(StatusCode::OK, delivery_page(true)),
            response(
                StatusCode::OK,
                json!({
                    "categories": [{
                        "id": "furniture/chairs",
                        "label": "Chairs",
                        "isSelectable": true
                    }]
                }),
            ),
        ])
    };
    let json_client = client();
    let value = invoke(
        &TestRuntime {
            client: json_client.clone(),
        },
        [
            "flea", "--format", "json", "tori", "draft", "validate", "draft-1",
        ],
    );

    assert_eq!(
        value["data"],
        json!({
            "draft_id": "draft-1",
            "revision": "one",
            "ready": true,
            "category_validation": {
                "value": "furniture/chairs",
                "label": "Chairs",
                "exists": true,
                "selectable": true,
                "compatible": true,
                "existence_source": "category_taxonomy",
                "selectability_source": "category_taxonomy",
                "compatibility_source": "listing_composer"
            }
        })
    );
    let requests = json_client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(
        requests
            .iter()
            .all(|request| request.method == reqwest::Method::GET)
    );
    drop(requests);

    let first = run_with_runtime(
        ["flea", "tori", "draft", "validate", "draft-1"],
        &TestRuntime { client: client() },
    );
    let second = run_with_runtime(
        ["flea", "tori", "draft", "validate", "draft-1"],
        &TestRuntime { client: client() },
    );
    assert_eq!(first.document, second.document);
    assert!(first.document.contains("ready: true"));
    assert!(!first.document.contains("missing"));
}

#[test]
fn draft_price_update_uses_the_item_creation_service_and_source_shape() {
    let client = MockClient::with_responses([
        response(
            StatusCode::OK,
            json!({
                "ad": {
                    "id": 46031010,
                    "etag": "one",
                    "values": { "trade_type": "1" }
                },
                "model": { "sections": [] }
            }),
        ),
        response(
            StatusCode::OK,
            json!({
                "id": 46031010,
                "etag": "two",
                "data": { "price": { "price_amount": 5 } },
                "violations": []
            }),
        ),
        response(
            StatusCode::OK,
            json!({
                "ad": {
                    "id": 46031010,
                    "etag": "three",
                    "values": {
                        "trade_type": "1",
                        "price": [{ "price_amount": "5" }]
                    }
                },
                "model": { "sections": [] }
            }),
        ),
    ]);
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        [
            "flea", "--format", "json", "tori", "draft", "update", "46031010", "--price", "5",
        ],
    );

    assert_eq!(value["data"]["draft"]["values"]["trade_type"], "sell");
    assert_eq!(value["data"]["draft"]["values"]["price"]["kind"], "fixed");
    assert_eq!(value["data"]["draft"]["values"]["price"]["amount"], 5);
    assert_eq!(value["data"]["draft"]["values"]["price"]["currency"], "EUR");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(requests[1].method, reqwest::Method::PATCH);
    assert_eq!(requests[1].path_and_query, "/items/46031010");
    assert_eq!(requests[1].service, "RC-ITEM-CREATION-FLOW-API");
    assert_eq!(requests[1].host, flea::api::client::ApiHost::Gateway);
    assert_eq!(requests[1].if_match.as_ref().unwrap(), "one");
    assert_eq!(
        requests[1].content_type.as_ref().unwrap(),
        "application/json"
    );
    let flea::api::client::RequestBody::Bytes(body) = &requests[1].body else {
        panic!("expected JSON request body")
    };
    assert_eq!(
        serde_json::from_slice::<Value>(body).unwrap(),
        json!({ "data": { "price": { "price_amount": 5 } } })
    );
}

#[test]
fn html_price_failure_is_transient_but_unsafe_partial_envelope() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
    let client = MockClient::with_responses([
        response(
            StatusCode::OK,
            json!({
                "ad": {
                    "id": 46031010,
                    "etag": "one",
                    "values": { "trade_type": "1" }
                },
                "model": { "sections": [] }
            }),
        ),
        HttpResponse {
            status: StatusCode::BAD_GATEWAY,
            headers,
            body: b"<html>bad gateway</html>".to_vec(),
        },
        response(
            StatusCode::OK,
            json!({
                "ad": {
                    "id": 46031010,
                    "etag": "one",
                    "values": { "trade_type": "1" }
                },
                "model": { "sections": [] }
            }),
        ),
    ]);

    let result = run_with_runtime(
        [
            "flea", "--format", "json", "tori", "draft", "update", "46031010", "--price", "5",
        ],
        &TestRuntime {
            client: client.clone(),
        },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 50);
    assert_eq!(value["error"]["code"], "mutation.uncertain");
    assert_eq!(value["error"]["upstream_transient"], true);
    assert_eq!(value["error"]["safe_to_retry"], false);
    assert_eq!(value["error"]["details"]["stage"], "apply_price");
    assert_eq!(value["error"]["details"]["status"], 502);
    assert_eq!(value["error"]["details"]["content_type"], "text/html");
    assert_eq!(value["partial"]["completed_steps"], json!(["fetch_draft"]));
    assert_eq!(
        value["next_actions"][0]["command"],
        "flea tori draft show 46031010"
    );
    assert_eq!(
        value["next_actions"][1]["command"],
        "flea tori draft update 46031010 --price VALUE"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 3);
}

#[test]
fn partial_draft_failure_preserves_recovery_envelope_and_exit_code() {
    let client = MockClient::with_responses([
        response(StatusCode::CREATED, draft_state("one")),
        response(
            StatusCode::OK,
            json!({
                "categories": [{
                    "id": "chairs",
                    "label": "Chairs",
                    "isSelectable": true
                }]
            }),
        ),
        response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "message": "category service unavailable" }),
        ),
        response(StatusCode::OK, draft_state("one")),
    ]);
    let result = run_with_runtime(
        [
            "flea",
            "--format",
            "json",
            "tori",
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
    assert_eq!(value["error"]["code"], "draft.create_incomplete");
    assert_eq!(value["error"]["safe_to_retry"], false);
    assert_eq!(value["error"]["details"]["duplicate_draft_risk"], true);
    assert!(
        value["error"]["retry_guidance"]
            .as_str()
            .unwrap()
            .contains("Repeating draft create risks a duplicate")
    );
    assert_eq!(value["partial"]["draft_id"], "draft-1");
    assert_eq!(value["partial"]["create"]["allocation"], "persisted");
    assert_eq!(value["partial"]["create"]["retry_create"], false);
    assert_eq!(value["partial"]["create"]["duplicate_draft_risk"], true);
    assert_eq!(value["partial"]["active_step"], "apply_category");
    assert_eq!(value["partial"]["failed_stage"], "apply_category");
    assert_eq!(value["partial"]["observed_etag"], "one");
    assert_eq!(value["partial"]["observation"]["status"], "observed");
    assert!(value["partial"]["observation"]["observed_at"].is_string());
    assert_eq!(value["partial"]["absent_fields"], json!(["category"]));
    assert_eq!(value["partial"]["field_summary"][0]["field"], "category");
    assert_eq!(value["partial"]["field_summary"][0]["status"], "absent");
    assert_eq!(
        value["next_actions"][0]["command"],
        "flea tori draft show draft-1"
    );
    assert_eq!(
        value["next_actions"][1]["command"],
        "flea tori draft update draft-1 --category VALUE"
    );
}

#[test]
fn uncertain_creation_separates_transience_from_replay_safety() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
    let client = MockClient::with_responses([HttpResponse {
        status: StatusCode::OK,
        headers,
        body: b"<html>unsupported success</html>".to_vec(),
    }]);

    let result = run_with_runtime(
        ["flea", "--format", "json", "tori", "draft", "create"],
        &TestRuntime { client },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 50);
    assert_eq!(value["error"]["code"], "mutation.uncertain");
    assert_eq!(value["error"]["upstream_transient"], false);
    assert_eq!(value["error"]["safe_to_retry"], false);
    assert!(
        value["error"]["retry_guidance"]
            .as_str()
            .unwrap()
            .contains("Do not repeat")
    );
    assert_eq!(value["error"]["details"]["stage"], "create_draft");
    assert_eq!(value["error"]["details"]["status"], 200);
    assert_eq!(value["error"]["details"]["content_type"], "text/html");
}

#[test]
fn bad_gateway_read_is_transient_and_safe_to_retry() {
    let client = MockClient::with_responses([response(
        StatusCode::BAD_GATEWAY,
        json!({ "message": "gateway unavailable" }),
    )]);
    let result = run_with_runtime(
        [
            "flea", "--format", "json", "tori", "draft", "show", "draft-1",
        ],
        &TestRuntime { client },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 40);
    assert_eq!(value["error"]["code"], "upstream.request_failed");
    assert_eq!(
        value["error"]["observation"]["state"],
        "temporarily_unavailable"
    );
    assert_eq!(value["error"]["upstream_transient"], true);
    assert_eq!(value["error"]["safe_to_retry"], true);
    assert!(
        value["error"]["retry_guidance"]
            .as_str()
            .unwrap()
            .contains("repeating this operation is safe")
    );
}

#[test]
fn bad_gateway_after_draft_mutation_is_transient_but_unsafe_to_retry() {
    let client = MockClient::with_responses([
        response(StatusCode::OK, draft_state("one")),
        response(
            StatusCode::BAD_GATEWAY,
            json!({ "message": "gateway unavailable" }),
        ),
        response(StatusCode::OK, draft_state("one")),
    ]);
    let result = run_with_runtime(
        [
            "flea", "--format", "json", "tori", "draft", "update", "draft-1", "--title", "Chair",
        ],
        &TestRuntime { client },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 50);
    assert_eq!(value["error"]["code"], "mutation.uncertain");
    assert_eq!(value["error"]["upstream_transient"], true);
    assert_eq!(value["error"]["safe_to_retry"], false);
    assert!(
        value["error"]["retry_guidance"]
            .as_str()
            .unwrap()
            .contains("Inspect authoritative state first")
    );
    assert_eq!(
        value["next_actions"][0]["command"],
        "flea tori draft show draft-1"
    );
    assert_eq!(
        value["next_actions"][1]["command"],
        "flea tori draft update draft-1 --title VALUE"
    );
}

#[test]
fn malformed_read_success_is_safe_to_repeat_but_not_transient() {
    let mut headers = HeaderMap::new();
    headers.insert("content-type", "text/html; charset=utf-8".parse().unwrap());
    let client = MockClient::with_responses([HttpResponse {
        status: StatusCode::OK,
        headers,
        body: b"<html>unsupported success</html>".to_vec(),
    }]);
    let result = run_with_runtime(
        [
            "flea", "--format", "json", "tori", "draft", "show", "draft-1",
        ],
        &TestRuntime { client },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 40);
    assert_eq!(value["error"]["code"], "upstream.unexpected_response");
    assert_eq!(
        value["error"]["observation"]["state"],
        "unrecognized_response"
    );
    assert_eq!(value["error"]["upstream_transient"], false);
    assert_eq!(value["error"]["safe_to_retry"], true);
}

#[test]
fn draft_http_absence_is_the_only_draft_not_found_outcome() {
    let result = run_with_runtime(
        [
            "flea", "--format", "json", "tori", "draft", "show", "draft-1",
        ],
        &TestRuntime {
            client: MockClient::with_responses([
                response(StatusCode::NOT_FOUND, Value::Null),
                response(StatusCode::OK, json!({ "summaries": [], "total": 0 })),
            ]),
        },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(value["error"]["code"], "draft.not_found");
    assert_eq!(value["error"]["observation"]["state"], "confirmed_absent");
    assert_eq!(
        value["error"]["observation"]["source"],
        "draft_lifecycle_reconciliation"
    );
    assert!(value["error"]["observation"]["observed_at"].is_string());
    assert_eq!(
        value["error"]["observation"]["status_evidence"]["http_status"],
        404
    );
    assert_eq!(value["error"]["safe_to_retry"], false);
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
            json!({
                "location": "https://img.tori.net/dynamic/default/image-1.png"
            }),
        ),
        response(StatusCode::OK, draft_with_images("two", "processing")),
        response(StatusCode::OK, draft_with_images("three", "processing")),
    ]);
    let value = invoke_vec(
        &TestRuntime {
            client: client.clone(),
        },
        vec![
            "flea".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
            "tori".to_owned(),
            "draft".to_owned(),
            "image".to_owned(),
            "add".to_owned(),
            "draft-1".to_owned(),
            image_path.to_string_lossy().into_owned(),
        ],
    );

    assert_eq!(
        value["data"]["images"][0]["image_id"],
        "https://img.tori.net/dynamic/default/image-1.png"
    );
    let processing = &value["data"]["image_processing"][0];
    assert_eq!(processing["source_format"], "png");
    assert_eq!(processing["uploaded_format"], "png");
    assert_eq!(processing["final_width"], 4);
    assert_eq!(processing["final_height"], 6);
    assert!(processing["final_byte_size"].as_u64().unwrap() > 0);
    assert_eq!(processing["metadata_stripped"], true);
    assert_eq!(processing["recompressed"], false);
    assert_eq!(client.requests.lock().unwrap().len(), 4);
}

#[test]
fn deterministic_publish_validation_has_local_remediation_guidance() {
    let client = MockClient::with_responses([
        response(StatusCode::OK, json!({ "summaries": [], "total": 0 })),
        response(StatusCode::OK, draft_state("one")),
        response(StatusCode::OK, delivery_page(false)),
        response(StatusCode::OK, json!({ "categories": [] })),
    ]);
    let result = run_with_runtime(
        [
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "publish",
            "draft-1",
            "--if-revision",
            "one",
        ],
        &TestRuntime { client },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 20);
    assert_eq!(value["error"]["code"], "draft.validation_failed");
    assert_eq!(value["error"]["upstream_transient"], false);
    assert_eq!(value["error"]["safe_to_retry"], false);
    assert_eq!(value["partial"]["publication"], "unattempted");
    assert_eq!(value["partial"]["failed_stage"], "validate");
    assert_eq!(
        value["next_actions"][0]["command"],
        "flea tori draft update draft-1 --category VALUE"
    );
    let guidance = value["error"]["retry_guidance"].as_str().unwrap();
    assert!(guidance.contains("Validation found `category`"));
    assert!(!guidance.contains("upstream failure"));
}

#[test]
fn stale_publish_revision_has_structured_read_only_recovery() {
    let values = json!({
        "category": "furniture/chairs",
        "title": "Chair",
        "description": "Solid birch chair",
        "trade_type": "sell",
        "price": 45,
        "postal_code": "00100"
    });
    let valid = json!({
        "draft_id": "draft-1",
        "etag": "observed",
        "values": values,
        "fields": [],
        "options": [],
        "required_fields": ["title", "delivery"],
        "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
    });
    let client = MockClient::with_responses([
        response(StatusCode::OK, json!({ "summaries": [], "total": 0 })),
        response(StatusCode::OK, valid.clone()),
        response(StatusCode::OK, delivery_page(true)),
        response(
            StatusCode::OK,
            json!({
                "categories": [{
                    "id": "furniture/chairs",
                    "label": "Chairs",
                    "isSelectable": true
                }]
            }),
        ),
        response(StatusCode::OK, valid),
    ]);

    let result = run_with_runtime(
        [
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "publish",
            "draft-1",
            "--if-revision",
            "expected",
        ],
        &TestRuntime {
            client: client.clone(),
        },
    );
    let value: Value = serde_json::from_str(&result.document).unwrap();

    assert_eq!(result.exit_code, 30);
    assert_eq!(value["error"]["code"], "draft.revision_conflict");
    assert_eq!(value["error"]["details"]["expected_revision"], "expected");
    assert_eq!(value["error"]["details"]["observed_revision"], "observed");
    assert_eq!(value["error"]["details"]["safe_to_retry"], false);
    assert_eq!(value["error"]["safe_to_retry"], false);
    assert_eq!(
        value["error"]["details"]["next_action"],
        "flea tori draft show draft-1"
    );
    assert_eq!(
        value["next_actions"],
        json!([
            { "command": "flea tori draft show draft-1" },
            { "command": "flea tori draft validate draft-1" }
        ])
    );
    assert!(
        client
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|request| request.method == reqwest::Method::GET)
    );
}

#[test]
fn publish_flows_through_every_http_step() {
    let values = json!({
        "category": "furniture/chairs",
        "title": "Chair",
        "description": "Solid birch chair",
        "trade_type": "sell",
        "price": 45,
        "postal_code": "00100"
    });
    let valid = json!({
        "draft_id": "draft-1",
        "etag": "one",
        "values": values,
        "fields": [],
        "options": [],
        "required_fields": ["title", "delivery"],
        "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
    });
    let client = MockClient::with_responses([
        response(StatusCode::OK, json!({ "summaries": [], "total": 0 })),
        response(StatusCode::OK, valid.clone()),
        response(StatusCode::OK, delivery_page(true)),
        response(
            StatusCode::OK,
            json!({
                "categories": [{
                    "id": "furniture/chairs",
                    "label": "Chairs",
                    "isSelectable": true
                }]
            }),
        ),
        response(StatusCode::OK, valid.clone()),
        response(StatusCode::OK, json!({ "id": "draft-1", "etag": "two" })),
        response(StatusCode::OK, valid.clone()),
        response(
            StatusCode::OK,
            json!({
                "id": "draft-1",
                "ad-type": "recommerce",
                "etag": "revision-1",
                "values": valid["values"].clone()
            }),
        ),
        response(StatusCode::NO_CONTENT, Value::Null),
        response(StatusCode::OK, delivery_page(true)),
        response(
            StatusCode::OK,
            json!({
                "id": "draft-1",
                "choices": [{
                    "package-identifier": 10,
                    "specification-urn": "urn:product:package-specification:10"
                }]
            }),
        ),
        response(
            StatusCode::OK,
            json!({ "order-id": 11, "is-completed": true }),
        ),
        response(StatusCode::OK, json!({ "title": "Published" })),
        response(StatusCode::OK, json!({ "transactionId": 11 })),
        response(
            StatusCode::OK,
            json!({
                "listing_id": "draft-1",
                "state": "pending",
                "fields": {"trade_type": "1", "price": "5.25"}
            }),
        ),
    ]);
    let value = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        [
            "flea",
            "--format",
            "json",
            "tori",
            "draft",
            "publish",
            "draft-1",
            "--if-revision",
            "one",
        ],
    );

    assert_eq!(value["data"]["listing_id"], "draft-1");
    assert_eq!(value["data"]["observed_listing"]["trade_type"], "sell");
    assert_eq!(value["data"]["observed_listing"]["price"]["amount"], 5.25);
    assert_eq!(
        value["data"]["observed_listing"]["price"]["currency"],
        "EUR"
    );
    assert!(
        value["data"]["observed_listing"]["fields"]
            .get("price")
            .is_none()
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 15);
    assert_eq!(requests[0].path_and_query, "/search?limit=50&offset=0");
    assert_eq!(requests[0].service, "AD-SUMMARIES");
    assert_eq!(requests[5].method, reqwest::Method::PATCH);
    assert_eq!(requests[5].path_and_query, "/items/draft-1");
    assert_eq!(requests[5].service, "RC-ITEM-CREATION-FLOW-API");
    assert_eq!(
        requests[7].path_and_query,
        "/adinput/ad/recommerce/draft-1/update"
    );
    assert_eq!(requests[7].service, "APPS-ADINPUT");
    assert_eq!(requests[8].path_and_query, "/ads/draft-1/delivery");
    assert_eq!(requests[8].service, "TJT-API");
    assert_eq!(
        requests[11].path_and_query,
        "/adinput/order/choices/draft-1"
    );
    assert_eq!(requests[11].service, "APPS-ADINPUT");
    assert_eq!(
        requests[11].content_type.as_ref().unwrap(),
        "application/x-www-form-urlencoded"
    );
    let flea::api::client::RequestBody::Bytes(body) = &requests[11].body else {
        panic!("expected encoded package choice")
    };
    assert_eq!(body, b"choices=urn%3Aproduct%3Apackage-specification%3A10");
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
        ["flea", "--format", "json", "tori", "search", "tuoli"],
    );

    assert_eq!(value["data"]["results"][0]["listing_id"], "42346404");
    assert_eq!(value["data"]["pagination"]["limit"], 20);
    assert_eq!(
        value["next_actions"][0]["command"],
        "flea tori search 'tuoli' --page 2 --limit 20"
    );
    assert!(value["data"].get("_next_actions").is_none());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].service, "SEARCH-QUEST");
    assert!(requests[0].path_and_query.contains("client=android"));
    assert!(!requests[0].path_and_query.contains("include_filters"));
}

#[test]
fn public_search_result_flows_into_unauthenticated_item_detail() {
    let client = MockClient::with_responses([
        response(
            StatusCode::OK,
            json!({
                "docs": [{"id": "42346404", "heading": "Potkulauta"}],
                "metadata": {
                    "result_size": {"match_count": 1},
                    "paging": {"current": 1, "last": 1}
                }
            }),
        ),
        response(
            StatusCode::OK,
            json!({
                "ad": {
                    "title": "Potkulauta",
                    "description": "Micro Mini lasten potkulauta",
                    "price": 25,
                    "location": {"postalName": "Helsinki", "postalCode": "00100"},
                    "condition": {"id": 3, "value": "Hyvä"},
                    "images": [{"uri": "https://img.tori.net/item/one"}]
                },
                "meta": {
                    "adId": 42346404,
                    "history": [{"mode": "PLAY", "broadcasted": "2026-08-22T12:00:00+03:00"}]
                },
                "canonical_url": "https://www.tori.fi/recommerce/forsale/item/42346404"
            }),
        ),
    ]);
    let runtime = TestRuntime {
        client: client.clone(),
    };
    let search = invoke(
        &runtime,
        ["flea", "--format", "json", "tori", "search", "Micro Mini"],
    );
    let listing_id = search["data"]["results"][0]["listing_id"].as_str().unwrap();
    let value = invoke_vec(
        &runtime,
        vec![
            "flea".to_owned(),
            "--format".to_owned(),
            "json".to_owned(),
            "tori".to_owned(),
            "item".to_owned(),
            "show".to_owned(),
            listing_id.to_owned(),
        ],
    );

    assert_eq!(value["data"]["title"], "Potkulauta");
    assert!(
        value["data"]["description"]
            .as_str()
            .unwrap()
            .contains("Micro Mini")
    );
    assert_eq!(value["data"]["condition"]["value"], "Hyvä");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[1].path_and_query, "/adview/42346404");
    assert_eq!(requests[1].service, "ADVIEW-PROVIDER-RC");
}

#[test]
fn category_and_listing_commands_flow_through_http_normalization() {
    let categories = MockClient::with_responses([response(
        StatusCode::OK,
        json!({
            "categories": [{
                "id": 100,
                "label": "Furniture",
                "isSelectable": true
            }]
        }),
    )]);
    let category = invoke(
        &TestRuntime {
            client: categories.clone(),
        },
        ["flea", "--format", "json", "tori", "category", "list"],
    );
    assert_eq!(category["data"]["categories"][0]["category_id"], "100");
    let requests = categories.requests.lock().unwrap();
    assert_eq!(requests[0].path_and_query, "/categories/taxonomy");
    assert_eq!(requests[0].service, "RC-ITEM-CREATION-FLOW-API");

    let listings = MockClient::with_responses([response(
        StatusCode::OK,
        json!({
            "id": "listing-1",
            "state": { "type": "pending", "label": "Under review" },
            "data": {
                "title": "Chair",
                "subtitle": "Tori myydään 5 €",
                "image": "item/listing-1/image"
            }
        }),
    )]);
    let listing = invoke(
        &TestRuntime {
            client: listings.clone(),
        },
        [
            "flea",
            "--format",
            "json",
            "tori",
            "listing",
            "show",
            "listing-1",
        ],
    );
    assert_eq!(listing["data"]["listing_id"], "listing-1");
    assert_eq!(listing["data"]["state"], "pending");
    assert_eq!(listing["data"]["fields"]["title"], "Chair");
    assert_eq!(listing["observation"]["state"], "confirmed_present");
    assert_eq!(listing["observation"]["source"], "listing_detail");
    let requests = listings.requests.lock().unwrap();
    assert_eq!(requests[0].path_and_query, "/listing-1");
    assert_eq!(requests[0].service, "AD-SUMMARIES");
}

#[test]
fn truncated_category_search_actions_preserve_query_and_hierarchy_context() {
    let taxonomy = json!({
        "categories": [{
            "id": 100,
            "label": "Root",
            "isSelectable": false,
            "children": [
                { "id": 101, "label": "Tarvikkeet A", "isSelectable": true },
                { "id": 102, "label": "Tarvikkeet B", "isSelectable": true },
                { "id": 103, "label": "Tarvikkeet C", "isSelectable": true }
            ]
        }]
    });
    let client = MockClient::with_responses([
        response(StatusCode::OK, taxonomy.clone()),
        response(StatusCode::OK, taxonomy),
    ]);
    let runtime = TestRuntime { client };

    let by_parent = invoke(
        &runtime,
        [
            "flea",
            "--format",
            "json",
            "tori",
            "category",
            "search",
            "tarvikkeet",
            "--parent",
            "100",
            "--limit",
            "2",
        ],
    );
    assert_eq!(by_parent["data"]["returned"], 2);
    assert_eq!(by_parent["data"]["total"], 3);
    assert_eq!(by_parent["data"]["truncated"], true);
    assert_eq!(
        by_parent["next_actions"][0]["command"],
        "flea tori category search 'tarvikkeet' --parent '100' --offset 2 --limit 2"
    );

    let by_path = invoke(
        &runtime,
        [
            "flea",
            "--format",
            "json",
            "tori",
            "category",
            "search",
            "tarvikkeet",
            "--path",
            "Root",
            "--limit",
            "2",
        ],
    );
    assert_eq!(by_path["data"]["context"]["category_id"], "100");
    assert_eq!(
        by_path["next_actions"][0]["command"],
        "flea tori category search 'tarvikkeet' --path 'Root' --offset 2 --limit 2"
    );
}

#[test]
fn category_http_failures_have_specific_structured_errors() {
    let endpoint = invoke_error(
        &TestRuntime {
            client: MockClient::with_responses([response(StatusCode::NOT_FOUND, Value::Null)]),
        },
        ["flea", "--format", "json", "tori", "category", "list"],
    );
    assert_eq!(endpoint["error"]["code"], "category.endpoint_unavailable");

    let authentication = invoke_error(
        &TestRuntime {
            client: MockClient::with_responses([response(StatusCode::UNAUTHORIZED, Value::Null)]),
        },
        ["flea", "--format", "json", "tori", "category", "list"],
    );
    assert_eq!(
        authentication["error"]["code"],
        "category.authentication_failed"
    );

    let malformed = invoke_error(
        &TestRuntime {
            client: MockClient::with_responses([response(
                StatusCode::OK,
                json!({ "categories": [{ "id": "bad" }] }),
            )]),
        },
        ["flea", "--format", "json", "tori", "category", "list"],
    );
    assert_eq!(malformed["error"]["code"], "category.protocol_drift");
}

#[test]
fn changed_listing_detail_model_is_unrecognized_instead_of_not_found() {
    let value = invoke_error(
        &TestRuntime {
            client: MockClient::with_responses([
                response(StatusCode::OK, json!({ "model": 2 })),
                response(StatusCode::OK, json!({ "summaries": [], "total": 0 })),
            ]),
        },
        [
            "flea",
            "--format",
            "json",
            "tori",
            "listing",
            "show",
            "listing-1",
        ],
    );

    assert_eq!(value["error"]["code"], "upstream.unexpected_response");
    assert_ne!(value["error"]["code"], "listing.not_found");
    assert_eq!(
        value["error"]["observation"]["state"],
        "unrecognized_response"
    );
    assert_eq!(value["error"]["safe_to_retry"], true);
}

#[test]
fn listing_list_uses_the_published_listing_search_endpoint_and_paginates() {
    let first_page: Value =
        serde_json::from_str(include_str!("fixtures/listings/page-1.json")).unwrap();
    let second_page: Value =
        serde_json::from_str(include_str!("fixtures/listings/page-2.json")).unwrap();
    let mut responses = vec![response(StatusCode::OK, first_page)];
    for index in 0..50 {
        responses.push(response(
            StatusCode::OK,
            json!({
                "id": format!("10{index:02}"),
                "etag": format!("listing-{index}"),
                "fields": {"trade_type": "sell", "price": index + 10}
            }),
        ));
    }
    responses.push(response(StatusCode::OK, second_page));
    for index in 50..52 {
        responses.push(response(
            StatusCode::OK,
            json!({
                "id": format!("10{index:02}"),
                "etag": format!("listing-{index}"),
                "fields": {"trade_type": "sell", "price": index + 10}
            }),
        ));
    }
    let client = MockClient::with_responses(responses);

    let output = invoke(
        &TestRuntime {
            client: client.clone(),
        },
        ["flea", "--format", "json", "tori", "listing", "list"],
    );

    assert_eq!(output["data"]["total"], 52);
    assert_eq!(output["data"]["listings"].as_array().unwrap().len(), 52);
    assert_eq!(output["data"]["listings"][0]["listing_id"], "1000");
    assert_eq!(output["data"]["listings"][0]["statistics"]["views"], 100);
    assert_eq!(output["data"]["listings"][0]["trade_type"], "sell");
    assert_eq!(output["data"]["listings"][0]["price"]["amount"], 10);
    assert_eq!(output["data"]["listings"][0]["price"]["currency"], "EUR");

    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 54);
    assert_eq!(requests[0].service, "AD-SUMMARIES");
    assert_eq!(requests[0].path_and_query, "/search?limit=50&offset=0");
    assert_eq!(requests[51].path_and_query, "/search?limit=50&offset=50");
}

fn invoke<const N: usize>(runtime: &TestRuntime, args: [&str; N]) -> Value {
    let result = run_with_runtime(args, runtime);
    assert_eq!(result.exit_code, 0, "{}", result.document);
    assert_eq!(result.presentation, Presentation::Structured);
    serde_json::from_str(&result.document).expect("one JSON envelope")
}

fn invoke_error<const N: usize>(runtime: &TestRuntime, args: [&str; N]) -> Value {
    let result = run_with_runtime(args, runtime);
    assert_ne!(result.exit_code, 0, "{}", result.document);
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

fn normalize_observation_timestamp(mut document: String) -> String {
    for marker in ["observed_at\": \"", "observed_at: \""] {
        if let Some(start) = document.find(marker).map(|index| index + marker.len())
            && let Some(end) = document[start..].find('\"').map(|index| start + index)
        {
            document.replace_range(start..end, "<observed-at>");
        }
    }
    document
}

fn draft_show_snapshot_client() -> MockClient {
    MockClient::with_responses([
        response(
            StatusCode::OK,
            json!({
                "draft_id": "draft-1",
                "etag": "etag-7",
                "revision": "revision-7",
                "values": {
                    "category": "2",
                    "title": "Chair",
                    "description": "Solid birch chair",
                    "trade_type": "sell",
                    "price": 45,
                    "postal_code": "00100"
                },
                "fields": [{
                    "key": "category",
                    "label": "Category",
                    "type": "select",
                    "requirement": "required",
                    "status": "set",
                    "value": "2",
                    "section": "details",
                    "option_count": 3,
                    "options_returned": 3,
                    "options_truncated": false
                }],
                "options": [
                    { "field": "category", "value": "1", "label": "Tables" },
                    { "field": "category", "value": "2", "label": "Chairs" },
                    { "field": "category", "value": "3", "label": "Sofas" }
                ],
                "required_fields": [],
                "images": [{ "image_id": "image-1", "position": 0, "state": "ready" }]
            }),
        ),
        response(StatusCode::OK, delivery_page(true)),
        response(
            StatusCode::OK,
            json!({
                "categories": [{ "id": "2", "label": "Chairs", "isSelectable": true }]
            }),
        ),
    ])
}

fn delivery_page(selected: bool) -> Value {
    json!({
        "context": {
            "adId": "draft-1",
            "shipping": false,
            "meetup": selected,
            "shippingProducts": []
        },
        "sections": {
            "deliveryOptions": {
                "shipping": { "title": "Tori delivery" },
                "meetup": { "title": "Pickup" }
            },
            "shipping": {
                "packageSizes": {
                    "small": { "title": "Small", "size": "SMALL" }
                }
            }
        }
    })
}

fn draft_state(etag: &str) -> Value {
    json!({
        "draft_id": "draft-1",
        "etag": etag,
        "values": {},
        "fields": [],
        "options": [],
        "required_fields": [],
        "images": []
    })
}

fn draft_with_images(etag: &str, state: &str) -> Value {
    json!({
        "draft_id": "draft-1",
        "etag": etag,
        "values": {},
        "fields": [],
        "options": [],
        "required_fields": [],
        "images": [{
            "image_id": "https://img.tori.net/dynamic/default/image-1.png",
            "url": "https://img.tori.net/dynamic/default/image-1.png",
            "position": 0,
            "state": state,
            "width": 4,
            "height": 6,
            "mime_type": "image/png"
        }]
    })
}
