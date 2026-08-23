use std::sync::Arc;

use serde_json::Value;

use crate::{
    api::{
        auth::SchibstedToriAuthenticationApi,
        client::{ClientConfig, DeviceIdentity, HttpClient, ReqwestTransport},
        favorites::HttpFavoritesApi,
        item::HttpPublicItemApi,
        listings::HttpListingsApi,
        saved_searches::HttpSavedSearchesApi,
        search::HttpPublicSearchApi,
    },
    cli::{
        Command, CommandRuntime, ToriCommand, VintedCommand,
        auth::{AuthCommandHandler, FileAuthStore, unix_time_now},
        auth_callback, category, draft, favorite, listing, saved_search, vinted_search,
    },
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{
        MarketplaceContext, MarketplaceId, PortalId, marketplace, marketplaces,
        tori::{
            adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
            session as tori_session,
        },
    },
    storage::{
        StatePaths,
        credentials::{VintedCredentialRecord, VintedCredentialStore},
    },
};

#[derive(Default)]
pub struct ProductionRuntime;

impl CommandRuntime for ProductionRuntime {
    fn execute(&self, command: Command) -> Result<Value, AppError> {
        match command {
            Command::Capabilities => capabilities_output(),
            Command::Marketplaces => marketplaces_output(),
            Command::Tori(args) => execute_tori(args.command),
            Command::Vinted(args) => execute_vinted(args.portal, args.command),
            Command::Skill(args) => super::skill::dispatch(args),
            Command::Unsupported(parts) => Err(unsupported_root_command(&parts)),
        }
    }
}

fn execute_tori(command: ToriCommand) -> Result<Value, AppError> {
    match command {
        ToriCommand::Auth(args) => execute_tori_auth(args),
        ToriCommand::Capabilities => marketplace_capabilities(MarketplaceId::Tori),
        ToriCommand::Category(args) => {
            let client = tori_session::authenticated_client()?;
            let api = HttpListingsApi::new(Arc::new(client));
            category::dispatch_with_api(args, &api)
        }
        ToriCommand::Draft(args) => match args.command {
            command @ super::draft::DraftCommand::Preview {
                verify_category: false,
                ..
            } => draft::execute_preview(command, None),
            command @ super::draft::DraftCommand::Preview {
                verify_category: true,
                ..
            } => {
                let client = tori_session::authenticated_client()?;
                let api = HttpListingsApi::new(Arc::new(client));
                draft::execute_preview(command, Some(&api))
            }
            command => {
                let client = tori_session::authenticated_client()?;
                let api = HttpAdInputApi::new(ClientTransport::new(client));
                block_on(draft::execute(command, api, WorkflowConfig::default()))
            }
        },
        ToriCommand::Favorite(args) => {
            let client = tori_session::authenticated_client()?;
            let api = HttpFavoritesApi::new(Arc::new(client));
            favorite::dispatch_with_api(args, &api)
        }
        ToriCommand::Item(args) => {
            let api = HttpPublicItemApi::new(Arc::new(public_client()));
            super::item::dispatch_with_api(args, &api)
        }
        ToriCommand::Listing(args) => {
            let client = tori_session::authenticated_client()?;
            let api = HttpListingsApi::new(Arc::new(client));
            listing::dispatch_with_api(args, &api)
        }
        ToriCommand::Search(args) => {
            let search_api = HttpPublicSearchApi::new(Arc::new(public_client()));
            let item_api = HttpPublicItemApi::new(Arc::new(public_client()));
            super::search::dispatch_with_apis(*args, &search_api, Some(&item_api))
        }
        ToriCommand::SavedSearch(args) => {
            let client: Arc<dyn crate::api::client::ToriClient> =
                Arc::new(tori_session::authenticated_client()?);
            let api = HttpSavedSearchesApi::new(Arc::clone(&client));
            let search_api = HttpPublicSearchApi::new(client);
            saved_search::dispatch_with_apis(*args, &api, &search_api)
        }
        ToriCommand::Location(args) => {
            let api = HttpPublicSearchApi::new(Arc::new(public_client()));
            super::location::dispatch_with_api(args, &api)
        }
    }
}

fn execute_vinted(
    portal: crate::marketplace::PortalId,
    command: VintedCommand,
) -> Result<Value, AppError> {
    match command {
        VintedCommand::Auth(args) => {
            crate::marketplace::vinted::interactive::execute_command(portal, args)
        }
        VintedCommand::Capabilities => marketplace_capabilities(MarketplaceId::Vinted),
        VintedCommand::Search(args) => {
            let credentials = vinted_credentials(portal)?;
            block_on(vinted_search::dispatch(args, &credentials))
        }
        VintedCommand::Unsupported(parts) => Err(capability_unavailable(
            MarketplaceContext::VINTED_FI,
            first_external_command(&parts),
        )),
    }
}

fn capabilities_output() -> Result<Value, AppError> {
    Ok(serde_json::json!({ "marketplaces": marketplaces() }))
}

fn marketplaces_output() -> Result<Value, AppError> {
    let configured = marketplaces()
        .iter()
        .map(|descriptor| {
            serde_json::json!({
                "marketplace": descriptor.marketplace,
                "portals": descriptor.portals,
            })
        })
        .collect::<Vec<_>>();
    Ok(serde_json::json!({ "marketplaces": configured }))
}

fn marketplace_capabilities(marketplace_id: MarketplaceId) -> Result<Value, AppError> {
    let descriptor = marketplace(marketplace_id);
    Ok(serde_json::json!({
        "marketplace": descriptor.marketplace,
        "portals": descriptor.portals,
        "capabilities": descriptor.capabilities,
    }))
}

fn unsupported_root_command(parts: &[std::ffi::OsString]) -> AppError {
    let command = first_external_command(parts);
    if !matches!(
        command,
        "auth"
            | "category"
            | "draft"
            | "favorite"
            | "item"
            | "listing"
            | "location"
            | "saved-search"
            | "search"
    ) {
        return AppError::usage("the root command is not recognized")
            .with_details(serde_json::json!({ "command": command }));
    }
    let mut error = AppError::new(
        "marketplace.required",
        "marketplace commands require an explicit marketplace",
        ExitClass::Usage,
    )
    .with_details(serde_json::json!({ "command": command }));
    error.next_actions.push(NextAction {
        command: crate::invocation::marketplaces(),
    });
    error
}

fn capability_unavailable(context: MarketplaceContext, command: &str) -> AppError {
    let mut error = AppError::new(
        "marketplace.capability_unavailable",
        "the selected marketplace does not provide this capability",
        ExitClass::Usage,
    )
    .with_details(serde_json::json!({
        "context": context,
        "command": command,
    }));
    error.next_actions.push(NextAction {
        command: crate::invocation::capabilities(context),
    });
    error
}

fn first_external_command(parts: &[std::ffi::OsString]) -> &str {
    parts
        .first()
        .and_then(|part| part.to_str())
        .filter(|part| !part.is_empty() && part.len() <= 64 && !part.chars().any(char::is_control))
        .unwrap_or("unknown")
}

fn execute_tori_auth(args: super::auth::AuthArgs) -> Result<Value, AppError> {
    let command = match args.command {
        super::auth::AuthCommand::Callback {
            state_root,
            callback_url,
        } => {
            return auth_callback::capture(
                &StatePaths::from_root(state_root, MarketplaceContext::TORI_FI),
                &callback_url,
            );
        }
        command => command,
    };
    let paths = tori_session::state_paths()?;
    match command {
        super::auth::AuthCommand::Login => execute_interactive_login(paths),
        super::auth::AuthCommand::Status => tori_session::status(),
        command => {
            let store = FileAuthStore::new(paths);
            let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(), store);
            block_on(handler.dispatch(command))
        }
    }
}

fn execute_interactive_login(paths: StatePaths) -> Result<Value, AppError> {
    auth_callback::prepare(&paths)?;
    let result = (|| {
        let store = FileAuthStore::new(paths.clone());
        let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(), store);
        let started = handler.start(unix_time_now()?)?;
        let flow_id = auth_value(&started, "flow_id")?.to_owned();
        let login_url = auth_value(&started, "login_url")?;
        let expires_at_unix = started
            .get("expires_at_unix")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                AppError::unexpected("authentication start returned an invalid expiry")
            })?;
        let callback = auth_callback::open_and_wait(&paths, login_url, expires_at_unix)?;
        block_on(handler.complete(&flow_id, &callback, unix_time_now()?))
    })();
    let cleared = auth_callback::clear(&paths);
    match result {
        Ok(value) => cleared.map(|()| value),
        Err(error) => Err(error),
    }
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

fn vinted_credentials(portal: PortalId) -> Result<VintedCredentialRecord, AppError> {
    if portal != PortalId::Fi {
        return Err(AppError::usage("the selected Vinted portal is unavailable"));
    }
    let paths = StatePaths::discover(MarketplaceContext::VINTED_FI)
        .map_err(|error| vinted_auth_storage(error, "discover"))?;
    let credentials = VintedCredentialStore::new(paths)
        .load()
        .map_err(|error| vinted_auth_storage(error, "read"))?
        .ok_or_else(vinted_auth_required)?;
    if credentials.access_expires_at_unix <= unix_time_now()? {
        return Err(vinted_auth_required());
    }
    Ok(credentials)
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

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("the Tokio runtime uses static configuration")
        .block_on(future)
}
