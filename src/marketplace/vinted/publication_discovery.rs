use std::{future::Future, pin::Pin};

use reqwest::{
    Method,
    header::{HeaderName, HeaderValue},
};
use serde_json::{Value, json};
use url::Url;

use crate::{
    error::AppError,
    marketplace::vinted::{
        auth::{VintedAuthentication, VintedCredentialRecord},
        binding::VINTED_FI_BINDING,
    },
    transport::{RequestBody, Transport, TransportError, TransportErrorKind, TransportResponse},
};

const API_V2_PATH: &str = "/api/v2/";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DiscoveryRequest {
    Catalogs,
    SearchCatalog { keyword: String },
    Attributes { selections: Value },
    Brands { category_id: u64, keyword: String },
    Colors,
    Configuration,
    PackageSizes { category_id: u64 },
}

pub trait VintedPublicationDiscoveryApi: Send + Sync {
    fn execute<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a DiscoveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;
}

pub struct HttpVintedPublicationDiscoveryApi {
    auth: VintedAuthentication,
    api_base_url: String,
}

impl HttpVintedPublicationDiscoveryApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            api_base_url: VINTED_FI_BINDING.api_host.to_owned(),
        }
    }

    async fn execute_request(
        &self,
        credentials: &VintedCredentialRecord,
        request: &DiscoveryRequest,
    ) -> Result<Value, AppError> {
        let (method, path) = endpoint(request);
        let mut url = self.url(&path)?;
        apply_query(&mut url, request);
        let mut transport_request = self.auth.authenticated_request(
            method,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        match request {
            DiscoveryRequest::Catalogs => {
                insert_header(&mut transport_request.headers, "mda-catalog", "true")
            }
            DiscoveryRequest::Brands { .. } => {
                insert_header(&mut transport_request.headers, "mda-brand", "true")
            }
            DiscoveryRequest::Attributes { selections } => {
                insert_header(&mut transport_request.headers, "accept-features", "ALL");
                insert_header(
                    &mut transport_request.headers,
                    "content-type",
                    "application/json",
                );
                transport_request.body = RequestBody::Bytes(
                    serde_json::to_vec(&json!({ "attributes": selections })).map_err(|error| {
                        AppError::unexpected("Failed to serialize Vinted attribute discovery")
                            .with_source(error)
                    })?,
                );
            }
            _ => {}
        }
        let response = self
            .auth
            .executor()
            .execute(transport_request)
            .await
            .map_err(execution_error)?;
        let status = response.status;
        let value = bounded_json(response)?;
        if status.is_success() {
            Ok(value)
        } else {
            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("Vinted rejected publication discovery");
            let mut error = AppError::upstream("vinted.discovery_failed", message);
            error.details = Some(Box::new(json!({
                "http_status": status.as_u16(),
                "code": value.get("code"),
                "message_code": value.get("message_code"),
                "errors": value.get("errors")
            })));
            error.safe_to_retry = status.is_server_error();
            error.upstream_transient = status.is_server_error();
            Err(error)
        }
    }

    fn url(&self, path: &str) -> Result<Url, AppError> {
        let mut url = Url::parse(&self.api_base_url).map_err(|error| {
            AppError::unexpected("Vinted API binding is invalid").with_source(error)
        })?;
        url.set_path(&format!("{API_V2_PATH}{path}"));
        Ok(url)
    }

    #[cfg(test)]
    fn with_api_base_url(mut self, api_base_url: String) -> Self {
        self.api_base_url = api_base_url;
        self
    }
}

impl Default for HttpVintedPublicationDiscoveryApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedPublicationDiscoveryApi for HttpVintedPublicationDiscoveryApi {
    fn execute<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        request: &'a DiscoveryRequest,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.execute_request(credentials, request))
    }
}

pub fn validate_request(request: &DiscoveryRequest) -> Result<(), AppError> {
    match request {
        DiscoveryRequest::SearchCatalog { keyword } | DiscoveryRequest::Brands { keyword, .. }
            if keyword.len() > 256 =>
        {
            Err(AppError::usage(
                "Discovery keyword must be at most 256 bytes",
            ))
        }
        DiscoveryRequest::Attributes { selections } if !selections.is_array() => {
            Err(AppError::usage("Attribute selections must be a JSON array"))
        }
        DiscoveryRequest::Attributes { selections }
            if selections.as_array().is_none_or(|values| values.is_empty()) =>
        {
            Err(AppError::usage(
                "Attribute selections must include at least one runtime selection",
            ))
        }
        DiscoveryRequest::Attributes { selections }
            if selections.as_array().is_some_and(|values| {
                values.iter().any(|value| {
                    value
                        .get("code")
                        .and_then(Value::as_str)
                        .is_none_or(str::is_empty)
                        || value.get("value").and_then(Value::as_array).is_none()
                })
            }) =>
        {
            Err(AppError::usage(
                "Each attribute selection requires a nonempty code and value array",
            ))
        }
        _ => Ok(()),
    }
}

fn endpoint(request: &DiscoveryRequest) -> (Method, String) {
    match request {
        DiscoveryRequest::Catalogs => (Method::GET, "item_upload/catalogs".to_owned()),
        DiscoveryRequest::SearchCatalog { .. } => {
            (Method::GET, "item_upload/catalogs/search".to_owned())
        }
        DiscoveryRequest::Attributes { .. } => (Method::POST, "item_upload/attributes".to_owned()),
        DiscoveryRequest::Brands { .. } => (Method::GET, "item_upload/brands".to_owned()),
        DiscoveryRequest::Colors => (Method::GET, "item_upload/colors".to_owned()),
        DiscoveryRequest::Configuration => (Method::GET, "items/configuration".to_owned()),
        DiscoveryRequest::PackageSizes { category_id } => (
            Method::GET,
            format!("shipping-estimation/external/catalogs/{category_id}/package_sizes"),
        ),
    }
}

fn apply_query(url: &mut Url, request: &DiscoveryRequest) {
    let mut query = url.query_pairs_mut();
    match request {
        DiscoveryRequest::SearchCatalog { keyword } => {
            query.append_pair("keyword", keyword);
        }
        DiscoveryRequest::Brands {
            category_id,
            keyword,
        } => {
            query.append_pair("category_id", &category_id.to_string());
            query.append_pair("keyword", keyword);
        }
        _ => {}
    }
}

fn bounded_json(response: TransportResponse) -> Result<Value, AppError> {
    serde_json::from_slice(&response.body).map_err(|error| invalid_response().with_source(error))
}

fn invalid_response() -> AppError {
    AppError::upstream(
        "vinted.discovery_invalid_response",
        "Vinted returned an invalid publication discovery response",
    )
}

fn insert_header(
    headers: &mut reqwest::header::HeaderMap,
    name: &'static str,
    value: &'static str,
) {
    headers.insert(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    );
}

fn transport_error(error: TransportError) -> AppError {
    let mut app_error = AppError::upstream(
        "vinted.discovery_transport_failed",
        "Vinted publication discovery failed",
    )
    .with_source(error);
    app_error.upstream_transient = true;
    app_error.safe_to_retry = true;
    app_error
}

fn execution_error(error: TransportError) -> AppError {
    if error.kind == TransportErrorKind::ResponseTooLarge {
        invalid_response()
    } else {
        transport_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_match_source_derived_contract() {
        assert_eq!(
            endpoint(&DiscoveryRequest::PackageSizes { category_id: 42 }),
            (
                Method::GET,
                "shipping-estimation/external/catalogs/42/package_sizes".to_owned()
            )
        );
        assert_eq!(
            endpoint(&DiscoveryRequest::Attributes {
                selections: json!([{"code": "runtime_code", "value": ["runtime_value"]}])
            }),
            (Method::POST, "item_upload/attributes".to_owned())
        );
    }

    #[test]
    fn attribute_discovery_requires_a_nonempty_array() {
        assert!(
            validate_request(&DiscoveryRequest::Attributes {
                selections: json!([])
            })
            .is_err()
        );
        assert!(
            validate_request(&DiscoveryRequest::Attributes {
                selections: json!([{"code": "runtime_code", "value": ["runtime_value"]}])
            })
            .is_ok()
        );
    }

    #[test]
    fn test_api_base_url_can_be_rebound() {
        let api = HttpVintedPublicationDiscoveryApi::new()
            .with_api_base_url("http://127.0.0.1:9".to_owned());
        assert_eq!(
            api.url("item_upload/catalogs").unwrap().as_str(),
            "http://127.0.0.1:9/api/v2/item_upload/catalogs"
        );
    }
}
