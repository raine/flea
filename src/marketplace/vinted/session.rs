use serde::Serialize;

use crate::{
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{
        CapabilityMaturity, MarketplaceContext, PortalId,
        vinted::auth::{VintedAuthentication, VintedCredentialRecord},
    },
    storage::{StatePaths, credentials::TypedCredentialStore},
};

type VintedCredentialStore = TypedCredentialStore<VintedCredentialRecord>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AuthOperation {
    Login,
    Status,
    Logout,
}

pub(crate) enum AuthResult {
    Login(crate::marketplace::vinted::auth::VintedLoginResult),
    Status(AuthStatusResult),
    Logout(VintedLogoutOutput),
}

pub(crate) async fn execute_auth(
    portal: PortalId,
    operation: AuthOperation,
) -> Result<AuthResult, AppError> {
    if portal != PortalId::Fi {
        return Err(AppError::usage("the selected Vinted portal is unavailable"));
    }
    let paths = StatePaths::discover(MarketplaceContext::VINTED_FI)
        .map_err(|error| storage_error(error, "discover"))?;
    match operation {
        AuthOperation::Login => execute_login(paths).await.map(AuthResult::Login),
        AuthOperation::Status => execute_status(paths).await.map(AuthResult::Status),
        AuthOperation::Logout => execute_logout(paths).map(AuthResult::Logout),
    }
}

async fn execute_login(
    paths: StatePaths,
) -> Result<crate::marketplace::vinted::auth::VintedLoginResult, AppError> {
    let auth = VintedAuthentication::new();
    let (flow, start) = auth.start(unix_time_now()?)?;
    let callback = super::interactive::open_and_capture_callback(
        &paths,
        &start.login_url,
        start.expires_at_unix,
    )?;
    let completion = auth.complete(&flow, &callback, unix_time_now()?).await?;
    VintedCredentialStore::new(paths)
        .save(&completion.credentials)
        .map_err(|error| storage_error(error, "write"))?;
    Ok(completion.output)
}

pub(crate) struct AuthStatusResult {
    pub(crate) data: VintedAuthStatus,
    pub(crate) next_actions: Vec<NextAction>,
}

#[derive(Debug, Serialize)]
pub struct VintedAuthStatus {
    authenticated: bool,
    health: &'static str,
    validation: &'static str,
    refresh_maturity: CapabilityMaturity,
    refresh_performed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    login: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    access_expires_at_unix: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_in_seconds: Option<u64>,
    #[serde(skip)]
    next_actions: Vec<NextAction>,
}

const MINIMUM_ACCESS_LIFETIME_SECONDS: u64 = 30;

struct ResolvedCredentials {
    credentials: VintedCredentialRecord,
    refresh_performed: bool,
}

async fn resolve_credentials<T: crate::transport::Transport>(
    paths: StatePaths,
    auth: &VintedAuthentication<T>,
    now: u64,
) -> Result<Option<ResolvedCredentials>, AppError> {
    let store = VintedCredentialStore::new(paths);
    let locked = store
        .lock()
        .map_err(|error| vinted_auth_storage(error, "lock"))?;
    let Some(credentials) = locked
        .load()
        .map_err(|error| vinted_auth_storage(error, "read"))?
    else {
        return Ok(None);
    };
    if credentials.access_expires_at_unix.saturating_sub(now) > MINIMUM_ACCESS_LIFETIME_SECONDS {
        return Ok(Some(ResolvedCredentials {
            credentials,
            refresh_performed: false,
        }));
    }

    let refreshed = auth.refresh_credentials(&credentials, now).await?;
    locked
        .save(&refreshed)
        .map_err(|error| vinted_auth_storage(error, "write_refresh"))?;
    Ok(Some(ResolvedCredentials {
        credentials: refreshed,
        refresh_performed: true,
    }))
}

async fn execute_status(paths: StatePaths) -> Result<AuthStatusResult, AppError> {
    execute_status_with(paths, &VintedAuthentication::new(), unix_time_now()?).await
}

async fn execute_status_with<T: crate::transport::Transport>(
    paths: StatePaths,
    auth: &VintedAuthentication<T>,
    now: u64,
) -> Result<AuthStatusResult, AppError> {
    let resolved = match resolve_credentials(paths, auth, now).await {
        Ok(Some(resolved)) => resolved,
        Ok(None) => return serialize_status(unavailable_status("missing", "local_storage")),
        Err(error) => return serialize_status(refresh_failure_status(&error)),
    };
    let credentials = resolved.credentials;
    let login = if resolved.refresh_performed {
        credentials.login.clone()
    } else {
        match auth.validate_credentials(&credentials).await {
            Ok((_, login)) => login,
            Err(error) => {
                return serialize_status(validation_failure_status(&credentials, &error));
            }
        }
    };
    serialize_status(VintedAuthStatus {
        authenticated: true,
        health: if resolved.refresh_performed {
            "refreshed"
        } else {
            "valid"
        },
        validation: if resolved.refresh_performed {
            "online_refresh"
        } else {
            "online_current_user"
        },
        refresh_maturity: CapabilityMaturity::SourceDerived,
        refresh_performed: resolved.refresh_performed,
        user_id: Some(credentials.user_id),
        login,
        access_expires_at_unix: Some(credentials.access_expires_at_unix),
        expires_in_seconds: Some(credentials.access_expires_at_unix.saturating_sub(now)),
        next_actions: Vec::new(),
    })
}

#[derive(Debug, Serialize)]
pub struct VintedLogoutOutput {
    authenticated: bool,
    marketplace: &'static str,
    portal: &'static str,
}

fn execute_logout(paths: StatePaths) -> Result<VintedLogoutOutput, AppError> {
    VintedCredentialStore::new(paths)
        .delete()
        .map_err(|error| storage_error(error, "delete"))?;
    Ok(VintedLogoutOutput {
        authenticated: false,
        marketplace: "vinted",
        portal: "fi",
    })
}

fn unavailable_status(health: &'static str, validation: &'static str) -> VintedAuthStatus {
    VintedAuthStatus {
        authenticated: false,
        health,
        validation,
        refresh_maturity: CapabilityMaturity::SourceDerived,
        refresh_performed: false,
        user_id: None,
        login: None,
        access_expires_at_unix: None,
        expires_in_seconds: None,
        next_actions: vec![retry_action()],
    }
}

fn refresh_failure_status(error: &AppError) -> VintedAuthStatus {
    let rejected = error.code == "vinted_auth.refresh_rejected";
    VintedAuthStatus {
        authenticated: false,
        health: if rejected {
            "refresh_rejected"
        } else if error.code == "vinted_auth.refresh_malformed" {
            "malformed"
        } else {
            "temporarily_unavailable"
        },
        validation: "online_refresh",
        refresh_maturity: CapabilityMaturity::SourceDerived,
        refresh_performed: false,
        user_id: None,
        login: None,
        access_expires_at_unix: None,
        expires_in_seconds: None,
        next_actions: vec![if rejected {
            retry_action()
        } else {
            status_action()
        }],
    }
}

fn validation_failure_status(
    credentials: &VintedCredentialRecord,
    error: &AppError,
) -> VintedAuthStatus {
    let rejected = error.code == "vinted_auth.validation_rejected";
    VintedAuthStatus {
        authenticated: false,
        health: if rejected {
            "rejected"
        } else {
            "temporarily_unavailable"
        },
        validation: "online_current_user",
        refresh_maturity: CapabilityMaturity::SourceDerived,
        refresh_performed: false,
        user_id: Some(credentials.user_id.clone()),
        login: credentials.login.clone(),
        access_expires_at_unix: Some(credentials.access_expires_at_unix),
        expires_in_seconds: None,
        next_actions: vec![if rejected {
            retry_action()
        } else {
            NextAction {
                command: crate::invocation::vinted_fi("auth status"),
            }
        }],
    }
}

fn serialize_status(mut status: VintedAuthStatus) -> Result<AuthStatusResult, AppError> {
    let next_actions = std::mem::take(&mut status.next_actions);
    Ok(AuthStatusResult {
        data: status,
        next_actions,
    })
}

fn storage_error(
    error: impl std::error::Error + Send + Sync + 'static,
    operation: &'static str,
) -> AppError {
    let mut result = AppError::new(
        "auth.storage_failed",
        "Vinted authentication state could not be updated safely",
        ExitClass::Authentication,
    )
    .with_details(serde_json::json!({ "operation": operation }))
    .with_source(error);
    result.next_actions.push(NextAction {
        command: crate::invocation::vinted_fi("auth status"),
    });
    result
}

pub struct VintedCredentialResolver;

impl VintedCredentialResolver {
    pub const fn new() -> Self {
        Self
    }

    pub async fn credentials(&self, portal: PortalId) -> Result<VintedCredentialRecord, AppError> {
        if portal != PortalId::Fi {
            return Err(AppError::usage("the selected Vinted portal is unavailable"));
        }
        let paths = StatePaths::discover(MarketplaceContext::VINTED_FI)
            .map_err(|error| vinted_auth_storage(error, "discover"))?;
        resolve_credentials(paths, &VintedAuthentication::new(), unix_time_now()?)
            .await?
            .map(|resolved| resolved.credentials)
            .ok_or_else(vinted_auth_required)
    }
}

impl Default for VintedCredentialResolver {
    fn default() -> Self {
        Self::new()
    }
}

fn vinted_auth_storage(
    error: impl std::error::Error + Send + Sync + 'static,
    operation: &'static str,
) -> AppError {
    let mut result = AppError::new(
        "vinted_auth.storage_failed",
        "Vinted authentication credential storage is unavailable",
        ExitClass::Authentication,
    )
    .with_details(serde_json::json!({ "operation": operation }))
    .with_source(error);
    result.next_actions.push(NextAction {
        command: crate::invocation::vinted_fi("auth status"),
    });
    result
}

fn vinted_auth_required() -> AppError {
    let mut error =
        AppError::authentication("vinted_auth.required", "Vinted authentication is required");
    error.next_actions.push(NextAction {
        command: crate::invocation::vinted_fi("auth login"),
    });
    error
}

fn retry_action() -> NextAction {
    NextAction {
        command: crate::invocation::vinted_fi("auth login"),
    }
}

fn status_action() -> NextAction {
    NextAction {
        command: crate::invocation::vinted_fi("auth status"),
    }
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
        thread,
    };

    use super::*;

    fn serve(responses: Vec<(&'static str, &'static str)>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = Vec::new();
                loop {
                    let mut buffer = [0_u8; 4096];
                    let count = stream.read(&mut buffer).unwrap();
                    request.extend_from_slice(&buffer[..count]);
                    if request.windows(4).any(|part| part == b"\r\n\r\n") {
                        break;
                    }
                }
                write!(
                    stream,
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            }
        });
        (base_url, worker)
    }

    fn credentials() -> VintedCredentialRecord {
        VintedCredentialRecord {
            portal: PortalId::Fi,
            user_id: "user-1".to_owned(),
            login: Some("fixture".to_owned()),
            access_token: "access".to_owned(),
            refresh_token: "refresh".to_owned(),
            access_expires_at_unix: 2_000,
            device_uuid: "device".to_owned(),
            anonymous_id: "anonymous".to_owned(),
            user_device_token: None,
        }
    }

    #[tokio::test]
    async fn missing_status_preserves_the_exact_auth_document() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            MarketplaceContext::VINTED_FI,
        );

        let status = execute_status(paths).await.unwrap();
        let data = serde_json::to_value(&status.data).unwrap();

        assert_eq!(
            data,
            serde_json::json!({
                "authenticated": false,
                "health": "missing",
                "validation": "local_storage",
                "refresh_maturity": "source_derived",
                "refresh_performed": false,
            })
        );
        assert_eq!(
            status.next_actions[0].command,
            "flea vinted --portal fi auth login"
        );
    }

    #[tokio::test]
    async fn status_refreshes_validates_and_persists_expired_credentials() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            MarketplaceContext::VINTED_FI,
        );
        let mut expired = credentials();
        expired.access_expires_at_unix = 999;
        VintedCredentialStore::new(paths.clone())
            .save(&expired)
            .unwrap();
        let (base_url, worker) = serve(vec![
            (
                "200 OK",
                r#"{"access_token":"access-new","refresh_token":"refresh-new","token_type":"Bearer","expires_in":3600}"#,
            ),
            ("200 OK", r#"{"user":{"id":"user-1","login":"refreshed"}}"#),
        ]);
        let auth = VintedAuthentication::new().with_portal_base_url(base_url);

        let status = execute_status_with(paths.clone(), &auth, 1_000)
            .await
            .unwrap();
        worker.join().unwrap();
        let data = serde_json::to_value(&status.data).unwrap();

        assert_eq!(data["authenticated"], true);
        assert_eq!(data["health"], "refreshed");
        assert_eq!(data["validation"], "online_refresh");
        assert_eq!(data["refresh_performed"], true);
        assert_eq!(data["access_expires_at_unix"], 4_600);
        assert_eq!(data["expires_in_seconds"], 3_600);
        let persisted = VintedCredentialStore::new(paths).load().unwrap().unwrap();
        assert_eq!(persisted.access_token, "access-new");
        assert_eq!(persisted.refresh_token, "refresh-new");
        assert_eq!(persisted.login.as_deref(), Some("refreshed"));
    }

    #[tokio::test]
    async fn failed_refresh_preserves_stored_credentials() {
        let temporary = tempfile::tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            MarketplaceContext::VINTED_FI,
        );
        let mut expired = credentials();
        expired.access_expires_at_unix = 999;
        VintedCredentialStore::new(paths.clone())
            .save(&expired)
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let auth = VintedAuthentication::new().with_portal_base_url(base_url);

        let status = execute_status_with(paths.clone(), &auth, 1_000)
            .await
            .unwrap();
        let data = serde_json::to_value(&status.data).unwrap();

        assert_eq!(data["health"], "temporarily_unavailable");
        assert_eq!(
            status.next_actions[0].command,
            "flea vinted --portal fi auth status"
        );
        assert_eq!(
            VintedCredentialStore::new(paths).load().unwrap().unwrap(),
            expired
        );
    }

    #[test]
    fn status_maps_online_validation_failures_to_health_documents() {
        let rejected =
            AppError::authentication("vinted_auth.validation_rejected", "token rejected");
        let rejected = validation_failure_status(&credentials(), &rejected);
        assert_eq!(rejected.health, "rejected");
        assert_eq!(
            rejected.next_actions[0].command,
            "flea vinted --portal fi auth login"
        );

        let unavailable = AppError::upstream(
            "vinted_auth.validation_transport_failed",
            "network unavailable",
        );
        let unavailable = validation_failure_status(&credentials(), &unavailable);
        assert_eq!(unavailable.health, "temporarily_unavailable");
        assert_eq!(
            unavailable.next_actions[0].command,
            "flea vinted --portal fi auth status"
        );
    }
}
