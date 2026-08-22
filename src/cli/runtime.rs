use std::{fs, io::Read, path::PathBuf, sync::Arc};

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

const MAX_CAPTURED_CALLBACK_BYTES: u64 = 8 * 1024;

fn execute_auth(mut args: super::auth::AuthArgs) -> Result<Value, AppError> {
    let uses_captured_callback = match &mut args.command {
        super::auth::AuthCommand::Start => {
            clear_captured_callback()?;
            false
        }
        super::auth::AuthCommand::Complete { callback_url, .. } if callback_url.is_none() => {
            *callback_url = Some(read_captured_callback()?);
            true
        }
        _ => false,
    };

    let paths = state_paths()?;
    let store = FileAuthStore::new(paths);
    let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(), store);
    let result = block_on(handler.dispatch(args.command, unix_time_now()?));
    if uses_captured_callback {
        let _ = clear_captured_callback();
    }
    result
}

fn callback_capture_path() -> PathBuf {
    std::env::temp_dir().join("tori_auth_callback.txt")
}

fn clear_captured_callback() -> Result<(), AppError> {
    match fs::remove_file(callback_capture_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(callback_capture_error().with_source(error)),
    }
}

fn read_captured_callback() -> Result<String, AppError> {
    let path = callback_capture_path();
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| callback_capture_error().with_source(error))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_CAPTURED_CALLBACK_BYTES
    {
        return Err(callback_capture_error());
    }

    let mut callback = String::new();
    fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_CAPTURED_CALLBACK_BYTES + 1)
                .read_to_string(&mut callback)
        })
        .map_err(|error| callback_capture_error().with_source(error))?;
    if callback.len() as u64 > MAX_CAPTURED_CALLBACK_BYTES {
        return Err(callback_capture_error());
    }
    let callback = callback.trim();
    if callback.is_empty() {
        return Err(callback_capture_error());
    }
    Ok(callback.to_owned())
}

fn callback_capture_error() -> AppError {
    AppError::authentication(
        "auth.callback_not_captured",
        "finish browser sign-in and allow the browser to open ToriAuthHelper.app, then retry the completion command",
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
