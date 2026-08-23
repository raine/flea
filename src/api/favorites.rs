use std::{fmt, sync::Arc};

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{
    api::client::{HttpError, RequestSpec, ToriClient, TransportErrorKind, compatibility},
    domain::{
        envelope::NextAction,
        observation::{Observation, ObservationOperation},
    },
    error::{AppError, ExitClass},
};

const AD_ITEM_TYPE: &str = "Ad";

pub trait FavoritesApi: Send + Sync {
    fn folders(&self) -> Result<Vec<FavoriteFolder>, FavoritesApiError>;
    fn favorite_folders(&self, listing_id: u64) -> Result<Vec<u64>, FavoritesApiError>;
    fn add(&self, folder_id: u64, listing_id: u64) -> Result<(), FavoritesApiError>;
    fn remove(&self, folder_id: u64, listing_id: u64) -> Result<(), FavoritesApiError>;
}

pub struct HttpFavoritesApi {
    client: Arc<dyn ToriClient>,
}

impl HttpFavoritesApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    fn execute(
        &self,
        request: RequestSpec,
    ) -> Result<crate::api::client::HttpResponse, FavoritesApiError> {
        let client = Arc::clone(&self.client);
        std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| FavoritesApiError::Unexpected)?
                        .block_on(client.execute(request))
                        .map_err(favorites_http_error)
                })
                .join()
                .map_err(|_| FavoritesApiError::Unexpected)?
        })
    }

    fn mutate(&self, method: Method, path: String) -> Result<(), FavoritesApiError> {
        let response = self.execute(
            RequestSpec::new(method, path, compatibility::SERVICE_FAVORITES).empty_body(),
        )?;
        ensure_success(response.status)
    }
}

impl FavoritesApi for HttpFavoritesApi {
    fn folders(&self) -> Result<Vec<FavoriteFolder>, FavoritesApiError> {
        let response = self.execute(RequestSpec::new(
            Method::GET,
            "/favorites/".to_owned(),
            compatibility::SERVICE_FAVORITES,
        ))?;
        ensure_success(response.status)?;
        serde_json::from_slice(&response.body).map_err(|_| FavoritesApiError::Unexpected)
    }

    fn favorite_folders(&self, listing_id: u64) -> Result<Vec<u64>, FavoritesApiError> {
        let response = self.execute(RequestSpec::new(
            Method::GET,
            "/favorites/v2/minimal".to_owned(),
            compatibility::SERVICE_FAVORITES,
        ))?;
        ensure_success(response.status)?;
        let response: FavoritesMinimal =
            serde_json::from_slice(&response.body).map_err(|_| FavoritesApiError::Unexpected)?;
        Ok(response
            .items
            .into_iter()
            .find(|item| item.item_id == listing_id && item.item_type == AD_ITEM_TYPE)
            .map(|item| item.folders)
            .unwrap_or_default())
    }

    fn add(&self, folder_id: u64, listing_id: u64) -> Result<(), FavoritesApiError> {
        self.mutate(
            Method::PUT,
            format!("/favorites/{folder_id}/{AD_ITEM_TYPE}/{listing_id}"),
        )
    }

    fn remove(&self, folder_id: u64, listing_id: u64) -> Result<(), FavoritesApiError> {
        self.mutate(
            Method::DELETE,
            format!("/favorites/{folder_id}/{AD_ITEM_TYPE}/{listing_id}"),
        )
    }
}

fn ensure_success(status: StatusCode) -> Result<(), FavoritesApiError> {
    match status {
        status if status.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(FavoritesApiError::Authentication),
        StatusCode::NOT_FOUND => Err(FavoritesApiError::NotFound),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            Err(FavoritesApiError::Invalid)
        }
        status => Err(FavoritesApiError::Upstream(status.as_u16())),
    }
}

fn favorites_http_error(error: HttpError) -> FavoritesApiError {
    match error {
        HttpError::Transport(transport)
            if matches!(
                transport.kind,
                TransportErrorKind::Timeout | TransportErrorKind::Connection
            ) =>
        {
            FavoritesApiError::Transport
        }
        HttpError::InvalidRequest | HttpError::ResponseTooLarge | HttpError::Transport(_) => {
            FavoritesApiError::Unexpected
        }
    }
}

#[derive(Clone, thiserror::Error, PartialEq, Eq)]
pub enum FavoritesApiError {
    #[error("authentication failed")]
    Authentication,
    #[error("favorite resource was not found")]
    NotFound,
    #[error("favorite request was rejected")]
    Invalid,
    #[error("favorite transport failed")]
    Transport,
    #[error("Tori favorites service returned HTTP {0}")]
    Upstream(u16),
    #[error("unexpected favorites response")]
    Unexpected,
}

impl fmt::Debug for FavoritesApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication => formatter.write_str("Authentication"),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Invalid => formatter.write_str("Invalid"),
            Self::Transport => formatter.write_str("Transport"),
            Self::Upstream(status) => formatter.debug_tuple("Upstream").field(status).finish(),
            Self::Unexpected => formatter.write_str("Unexpected"),
        }
    }
}

#[derive(Deserialize)]
struct FavoritesMinimal {
    items: Vec<FavoriteFolderMapping>,
}

#[derive(Deserialize)]
struct FavoriteFolderMapping {
    #[serde(rename = "itemId")]
    item_id: u64,
    #[serde(rename = "itemType")]
    item_type: String,
    #[serde(rename = "folderIds")]
    folders: Vec<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FavoriteFolder {
    #[serde(rename(deserialize = "folderid"))]
    pub folder_id: u64,
    #[serde(rename(deserialize = "default"))]
    pub default_folder: bool,
    pub name: String,
    #[serde(rename(deserialize = "num"), default)]
    pub item_count: u64,
    #[serde(
        rename(deserialize = "favoriteLastAdded"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub favorite_last_added: Option<String>,
    #[serde(
        rename(deserialize = "shareid"),
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub share_id: Option<String>,
}

pub struct Favorites<'a> {
    api: &'a dyn FavoritesApi,
}

impl<'a> Favorites<'a> {
    pub fn new(api: &'a dyn FavoritesApi) -> Self {
        Self { api }
    }

    pub fn folders(&self) -> Result<Vec<FavoriteFolder>, AppError> {
        self.api
            .folders()
            .map_err(|error| favorite_error(error, None, true))
    }

    pub fn status(&self, listing_id: &str) -> Result<FavoriteStatus, AppError> {
        let listing_id = parse_listing_id(listing_id)?;
        let folder_ids = self
            .api
            .favorite_folders(listing_id)
            .map_err(|error| favorite_error(error, None, true))?;
        Ok(FavoriteStatus {
            listing_id,
            favorite: !folder_ids.is_empty(),
            folder_ids,
        })
    }

    pub fn add(
        &self,
        listing_id: &str,
        folder_id: Option<u64>,
    ) -> Result<FavoriteMutation, AppError> {
        let listing_id = parse_listing_id(listing_id)?;
        let folder_id = self.resolve_folder(folder_id)?;
        self.api
            .add(folder_id, listing_id)
            .map_err(|error| favorite_error(error, Some((folder_id, listing_id)), false))?;
        Ok(FavoriteMutation {
            listing_id,
            folder_id,
            favorite: true,
        })
    }

    pub fn remove(
        &self,
        listing_id: &str,
        folder_id: Option<u64>,
    ) -> Result<FavoriteMutation, AppError> {
        let listing_id = parse_listing_id(listing_id)?;
        let folder_id = self.resolve_folder(folder_id)?;
        self.api
            .remove(folder_id, listing_id)
            .map_err(|error| favorite_error(error, Some((folder_id, listing_id)), false))?;
        Ok(FavoriteMutation {
            listing_id,
            folder_id,
            favorite: false,
        })
    }

    fn resolve_folder(&self, folder_id: Option<u64>) -> Result<u64, AppError> {
        if let Some(folder_id) = folder_id {
            return Ok(folder_id);
        }
        let folders = self.folders()?;
        folders
            .iter()
            .find(|folder| folder.default_folder)
            .map(|folder| folder.folder_id)
            .ok_or_else(|| {
                let mut error = AppError::validation(
                    "favorite.default_folder_missing",
                    "Tori did not return a default favorites folder",
                );
                error.next_actions.push(NextAction {
                    command: "flea tori favorite folders".to_owned(),
                });
                error
            })
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FavoriteStatus {
    pub listing_id: u64,
    pub favorite: bool,
    pub folder_ids: Vec<u64>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct FavoriteMutation {
    pub listing_id: u64,
    pub folder_id: u64,
    pub favorite: bool,
}

fn parse_listing_id(value: &str) -> Result<u64, AppError> {
    value
        .parse::<u64>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(|| AppError::usage("listing ID must be a positive numeric Tori identifier"))
}

fn favorite_error(
    error: FavoritesApiError,
    resource: Option<(u64, u64)>,
    read_only: bool,
) -> AppError {
    let authentication = matches!(&error, FavoritesApiError::Authentication);
    let observable_failure = matches!(
        &error,
        FavoritesApiError::Transport
            | FavoritesApiError::Upstream(_)
            | FavoritesApiError::Unexpected
    );
    let observation = match &error {
        FavoritesApiError::Transport => {
            Observation::temporarily_unavailable("favorite_mutation_response", None, false)
        }
        _ => Observation::unrecognized_response("favorite_mutation_response", None),
    };
    let mut app_error = match error {
        FavoritesApiError::Authentication => AppError::authentication(
            "auth.required",
            "Tori rejected authentication for the favorites request",
        ),
        FavoritesApiError::NotFound => AppError::validation(
            "favorite.not_found",
            "the favorites folder or listing was not found",
        ),
        FavoritesApiError::Invalid => {
            AppError::validation("favorite.rejected", "Tori rejected the favorites request")
        }
        FavoritesApiError::Transport => {
            AppError::upstream("upstream.transport", "the Tori favorites request failed")
        }
        FavoritesApiError::Upstream(status) => AppError::new(
            "upstream.http",
            format!("the Tori favorites request failed with HTTP {status}"),
            ExitClass::Upstream,
        ),
        FavoritesApiError::Unexpected => AppError::upstream(
            "upstream.unexpected_response",
            "Tori returned an unexpected favorites response",
        ),
    };

    if authentication {
        app_error.next_actions.push(NextAction {
            command: crate::cli::invocation::tori("auth login"),
        });
    }
    if read_only && observable_failure {
        app_error = app_error.with_observation(observation.clone(), ObservationOperation::Read);
    }
    if let Some((folder_id, listing_id)) = resource {
        app_error.next_actions.push(NextAction {
            command: format!("flea tori favorite status {listing_id}"),
        });
        if !read_only {
            app_error = app_error.with_observation(observation, ObservationOperation::Mutation);
        }
        app_error.details = Some(Box::new(serde_json::json!({ "folder_id": folder_id })));
    }
    app_error
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    struct FakeApi {
        folders: Vec<FavoriteFolder>,
        mutations: Mutex<Vec<(bool, u64, u64)>>,
    }

    impl FavoritesApi for FakeApi {
        fn folders(&self) -> Result<Vec<FavoriteFolder>, FavoritesApiError> {
            Ok(self.folders.clone())
        }

        fn favorite_folders(&self, listing_id: u64) -> Result<Vec<u64>, FavoritesApiError> {
            Ok(self
                .folders
                .iter()
                .filter(|folder| folder.item_count == listing_id)
                .map(|folder| folder.folder_id)
                .collect())
        }

        fn add(&self, folder_id: u64, listing_id: u64) -> Result<(), FavoritesApiError> {
            self.mutations
                .lock()
                .unwrap()
                .push((true, folder_id, listing_id));
            Ok(())
        }

        fn remove(&self, folder_id: u64, listing_id: u64) -> Result<(), FavoritesApiError> {
            self.mutations
                .lock()
                .unwrap()
                .push((false, folder_id, listing_id));
            Ok(())
        }
    }

    fn folder(folder_id: u64, default_folder: bool) -> FavoriteFolder {
        FavoriteFolder {
            folder_id,
            default_folder,
            name: "Saved".to_owned(),
            item_count: 2,
            favorite_last_added: None,
            share_id: None,
        }
    }

    #[test]
    fn status_returns_every_matching_folder() {
        let api = FakeApi {
            folders: vec![folder(42, false), folder(43, true)]
                .into_iter()
                .map(|mut folder| {
                    folder.item_count = 123;
                    folder
                })
                .collect(),
            mutations: Mutex::new(Vec::new()),
        };

        let result = Favorites::new(&api).status("123").unwrap();

        assert!(result.favorite);
        assert_eq!(result.folder_ids, vec![42, 43]);
    }

    #[test]
    fn add_uses_the_default_folder() {
        let api = FakeApi {
            folders: vec![folder(42, true)],
            mutations: Mutex::new(Vec::new()),
        };

        let result = Favorites::new(&api).add("123", None).unwrap();

        assert_eq!(result.folder_id, 42);
        assert_eq!(*api.mutations.lock().unwrap(), vec![(true, 42, 123)]);
    }

    #[test]
    fn explicit_folder_skips_folder_discovery() {
        let api = FakeApi {
            folders: Vec::new(),
            mutations: Mutex::new(Vec::new()),
        };

        Favorites::new(&api).remove("123", Some(7)).unwrap();

        assert_eq!(*api.mutations.lock().unwrap(), vec![(false, 7, 123)]);
    }

    #[test]
    fn rejects_non_numeric_listing_ids() {
        let api = FakeApi {
            folders: Vec::new(),
            mutations: Mutex::new(Vec::new()),
        };

        let error = Favorites::new(&api).add("abc", Some(7)).unwrap_err();

        assert_eq!(error.code, "cli.invalid_usage");
    }
}
