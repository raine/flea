use std::sync::Arc;

use serde_json::Value;

use crate::{
    api::{
        adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
        auth::{GatewaySigner, RefreshRequest, SchibstedToriAuthenticationApi},
        client::{ClientConfig, DeviceIdentity, HttpClient, ReqwestTransport},
        item::HttpPublicItemApi,
        listings::HttpListingsApi,
        search::HttpPublicSearchApi,
    },
    cli::{
        Command, CommandRuntime,
        auth::{AuthCommandHandler, FileAuthStore, unix_time_now},
        auth_callback, category, draft, listing,
    },
    error::{AppError, ExitClass},
    storage::{StatePaths, credentials::CredentialStore},
};

#[derive(Default)]
pub struct ProductionRuntime;

impl CommandRuntime for ProductionRuntime {
    fn execute(&self, command: Command) -> Result<Value, AppError> {
        match command {
            Command::Auth(args) => execute_auth(args),
            Command::Category(args) => {
                let client = authenticated_client()?;
                let api = HttpListingsApi::new(Arc::new(client));
                category::dispatch_with_api(args, &api)
            }
            Command::Draft(args) => {
                let client = authenticated_client()?;
                let api = HttpAdInputApi::new(ClientTransport::new(client));
                block_on(draft::execute(args.command, api, WorkflowConfig::default()))
            }
            Command::Item(args) => {
                let api = HttpPublicItemApi::new(Arc::new(public_client()));
                super::item::dispatch_with_api(args, &api)
            }
            Command::Listing(args) => {
                let client = authenticated_client()?;
                let api = HttpListingsApi::new(Arc::new(client));
                listing::dispatch_with_api(args, &api)
            }
            Command::Search(args) => {
                let search_api = HttpPublicSearchApi::new(Arc::new(public_client()));
                let item_api = HttpPublicItemApi::new(Arc::new(public_client()));
                super::search::dispatch_with_apis(*args, &search_api, Some(&item_api))
            }
            Command::Location(args) => {
                let api = HttpPublicSearchApi::new(Arc::new(public_client()));
                super::location::dispatch_with_api(args, &api)
            }
            Command::Skill(args) => super::skill::dispatch(args),
        }
    }
}

fn execute_auth(args: super::auth::AuthArgs) -> Result<Value, AppError> {
    let paths = state_paths()?;
    if matches!(args.command, super::auth::AuthCommand::Login) {
        return execute_interactive_login(paths);
    }

    let store = FileAuthStore::new(paths);
    let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(), store);
    block_on(handler.dispatch(args.command))
}

fn execute_interactive_login(paths: StatePaths) -> Result<Value, AppError> {
    auth_callback::prepare(&paths)?;
    let store = FileAuthStore::new(paths.clone());
    let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(), store);
    let started = handler.start(unix_time_now()?)?;
    let flow_id = auth_value(&started, "flow_id")?.to_owned();
    let login_url = auth_value(&started, "login_url")?;
    let expires_at_unix = started
        .get("expires_at_unix")
        .and_then(Value::as_u64)
        .ok_or_else(|| AppError::unexpected("authentication start returned an invalid expiry"))?;
    let callback = auth_callback::open_and_wait(&paths, login_url, expires_at_unix)?;
    let result = block_on(handler.complete(&flow_id, &callback, unix_time_now()?));
    let _ = auth_callback::clear(&paths);
    result
}

fn auth_value<'a>(value: &'a Value, key: &str) -> Result<&'a str, AppError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::unexpected(format!("authentication start returned an invalid {key}"))
        })
}

fn public_client() -> HttpClient<ReqwestTransport> {
    HttpClient::new(
        ClientConfig {
            include_device_identity: false,
            ..ClientConfig::default()
        },
        DeviceIdentity {
            installation_id: String::new(),
            ab_test_device_id: String::new(),
        },
        None,
    )
}

fn authenticated_client() -> Result<HttpClient<ReqwestTransport>, AppError> {
    authenticated_client_with(
        state_paths()?,
        &SchibstedToriAuthenticationApi::new(),
        unix_time_now()?,
    )
}

fn authenticated_client_with<S: GatewaySigner>(
    paths: StatePaths,
    api: &SchibstedToriAuthenticationApi<S>,
    now: u64,
) -> Result<HttpClient<ReqwestTransport>, AppError> {
    let store = CredentialStore::new(paths);
    let locked = store.lock().map_err(|error| auth_storage(error, "lock"))?;
    let mut record = locked
        .load()
        .map_err(|error| auth_storage(error, "read"))?
        .ok_or_else(auth_required)?;
    if !record.bearer_is_valid_at(now, 30) {
        let refresh_token = record.refresh_token.clone();
        let id_token = record.id_token.clone();
        let device_id = record.device_id.clone();
        let installation_id = record.installation_id.clone();
        let ab_test_device_id = record.ab_test_device_id.clone();
        let credentials = block_on(api.refresh_credentials(
            RefreshRequest {
                refresh_token: &refresh_token,
                id_token: id_token.as_deref(),
                device_id: &device_id,
                installation_id: &installation_id,
                ab_test_device_id: &ab_test_device_id,
                now_unix: now,
            },
            |rotated_refresh_token, refreshed_id_token| {
                record.refresh_token = rotated_refresh_token.to_owned();
                record.id_token = refreshed_id_token.map(str::to_owned);
                locked
                    .save(&record)
                    .map_err(|error| auth_storage(error, "write_rotation"))
            },
        ))?;
        record = serde_json::to_value(credentials)
            .and_then(serde_json::from_value)
            .map_err(|error| {
                AppError::unexpected("authentication state types are incompatible")
                    .with_source(error)
            })?;
        locked
            .save(&record)
            .map_err(|error| auth_storage(error, "write"))?;
    }
    Ok(HttpClient::new(
        ClientConfig::default(),
        DeviceIdentity {
            installation_id: record.installation_id,
            ab_test_device_id: record.ab_test_device_id,
        },
        Some(record.bearer_token),
    ))
}

fn state_paths() -> Result<StatePaths, AppError> {
    StatePaths::discover().map_err(|error| auth_storage(error, "discover"))
}

fn auth_storage(
    error: impl std::error::Error + Send + Sync + 'static,
    operation: &'static str,
) -> AppError {
    let mut result = AppError::new(
        "auth.storage_failed",
        "authentication credential storage is unavailable",
        ExitClass::Authentication,
    )
    .with_details(serde_json::json!({ "operation": operation }))
    .with_source(error);
    result
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: "tori auth status".to_owned(),
        });
    result
}

fn auth_required() -> AppError {
    let mut error = AppError::authentication("auth.required", "authentication is required");
    error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: "tori auth start".to_owned(),
        });
    error
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the Tokio runtime uses static configuration")
        .block_on(future)
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Arc, Barrier, Mutex},
        thread,
    };

    use tempfile::tempdir;

    use super::*;
    use crate::storage::credentials::CredentialRecord;

    fn serve_refresh() -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let paths = ["/oauth/token", "/api/2/oauth/exchange", "/public/login"];
        let bodies = [
            r#"{"access_token":"access-new","refresh_token":"refresh-new"}"#,
            r#"{"data":{"code":"spid-new"}}"#,
            r#"{"userId":42,"token":{"value":"bearer-new"}}"#,
        ];
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            for (expected_path, body) in paths.into_iter().zip(bodies) {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0_u8; 4096];
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                    if let Some(headers_end) =
                        request.windows(4).position(|part| part == b"\r\n\r\n")
                    {
                        let headers = String::from_utf8_lossy(&request[..headers_end]);
                        let length = headers
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>())
                            })
                            .transpose()
                            .unwrap()
                            .unwrap_or(0);
                        if request.len() >= headers_end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with(&format!("POST {expected_path} ")));
                captured.lock().unwrap().push(expected_path.to_owned());
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (base_url, requests, worker)
    }

    fn expired_credentials() -> CredentialRecord {
        CredentialRecord {
            user_id: "user".to_owned(),
            refresh_token: "refresh-old".to_owned(),
            bearer_token: "bearer-old".to_owned(),
            id_token: None,
            bearer_expires_at_unix: 1,
            device_id: "device".to_owned(),
            installation_id: "installation".to_owned(),
            ab_test_device_id: "ab-test".to_owned(),
        }
    }

    #[test]
    fn concurrent_clients_share_one_rotating_refresh() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("state"));
        CredentialStore::new(paths.clone())
            .save(&expired_credentials())
            .unwrap();
        let (base_url, requests, server) = serve_refresh();
        let api = Arc::new(
            SchibstedToriAuthenticationApi::new().with_base_urls(base_url.clone(), base_url),
        );
        let barrier = Arc::new(Barrier::new(3));
        let workers = (0..2)
            .map(|_| {
                let api = Arc::clone(&api);
                let paths = paths.clone();
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    authenticated_client_with(paths, api.as_ref(), 1_000).unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for worker in workers {
            worker.join().unwrap();
        }
        server.join().unwrap();

        assert_eq!(requests.lock().unwrap().len(), 3);
        let stored = CredentialStore::new(paths).load().unwrap().unwrap();
        assert_eq!(stored.refresh_token, "refresh-new");
        assert_eq!(stored.bearer_token, "bearer-new");
        assert_eq!(stored.bearer_expires_at_unix, 4_600);
    }

    #[test]
    fn storage_failure_identifies_the_failed_operation() {
        let error = auth_storage(std::io::Error::other("fixture"), "write");

        assert_eq!(error.code, "auth.storage_failed");
        assert_eq!(error.details.unwrap()["operation"], "write");
        assert_eq!(error.next_actions[0].command, "tori auth status");
    }
}
