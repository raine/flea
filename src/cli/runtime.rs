use std::sync::Arc;

use serde_json::Value;

use crate::{
    api::{
        adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
        auth::SchibstedToriAuthenticationApi,
        client::{ClientConfig, DeviceIdentity, HttpClient, ReqwestTransport},
        listings::HttpListingsApi,
    },
    cli::{
        Command, CommandRuntime,
        auth::{AuthCommandHandler, FileAuthStore, unix_time_now},
        category, draft, listing,
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
            Command::Listing(args) => {
                let client = authenticated_client()?;
                let api = HttpListingsApi::new(Arc::new(client));
                listing::dispatch_with_api(args, &api)
            }
        }
    }
}

fn execute_auth(args: super::auth::AuthArgs) -> Result<Value, AppError> {
    let paths = state_paths()?;
    let store = FileAuthStore::new(paths);
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| upstream("failed to initialize authentication HTTP client", error))?;
    let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(client), store);
    block_on(handler.dispatch(args.command, unix_time_now()?))
}

fn authenticated_client() -> Result<HttpClient<ReqwestTransport>, AppError> {
    let paths = state_paths()?;
    let store = CredentialStore::new(paths);
    let locked = store.lock().map_err(auth_storage)?;
    let mut record = locked
        .load()
        .map_err(auth_storage)?
        .ok_or_else(auth_required)?;
    let now = unix_time_now()?;
    if !record.bearer_is_valid_at(now, 30) {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|error| upstream("failed to initialize authentication HTTP client", error))?;
        let api = SchibstedToriAuthenticationApi::new(client);
        let credentials = block_on(api.refresh_credentials(
            &record.refresh_token,
            &record.device_id,
            &record.installation_id,
            &record.ab_test_device_id,
            now,
        ))?;
        record = serde_json::to_value(credentials)
            .and_then(serde_json::from_value)
            .map_err(|error| {
                AppError::unexpected("authentication state types are incompatible")
                    .with_source(error)
            })?;
        locked.save(&record).map_err(auth_storage)?;
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
    StatePaths::discover().map_err(auth_storage)
}

fn auth_storage(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    AppError::new(
        "auth.storage_failed",
        "authentication state could not be read safely",
        ExitClass::Authentication,
    )
    .with_source(error)
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

fn upstream(
    message: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> AppError {
    AppError::upstream("upstream.client_initialization_failed", message).with_source(source)
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the Tokio runtime uses static configuration")
        .block_on(future)
}
