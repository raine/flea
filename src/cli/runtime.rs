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
        Command, CommandFuture, CommandRuntime, ToriCommand, VintedCommand,
        auth::{AuthCommandHandler, FileAuthStore, unix_time_now},
        auth_callback, category, draft, favorite, listing, saved_search, vinted_search,
    },
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{
        MarketplaceContext, MarketplaceId, marketplace, marketplaces,
        tori::{
            adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
            session as tori_session,
        },
    },
    storage::StatePaths,
};

#[derive(Default)]
pub struct ProductionRuntime;

impl CommandRuntime for ProductionRuntime {
    fn execute(&self, command: Command) -> CommandFuture<'_> {
        Box::pin(async move {
            match command {
                Command::Capabilities => capabilities_output(),
                Command::Marketplaces => marketplaces_output(),
                Command::Tori(args) => execute_tori(args.command).await,
                Command::Vinted(args) => execute_vinted(args.portal, args.command).await,
                Command::Skill(args) => super::skill::dispatch(args),
                Command::Unsupported(parts) => Err(unsupported_root_command(&parts)),
            }
        })
    }
}

async fn execute_tori(command: ToriCommand) -> Result<Value, AppError> {
    match command {
        ToriCommand::Auth(args) => execute_tori_auth(args).await,
        ToriCommand::Capabilities => marketplace_capabilities(MarketplaceId::Tori),
        ToriCommand::Category(args) => {
            let client = tori_session::authenticated_client().await?;
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
                let client = tori_session::authenticated_client().await?;
                let api = HttpListingsApi::new(Arc::new(client));
                draft::execute_preview(command, Some(&api))
            }
            command => {
                let client = tori_session::authenticated_client().await?;
                let api = HttpAdInputApi::new(ClientTransport::new(client));
                draft::execute(command, api, WorkflowConfig::default()).await
            }
        },
        ToriCommand::Favorite(args) => {
            let client = tori_session::authenticated_client().await?;
            let api = HttpFavoritesApi::new(Arc::new(client));
            favorite::dispatch_with_api(args, &api).await
        }
        ToriCommand::Item(args) => {
            let api = HttpPublicItemApi::new(Arc::new(public_client()));
            super::item::dispatch_with_api(args, &api).await
        }
        ToriCommand::Listing(args) => {
            let client = tori_session::authenticated_client().await?;
            let api = HttpListingsApi::new(Arc::new(client));
            listing::dispatch_with_api(args, &api)
        }
        ToriCommand::Search(args) => {
            let search_api = HttpPublicSearchApi::new(Arc::new(public_client()));
            let item_api = HttpPublicItemApi::new(Arc::new(public_client()));
            super::search::dispatch_with_apis(*args, &search_api, Some(&item_api)).await
        }
        ToriCommand::SavedSearch(args) => {
            let client: Arc<dyn crate::api::client::ToriClient> =
                Arc::new(tori_session::authenticated_client().await?);
            let api = HttpSavedSearchesApi::new(Arc::clone(&client));
            let search_api = HttpPublicSearchApi::new(client);
            saved_search::dispatch_with_apis(*args, &api, &search_api).await
        }
        ToriCommand::Location(args) => {
            let api = HttpPublicSearchApi::new(Arc::new(public_client()));
            super::location::dispatch_with_api(args, &api).await
        }
    }
}

async fn execute_vinted(
    portal: crate::marketplace::PortalId,
    command: VintedCommand,
) -> Result<Value, AppError> {
    match command {
        VintedCommand::Auth(args) => {
            use crate::marketplace::vinted::session::{self, AuthOperation};

            let operation = match args.command {
                super::auth::AuthCommand::Login => AuthOperation::Login,
                super::auth::AuthCommand::Status => AuthOperation::Status,
                super::auth::AuthCommand::Logout => AuthOperation::Logout,
                super::auth::AuthCommand::Callback { .. } => {
                    return Err(AppError::unexpected(
                        "the Vinted callback receiver does not use the CLI callback command",
                    ));
                }
            };
            session::execute_auth(portal, operation).await
        }
        VintedCommand::Capabilities => marketplace_capabilities(MarketplaceId::Vinted),
        VintedCommand::Search(args) => {
            let credentials = crate::marketplace::vinted::session::credentials(portal)?;
            vinted_search::dispatch(args, &credentials).await
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

async fn execute_tori_auth(args: super::auth::AuthArgs) -> Result<Value, AppError> {
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
        super::auth::AuthCommand::Login => execute_interactive_login(paths).await,
        super::auth::AuthCommand::Status => tori_session::status().await,
        command => {
            let store = FileAuthStore::new(paths);
            let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(), store);
            handler.dispatch(command).await
        }
    }
}

async fn execute_interactive_login(paths: StatePaths) -> Result<Value, AppError> {
    auth_callback::prepare(&paths)?;
    let result = async {
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
        handler
            .complete(&flow_id, &callback, unix_time_now()?)
            .await
    }
    .await;
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
