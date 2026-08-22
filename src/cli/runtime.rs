use std::sync::Arc;

use serde_json::Value;

use crate::{
    api::{
        adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
        auth::SchibstedToriAuthenticationApi,
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
                let api = HttpPublicSearchApi::new(Arc::new(public_client()));
                super::search::dispatch_with_api(*args, &api)
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
    let paths = state_paths()?;
    let store = CredentialStore::new(paths);
    let locked = store.lock().map_err(auth_storage)?;
    let mut record = locked
        .load()
        .map_err(auth_storage)?
        .ok_or_else(auth_required)?;
    let now = unix_time_now()?;
    if !record.bearer_is_valid_at(now, 30) {
        let api = SchibstedToriAuthenticationApi::new();
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

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the Tokio runtime uses static configuration")
        .block_on(future)
}
