use std::{collections::BTreeMap, fmt, future::Future, pin::Pin, sync::Arc};

use reqwest::{Method, StatusCode, header::HeaderValue};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

use super::{TaxonomyApi, UpstreamCategory, UpstreamCategoryTaxonomy};
use crate::marketplace::tori::client::{
    HttpFailure, RequestSpec, ToriClient, compatibility, map_http_error,
};

pub trait ListingsApi: Send + Sync {
    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListingPage, ListingsApiError>> + Send + '_>>;
    fn listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>>;
    fn update_listing<'a>(
        &'a self,
        listing_id: &'a str,
        etag: &'a str,
        fields: &'a BTreeMap<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>>;
    fn dispose_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>>;
    fn delete_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>>;
}

pub struct HttpListingsApi {
    client: Arc<dyn ToriClient>,
}

impl HttpListingsApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }

    async fn request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        body: Option<Value>,
        etag: Option<&str>,
    ) -> Result<T, ListingsApiError> {
        let mut request = RequestSpec::new(method, path, compatibility::SERVICE_AD_SUMMARIES);
        if let Some(body) = body {
            request = request.body(
                serde_json::to_vec(&body)
                    .map_err(|error| ListingsApiError::UnexpectedResponse(error.to_string()))?,
                HeaderValue::from_static("application/json"),
            );
        }
        if let Some(etag) = etag {
            request = request
                .if_match(HeaderValue::from_str(etag).map_err(|_| {
                    ListingsApiError::UnexpectedResponse("invalid ETag".to_owned())
                })?);
        }
        let response = self
            .client
            .execute(request)
            .await
            .map_err(map_http_error::<ListingsApiError>)?;
        decode_response(response.status, &response.body)
    }

    async fn request_with_service<T: DeserializeOwned>(
        &self,
        method: Method,
        path: String,
        service: &str,
    ) -> Result<T, ListingsApiError> {
        let request = RequestSpec::new(method, path, service);
        let response = self
            .client
            .execute(request)
            .await
            .map_err(map_http_error::<ListingsApiError>)?;
        decode_response(response.status, &response.body)
    }

    async fn empty(&self, method: Method, path: String) -> Result<(), ListingsApiError> {
        self.request::<Value>(method, path, None, None)
            .await
            .map(|_| ())
    }
}

impl From<HttpFailure> for ListingsApiError {
    fn from(failure: HttpFailure) -> Self {
        match failure {
            HttpFailure::Transport(_) => Self::Transport,
            HttpFailure::Local(_) => Self::UnexpectedResponse("HTTP adapter failed".to_owned()),
        }
    }
}

fn decode_response<T: DeserializeOwned>(
    status: StatusCode,
    body: &[u8],
) -> Result<T, ListingsApiError> {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            return Err(ListingsApiError::Authentication);
        }
        StatusCode::NOT_FOUND => return Err(ListingsApiError::NotFound),
        StatusCode::PRECONDITION_FAILED | StatusCode::CONFLICT => {
            return Err(ListingsApiError::Conflict);
        }
        status if !status.is_success() => {
            return Err(ListingsApiError::Upstream(status.as_u16()));
        }
        _ => {}
    }
    if body.is_empty() {
        serde_json::from_value(Value::Null)
    } else {
        serde_json::from_slice(body)
    }
    .map_err(|error| ListingsApiError::UnexpectedResponse(error.to_string()))
}

impl TaxonomyApi for HttpListingsApi {
    fn categories(
        &self,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<UpstreamCategory>, ListingsApiError>> + Send + '_>>
    {
        Box::pin(async move {
            self.request_with_service::<UpstreamCategoryTaxonomy>(
                Method::GET,
                "/categories/taxonomy".to_owned(),
                compatibility::SERVICE_ITEM_CREATION,
            )
            .await
            .map(|taxonomy| taxonomy.categories)
        })
    }
}

impl ListingsApi for HttpListingsApi {
    fn listing_page(
        &self,
        offset: usize,
        limit: usize,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListingPage, ListingsApiError>> + Send + '_>>
    {
        Box::pin(self.request(
            Method::GET,
            format!("/search?limit={limit}&offset={offset}"),
            None,
            None,
        ))
    }

    fn listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>> {
        Box::pin(self.request(Method::GET, format!("/{listing_id}"), None, None))
    }

    fn update_listing<'a>(
        &'a self,
        listing_id: &'a str,
        etag: &'a str,
        fields: &'a BTreeMap<String, Value>,
    ) -> Pin<Box<dyn Future<Output = Result<UpstreamListing, ListingsApiError>> + Send + 'a>> {
        Box::pin(self.request(
            Method::PUT,
            format!("/my/listings/{listing_id}"),
            Some(Value::Object(fields.clone().into_iter().collect())),
            Some(etag),
        ))
    }

    fn dispose_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>> {
        Box::pin(self.empty(Method::POST, format!("/my/listings/{listing_id}/dispose")))
    }

    fn delete_listing<'a>(
        &'a self,
        listing_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<(), ListingsApiError>> + Send + 'a>> {
        Box::pin(self.empty(Method::DELETE, format!("/my/listings/{listing_id}")))
    }
}

#[derive(Clone, thiserror::Error, PartialEq)]
pub enum ListingsApiError {
    #[error("authentication failed")]
    Authentication,
    #[error("resource was not found")]
    NotFound,
    #[error("the resource changed remotely")]
    Conflict,
    #[cfg(test)]
    #[error("listing validation failed: {message}")]
    Validation {
        message: String,
        fields: BTreeMap<String, String>,
    },
    #[error("listing transport failed")]
    Transport,
    #[error("Tori listing service returned HTTP {0}")]
    Upstream(u16),
    #[error("unexpected upstream response: {0}")]
    UnexpectedResponse(String),
}

impl fmt::Debug for ListingsApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Authentication => formatter.write_str("Authentication"),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Conflict => formatter.write_str("Conflict"),
            #[cfg(test)]
            Self::Validation { fields, .. } => formatter
                .debug_struct("Validation")
                .field("message", &"[REDACTED]")
                .field("field_names", &fields.keys().collect::<Vec<_>>())
                .finish(),
            Self::Transport => formatter.write_str("Transport"),
            Self::Upstream(status) => formatter.debug_tuple("Upstream").field(status).finish(),
            Self::UnexpectedResponse(_) => formatter.write_str("UnexpectedResponse([REDACTED])"),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UpstreamListingPage {
    #[serde(default, alias = "listings")]
    pub summaries: Vec<UpstreamListingSummary>,
    pub total: usize,
    #[serde(default)]
    pub facets: Vec<UpstreamFacet>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UpstreamListingSummary {
    pub id: Value,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub updated: Option<String>,
    #[serde(default)]
    pub expires: Option<String>,
    #[serde(default, rename = "daysUntilExpires", alias = "days_until_expires")]
    pub days_until_expires: Option<u64>,
    #[serde(default)]
    pub state: UpstreamState,
    #[serde(default)]
    pub actions: Vec<UpstreamAction>,
    #[serde(default)]
    pub data: UpstreamSummaryData,
    #[serde(default, rename = "externalData", alias = "external_data")]
    pub external_data: UpstreamStatistics,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamState {
    #[serde(default)]
    pub label: String,
    #[serde(default, rename = "type")]
    pub state_type: String,
    #[serde(default)]
    pub display: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamSummaryData {
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub subtitle: String,
    #[serde(default)]
    pub image: String,
    #[serde(default, alias = "area", alias = "place")]
    pub location: String,
    #[serde(default, alias = "url", alias = "publicUrl")]
    pub public_url: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamStatistics {
    #[serde(default, alias = "views")]
    pub clicks: UpstreamStatistic,
    #[serde(default)]
    pub favorites: UpstreamStatistic,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamStatistic {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub value: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamAction {
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub name: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpstreamFacet {
    #[serde(default)]
    pub label: String,
    pub name: String,
    pub total: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct UpstreamListing {
    pub id: Value,
    #[serde(default)]
    pub etag: String,
    #[serde(default)]
    pub state: UpstreamState,
    #[serde(default)]
    pub fields: BTreeMap<String, Value>,
    #[serde(default)]
    pub data: UpstreamSummaryData,
    #[serde(default)]
    pub actions: Vec<UpstreamAction>,
    #[serde(default, rename = "externalData", alias = "external_data")]
    pub external_data: UpstreamStatistics,
}
