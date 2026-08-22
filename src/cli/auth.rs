#![allow(clippy::result_large_err)]

use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use crate::{
    api::auth::{AuthCredentials, AuthenticationApi, BrowserAuth, OAuthFlow},
    error::{AppError, ExitClass},
    storage::{
        StatePaths,
        auth_flow::{AuthFlowStore, AuthFlowStoreError},
        credentials::{CredentialRecord, CredentialStore},
    },
};

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Subcommand)]
pub enum AuthCommand {
    #[command(
        about = "Start browser authentication",
        long_about = "Create a short-lived OAuth flow and print the login URL and exact completion command."
    )]
    Start,
    #[command(
        about = "Complete browser authentication",
        long_about = "Validate an OAuth callback for a saved flow, exchange its code, and store Tori credentials."
    )]
    Complete {
        /// Flow identifier returned by `tori auth start`.
        flow_id: String,
        /// Full callback URL received after browser authentication.
        ///
        /// On macOS this is read from the Tori CLI Auth receiver when omitted.
        callback_url: Option<String>,
    },
    #[command(
        about = "Show authentication status",
        long_about = "Report whether Tori credentials are stored and identify the authenticated account when available."
    )]
    Status,
    #[command(
        about = "Clear authentication state",
        long_about = "Remove stored Tori credentials and any incomplete OAuth flows."
    )]
    Logout,
}

impl std::fmt::Debug for AuthCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Start => formatter.write_str("Start"),
            Self::Complete { flow_id, .. } => formatter
                .debug_struct("Complete")
                .field("flow_id", flow_id)
                .field("callback_url", &"<redacted>")
                .finish(),
            Self::Status => formatter.write_str("Status"),
            Self::Logout => formatter.write_str("Logout"),
        }
    }
}

pub trait AuthStore: Send + Sync {
    fn save_flow(&self, flow: &OAuthFlow) -> Result<(), AppError>;
    fn load_flow(&self, flow_id: &str) -> Result<Option<OAuthFlow>, AppError>;
    fn delete_flow(&self, flow_id: &str) -> Result<(), AppError>;
    fn load_credentials(&self) -> Result<Option<AuthCredentials>, AppError>;

    /// Stores credentials and removes their OAuth flow as one recoverable transaction.
    fn commit_credentials(
        &self,
        flow_id: &str,
        credentials: &AuthCredentials,
    ) -> Result<(), AppError>;

    /// Removes credentials and temporary OAuth flows without failing when they are absent.
    fn clear_auth(&self) -> Result<(), AppError>;
}

pub struct FileAuthStore {
    paths: StatePaths,
}

impl FileAuthStore {
    pub fn new(paths: StatePaths) -> Self {
        Self { paths }
    }

    fn flow_store(&self) -> AuthFlowStore {
        AuthFlowStore::new(self.paths.clone())
    }

    fn credential_store(&self) -> CredentialStore {
        CredentialStore::new(self.paths.clone())
    }
}

impl AuthStore for FileAuthStore {
    fn save_flow(&self, flow: &OAuthFlow) -> Result<(), AppError> {
        self.flow_store().save(&convert(flow)?).map_err(store_error)
    }

    fn load_flow(&self, flow_id: &str) -> Result<Option<OAuthFlow>, AppError> {
        match self.flow_store().load(flow_id, 0) {
            Ok(flow) => convert(&flow).map(Some),
            Err(AuthFlowStoreError::NotFound) => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }

    fn delete_flow(&self, flow_id: &str) -> Result<(), AppError> {
        self.flow_store().delete(flow_id).map_err(store_error)
    }

    fn load_credentials(&self) -> Result<Option<AuthCredentials>, AppError> {
        self.credential_store()
            .load()
            .map_err(store_error)?
            .map(|record| convert(&record))
            .transpose()
    }

    fn commit_credentials(
        &self,
        flow_id: &str,
        credentials: &AuthCredentials,
    ) -> Result<(), AppError> {
        let record: CredentialRecord = convert(credentials)?;
        self.credential_store().save(&record).map_err(store_error)?;
        self.flow_store().delete(flow_id).map_err(store_error)
    }

    fn clear_auth(&self) -> Result<(), AppError> {
        self.credential_store().delete().map_err(store_error)?;
        self.paths.ensure().map_err(store_error)?;
        for entry in std::fs::read_dir(self.paths.flows_dir()).map_err(store_error)? {
            let entry = entry.map_err(store_error)?;
            if entry.file_type().map_err(store_error)?.is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "json")
            {
                std::fs::remove_file(entry.path()).map_err(store_error)?;
            }
        }
        Ok(())
    }
}

fn convert<T: Serialize, U: serde::de::DeserializeOwned>(value: &T) -> Result<U, AppError> {
    serde_json::to_value(value)
        .and_then(serde_json::from_value)
        .map_err(|error| {
            AppError::unexpected("authentication state types are incompatible").with_source(error)
        })
}

fn store_error(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    AppError::new(
        "auth.storage_failed",
        "authentication state could not be updated safely",
        ExitClass::Authentication,
    )
    .with_source(error)
}

pub struct AuthCommandHandler<A, S> {
    auth: BrowserAuth<A>,
    store: S,
}

impl<A, S> AuthCommandHandler<A, S> {
    pub fn new(api: A, store: S) -> Self {
        Self {
            auth: BrowserAuth::new(api),
            store,
        }
    }
}

impl<A: AuthenticationApi, S: AuthStore> AuthCommandHandler<A, S> {
    pub async fn dispatch(&self, command: AuthCommand, now_unix: u64) -> Result<Value, AppError> {
        match command {
            AuthCommand::Start => self.start(now_unix),
            AuthCommand::Complete {
                flow_id,
                callback_url,
            } => {
                let callback_url = callback_url.ok_or_else(callback_not_captured)?;
                self.complete(&flow_id, &callback_url, now_unix).await
            }
            AuthCommand::Status => self.status(),
            AuthCommand::Logout => self.logout(),
        }
    }

    fn start(&self, now_unix: u64) -> Result<Value, AppError> {
        let (flow, output) = self.auth.start(now_unix)?;
        self.store.save_flow(&flow).map_err(storage_error)?;
        serialize(output)
    }

    async fn complete(
        &self,
        flow_id: &str,
        callback_url: &str,
        now_unix: u64,
    ) -> Result<Value, AppError> {
        let flow = self
            .store
            .load_flow(flow_id)
            .map_err(storage_error)?
            .ok_or_else(flow_not_found)?;
        if now_unix >= flow.expires_at_unix {
            self.store.delete_flow(flow_id).map_err(storage_error)?;
            return Err(flow_expired());
        }

        let (credentials, account) = self.auth.complete(&flow, callback_url, now_unix).await?;
        self.store
            .commit_credentials(flow_id, &credentials)
            .map_err(storage_error)?;
        serialize(AuthCompleteOutput {
            authenticated: true,
            user_id: account.user_id,
        })
    }

    fn status(&self) -> Result<Value, AppError> {
        let output = match self.store.load_credentials().map_err(storage_error)? {
            Some(credentials) => AuthStatusOutput {
                authenticated: true,
                user_id: Some(credentials.user_id),
            },
            None => AuthStatusOutput {
                authenticated: false,
                user_id: None,
            },
        };
        serialize(output)
    }

    fn logout(&self) -> Result<Value, AppError> {
        self.store.clear_auth().map_err(storage_error)?;
        serialize(AuthLogoutOutput {
            authenticated: false,
        })
    }
}

#[derive(Serialize)]
struct AuthCompleteOutput {
    authenticated: bool,
    user_id: String,
}

#[derive(Serialize)]
struct AuthStatusOutput {
    authenticated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    user_id: Option<String>,
}

#[derive(Serialize)]
struct AuthLogoutOutput {
    authenticated: bool,
}

pub fn unix_time_now() -> Result<u64, AppError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| {
            AppError::new(
                "auth.clock_invalid",
                "the system clock is invalid",
                ExitClass::Authentication,
            )
        })
}

fn serialize<T: Serialize>(value: T) -> Result<Value, AppError> {
    serde_json::to_value(value)
        .map_err(|error| AppError::output("failed to serialize auth output").with_source(error))
}

fn flow_not_found() -> AppError {
    let mut error = AppError::new(
        "auth.flow_not_found",
        "the authentication flow does not exist",
        ExitClass::Authentication,
    );
    error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: "tori auth start".to_owned(),
        });
    error
}

fn flow_expired() -> AppError {
    let mut error = AppError::new(
        "auth.flow_expired",
        "the authentication flow expired",
        ExitClass::Authentication,
    );
    error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: "tori auth start".to_owned(),
        });
    error
}

fn callback_not_captured() -> AppError {
    AppError::new(
        "auth.callback_not_captured",
        "the browser authentication callback has not been captured",
        ExitClass::Authentication,
    )
}

fn storage_error(source: AppError) -> AppError {
    AppError::new(
        "auth.storage_failed",
        "authentication state could not be updated safely",
        ExitClass::Authentication,
    )
    .with_source(source)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use crate::api::auth::{AuthenticatedAccount, SecretString};

    use super::*;

    #[derive(Default)]
    struct FakeApi;

    impl AuthenticationApi for FakeApi {
        async fn exchange_authorization_code(
            &self,
            _code: &str,
            _pkce_verifier: &str,
        ) -> Result<crate::api::auth::SchibstedTokens, AppError> {
            Ok(crate::api::auth::SchibstedTokens::new_for_adapter(
                "access".into(),
                "refresh".into(),
                "id".into(),
            ))
        }

        async fn exchange_spid_code(&self, _access_token: &str) -> Result<SecretString, AppError> {
            Ok(SecretString::new_for_adapter("spid".into()))
        }

        async fn login_to_tori(
            &self,
            _spid_code: &str,
            _id_token: &str,
            _device_id: &str,
            _installation_id: &str,
            _ab_test_device_id: &str,
        ) -> Result<crate::api::auth::ToriSession, AppError> {
            Ok(crate::api::auth::ToriSession::new_for_adapter(
                AuthenticatedAccount {
                    user_id: "42".into(),
                }
                .user_id,
                "bearer".into(),
            ))
        }
    }

    #[derive(Default)]
    struct MemoryStore {
        flow: Mutex<Option<OAuthFlow>>,
        credentials: Mutex<Option<AuthCredentials>>,
    }

    impl AuthStore for MemoryStore {
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

        fn delete_flow(&self, flow_id: &str) -> Result<(), AppError> {
            if self
                .flow
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|flow| flow.flow_id == flow_id)
            {
                *self.flow.lock().unwrap() = None;
            }
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
            *self.credentials.lock().unwrap() = None;
            *self.flow.lock().unwrap() = None;
            Ok(())
        }
    }

    #[tokio::test]
    async fn status_and_logout_are_idempotent_and_secret_free() {
        let handler = AuthCommandHandler::new(FakeApi, MemoryStore::default());
        let initial = handler.dispatch(AuthCommand::Status, 1_000).await.unwrap();
        assert_eq!(initial, serde_json::json!({ "authenticated": false }));

        let started = handler.dispatch(AuthCommand::Start, 1_000).await.unwrap();
        let flow_id = started["flow_id"].as_str().unwrap().to_owned();
        let flow = handler.store.load_flow(&flow_id).unwrap().unwrap();
        let callback = format!(
            "{}://login?code=code&state={}",
            crate::api::auth::CALLBACK_SCHEME,
            flow.state_for_adapter()
        );
        handler
            .dispatch(
                AuthCommand::Complete {
                    flow_id,
                    callback_url: Some(callback),
                },
                1_001,
            )
            .await
            .unwrap();

        let status = handler.dispatch(AuthCommand::Status, 1_001).await.unwrap();
        assert_eq!(
            status,
            serde_json::json!({ "authenticated": true, "user_id": "42" })
        );
        assert!(!status.to_string().contains("bearer"));
        assert!(!status.to_string().contains("refresh"));

        assert_eq!(
            handler.dispatch(AuthCommand::Logout, 1_001).await.unwrap(),
            serde_json::json!({ "authenticated": false })
        );
        assert_eq!(
            handler.dispatch(AuthCommand::Logout, 1_001).await.unwrap(),
            serde_json::json!({ "authenticated": false })
        );
    }

    #[tokio::test]
    async fn expired_completion_deletes_sensitive_flow_material() {
        let handler = AuthCommandHandler::new(FakeApi, MemoryStore::default());
        let started = handler.dispatch(AuthCommand::Start, 1_000).await.unwrap();
        let flow_id = started["flow_id"].as_str().unwrap().to_owned();

        let error = handler
            .dispatch(
                AuthCommand::Complete {
                    flow_id: flow_id.clone(),
                    callback_url: Some("redacted".into()),
                },
                1_600,
            )
            .await
            .unwrap_err();

        assert_eq!(error.code, "auth.flow_expired");
        assert!(handler.store.load_flow(&flow_id).unwrap().is_none());
    }

    #[test]
    fn complete_debug_redacts_callback() {
        let command = AuthCommand::Complete {
            flow_id: "flow".into(),
            callback_url: Some("sensitive-callback".into()),
        };
        let rendered = format!("{command:?}");
        assert!(!rendered.contains("sensitive-callback"));
        assert!(rendered.contains("<redacted>"));
    }
}
