use serde::Serialize;

use crate::{
    cli::outcome::CommandOutcome,
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::MarketplaceContext,
    marketplace::tori::auth::{GatewaySigner, RefreshRequest, SchibstedToriAuthenticationApi},
    marketplace::tori::client::{ClientConfig, DeviceIdentity, HttpClient, ReqwestTransport},
    storage::{
        StatePaths,
        credentials::{CredentialRecord, CredentialStore, CredentialStoreError},
    },
};

pub(crate) async fn authenticated_client() -> Result<HttpClient<ReqwestTransport>, AppError> {
    authenticated_client_with(
        state_paths()?,
        &SchibstedToriAuthenticationApi::new(),
        unix_time_now()?,
    )
    .await
}

pub(crate) async fn status() -> Result<CommandOutcome, AppError> {
    auth_status(
        state_paths()?,
        &SchibstedToriAuthenticationApi::new(),
        unix_time_now()?,
    )
    .await
}

const MINIMUM_BEARER_LIFETIME_SECONDS: u64 = 30;

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
enum StoredBearerState {
    Valid,
    NearExpiry,
    Expired,
}

struct ResolvedCredentials {
    record: CredentialRecord,
    refreshed_from: Option<StoredBearerState>,
}

struct CredentialResolutionFailure {
    error: Box<AppError>,
    stored_bearer_state: Option<StoredBearerState>,
    bearer_expires_at_unix: Option<u64>,
}

#[derive(Serialize)]
struct AuthStatusOutput {
    authenticated: bool,
    health: &'static str,
    validation: &'static str,
    refresh_performed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stored_bearer_state: Option<StoredBearerState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bearer_expires_at_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_seconds: Option<u64>,
    #[serde(skip)]
    next_actions: Vec<NextAction>,
}

async fn authenticated_client_with<S: GatewaySigner>(
    paths: StatePaths,
    api: &SchibstedToriAuthenticationApi<S>,
    now: u64,
) -> Result<HttpClient<ReqwestTransport>, AppError> {
    let resolved = resolve_credentials_with(paths, api, now)
        .await
        .map_err(|failure| *failure.error)?
        .ok_or_else(auth_required)?;
    let record = resolved.record;
    Ok(HttpClient::new(
        ClientConfig::default(),
        DeviceIdentity {
            installation_id: record.installation_id,
            ab_test_device_id: record.ab_test_device_id,
        },
        Some(record.bearer_token),
    ))
}

async fn resolve_credentials_with<S: GatewaySigner>(
    paths: StatePaths,
    api: &SchibstedToriAuthenticationApi<S>,
    now: u64,
) -> Result<Option<ResolvedCredentials>, CredentialResolutionFailure> {
    let store = CredentialStore::new(paths);
    let locked = store
        .lock()
        .map_err(|error| resolution_storage(error, "lock"))?;
    let Some(mut record) = locked
        .load()
        .map_err(|error| resolution_storage(error, "read"))?
    else {
        return Ok(None);
    };
    let stored_bearer_state = bearer_state(&record, now);
    if matches!(stored_bearer_state, StoredBearerState::Valid) {
        return Ok(Some(ResolvedCredentials {
            record,
            refreshed_from: None,
        }));
    }

    let expires_at = record.bearer_expires_at_unix;
    let refresh_token = record.refresh_token.clone();
    let id_token = record.id_token.clone();
    let device_id = record.device_id.clone();
    let installation_id = record.installation_id.clone();
    let ab_test_device_id = record.ab_test_device_id.clone();
    let credentials = api
        .refresh_credentials(
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
        )
        .await
        .map_err(|error| CredentialResolutionFailure {
            error: Box::new(error),
            stored_bearer_state: Some(stored_bearer_state),
            bearer_expires_at_unix: Some(expires_at),
        })?;
    record = serde_json::to_value(credentials)
        .and_then(serde_json::from_value)
        .map_err(|error| CredentialResolutionFailure {
            error: Box::new(
                AppError::unexpected("authentication state types are incompatible")
                    .with_source(error),
            ),
            stored_bearer_state: Some(stored_bearer_state),
            bearer_expires_at_unix: Some(expires_at),
        })?;
    locked
        .save(&record)
        .map_err(|error| resolution_storage(error, "write"))?;
    Ok(Some(ResolvedCredentials {
        record,
        refreshed_from: Some(stored_bearer_state),
    }))
}

fn bearer_state(record: &CredentialRecord, now: u64) -> StoredBearerState {
    if record.bearer_expires_at_unix <= now {
        StoredBearerState::Expired
    } else if record.bearer_is_valid_at(now, MINIMUM_BEARER_LIFETIME_SECONDS) {
        StoredBearerState::Valid
    } else {
        StoredBearerState::NearExpiry
    }
}

async fn auth_status<S: GatewaySigner>(
    paths: StatePaths,
    api: &SchibstedToriAuthenticationApi<S>,
    now: u64,
) -> Result<CommandOutcome, AppError> {
    let mut output = match resolve_credentials_with(paths, api, now).await {
        Ok(Some(resolved)) => AuthStatusOutput {
            authenticated: true,
            health: if resolved.refreshed_from.is_some() {
                "refreshed"
            } else {
                "valid"
            },
            validation: if resolved.refreshed_from.is_some() {
                "online_refresh"
            } else {
                "local_expiry"
            },
            refresh_performed: resolved.refreshed_from.is_some(),
            stored_bearer_state: resolved.refreshed_from,
            user_id: Some(resolved.record.user_id),
            bearer_expires_at_unix: Some(resolved.record.bearer_expires_at_unix),
            expires_in_seconds: Some(resolved.record.bearer_expires_at_unix.saturating_sub(now)),
            next_actions: Vec::new(),
        },
        Ok(None) => unavailable_status("missing", "local_storage", None, None, true),
        Err(failure) => status_from_failure(failure),
    };
    let next_actions = std::mem::take(&mut output.next_actions);
    serde_json::to_value(output)
        .map(|data| CommandOutcome::new(data).with_next_actions(next_actions))
        .map_err(|error| AppError::output("failed to serialize auth status").with_source(error))
}

fn status_from_failure(failure: CredentialResolutionFailure) -> AuthStatusOutput {
    let (health, validation, login_required) = match failure.error.code.as_str() {
        "auth.refresh_rejected" | "auth.exchange_rejected" => {
            ("refresh_rejected", "online_refresh", true)
        }
        "auth.refresh_malformed"
        | "auth.credentials_malformed"
        | "upstream.unexpected_response" => ("malformed", "unverified", true),
        _ => (
            "temporarily_unavailable",
            "unverified",
            !failure.error.safe_to_retry,
        ),
    };
    unavailable_status(
        health,
        validation,
        failure.stored_bearer_state,
        failure.bearer_expires_at_unix,
        login_required,
    )
}

fn unavailable_status(
    health: &'static str,
    validation: &'static str,
    stored_bearer_state: Option<StoredBearerState>,
    bearer_expires_at_unix: Option<u64>,
    login_required: bool,
) -> AuthStatusOutput {
    AuthStatusOutput {
        authenticated: false,
        health,
        validation,
        refresh_performed: stored_bearer_state.is_some(),
        stored_bearer_state,
        user_id: None,
        bearer_expires_at_unix,
        expires_in_seconds: None,
        next_actions: vec![NextAction {
            command: if login_required {
                "flea tori auth login"
            } else {
                "flea tori auth status"
            }
            .to_owned(),
        }],
    }
}

fn resolution_storage(
    error: CredentialStoreError,
    operation: &'static str,
) -> CredentialResolutionFailure {
    let error = match error {
        CredentialStoreError::InvalidData(_)
        | CredentialStoreError::MissingRequiredValue
        | CredentialStoreError::InvalidAccountSelection
        | CredentialStoreError::AccountMismatch => {
            let mut malformed = AppError::authentication(
                "auth.credentials_malformed",
                "stored authentication credentials are malformed",
            );
            malformed.next_actions.push(NextAction {
                command: crate::invocation::tori("auth login"),
            });
            malformed
        }
        error => auth_storage(error, operation),
    };
    CredentialResolutionFailure {
        error: Box::new(error),
        stored_bearer_state: None,
        bearer_expires_at_unix: None,
    }
}

pub(crate) fn state_paths() -> Result<StatePaths, AppError> {
    StatePaths::discover(MarketplaceContext::TORI_FI)
        .map_err(|error| auth_storage(error, "discover"))
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
            command: crate::invocation::tori("auth status"),
        });
    result
}

fn auth_required() -> AppError {
    let mut error = AppError::authentication("auth.required", "authentication is required");
    error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: crate::invocation::tori("auth login"),
        });
    error
}

fn unix_time_now() -> Result<u64, AppError> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| {
            AppError::new(
                "auth.clock_invalid",
                "the system clock is invalid",
                ExitClass::Authentication,
            )
        })
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

    type MockResponses = Vec<(&'static str, &'static str, &'static str)>;

    fn serve_responses(
        responses: MockResponses,
    ) -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let captured = Arc::clone(&requests);
        let worker = thread::spawn(move || {
            for (expected_path, status, body) in responses {
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
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (base_url, requests, worker)
    }

    fn serve_refresh() -> (String, Arc<Mutex<Vec<String>>>, thread::JoinHandle<()>) {
        serve_responses(vec![
            (
                "/oauth/token",
                "200 OK",
                r#"{"access_token":"access-new","refresh_token":"refresh-new"}"#,
            ),
            (
                "/api/2/oauth/exchange",
                "200 OK",
                r#"{"data":{"code":"spid-new"}}"#,
            ),
            (
                "/public/login",
                "200 OK",
                r#"{"userId":42,"token":{"value":"bearer-new"}}"#,
            ),
        ])
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

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_clients_share_one_rotating_refresh() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
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
                tokio::spawn(async move {
                    barrier.wait();
                    authenticated_client_with(paths, api.as_ref(), 1_000)
                        .await
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        barrier.wait();
        for worker in workers {
            worker.await.unwrap();
        }
        server.join().unwrap();

        assert_eq!(requests.lock().unwrap().len(), 3);
        let stored = CredentialStore::new(paths).load().unwrap().unwrap();
        assert_eq!(stored.refresh_token, "refresh-new");
        assert_eq!(stored.bearer_token, "bearer-new");
        assert_eq!(stored.bearer_expires_at_unix, 4_600);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_reports_missing_credentials_with_login_action() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );

        let status = auth_status(paths, &SchibstedToriAuthenticationApi::new(), 1_000)
            .await
            .unwrap();

        assert_eq!(
            status,
            serde_json::json!({
                "authenticated": false,
                "health": "missing",
                "validation": "local_storage",
                "refresh_performed": false,
            })
        );
        assert_eq!(status.next_actions[0].command, "flea tori auth login");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_reports_locally_valid_credentials_without_network_access() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        let mut credentials = expired_credentials();
        credentials.bearer_expires_at_unix = 1_031;
        CredentialStore::new(paths.clone())
            .save(&credentials)
            .unwrap();

        let status = auth_status(paths, &SchibstedToriAuthenticationApi::new(), 1_000)
            .await
            .unwrap();

        assert_eq!(status["authenticated"], true);
        assert_eq!(status["health"], "valid");
        assert_eq!(status["validation"], "local_expiry");
        assert_eq!(status["refresh_performed"], false);
        assert_eq!(status["expires_in_seconds"], 31);
        assert_eq!(status["user_id"], "user");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_refreshes_near_expiry_and_expired_credentials() {
        for (expires_at, expected_state) in [(1_030, "near_expiry"), (999, "expired")] {
            let temporary = tempdir().unwrap();
            let paths = StatePaths::from_root(
                temporary.path().join("state"),
                crate::marketplace::MarketplaceContext::TORI_FI,
            );
            let mut credentials = expired_credentials();
            credentials.bearer_expires_at_unix = expires_at;
            CredentialStore::new(paths.clone())
                .save(&credentials)
                .unwrap();
            let (base_url, requests, server) = serve_refresh();
            let api =
                SchibstedToriAuthenticationApi::new().with_base_urls(base_url.clone(), base_url);

            let status = auth_status(paths.clone(), &api, 1_000).await.unwrap();
            server.join().unwrap();

            assert_eq!(status["authenticated"], true);
            assert_eq!(status["health"], "refreshed");
            assert_eq!(status["validation"], "online_refresh");
            assert_eq!(status["stored_bearer_state"], expected_state);
            assert_eq!(status["bearer_expires_at_unix"], 4_600);
            assert_eq!(requests.lock().unwrap().len(), 3);
            assert_eq!(
                CredentialStore::new(paths)
                    .load()
                    .unwrap()
                    .unwrap()
                    .bearer_token,
                "bearer-new"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_distinguishes_refresh_rejection_without_exposing_secrets() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        CredentialStore::new(paths.clone())
            .save(&expired_credentials())
            .unwrap();
        let (base_url, _, server) = serve_responses(vec![(
            "/oauth/token",
            "400 Bad Request",
            r#"{"error":"invalid_grant","refresh_token":"response-secret"}"#,
        )]);
        let api = SchibstedToriAuthenticationApi::new().with_base_urls(base_url.clone(), base_url);

        let status = auth_status(paths, &api, 1_000).await.unwrap();
        server.join().unwrap();
        let rendered = status.to_string();

        assert_eq!(status["authenticated"], false);
        assert_eq!(status["health"], "refresh_rejected");
        assert_eq!(status["stored_bearer_state"], "expired");
        assert_eq!(status.next_actions[0].command, "flea tori auth login");
        for secret in ["user", "refresh-old", "bearer-old", "response-secret"] {
            assert!(!rendered.contains(secret));
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_distinguishes_malformed_refresh_and_stored_credentials() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        CredentialStore::new(paths.clone())
            .save(&expired_credentials())
            .unwrap();
        let (base_url, _, server) =
            serve_responses(vec![("/oauth/token", "200 OK", r#"{"access_token":""}"#)]);
        let api = SchibstedToriAuthenticationApi::new().with_base_urls(base_url.clone(), base_url);

        let refresh_status = auth_status(paths, &api, 1_000).await.unwrap();
        server.join().unwrap();
        assert_eq!(refresh_status["health"], "malformed");
        assert_eq!(refresh_status["stored_bearer_state"], "expired");
        assert_eq!(
            refresh_status.next_actions[0].command,
            "flea tori auth login"
        );

        let malformed_temporary = tempdir().unwrap();
        let malformed_paths = StatePaths::from_root(
            malformed_temporary.path().join("state"),
            MarketplaceContext::TORI_FI,
        );
        CredentialStore::new(malformed_paths.clone())
            .save(&expired_credentials())
            .unwrap();
        let account_key = std::fs::read_to_string(malformed_paths.current_account_file()).unwrap();
        std::fs::write(
            malformed_paths.account_credentials_file(&account_key),
            b"not-json-secret",
        )
        .unwrap();
        let stored_status = auth_status(
            malformed_paths,
            &SchibstedToriAuthenticationApi::new(),
            1_000,
        )
        .await
        .unwrap();
        assert_eq!(stored_status["authenticated"], false);
        assert_eq!(stored_status["health"], "malformed");
        assert!(!stored_status.to_string().contains("not-json-secret"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_marks_network_failure_as_temporarily_unavailable() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        CredentialStore::new(paths.clone())
            .save(&expired_credentials())
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let api = SchibstedToriAuthenticationApi::new().with_base_urls(base_url.clone(), base_url);

        let status = auth_status(paths, &api, 1_000).await.unwrap();

        assert_eq!(status["authenticated"], false);
        assert_eq!(status["health"], "temporarily_unavailable");
        assert_eq!(status["validation"], "unverified");
        assert_eq!(status["stored_bearer_state"], "expired");
        assert_eq!(status.next_actions[0].command, "flea tori auth login");
    }

    #[test]
    fn storage_failure_identifies_the_failed_operation() {
        let error = auth_storage(std::io::Error::other("fixture"), "write");

        assert_eq!(error.code, "auth.storage_failed");
        assert_eq!(error.details.unwrap()["operation"], "write");
        assert_eq!(error.next_actions[0].command, "flea tori auth status");
    }

    #[test]
    fn missing_authentication_recommends_public_login() {
        let error = auth_required();

        assert_eq!(error.code, "auth.required");
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
    }
}
