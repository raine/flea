#![allow(clippy::result_large_err)]

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::{
    error::{AppError, ExitClass},
    marketplace::tori::{
        auth::{AuthCredentials, AuthStart, AuthenticationApi, BrowserAuth, OAuthFlow},
        session::{CredentialRecord, CredentialStore},
    },
    storage::{
        StatePaths,
        auth_flow::{AuthFlowStore, AuthFlowStoreError},
    },
};

pub trait AuthStore: Send + Sync {
    fn save_flow(&self, flow: &OAuthFlow) -> Result<(), AppError>;
    fn load_flow(&self, flow_id: &str) -> Result<Option<OAuthFlow>, AppError>;
    fn delete_flow(&self, flow_id: &str) -> Result<(), AppError>;
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
        self.flow_store().save(&flow.into()).map_err(store_error)
    }

    fn load_flow(&self, flow_id: &str) -> Result<Option<OAuthFlow>, AppError> {
        match self.flow_store().load(flow_id, 0) {
            Ok(flow) => Ok(Some(flow.into())),
            Err(AuthFlowStoreError::NotFound) => Ok(None),
            Err(error) => Err(store_error(error)),
        }
    }

    fn delete_flow(&self, flow_id: &str) -> Result<(), AppError> {
        self.flow_store().delete(flow_id).map_err(store_error)
    }

    fn commit_credentials(
        &self,
        flow_id: &str,
        credentials: &AuthCredentials,
    ) -> Result<(), AppError> {
        self.credential_store()
            .save(&CredentialRecord::from(credentials))
            .map_err(store_error)?;
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

fn store_error(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    AppError::new(
        "auth.storage_failed",
        "authentication state could not be updated safely",
        ExitClass::Authentication,
    )
    .with_source(error)
}

pub struct ToriAuthentication<A, S> {
    auth: BrowserAuth<A>,
    store: S,
}

impl<A, S> ToriAuthentication<A, S> {
    pub fn new(api: A, store: S) -> Self {
        Self {
            auth: BrowserAuth::new(api),
            store,
        }
    }
}

impl<A: AuthenticationApi, S: AuthStore> ToriAuthentication<A, S> {
    pub(crate) fn start(&self, now_unix: u64) -> Result<AuthStart, AppError> {
        let (flow, output) = self.auth.start(now_unix)?;
        self.store.save_flow(&flow).map_err(storage_error)?;
        Ok(output)
    }

    pub(crate) async fn complete(
        &self,
        flow_id: &str,
        callback_url: &str,
        now_unix: u64,
    ) -> Result<AuthCompleteOutput, AppError> {
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
        Ok(AuthCompleteOutput {
            authenticated: true,
            user_id: account.user_id,
        })
    }

    pub(crate) fn logout(&self) -> Result<AuthLogoutOutput, AppError> {
        self.store.clear_auth().map_err(storage_error)?;
        Ok(AuthLogoutOutput {
            authenticated: false,
        })
    }
}

#[derive(Serialize)]
pub struct AuthCompleteOutput {
    authenticated: bool,
    user_id: String,
}

impl std::fmt::Debug for AuthCompleteOutput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AuthCompleteOutput")
            .field("authenticated", &self.authenticated)
            .field("user_id", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Serialize)]
pub struct AuthLogoutOutput {
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

fn flow_not_found() -> AppError {
    let mut error = AppError::new(
        "auth.flow_not_found",
        "the authentication flow does not exist",
        ExitClass::Authentication,
    );
    error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: crate::invocation::tori("auth login"),
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
            command: crate::invocation::tori("auth login"),
        });
    error
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

    use crate::marketplace::tori::auth::{AuthenticatedAccount, SecretString};

    use super::*;

    #[derive(Default)]
    struct FakeApi;

    impl AuthenticationApi for FakeApi {
        async fn exchange_authorization_code(
            &self,
            _code: &str,
            _pkce_verifier: &str,
        ) -> Result<crate::marketplace::tori::auth::SchibstedTokens, AppError> {
            Ok(
                crate::marketplace::tori::auth::SchibstedTokens::new_for_adapter(
                    "access-secret-fixture".into(),
                    "refresh-secret-fixture".into(),
                    "id-secret-fixture".into(),
                ),
            )
        }

        async fn exchange_spid_code(&self, _access_token: &str) -> Result<SecretString, AppError> {
            Ok(SecretString::new_for_adapter("spid".into()))
        }

        async fn login_to_tori(
            &self,
            _spid_code: &str,
            _id_token: Option<&str>,
            _device_id: &str,
            _installation_id: &str,
            _ab_test_device_id: &str,
        ) -> Result<crate::marketplace::tori::auth::ToriSession, AppError> {
            Ok(
                crate::marketplace::tori::auth::ToriSession::new_for_adapter(
                    AuthenticatedAccount {
                        user_id: "42".into(),
                    }
                    .user_id,
                    "bearer-secret-fixture".into(),
                ),
            )
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
    async fn logout_is_idempotent() {
        let handler = ToriAuthentication::new(FakeApi, MemoryStore::default());

        assert_eq!(
            serde_json::to_value(handler.logout().unwrap()).unwrap(),
            serde_json::json!({ "authenticated": false })
        );
        assert_eq!(
            serde_json::to_value(handler.logout().unwrap()).unwrap(),
            serde_json::json!({ "authenticated": false })
        );
    }

    #[test]
    fn start_output_contains_only_public_flow_fields() {
        let handler = ToriAuthentication::new(FakeApi, MemoryStore::default());

        let started = handler.start(1_000).unwrap();
        let document = serde_json::to_value(&started).unwrap();
        let object = document.as_object().unwrap();

        assert_eq!(object.len(), 4);
        assert!(
            object["flow_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            object["login_url"]
                .as_str()
                .is_some_and(|value| value.starts_with("https://login.vend.fi/oauth/authorize?"))
        );
        assert_eq!(object["expires_at_unix"], 1_600);
        assert_eq!(object["completion_command"], "flea tori auth login");
        let rendered = document.to_string();
        let flow = handler.store.flow.lock().unwrap();
        let flow = flow.as_ref().unwrap();
        for secret in [
            flow.pkce_verifier.expose(),
            &flow.device_id,
            &flow.installation_id,
            &flow.ab_test_device_id,
        ] {
            assert!(!rendered.contains(secret));
        }
    }

    #[tokio::test]
    async fn completion_output_contains_only_public_account_state() {
        let handler = ToriAuthentication::new(FakeApi, MemoryStore::default());
        let started = handler.start(1_000).unwrap();
        let flow_id = started.flow_id;
        let state = handler
            .store
            .flow
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .state
            .expose()
            .to_owned();
        let callback = format!(
            "{}://login?code=authorization-code&state={state}",
            crate::marketplace::tori::auth::CALLBACK_SCHEME
        );

        let completed =
            serde_json::to_value(handler.complete(&flow_id, &callback, 1_001).await.unwrap())
                .unwrap();

        assert_eq!(
            completed,
            serde_json::json!({ "authenticated": true, "user_id": "42" })
        );
        assert!(handler.store.load_flow(&flow_id).unwrap().is_none());
        let debug = format!("{:?}", handler.store.credentials.lock().unwrap());
        for secret in [
            "refresh-secret-fixture",
            "bearer-secret-fixture",
            "id-secret-fixture",
        ] {
            assert!(!debug.contains(secret));
        }
    }

    #[tokio::test]
    async fn expired_completion_deletes_sensitive_flow_material() {
        let handler = ToriAuthentication::new(FakeApi, MemoryStore::default());
        let started = handler.start(1_000).unwrap();
        let flow_id = started.flow_id;

        let error = handler
            .complete(&flow_id, "redacted", 1_600)
            .await
            .unwrap_err();

        assert_eq!(error.code, "auth.flow_expired");
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
        assert!(handler.store.load_flow(&flow_id).unwrap().is_none());
    }

    #[tokio::test]
    async fn missing_completion_restarts_public_login() {
        let handler = ToriAuthentication::new(FakeApi, MemoryStore::default());

        let error = handler
            .complete("missing", "redacted", 1_000)
            .await
            .unwrap_err();

        assert_eq!(error.code, "auth.flow_not_found");
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
    }
}
