use std::{fmt, future::Future, pin::Pin, sync::Arc};

use reqwest::{Method, StatusCode};
use serde::{Deserialize, Serialize};

use crate::{
    domain::{
        envelope::NextAction,
        observation::{Observation, ObservationOperation},
    },
    error::{AppError, ExitClass},
    marketplace::tori::client::{
        HttpFailure, RequestSpec, ToriClient, compatibility, map_http_error,
    },
};

const AD_ITEM_TYPE: &str = "Ad";

pub trait FavoritesApi: Send + Sync {
    fn folders(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<FavoriteFolder>, FavoritesApiError>> + Send + '_>>;
    fn favorite_folders(
        &self,
        listing_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, FavoritesApiError>> + Send + '_>>;
    fn add(
        &self,
        folder_id: u64,
        listing_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), FavoritesApiError>> + Send + '_>>;
    fn remove(
        &self,
        folder_id: u64,
        listing_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), FavoritesApiError>> + Send + '_>>;
}

pub struct HttpFavoritesApi {
    client: Arc<dyn ToriClient>,
}

impl HttpFavoritesApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    async fn execute(
        &self,
        request: RequestSpec,
    ) -> Result<crate::marketplace::tori::client::HttpResponse, FavoritesApiError> {
        self.client
            .execute(request)
            .await
            .map_err(map_http_error::<FavoritesApiError>)
    }

    async fn mutate(&self, method: Method, path: String) -> Result<(), FavoritesApiError> {
        let response = self
            .execute(RequestSpec::new(method, path, compatibility::SERVICE_FAVORITES).empty_body())
            .await?;
        ensure_success(response.status)
    }
}

impl FavoritesApi for HttpFavoritesApi {
    fn folders(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<FavoriteFolder>, FavoritesApiError>> + Send + '_>>
    {
        Box::pin(async move {
            let response = self
                .execute(RequestSpec::new(
                    Method::GET,
                    "/favorites/".to_owned(),
                    compatibility::SERVICE_FAVORITES,
                ))
                .await?;
            ensure_success(response.status)?;
            serde_json::from_slice(&response.body).map_err(|_| FavoritesApiError::Unexpected)
        })
    }

    fn favorite_folders(
        &self,
        listing_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, FavoritesApiError>> + Send + '_>> {
        Box::pin(async move {
            let response = self
                .execute(RequestSpec::new(
                    Method::GET,
                    "/favorites/v2/minimal".to_owned(),
                    compatibility::SERVICE_FAVORITES,
                ))
                .await?;
            ensure_success(response.status)?;
            let response: FavoritesMinimal = serde_json::from_slice(&response.body)
                .map_err(|_| FavoritesApiError::Unexpected)?;
            Ok(response
                .items
                .into_iter()
                .find(|item| item.item_id == listing_id && item.item_type == AD_ITEM_TYPE)
                .map(|item| item.folders)
                .unwrap_or_default())
        })
    }

    fn add(
        &self,
        folder_id: u64,
        listing_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), FavoritesApiError>> + Send + '_>> {
        Box::pin(async move {
            self.mutate(
                Method::PUT,
                format!("/favorites/{folder_id}/{AD_ITEM_TYPE}/{listing_id}"),
            )
            .await
        })
    }

    fn remove(
        &self,
        folder_id: u64,
        listing_id: u64,
    ) -> Pin<Box<dyn Future<Output = Result<(), FavoritesApiError>> + Send + '_>> {
        Box::pin(async move {
            self.mutate(
                Method::DELETE,
                format!("/favorites/{folder_id}/{AD_ITEM_TYPE}/{listing_id}"),
            )
            .await
        })
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

impl From<HttpFailure> for FavoritesApiError {
    fn from(failure: HttpFailure) -> Self {
        match failure {
            HttpFailure::Transport(_) => Self::Transport,
            HttpFailure::Local(_) => Self::Unexpected,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FavoriteRequest {
    Folders,
    Status {
        listing_id: String,
    },
    Add {
        listing_id: String,
        folder_id: Option<u64>,
    },
    Remove {
        listing_id: String,
        folder_id: Option<u64>,
    },
}

#[derive(Debug, PartialEq)]
pub enum FavoriteResult {
    Folders {
        folders: Vec<FavoriteFolder>,
        observation: Observation,
    },
    Status {
        status: FavoriteStatus,
        observation: Observation,
    },
    Mutation {
        mutation: FavoriteMutation,
        observation: Observation,
    },
}

pub struct Favorites<'a> {
    api: &'a dyn FavoritesApi,
}

impl<'a> Favorites<'a> {
    pub fn new(api: &'a dyn FavoritesApi) -> Self {
        Self { api }
    }

    pub async fn execute(&self, request: FavoriteRequest) -> Result<FavoriteResult, AppError> {
        match request {
            FavoriteRequest::Folders => Ok(FavoriteResult::Folders {
                folders: self.folders().await?,
                observation: Observation::confirmed_present("favorites_folders", None),
            }),
            FavoriteRequest::Status { listing_id } => {
                let status = self.status(&listing_id).await?;
                let observation = if status.favorite {
                    Observation::confirmed_present("favorites_minimal", None)
                } else {
                    Observation::confirmed_absent("favorites_minimal", None)
                };
                Ok(FavoriteResult::Status {
                    status,
                    observation,
                })
            }
            FavoriteRequest::Add {
                listing_id,
                folder_id,
            } => {
                let mutation = self.add(&listing_id, folder_id).await?;
                Ok(favorite_mutation_result(mutation))
            }
            FavoriteRequest::Remove {
                listing_id,
                folder_id,
            } => {
                let mutation = self.remove(&listing_id, folder_id).await?;
                Ok(favorite_mutation_result(mutation))
            }
        }
    }

    pub async fn folders(&self) -> Result<Vec<FavoriteFolder>, AppError> {
        self.api
            .folders()
            .await
            .map_err(|error| favorite_error(error, None, true))
    }

    pub async fn status(&self, listing_id: &str) -> Result<FavoriteStatus, AppError> {
        let listing_id = parse_listing_id(listing_id)?;
        let folder_ids = self
            .api
            .favorite_folders(listing_id)
            .await
            .map_err(|error| favorite_error(error, None, true))?;
        Ok(FavoriteStatus {
            listing_id,
            favorite: !folder_ids.is_empty(),
            folder_ids,
        })
    }

    pub async fn add(
        &self,
        listing_id: &str,
        folder_id: Option<u64>,
    ) -> Result<FavoriteMutation, AppError> {
        let listing_id = parse_listing_id(listing_id)?;
        let folder_id = self.resolve_folder(folder_id).await?;
        self.api
            .add(folder_id, listing_id)
            .await
            .map_err(|error| favorite_error(error, Some((folder_id, listing_id)), false))?;
        Ok(FavoriteMutation {
            listing_id,
            folder_id,
            favorite: true,
        })
    }

    pub async fn remove(
        &self,
        listing_id: &str,
        folder_id: Option<u64>,
    ) -> Result<FavoriteMutation, AppError> {
        let listing_id = parse_listing_id(listing_id)?;
        let folder_id = self.resolve_folder(folder_id).await?;
        self.api
            .remove(folder_id, listing_id)
            .await
            .map_err(|error| favorite_error(error, Some((folder_id, listing_id)), false))?;
        Ok(FavoriteMutation {
            listing_id,
            folder_id,
            favorite: false,
        })
    }

    async fn resolve_folder(&self, folder_id: Option<u64>) -> Result<u64, AppError> {
        if let Some(folder_id) = folder_id {
            return Ok(folder_id);
        }
        let folders = self.folders().await?;
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

fn favorite_mutation_result(mutation: FavoriteMutation) -> FavoriteResult {
    let observation = if mutation.favorite {
        Observation::confirmed_present("favorite_mutation_response", None)
    } else {
        Observation::confirmed_absent("favorite_mutation_response", None)
    };
    FavoriteResult::Mutation {
        mutation,
        observation,
    }
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
            command: crate::invocation::tori("auth login"),
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
        fn folders(
            &self,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<FavoriteFolder>, FavoritesApiError>> + Send + '_>>
        {
            let folders = self.folders.clone();
            Box::pin(async move { Ok(folders) })
        }

        fn favorite_folders(
            &self,
            listing_id: u64,
        ) -> Pin<Box<dyn Future<Output = Result<Vec<u64>, FavoritesApiError>> + Send + '_>>
        {
            let folders = self
                .folders
                .iter()
                .filter(|folder| folder.item_count == listing_id)
                .map(|folder| folder.folder_id)
                .collect();
            Box::pin(async move { Ok(folders) })
        }

        fn add(
            &self,
            folder_id: u64,
            listing_id: u64,
        ) -> Pin<Box<dyn Future<Output = Result<(), FavoritesApiError>> + Send + '_>> {
            self.mutations
                .lock()
                .unwrap()
                .push((true, folder_id, listing_id));
            Box::pin(async { Ok(()) })
        }

        fn remove(
            &self,
            folder_id: u64,
            listing_id: u64,
        ) -> Pin<Box<dyn Future<Output = Result<(), FavoritesApiError>> + Send + '_>> {
            self.mutations
                .lock()
                .unwrap()
                .push((false, folder_id, listing_id));
            Box::pin(async { Ok(()) })
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

    #[tokio::test]
    async fn status_returns_every_matching_folder() {
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

        let result = Favorites::new(&api).status("123").await.unwrap();

        assert!(result.favorite);
        assert_eq!(result.folder_ids, vec![42, 43]);
    }

    #[tokio::test]
    async fn add_uses_the_default_folder() {
        let api = FakeApi {
            folders: vec![folder(42, true)],
            mutations: Mutex::new(Vec::new()),
        };

        let result = Favorites::new(&api).add("123", None).await.unwrap();

        assert_eq!(result.folder_id, 42);
        assert_eq!(*api.mutations.lock().unwrap(), vec![(true, 42, 123)]);
    }

    #[tokio::test]
    async fn explicit_folder_skips_folder_discovery() {
        let api = FakeApi {
            folders: Vec::new(),
            mutations: Mutex::new(Vec::new()),
        };

        Favorites::new(&api).remove("123", Some(7)).await.unwrap();

        assert_eq!(*api.mutations.lock().unwrap(), vec![(false, 7, 123)]);
    }

    #[tokio::test]
    async fn rejects_non_numeric_listing_ids() {
        let api = FakeApi {
            folders: Vec::new(),
            mutations: Mutex::new(Vec::new()),
        };

        let error = Favorites::new(&api).add("abc", Some(7)).await.unwrap_err();

        assert_eq!(error.code, "cli.invalid_usage");
    }
}
