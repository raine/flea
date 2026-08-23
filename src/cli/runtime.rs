use std::{future::Future, pin::Pin, sync::Arc};

use serde_json::Value;

use crate::{
    cli::{
        Command, ToriCommand, VintedCommand,
        auth::{AuthArgs, AuthCommandHandler, FileAuthStore, unix_time_now},
        auth_callback, category, draft, favorite, listing, saved_search, vinted_search,
    },
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{
        MarketplaceContext, MarketplaceId, PortalId, marketplace, marketplaces,
        tori::{
            adinput::{ClientTransport, HttpAdInputApi, WorkflowConfig},
            auth::SchibstedToriAuthenticationApi,
            client::{ClientConfig, DeviceIdentity, HttpClient, ReqwestTransport, ToriClient},
            favorites::HttpFavoritesApi,
            item::HttpPublicItemApi,
            listings::HttpListingsApi,
            saved_searches::HttpSavedSearchesApi,
            search::HttpPublicSearchApi,
            session as tori_session,
        },
        vinted::{
            auth::VintedCredentialRecord,
            search::{VintedSearch, VintedSearchApi},
            session as vinted_session,
        },
    },
    storage::StatePaths,
};

use super::outcome::CommandOutcome;

type OutcomeFuture = Pin<Box<dyn Future<Output = Result<CommandOutcome, AppError>>>>;
type ToriClientFuture = Pin<Box<dyn Future<Output = Result<Arc<dyn ToriClient>, AppError>>>>;
type ToriAuthHandler = dyn Fn(AuthArgs) -> OutcomeFuture + Send + Sync;
type VintedAuthHandler = dyn Fn(PortalId, AuthArgs) -> OutcomeFuture + Send + Sync;
type VintedCredentialsProvider =
    dyn Fn(PortalId) -> Result<VintedCredentialRecord, AppError> + Send + Sync;

pub struct ApplicationDependencies {
    public_tori_client: Arc<dyn ToriClient>,
    authenticated_tori_client: Arc<dyn Fn() -> ToriClientFuture + Send + Sync>,
    tori_auth: Arc<ToriAuthHandler>,
    vinted_auth: Arc<VintedAuthHandler>,
    vinted_credentials: Arc<VintedCredentialsProvider>,
    vinted_search: Arc<dyn VintedSearchApi>,
}

impl ApplicationDependencies {
    pub fn production() -> Self {
        Self {
            public_tori_client: Arc::new(public_client()),
            authenticated_tori_client: Arc::new(|| {
                Box::pin(async {
                    tori_session::authenticated_client()
                        .await
                        .map(|client| Arc::new(client) as Arc<dyn ToriClient>)
                })
            }),
            tori_auth: Arc::new(|args| Box::pin(execute_tori_auth(args))),
            vinted_auth: Arc::new(|portal, args| Box::pin(execute_vinted_auth(portal, args))),
            vinted_credentials: Arc::new(vinted_session::credentials),
            vinted_search: Arc::new(VintedSearch::new()),
        }
    }

    pub fn with_tori_client(mut self, client: Arc<dyn ToriClient>) -> Self {
        self.public_tori_client = Arc::clone(&client);
        self.authenticated_tori_client = Arc::new(move || {
            let client = Arc::clone(&client);
            Box::pin(async move { Ok(client) })
        });
        self
    }

    pub fn with_tori_auth_handler<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(AuthArgs) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutcome, AppError>> + 'static,
    {
        self.tori_auth = Arc::new(move |args| Box::pin(handler(args)));
        self
    }

    pub fn with_vinted_auth_handler<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(PortalId, AuthArgs) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutcome, AppError>> + 'static,
    {
        self.vinted_auth = Arc::new(move |portal, args| Box::pin(handler(portal, args)));
        self
    }

    pub fn with_vinted_credentials_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn(PortalId) -> Result<VintedCredentialRecord, AppError> + Send + Sync + 'static,
    {
        self.vinted_credentials = Arc::new(provider);
        self
    }

    pub fn with_vinted_search_api(mut self, api: Arc<dyn VintedSearchApi>) -> Self {
        self.vinted_search = api;
        self
    }

    async fn authenticated_tori_client(&self) -> Result<Arc<dyn ToriClient>, AppError> {
        (self.authenticated_tori_client)().await
    }
}

pub async fn dispatch(
    command: Command,
    dependencies: &ApplicationDependencies,
) -> Result<CommandOutcome, AppError> {
    match command {
        Command::Capabilities => capabilities_output().map(Into::into),
        Command::Marketplaces => marketplaces_output().map(Into::into),
        Command::Tori(args) => execute_tori(args.command, dependencies).await,
        Command::Vinted(args) => execute_vinted(args.portal, args.command, dependencies).await,
        Command::Skill(args) => super::skill::dispatch(args).map(Into::into),
        Command::Unsupported(parts) => Err(unsupported_root_command(&parts)),
    }
}

async fn execute_tori(
    command: ToriCommand,
    dependencies: &ApplicationDependencies,
) -> Result<CommandOutcome, AppError> {
    match command {
        ToriCommand::Auth(args) => (dependencies.tori_auth)(args).await,
        ToriCommand::Capabilities => marketplace_capabilities(MarketplaceId::Tori).map(Into::into),
        ToriCommand::Category(args) => {
            let client = dependencies.authenticated_tori_client().await?;
            let api = HttpListingsApi::new(client);
            category::dispatch(args, &api).await
        }
        ToriCommand::Draft(args) => match args.command {
            command @ super::draft::DraftCommand::Preview {
                verify_category: false,
                ..
            } => draft::execute_preview(command, None).await,
            command @ super::draft::DraftCommand::Preview {
                verify_category: true,
                ..
            } => {
                let client = dependencies.authenticated_tori_client().await?;
                let api = HttpListingsApi::new(client);
                draft::execute_preview(command, Some(&api)).await
            }
            command => {
                let client = dependencies.authenticated_tori_client().await?;
                let api = HttpAdInputApi::new(ClientTransport::new(client));
                draft::execute(command, api, WorkflowConfig::default()).await
            }
        },
        ToriCommand::Favorite(args) => {
            let client = dependencies.authenticated_tori_client().await?;
            let api = HttpFavoritesApi::new(client);
            favorite::dispatch(args, &api).await
        }
        ToriCommand::Item(args) => {
            let api = HttpPublicItemApi::new(Arc::clone(&dependencies.public_tori_client));
            super::item::dispatch(args, &api).await
        }
        ToriCommand::Listing(args) => {
            let client = dependencies.authenticated_tori_client().await?;
            let api = HttpListingsApi::new(client);
            listing::dispatch(args, &api).await
        }
        ToriCommand::Search(args) => {
            let search_api = HttpPublicSearchApi::new(Arc::clone(&dependencies.public_tori_client));
            let item_api = HttpPublicItemApi::new(Arc::clone(&dependencies.public_tori_client));
            super::search::dispatch(*args, &search_api, Some(&item_api)).await
        }
        ToriCommand::SavedSearch(args) => {
            let client = dependencies.authenticated_tori_client().await?;
            let api = HttpSavedSearchesApi::new(Arc::clone(&client));
            let search_api = HttpPublicSearchApi::new(client);
            saved_search::dispatch(*args, &api, &search_api).await
        }
        ToriCommand::Location(args) => {
            let api = HttpPublicSearchApi::new(Arc::clone(&dependencies.public_tori_client));
            super::location::dispatch(args, &api).await.map(Into::into)
        }
    }
}

async fn execute_vinted_auth(portal: PortalId, args: AuthArgs) -> Result<CommandOutcome, AppError> {
    let operation = match args.command {
        super::auth::AuthCommand::Login => vinted_session::AuthOperation::Login,
        super::auth::AuthCommand::Status => vinted_session::AuthOperation::Status,
        super::auth::AuthCommand::Logout => vinted_session::AuthOperation::Logout,
        super::auth::AuthCommand::Callback { .. } => {
            return Err(AppError::unexpected(
                "the Vinted callback receiver does not use the CLI callback command",
            ));
        }
    };
    vinted_session::execute_auth(portal, operation).await
}

async fn execute_vinted(
    portal: PortalId,
    command: VintedCommand,
    dependencies: &ApplicationDependencies,
) -> Result<CommandOutcome, AppError> {
    match command {
        VintedCommand::Auth(args) => (dependencies.vinted_auth)(portal, args).await,
        VintedCommand::Capabilities => {
            marketplace_capabilities(MarketplaceId::Vinted).map(Into::into)
        }
        VintedCommand::Search(args) => {
            let credentials = (dependencies.vinted_credentials)(portal)?;
            vinted_search::dispatch(args, &credentials, dependencies.vinted_search.as_ref())
                .await
                .map(Into::into)
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

async fn execute_tori_auth(
    args: super::auth::AuthArgs,
) -> Result<super::outcome::CommandOutcome, AppError> {
    let command = match args.command {
        super::auth::AuthCommand::Callback {
            state_root,
            callback_url,
        } => {
            return auth_callback::capture(
                &StatePaths::from_root(state_root, MarketplaceContext::TORI_FI),
                &callback_url,
            )
            .map(Into::into);
        }
        command => command,
    };
    let paths = tori_session::state_paths()?;
    match command {
        super::auth::AuthCommand::Login => execute_interactive_login(paths).await.map(Into::into),
        super::auth::AuthCommand::Status => tori_session::status().await,
        command => {
            let store = FileAuthStore::new(paths);
            let handler = AuthCommandHandler::new(SchibstedToriAuthenticationApi::new(), store);
            handler.dispatch(command).await.map(Into::into)
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
