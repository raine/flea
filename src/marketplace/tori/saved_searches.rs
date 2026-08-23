use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc};

use reqwest::{Method, StatusCode, header::HeaderValue};
use serde::Serialize;
use serde_json::{Value, json};
use url::form_urlencoded;

use crate::{
    domain::{
        envelope::NextAction,
        observation::{Observation, ObservationOperation},
    },
    error::{AppError, ExitClass},
    marketplace::tori::client::{
        HttpError, RequestSpec, ToriClient, TransportErrorKind, compatibility,
    },
    retry::RetryClassification,
};

const PATH: &str = "/public/search";
const CLIENT_ID: &str = "ANDROID";
const SEARCH_KEY: &str = "SEARCH_ID_BAP_COMMON";
const SEARCH_TYPE: &str = "alert";

pub trait SavedSearchesApi: Send + Sync {
    fn list(
        &self,
        rows: Option<usize>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, SavedSearchApiError>> + Send + '_>>;
    fn show<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, SavedSearchApiError>> + Send + 'a>>;
    fn create<'a>(
        &'a self,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, SavedSearchApiError>> + Send + 'a>>;
    fn update<'a>(
        &'a self,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>>;
    fn delete<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>>;
}

pub struct HttpSavedSearchesApi {
    client: Arc<dyn ToriClient>,
}

impl HttpSavedSearchesApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    async fn execute(
        &self,
        request: RequestSpec,
    ) -> Result<crate::marketplace::tori::client::HttpResponse, SavedSearchApiError> {
        self.client.execute(request).await.map_err(http_error)
    }

    fn query(pairs: &[(&str, String)]) -> String {
        let mut encoder = form_urlencoded::Serializer::new(String::new());
        for (name, value) in pairs {
            encoder.append_pair(name, value);
        }
        encoder.append_pair("clientId", CLIENT_ID);
        format!("{PATH}?{}", encoder.finish())
    }

    async fn read(&self, pairs: &[(&str, String)]) -> Result<Vec<Value>, SavedSearchApiError> {
        let response = self
            .execute(RequestSpec::new(
                Method::GET,
                Self::query(pairs),
                compatibility::SERVICE_SAVED_SEARCHES,
            ))
            .await?;
        ensure_success(response.status)?;
        serde_json::from_slice(&response.body).map_err(|_| SavedSearchApiError::Unexpected)
    }

    async fn mutation(
        &self,
        method: Method,
        target: String,
        body: Option<&Value>,
    ) -> Result<crate::marketplace::tori::client::HttpResponse, SavedSearchApiError> {
        let mut request = RequestSpec::new(method, target, compatibility::SERVICE_SAVED_SEARCHES);
        if let Some(body) = body {
            request = request.body(
                serde_json::to_vec(body).map_err(|_| SavedSearchApiError::Unexpected)?,
                HeaderValue::from_static("application/json"),
            );
        }
        let response = self.execute(request).await?;
        ensure_success(response.status)?;
        Ok(response)
    }
}

impl SavedSearchesApi for HttpSavedSearchesApi {
    fn list(
        &self,
        rows: Option<usize>,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Value>, SavedSearchApiError>> + Send + '_>> {
        Box::pin(async move {
            let mut pairs = vec![("type", SEARCH_TYPE.to_owned())];
            if let Some(rows) = rows {
                pairs.push(("rows", rows.to_string()));
            }
            self.read(&pairs).await
        })
    }

    fn show<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Option<Value>, SavedSearchApiError>> + Send + 'a>> {
        Box::pin(async move {
            Ok(self
                .read(&[("id", id.to_owned())])
                .await?
                .into_iter()
                .next())
        })
    }

    fn create<'a>(
        &'a self,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<String, SavedSearchApiError>> + Send + 'a>> {
        Box::pin(async move {
            let response = self
                .mutation(Method::POST, Self::query(&[]), Some(body))
                .await?;
            let value: Value = serde_json::from_slice(&response.body)
                .map_err(|_| SavedSearchApiError::Unexpected)?;
            opaque(&value).ok_or(SavedSearchApiError::Unexpected)
        })
    }

    fn update<'a>(
        &'a self,
        body: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>> {
        Box::pin(async move {
            self.mutation(Method::PUT, Self::query(&[]), Some(body))
                .await
                .map(|_| ())
        })
    }

    fn delete<'a>(
        &'a self,
        id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), SavedSearchApiError>> + Send + 'a>> {
        Box::pin(async move {
            self.mutation(Method::DELETE, Self::query(&[("id", id.to_owned())]), None)
                .await
                .map(|_| ())
        })
    }
}

fn ensure_success(status: StatusCode) -> Result<(), SavedSearchApiError> {
    match status {
        status if status.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(SavedSearchApiError::Authentication)
        }
        StatusCode::NOT_FOUND => Err(SavedSearchApiError::NotFound),
        StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
            Err(SavedSearchApiError::Rejected)
        }
        status => Err(SavedSearchApiError::Upstream(status.as_u16())),
    }
}

fn http_error(error: HttpError) -> SavedSearchApiError {
    match error {
        HttpError::Transport(error)
            if matches!(
                error.kind,
                TransportErrorKind::Timeout | TransportErrorKind::Connection
            ) =>
        {
            SavedSearchApiError::Transport
        }
        HttpError::InvalidRequest | HttpError::ResponseTooLarge | HttpError::Transport(_) => {
            SavedSearchApiError::Unexpected
        }
    }
}

#[derive(Clone, thiserror::Error, PartialEq, Eq)]
pub enum SavedSearchApiError {
    #[error("authentication failed")]
    Authentication,
    #[error("saved search was not found")]
    NotFound,
    #[error("saved search request was rejected")]
    Rejected,
    #[error("saved search transport failed")]
    Transport,
    #[error("Tori saved search service returned HTTP {0}")]
    Upstream(u16),
    #[error("unexpected saved search response")]
    Unexpected,
}

impl fmt::Debug for SavedSearchApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication => formatter.write_str("Authentication"),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Rejected => formatter.write_str("Rejected"),
            Self::Transport => formatter.write_str("Transport"),
            Self::Upstream(status) => formatter.debug_tuple("Upstream").field(status).finish(),
            Self::Unexpected => formatter.write_str("Unexpected"),
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SavedSearch {
    pub id: String,
    pub name: String,
    pub search_key: String,
    pub search_type: String,
    pub notifications: Vec<String>,
    pub parameters: BTreeMap<String, Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub changed: Option<String>,
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vertical_description: Option<String>,
}

#[derive(Clone, Debug)]
pub struct CreateSavedSearch {
    pub name: String,
    pub notifications: Vec<String>,
    pub parameters: BTreeMap<String, Vec<String>>,
}

pub struct SavedSearches<'a> {
    api: &'a dyn SavedSearchesApi,
}

impl<'a> SavedSearches<'a> {
    pub fn new(api: &'a dyn SavedSearchesApi) -> Self {
        Self { api }
    }

    pub async fn list(&self, rows: Option<usize>) -> Result<Vec<SavedSearch>, AppError> {
        self.api
            .list(rows)
            .await
            .map_err(|error| api_error(error, None, true))?
            .iter()
            .map(normalize)
            .collect()
    }

    pub async fn show(&self, id: &str) -> Result<SavedSearch, AppError> {
        validate_id(id)?;
        let raw = self
            .api
            .show(id)
            .await
            .map_err(|error| api_error(error, Some(id), true))?
            .ok_or_else(|| not_found(id))?;
        normalize(&raw)
    }

    pub async fn create(&self, input: CreateSavedSearch) -> Result<SavedSearch, AppError> {
        validate_name(&input.name)?;
        let body = create_body(&input);
        match self.api.create(&body).await {
            Ok(id) => self.verify_present(&id).await,
            Err(error) => {
                let recovery = self.api.list(None).await;
                if let Ok(ref raw) = recovery {
                    let matches = raw
                        .iter()
                        .filter(|candidate| intended_create(candidate, &input))
                        .collect::<Vec<_>>();
                    if matches.len() == 1 {
                        return normalize(matches[0]);
                    }
                    if matches.is_empty() {
                        return Err(mutation_error(
                            error,
                            None,
                            Observation::confirmed_absent("saved_search_list", Some(200)),
                            true,
                        ));
                    }
                }
                Err(mutation_error(
                    error,
                    None,
                    recovery_observation(&recovery),
                    false,
                ))
            }
        }
    }

    pub async fn update(
        &self,
        id: &str,
        name: Option<String>,
        notifications: Option<Vec<String>>,
    ) -> Result<SavedSearch, AppError> {
        validate_id(id)?;
        if name.is_none() && notifications.is_none() {
            return Err(AppError::usage(
                "provide --name or at least one notification option",
            ));
        }
        if let Some(name) = name.as_deref() {
            validate_name(name)?;
        }
        let current = self
            .api
            .show(id)
            .await
            .map_err(|error| api_error(error, Some(id), true))?
            .ok_or_else(|| not_found(id))?;
        let raw = update_body(&current, name, notifications)?;
        match self.api.update(&raw).await {
            Ok(()) => self.verify_present(id).await,
            Err(error) => match self.api.show(id).await {
                Ok(Some(observed)) if intended_update(&observed, &raw) => normalize(&observed),
                Ok(Some(_)) => Err(mutation_error(
                    error,
                    Some(id),
                    Observation::confirmed_absent("saved_search_show", Some(200)),
                    true,
                )),
                Ok(None) => Err(mutation_error(
                    error,
                    Some(id),
                    Observation::confirmed_absent("saved_search_show", Some(200)),
                    false,
                )),
                Err(_) => Err(mutation_error(
                    error,
                    Some(id),
                    Observation::temporarily_unavailable("saved_search_show", None, false),
                    false,
                )),
            },
        }
    }

    async fn verify_present(&self, id: &str) -> Result<SavedSearch, AppError> {
        match self.api.show(id).await {
            Ok(Some(raw)) => normalize(&raw),
            Ok(None) | Err(SavedSearchApiError::NotFound) => Err(mutation_error(
                SavedSearchApiError::Unexpected,
                Some(id),
                Observation::confirmed_absent("saved_search_show", Some(200)),
                false,
            )),
            Err(error) => {
                let observation = recovery_observation(&Err(error.clone()));
                Err(mutation_error(error, Some(id), observation, false))
            }
        }
    }

    pub async fn delete(&self, id: &str) -> Result<DeletedSavedSearch, AppError> {
        validate_id(id)?;
        match self.api.delete(id).await {
            Ok(()) => Ok(DeletedSavedSearch {
                id: id.to_owned(),
                deleted: true,
            }),
            Err(error) => match self.api.show(id).await {
                Ok(None) | Err(SavedSearchApiError::NotFound) => Ok(DeletedSavedSearch {
                    id: id.to_owned(),
                    deleted: true,
                }),
                Ok(Some(_)) => Err(mutation_error(
                    error,
                    Some(id),
                    Observation::confirmed_present("saved_search_show", Some(200)),
                    true,
                )),
                Err(_) => Err(mutation_error(
                    error,
                    Some(id),
                    Observation::temporarily_unavailable("saved_search_show", None, false),
                    false,
                )),
            },
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DeletedSavedSearch {
    pub id: String,
    pub deleted: bool,
}

fn create_body(input: &CreateSavedSearch) -> Value {
    json!({
        "id": null, "type": SEARCH_TYPE, "userKey": null, "searchKey": SEARCH_KEY,
        "description": input.name, "changed": null, "created": null, "deleted": false,
        "notifications": input.notifications, "parameters": input.parameters,
        "searchKeyDescription": null, "vertical": null, "verticalDescription": null
    })
}

fn update_body(
    current: &Value,
    name: Option<String>,
    notifications: Option<Vec<String>>,
) -> Result<Value, AppError> {
    let object = current.as_object().ok_or_else(unexpected_read)?;
    let required = |key: &str| object.get(key).cloned().ok_or_else(unexpected_read);
    Ok(json!({
        "id": required("id")?, "type": SEARCH_TYPE, "userKey": null,
        "searchKey": required("searchKey")?,
        "description": name.map(Value::String).unwrap_or(required("description")?),
        "changed": object.get("changed").cloned().unwrap_or(Value::Null),
        "created": object.get("created").cloned().unwrap_or(Value::Null),
        "deleted": object.get("deleted").cloned().unwrap_or(Value::Bool(false)),
        "notifications": notifications.map(|value| json!(value)).unwrap_or(required("notifications")?),
        "parameters": required("parameters")?, "searchKeyDescription": null,
        "vertical": null, "verticalDescription": null
    }))
}

fn normalize(raw: &Value) -> Result<SavedSearch, AppError> {
    let object = raw.as_object().ok_or_else(unexpected_read)?;
    let id = object
        .get("id")
        .and_then(opaque)
        .ok_or_else(unexpected_read)?;
    let strings = |key: &str| -> Vec<String> {
        object
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(opaque)
            .collect()
    };
    let parameters = object
        .get("parameters")
        .and_then(Value::as_object)
        .map(|map| {
            map.iter()
                .map(|(key, value)| {
                    (
                        key.clone(),
                        value
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(opaque)
                            .collect(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let mut notifications = strings("notifications");
    notifications.sort();
    Ok(SavedSearch {
        id,
        name: object
            .get("description")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        search_key: object
            .get("searchKey")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        search_type: object
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        notifications,
        parameters,
        created: object.get("created").and_then(opaque),
        changed: object.get("changed").and_then(opaque),
        deleted: object
            .get("deleted")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        vertical: object
            .get("vertical")
            .and_then(Value::as_str)
            .map(str::to_owned),
        vertical_description: object
            .get("verticalDescription")
            .and_then(Value::as_str)
            .map(str::to_owned),
    })
}

fn opaque(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn intended_create(raw: &Value, input: &CreateSavedSearch) -> bool {
    raw.get("type").and_then(Value::as_str) == Some(SEARCH_TYPE)
        && raw.get("searchKey").and_then(Value::as_str) == Some(SEARCH_KEY)
        && raw.get("description").and_then(Value::as_str) == Some(&input.name)
        && notifications_match(raw.get("notifications"), Some(&json!(input.notifications)))
        && recovered_parameters_match(raw.get("parameters"), &input.parameters)
}

fn recovered_parameters_match(
    candidate: Option<&Value>,
    intended: &BTreeMap<String, Vec<String>>,
) -> bool {
    let Some(mut candidate) = candidate.and_then(Value::as_object).cloned() else {
        return false;
    };
    candidate.remove("stored-id");
    Value::Object(candidate) == serde_json::to_value(intended).unwrap_or(Value::Null)
}

fn intended_update(observed: &Value, intended: &Value) -> bool {
    ["id", "description", "parameters"]
        .iter()
        .all(|key| observed.get(key) == intended.get(key))
        && notifications_match(observed.get("notifications"), intended.get("notifications"))
}

fn notifications_match(left: Option<&Value>, right: Option<&Value>) -> bool {
    let sorted = |value: Option<&Value>| {
        let mut values = value
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(opaque)
            .collect::<Vec<_>>();
        values.sort();
        values
    };
    sorted(left) == sorted(right)
}

fn validate_id(id: &str) -> Result<(), AppError> {
    let length = id.chars().count();
    if length == 0 || length > 128 || id.chars().any(char::is_whitespace) {
        return Err(AppError::usage(
            "saved search ID must contain 1 through 128 non-whitespace characters",
        ));
    }
    Ok(())
}
fn validate_name(name: &str) -> Result<(), AppError> {
    if name.trim().is_empty() || name.chars().count() > 200 {
        return Err(AppError::usage(
            "saved search name must contain 1 through 200 characters",
        ));
    }
    Ok(())
}
fn not_found(id: &str) -> AppError {
    AppError::validation("saved_search.not_found", "the saved search was not found")
        .with_details(json!({"id": id}))
}
fn unexpected_read() -> AppError {
    AppError::new(
        "upstream.unexpected_response",
        "Tori returned an unexpected saved search response",
        ExitClass::Upstream,
    )
    .with_observation(
        Observation::unrecognized_response("saved_search_response", Some(200)),
        ObservationOperation::Read,
    )
}
fn recovery_observation(result: &Result<Vec<Value>, SavedSearchApiError>) -> Observation {
    match result {
        Ok(_) => Observation::unrecognized_response("saved_search_list", Some(200)),
        Err(SavedSearchApiError::Transport) => {
            Observation::temporarily_unavailable("saved_search_list", None, false)
        }
        Err(_) => Observation::unrecognized_response("saved_search_list", None),
    }
}
fn api_error(error: SavedSearchApiError, id: Option<&str>, read: bool) -> AppError {
    let mut result = base_error(&error);
    if matches!(error, SavedSearchApiError::Authentication) {
        result.next_actions.push(NextAction {
            command: crate::invocation::tori("auth login"),
        });
    }
    if let Some(id) = id {
        result.next_actions.push(NextAction {
            command: format!("flea tori saved-search show {id}"),
        });
    }
    if read {
        result = result.with_observation(
            recovery_observation(&Err(error)),
            ObservationOperation::Read,
        );
    }
    result
}
fn mutation_error(
    error: SavedSearchApiError,
    id: Option<&str>,
    observation: Observation,
    proven_absent: bool,
) -> AppError {
    let mut result = base_error(&error);
    result.code = "mutation.uncertain".to_owned();
    result.message = "the saved search mutation outcome required recovery verification".to_owned();
    result = result
        .with_observation(observation, ObservationOperation::PostMutationVerification)
        .retry_classification(RetryClassification {
            upstream_transient: matches!(
                error,
                SavedSearchApiError::Transport | SavedSearchApiError::Upstream(500..=599)
            ),
            safe_to_retry: proven_absent,
        });
    result.next_actions.push(NextAction {
        command: id.map_or_else(
            || "flea tori saved-search list".to_owned(),
            |id| format!("flea tori saved-search show {id}"),
        ),
    });
    result
}
fn base_error(error: &SavedSearchApiError) -> AppError {
    match error {
        SavedSearchApiError::Authentication => AppError::authentication(
            "auth.required",
            "Tori rejected authentication for the saved search request",
        ),
        SavedSearchApiError::NotFound => {
            AppError::validation("saved_search.not_found", "the saved search was not found")
        }
        SavedSearchApiError::Rejected => AppError::validation(
            "saved_search.rejected",
            "Tori rejected the saved search request",
        ),
        SavedSearchApiError::Transport => {
            AppError::upstream("upstream.transport", "the Tori saved search request failed")
        }
        SavedSearchApiError::Upstream(status) => AppError::new(
            "upstream.http",
            format!("the Tori saved search request failed with HTTP {status}"),
            ExitClass::Upstream,
        ),
        SavedSearchApiError::Unexpected => AppError::upstream(
            "upstream.unexpected_response",
            "Tori returned an unexpected saved search response",
        ),
    }
}
