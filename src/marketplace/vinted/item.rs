use std::{future::Future, pin::Pin, str::FromStr};

use reqwest::{Method, StatusCode};
use serde_json::{Map, Number, Value};
use url::Url;

use crate::{
    domain::{
        envelope::NextAction,
        item::ItemImage,
        search::SearchPrice,
        vinted_item::{
            VintedItemDetail, VintedItemSeller, VintedSellerDisclosedLocation,
            VintedSellerLocationSource,
        },
    },
    error::{AppError, ExitClass},
    marketplace::{
        PortalId,
        vinted::{
            auth::{VintedAuthentication, VintedCredentialRecord},
            binding::VINTED_FI_BINDING,
        },
    },
    transport::{Transport, TransportError, TransportErrorKind, TransportResponse},
};

const ITEM_PATH_PREFIX: &str = "/item-details/item/";
const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VintedItemRequest {
    pub item_id: String,
    pub raw: bool,
}

#[derive(Debug, PartialEq)]
pub enum VintedItemResult {
    Detail(Box<VintedItemDetail>),
    Raw(Value),
}

pub trait VintedItemSession: Send + Sync {
    fn credentials(&self, portal: PortalId) -> Result<VintedCredentialRecord, AppError>;
}

impl<F> VintedItemSession for F
where
    F: Fn(PortalId) -> Result<VintedCredentialRecord, AppError> + Send + Sync,
{
    fn credentials(&self, portal: PortalId) -> Result<VintedCredentialRecord, AppError> {
        self(portal)
    }
}

pub trait VintedItemApi: Send + Sync {
    fn item<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;
}

pub struct VintedItems<'a> {
    session: &'a dyn VintedItemSession,
    api: &'a dyn VintedItemApi,
}

impl<'a> VintedItems<'a> {
    pub fn new(session: &'a dyn VintedItemSession, api: &'a dyn VintedItemApi) -> Self {
        Self { session, api }
    }

    pub async fn execute(
        &self,
        portal: PortalId,
        request: VintedItemRequest,
    ) -> Result<VintedItemResult, AppError> {
        validate_item_id(&request.item_id)?;
        let credentials = self.session.credentials(portal)?;
        let raw = self.api.item(&credentials, &request.item_id).await?;
        let detail = normalize_item(&raw, &request.item_id)?;
        if request.raw {
            Ok(VintedItemResult::Raw(raw))
        } else {
            Ok(VintedItemResult::Detail(Box::new(detail)))
        }
    }
}

pub struct HttpVintedItemApi {
    auth: VintedAuthentication,
    api_base_url: String,
}

impl HttpVintedItemApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            api_base_url: VINTED_FI_BINDING.api_host.to_owned(),
        }
    }

    async fn execute_request(
        &self,
        credentials: &VintedCredentialRecord,
        item_id: &str,
    ) -> Result<Value, AppError> {
        let url = item_url(&self.api_base_url, item_id)?;
        let request = self.auth.authenticated_request(
            Method::GET,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        let response = self
            .auth
            .executor()
            .execute(request)
            .await
            .map_err(execution_error)?;
        if !response.status.is_success() {
            return Err(status_error(response.status, item_id));
        }
        decode_json(response)
    }

    #[cfg(test)]
    fn with_api_base_url(mut self, api_base_url: String) -> Self {
        self.api_base_url = api_base_url;
        self
    }
}

impl VintedItemApi for HttpVintedItemApi {
    fn item<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.execute_request(credentials, item_id))
    }
}

impl Default for HttpVintedItemApi {
    fn default() -> Self {
        Self::new()
    }
}

fn item_url(base_url: &str, item_id: &str) -> Result<Url, AppError> {
    let mut url = Url::parse(base_url).map_err(|error| {
        AppError::unexpected("Vinted API binding is invalid").with_source(error)
    })?;
    url.set_path(&format!("{ITEM_PATH_PREFIX}{item_id}"));
    Ok(url)
}

fn decode_json(response: TransportResponse) -> Result<Value, AppError> {
    serde_json::from_slice(&response.body)
        .map_err(|_| unexpected_response("response was not valid JSON"))
}

pub(crate) fn normalize_item(raw: &Value, expected_id: &str) -> Result<VintedItemDetail, AppError> {
    let body = raw.get("data").unwrap_or(raw);
    let root = body
        .as_object()
        .ok_or_else(|| unexpected_response("response was not an object"))?;
    let item = root
        .get("item")
        .and_then(Value::as_object)
        .ok_or_else(|| unexpected_response("item details were unavailable"))?;
    root.get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| unexpected_response("item plugins were unavailable"))?;

    let returned_id =
        identifier(item.get("id")).ok_or_else(|| unexpected_response("item ID was unavailable"))?;
    if returned_id != expected_id {
        return Err(unexpected_response("response returned a different item ID"));
    }
    let title = required_string(item, "title", "item title was unavailable")?;
    let description = required_string(item, "description", "item description was unavailable")?;
    let user = item
        .get("user")
        .and_then(Value::as_object)
        .ok_or_else(|| unexpected_response("seller details were unavailable"))?;

    let seller_disclosed_location = business_location(root).or_else(|| user_location(user));
    let display_name = string(user.get("login"));
    let business = user
        .get("business")
        .and_then(Value::as_bool)
        .or_else(|| user.get("business_account").and_then(Value::as_bool));

    Ok(VintedItemDetail {
        listing_id: returned_id.clone(),
        title,
        description,
        price: item.get("price").and_then(normalize_price),
        canonical_url: item
            .get("url")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(|value| absolute_url(value, &returned_id))
            .unwrap_or_else(|| format!("{}/items/{returned_id}", VINTED_FI_BINDING.host)),
        images: normalize_images(item),
        seller: VintedItemSeller {
            display_name,
            business,
            seller_disclosed_location,
        },
    })
}

fn user_location(user: &Map<String, Value>) -> Option<VintedSellerDisclosedLocation> {
    if user.get("expose_location").and_then(Value::as_bool) != Some(true) {
        return None;
    }
    string(user.get("city"))
        .map(|name| VintedSellerDisclosedLocation {
            name,
            source: VintedSellerLocationSource::City,
        })
        .or_else(|| {
            string(user.get("country_title_local")).map(|name| VintedSellerDisclosedLocation {
                name,
                source: VintedSellerLocationSource::Country,
            })
        })
}

fn business_location(root: &Map<String, Value>) -> Option<VintedSellerDisclosedLocation> {
    root.get("plugins")
        .and_then(Value::as_array)?
        .iter()
        .filter_map(Value::as_object)
        .find(|plugin| plugin.get("type").and_then(Value::as_str) == Some("seller_info_business"))
        .and_then(|plugin| plugin.get("data"))
        .and_then(Value::as_object)
        .and_then(|data| string(data.get("seller_location")))
        .map(|name| VintedSellerDisclosedLocation {
            name,
            source: VintedSellerLocationSource::BusinessProfile,
        })
}

fn normalize_price(value: &Value) -> Option<SearchPrice> {
    let object = value.as_object()?;
    let amount = match object.get("amount")? {
        Value::Number(value) => Value::Number(value.clone()),
        Value::String(value) => Value::Number(Number::from_str(value).ok()?),
        _ => return None,
    };
    let currency = ["currency_code", "currencyCode", "currency"]
        .into_iter()
        .find_map(|key| string(object.get(key)));
    Some(SearchPrice { amount, currency })
}

fn normalize_images(item: &Map<String, Value>) -> Vec<ItemImage> {
    item.get("photos")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
        .filter_map(|photo| {
            Some(ItemImage {
                url: string(photo.get("url"))?,
                width: photo.get("width").and_then(Value::as_u64),
                height: photo.get("height").and_then(Value::as_u64),
                description: None,
            })
        })
        .collect()
}

fn absolute_url(value: &str, item_id: &str) -> String {
    if value.starts_with("https://") {
        value.to_owned()
    } else if value.starts_with('/') {
        format!("{}{value}", VINTED_FI_BINDING.host)
    } else {
        format!("{}/items/{item_id}", VINTED_FI_BINDING.host)
    }
}

fn required_string(
    object: &Map<String, Value>,
    key: &str,
    reason: &str,
) -> Result<String, AppError> {
    string(object.get(key)).ok_or_else(|| unexpected_response(reason))
}

fn identifier(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) if !value.is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
}

pub(crate) fn validate_item_id(item_id: &str) -> Result<(), AppError> {
    if item_id.is_empty()
        || item_id.len() > 20
        || !item_id.bytes().all(|byte| byte.is_ascii_digit())
        || item_id.parse::<u64>().ok().filter(|id| *id > 0).is_none()
    {
        let mut error = AppError::validation(
            "vinted_item.invalid_id",
            "Vinted item ID must be a positive numeric ID returned by search",
        )
        .with_details(serde_json::json!({ "item_id": item_id }));
        add_search_action(&mut error);
        return Err(error);
    }
    Ok(())
}

fn transport_error(error: TransportError) -> AppError {
    let mut result = AppError::upstream(
        "vinted_item.transport_failed",
        "Vinted item details could not be reached",
    )
    .with_source(error);
    result.upstream_transient = true;
    result.safe_to_retry = true;
    result
}

fn execution_error(error: TransportError) -> AppError {
    if let Some(status) = error.status
        && !status.is_success()
    {
        return status_error(status, "unknown");
    }
    if error.kind == TransportErrorKind::ResponseTooLarge {
        unexpected_response("response exceeded the size limit")
    } else {
        transport_error(error)
    }
}

fn status_error(status: StatusCode, item_id: &str) -> AppError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        let mut error = AppError::authentication(
            "vinted_item.authentication_required",
            "Vinted item inspection requires a valid authenticated session",
        );
        error.next_actions.push(NextAction {
            command: "flea vinted --portal fi auth login".to_owned(),
        });
        return error;
    }
    if matches!(status, StatusCode::NOT_FOUND | StatusCode::GONE) {
        let mut error = AppError::validation(
            "vinted_item.not_found",
            "Vinted item was not found; it may have been removed or sold",
        )
        .with_details(serde_json::json!({ "item_id": item_id }));
        add_search_action(&mut error);
        return error;
    }
    let mut error = AppError::new(
        "vinted_item.upstream_failed",
        format!("Vinted item details returned HTTP {}", status.as_u16()),
        ExitClass::Upstream,
    );
    error.upstream_transient = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
    error.safe_to_retry = true;
    error
}

fn unexpected_response(reason: &str) -> AppError {
    AppError::upstream(
        "vinted_item.unexpected_response",
        "Vinted returned an unsupported item-detail response",
    )
    .with_details(serde_json::json!({ "reason": reason }))
}

fn add_search_action(error: &mut AppError) {
    error.next_actions.push(NextAction {
        command: "flea vinted --portal fi search".to_owned(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn central_item_route_uses_the_active_api_host_contract() {
        let url = item_url("https://api.vinted.com", "9757271392").unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.vinted.com/item-details/item/9757271392"
        );
    }

    #[test]
    fn local_validation_accepts_only_positive_u64_ids() {
        assert!(validate_item_id("9757271392").is_ok());
        for invalid in ["", "0", "-1", "1.2", "../1", "18446744073709551616"] {
            assert_eq!(
                validate_item_id(invalid).unwrap_err().code,
                "vinted_item.invalid_id"
            );
        }
    }

    #[test]
    fn status_errors_distinguish_authentication_absence_and_upstream_failure() {
        let auth = status_error(StatusCode::UNAUTHORIZED, "101");
        assert_eq!(auth.code, "vinted_item.authentication_required");
        assert_eq!(auth.exit_class, ExitClass::Authentication);

        for status in [StatusCode::NOT_FOUND, StatusCode::GONE] {
            let absent = status_error(status, "101");
            assert_eq!(absent.code, "vinted_item.not_found");
            assert_eq!(absent.exit_class, ExitClass::Validation);
        }

        let unavailable = status_error(StatusCode::SERVICE_UNAVAILABLE, "101");
        assert_eq!(unavailable.code, "vinted_item.upstream_failed");
        assert!(unavailable.upstream_transient);
        assert!(unavailable.safe_to_retry);
    }

    #[test]
    fn transport_and_oversized_responses_have_distinct_errors() {
        let transport = transport_error(TransportError::request(TransportErrorKind::Connection));
        assert_eq!(transport.code, "vinted_item.transport_failed");
        assert!(transport.upstream_transient);

        let oversized = execution_error(TransportError::response(
            TransportErrorKind::ResponseTooLarge,
            StatusCode::OK,
        ));
        assert_eq!(oversized.code, "vinted_item.unexpected_response");
    }

    #[test]
    fn test_client_can_override_the_api_host() {
        let api = HttpVintedItemApi::new().with_api_base_url("http://127.0.0.1:1".to_owned());
        assert_eq!(api.api_base_url, "http://127.0.0.1:1");
    }
}
