use std::{future::Future, pin::Pin, sync::Arc};

use crate::{
    cli::{
        Command, ToriCommand, VintedCommand,
        auth::{ToriAuthArgs, ToriAuthCommand, VintedAuthArgs, VintedAuthCommand},
        category, draft, favorite, listing, saved_search, vinted_category, vinted_publish,
    },
    domain::envelope::NextAction,
    error::{AppError, ExitClass},
    marketplace::{
        MarketplaceContext, MarketplaceId, PortalId, marketplace, marketplaces,
        tori::{
            adinput::{HttpAdInputApi, WorkflowConfig},
            auth::SchibstedToriAuthenticationApi,
            client::{ClientConfig, DeviceIdentity, HttpClient, ToriClient},
            favorites::HttpFavoritesApi,
            interactive as tori_interactive,
            item::HttpPublicItemApi,
            listings::HttpListingsApi,
            login::{FileAuthStore, ToriAuthentication, unix_time_now},
            saved_searches::HttpSavedSearchesApi,
            search::HttpPublicSearchApi,
            session as tori_session,
        },
        vinted::{
            auth::VintedCredentialRecord,
            draft::{HttpVintedDraftApi, VintedDraftApi},
            item::{
                HttpVintedItemApi, VintedItemApi, VintedItemRequest, VintedItemResult,
                VintedItemSession, VintedItems,
            },
            listing::{
                HttpVintedListingApi, VintedListingApi, VintedListingRequest, VintedListingResult,
                VintedListings,
            },
            publication::{HttpVintedPublicationApi, VintedPublicationApi},
            publication_discovery::{
                HttpVintedPublicationDiscoveryApi, VintedPublicationDiscoveryApi,
            },
            readiness::{HttpVintedReadinessApi, VintedReadinessApi},
            search::{
                HttpVintedSearchApi, SearchResult as VintedSearchResult, VintedSearch,
                VintedSearchApi, VintedSearchSession,
            },
            session as vinted_session,
        },
    },
    storage::StatePaths,
    transport::ReqwestTransport,
};

use super::outcome::{
    CapabilitiesOutput, CommandData, CommandOutcome, MarketplaceCapabilitiesOutput,
    MarketplaceSummary, MarketplacesOutput,
};

type OutcomeFuture = Pin<Box<dyn Future<Output = Result<CommandOutcome, AppError>>>>;
type ToriClientFuture = Pin<Box<dyn Future<Output = Result<Arc<dyn ToriClient>, AppError>>>>;
type ToriAuthHandler = dyn Fn(ToriAuthArgs) -> OutcomeFuture + Send + Sync;
type VintedAuthHandler = dyn Fn(PortalId, VintedAuthArgs) -> OutcomeFuture + Send + Sync;

pub struct ApplicationDependencies {
    public_tori_client: Arc<dyn ToriClient>,
    authenticated_tori_client: Arc<dyn Fn() -> ToriClientFuture + Send + Sync>,
    tori_auth: Arc<ToriAuthHandler>,
    vinted_auth: Arc<VintedAuthHandler>,
    vinted_search_session: Arc<dyn VintedSearchSession>,
    vinted_item_session: Arc<dyn VintedItemSession>,
    vinted_search: Arc<dyn VintedSearchApi>,
    vinted_item: Arc<dyn VintedItemApi>,
    vinted_draft: Arc<dyn VintedDraftApi>,
    vinted_listing: Arc<dyn VintedListingApi>,
    vinted_publication: Arc<dyn VintedPublicationApi>,
    vinted_publication_discovery: Arc<dyn VintedPublicationDiscoveryApi>,
    vinted_readiness: Arc<dyn VintedReadinessApi>,
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
            vinted_search_session: Arc::new(vinted_session::credentials),
            vinted_item_session: Arc::new(vinted_session::credentials),
            vinted_search: Arc::new(HttpVintedSearchApi::new()),
            vinted_item: Arc::new(HttpVintedItemApi::new()),
            vinted_draft: Arc::new(HttpVintedDraftApi::new()),
            vinted_listing: Arc::new(HttpVintedListingApi::new()),
            vinted_publication: Arc::new(HttpVintedPublicationApi::new()),
            vinted_publication_discovery: Arc::new(HttpVintedPublicationDiscoveryApi::new()),
            vinted_readiness: Arc::new(HttpVintedReadinessApi::new()),
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
        F: Fn(ToriAuthArgs) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutcome, AppError>> + 'static,
    {
        self.tori_auth = Arc::new(move |args| Box::pin(handler(args)));
        self
    }

    pub fn with_vinted_auth_handler<F, Fut>(mut self, handler: F) -> Self
    where
        F: Fn(PortalId, VintedAuthArgs) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<CommandOutcome, AppError>> + 'static,
    {
        self.vinted_auth = Arc::new(move |portal, args| Box::pin(handler(portal, args)));
        self
    }

    pub fn with_vinted_credentials_provider<F>(mut self, provider: F) -> Self
    where
        F: Fn(PortalId) -> Result<VintedCredentialRecord, AppError> + Send + Sync + 'static,
    {
        let provider = Arc::new(provider);
        let search_provider = Arc::clone(&provider);
        self.vinted_search_session = Arc::new(move |portal| search_provider(portal));
        self.vinted_item_session = Arc::new(move |portal| provider(portal));
        self
    }

    pub fn with_vinted_search_api(mut self, api: Arc<dyn VintedSearchApi>) -> Self {
        self.vinted_search = api;
        self
    }

    pub fn with_vinted_item_api(mut self, api: Arc<dyn VintedItemApi>) -> Self {
        self.vinted_item = api;
        self
    }

    pub fn with_vinted_draft_api(mut self, api: Arc<dyn VintedDraftApi>) -> Self {
        self.vinted_draft = api;
        self
    }

    pub fn with_vinted_listing_api(mut self, api: Arc<dyn VintedListingApi>) -> Self {
        self.vinted_listing = api;
        self
    }

    pub fn with_vinted_publication_api(mut self, api: Arc<dyn VintedPublicationApi>) -> Self {
        self.vinted_publication = api;
        self
    }

    pub fn with_vinted_publication_discovery_api(
        mut self,
        api: Arc<dyn VintedPublicationDiscoveryApi>,
    ) -> Self {
        self.vinted_publication_discovery = api;
        self
    }

    pub fn with_vinted_readiness_api(mut self, api: Arc<dyn VintedReadinessApi>) -> Self {
        self.vinted_readiness = api;
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
        Command::Capabilities => Ok(CommandOutcome::new(CommandData::Capabilities(
            capabilities_output(),
        ))),
        Command::Marketplaces => Ok(CommandOutcome::new(CommandData::Marketplaces(
            marketplaces_output(),
        ))),
        Command::Tori(args) => execute_tori(args.command, dependencies).await,
        Command::Vinted(args) => execute_vinted(args.portal, args.command, dependencies).await,
        Command::Skill(args) => super::skill::dispatch(args).map(|output| {
            let document = output.document.clone();
            CommandOutcome::new(CommandData::Skill(output)).with_plain_document(document)
        }),
        Command::Unsupported(parts) => Err(unsupported_root_command(&parts)),
    }
}

async fn execute_tori(
    command: ToriCommand,
    dependencies: &ApplicationDependencies,
) -> Result<CommandOutcome, AppError> {
    match command {
        ToriCommand::Auth(args) => (dependencies.tori_auth)(args).await,
        ToriCommand::Capabilities => Ok(CommandOutcome::new(CommandData::MarketplaceCapabilities(
            marketplace_capabilities(MarketplaceId::Tori),
        ))),
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
                let api = HttpAdInputApi::new(client);
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
            super::location::dispatch(args, &api)
                .await
                .map(CommandData::Location)
                .map(CommandOutcome::new)
        }
    }
}

async fn execute_vinted_auth(
    portal: PortalId,
    args: VintedAuthArgs,
) -> Result<CommandOutcome, AppError> {
    let operation = match args.command {
        VintedAuthCommand::Login => vinted_session::AuthOperation::Login,
        VintedAuthCommand::Status => vinted_session::AuthOperation::Status,
        VintedAuthCommand::Logout => vinted_session::AuthOperation::Logout,
    };
    match vinted_session::execute_auth(portal, operation).await? {
        vinted_session::AuthResult::Login(login) => {
            let authenticated = login.authenticated;
            Ok(CommandOutcome::new(CommandData::VintedAuthLogin(login))
                .with_plain_authentication(MarketplaceId::Vinted, authenticated))
        }
        vinted_session::AuthResult::Status(status) => Ok(CommandOutcome::new(
            CommandData::VintedAuthStatus(status.data),
        )
        .with_next_actions(status.next_actions)),
        vinted_session::AuthResult::Logout(logout) => {
            Ok(CommandOutcome::new(CommandData::VintedAuthLogout(logout)))
        }
    }
}

async fn execute_vinted(
    portal: PortalId,
    command: VintedCommand,
    dependencies: &ApplicationDependencies,
) -> Result<CommandOutcome, AppError> {
    match command {
        VintedCommand::Auth(args) => (dependencies.vinted_auth)(portal, args).await,
        VintedCommand::Capabilities => Ok(CommandOutcome::new(
            CommandData::MarketplaceCapabilities(marketplace_capabilities(MarketplaceId::Vinted)),
        )),
        VintedCommand::Category(args) => {
            vinted_category::execute(
                portal,
                args.command,
                dependencies.vinted_search_session.as_ref(),
                dependencies.vinted_publication_discovery.as_ref(),
            )
            .await
        }
        VintedCommand::Readiness => {
            vinted_publish::execute_readiness(
                portal,
                dependencies.vinted_search_session.as_ref(),
                dependencies.vinted_readiness.as_ref(),
            )
            .await
        }
        VintedCommand::Draft(args) => {
            vinted_publish::execute_draft(
                portal,
                args.command,
                dependencies.vinted_search_session.as_ref(),
                dependencies.vinted_publication.as_ref(),
                dependencies.vinted_draft.as_ref(),
                dependencies.vinted_readiness.as_ref(),
            )
            .await
        }
        VintedCommand::Publish(args) => {
            vinted_publish::execute_direct(
                portal,
                args,
                dependencies.vinted_search_session.as_ref(),
                dependencies.vinted_publication.as_ref(),
                dependencies.vinted_readiness.as_ref(),
            )
            .await
        }
        VintedCommand::Item(args) => {
            let (item_id, raw) = match args.command {
                super::vinted_item::VintedItemCommand::Show { item_id, raw } => (item_id, raw),
            };
            match VintedItems::new(
                dependencies.vinted_item_session.as_ref(),
                dependencies.vinted_item.as_ref(),
            )
            .execute(portal, VintedItemRequest { item_id, raw })
            .await?
            {
                VintedItemResult::Detail(detail) => {
                    Ok(CommandOutcome::new(CommandData::VintedItem(*detail)))
                }
                VintedItemResult::Raw(raw) => Ok(CommandOutcome::new(CommandData::Raw(raw))),
            }
        }
        VintedCommand::Listing(args) => {
            let request = match args.command {
                super::vinted_listing::VintedListingCommand::Show { item_id } => {
                    VintedListingRequest::Show { item_id }
                }
                super::vinted_listing::VintedListingCommand::List => VintedListingRequest::List,
            };
            match VintedListings::new(
                dependencies.vinted_item_session.as_ref(),
                dependencies.vinted_listing.as_ref(),
            )
            .execute(portal, request)
            .await?
            {
                VintedListingResult::Detail(detail) => Ok(CommandOutcome::new(
                    CommandData::VintedListingDetail(*detail),
                )),
                VintedListingResult::Collection(collection) => Ok(CommandOutcome::new(
                    CommandData::VintedListingCollection(*collection),
                )),
            }
        }
        VintedCommand::Search(args) => match VintedSearch::new(
            dependencies.vinted_search_session.as_ref(),
            dependencies.vinted_search.as_ref(),
        )
        .execute(portal, args.into())
        .await?
        {
            VintedSearchResult::Search(collection) => {
                Ok(CommandOutcome::new(CommandData::Search(*collection)))
            }
            VintedSearchResult::Raw(raw) => Ok(CommandOutcome::new(CommandData::Raw(raw))),
        },
        VintedCommand::Unsupported(parts) => Err(capability_unavailable(
            MarketplaceContext::VINTED_FI,
            first_external_command(&parts),
        )),
    }
}

fn capabilities_output() -> CapabilitiesOutput {
    CapabilitiesOutput {
        marketplaces: marketplaces(),
    }
}

fn marketplaces_output() -> MarketplacesOutput {
    let configured = marketplaces()
        .iter()
        .map(|descriptor| MarketplaceSummary {
            marketplace: descriptor.marketplace,
            portals: descriptor.portals,
        })
        .collect();
    MarketplacesOutput {
        marketplaces: configured,
    }
}

fn marketplace_capabilities(marketplace_id: MarketplaceId) -> MarketplaceCapabilitiesOutput {
    let descriptor = marketplace(marketplace_id);
    MarketplaceCapabilitiesOutput {
        marketplace: descriptor.marketplace,
        portals: descriptor.portals,
        capabilities: descriptor.capabilities,
    }
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

async fn execute_tori_auth(args: ToriAuthArgs) -> Result<super::outcome::CommandOutcome, AppError> {
    let command = match args.command {
        ToriAuthCommand::Callback {
            state_root,
            callback_url,
        } => {
            return tori_interactive::capture(
                &StatePaths::from_root(state_root, MarketplaceContext::TORI_FI),
                &callback_url,
            )
            .map(CommandData::ToriAuthCallback)
            .map(CommandOutcome::new);
        }
        command => command,
    };
    let paths = tori_session::state_paths()?;
    match command {
        ToriAuthCommand::Login => execute_interactive_login(paths).await,
        ToriAuthCommand::Status => {
            let status = tori_session::status().await?;
            Ok(
                CommandOutcome::new(CommandData::ToriAuthStatus(status.data))
                    .with_next_actions(status.next_actions),
            )
        }
        ToriAuthCommand::Logout => {
            let store = FileAuthStore::new(paths);
            let auth = ToriAuthentication::new(SchibstedToriAuthenticationApi::new(), store);
            auth.logout()
                .map(CommandData::ToriAuthLogout)
                .map(CommandOutcome::new)
        }
        ToriAuthCommand::Callback { .. } => unreachable!("callbacks return before dispatch"),
    }
}

async fn execute_interactive_login(paths: StatePaths) -> Result<CommandOutcome, AppError> {
    tori_interactive::prepare(&paths)?;
    let result = async {
        let store = FileAuthStore::new(paths.clone());
        let handler = ToriAuthentication::new(SchibstedToriAuthenticationApi::new(), store);
        let started = handler.start(unix_time_now()?)?;
        let callback =
            tori_interactive::open_and_wait(&paths, &started.login_url, started.expires_at_unix)?;
        handler
            .complete(&started.flow_id, &callback, unix_time_now()?)
            .await
    }
    .await;
    let cleared = tori_interactive::clear(&paths);
    match result {
        Ok(value) => {
            cleared?;
            Ok(CommandOutcome::new(CommandData::ToriAuthComplete(value))
                .with_plain_authentication(MarketplaceId::Tori, true))
        }
        Err(error) => Err(error),
    }
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
