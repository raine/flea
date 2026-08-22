use std::{fmt, sync::Arc};

use reqwest::{Method, StatusCode};
use serde_json::{Map, Value, json};

use crate::{
    api::client::{HttpError, RequestSpec, ToriClient, TransportErrorKind, compatibility},
    domain::item::{
        ItemAttribute, ItemImage, ItemLocation, ItemPrice, ItemSeller, ItemShipping,
        PublicItemDetail,
    },
    error::{AppError, ExitClass},
    retry::{FailureKind, OperationMethod, RetryClassification, RetryContext, classify},
};

pub trait PublicItemApi: Send + Sync {
    fn item(&self, listing_id: &str) -> Result<Value, PublicItemApiError>;
}

pub struct HttpPublicItemApi {
    client: Arc<dyn ToriClient>,
}

impl HttpPublicItemApi {
    pub fn new(client: Arc<dyn ToriClient>) -> Self {
        Self { client }
    }
}

impl PublicItemApi for HttpPublicItemApi {
    fn item(&self, listing_id: &str) -> Result<Value, PublicItemApiError> {
        let request = RequestSpec::new(
            Method::GET,
            format!("/adview/{listing_id}"),
            compatibility::SERVICE_ADVIEW,
        );
        let client = Arc::clone(&self.client);
        let response = std::thread::scope(|scope| {
            scope
                .spawn(move || {
                    tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| {
                            PublicItemApiError::Unexpected("HTTP runtime failed".to_owned())
                        })?
                        .block_on(client.execute(request))
                        .map_err(item_http_error)
                })
                .join()
                .map_err(|_| PublicItemApiError::Unexpected("HTTP worker failed".to_owned()))?
        })?;

        if !response.status.is_success() {
            return Err(match response.status {
                StatusCode::BAD_REQUEST | StatusCode::UNPROCESSABLE_ENTITY => {
                    PublicItemApiError::Invalid
                }
                StatusCode::NOT_FOUND => PublicItemApiError::NotFound,
                StatusCode::GONE => PublicItemApiError::Expired,
                status => PublicItemApiError::Upstream(status.as_u16()),
            });
        }
        serde_json::from_slice(&response.body)
            .map_err(|_| PublicItemApiError::Unexpected("invalid JSON response".to_owned()))
    }
}

fn item_http_error(error: HttpError) -> PublicItemApiError {
    match error {
        HttpError::Transport(transport)
            if matches!(
                transport.kind,
                TransportErrorKind::Timeout | TransportErrorKind::Connection
            ) =>
        {
            PublicItemApiError::Transport(transport.to_string())
        }
        HttpError::InvalidRequest | HttpError::ResponseTooLarge | HttpError::Transport(_) => {
            PublicItemApiError::Unexpected("HTTP adapter failed".to_owned())
        }
    }
}

#[derive(Clone, thiserror::Error, PartialEq, Eq)]
pub enum PublicItemApiError {
    #[error("listing ID was rejected")]
    Invalid,
    #[error("listing was not found")]
    NotFound,
    #[error("listing has expired")]
    Expired,
    #[error("item transport failed")]
    Transport(String),
    #[error("Tori item service returned HTTP {0}")]
    Upstream(u16),
    #[error("unexpected item response: {0}")]
    Unexpected(String),
}

impl fmt::Debug for PublicItemApiError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid => formatter.write_str("Invalid"),
            Self::NotFound => formatter.write_str("NotFound"),
            Self::Expired => formatter.write_str("Expired"),
            Self::Upstream(status) => formatter.debug_tuple("Upstream").field(status).finish(),
            Self::Transport(_) => formatter.write_str("Transport([REDACTED])"),
            Self::Unexpected(_) => formatter.write_str("Unexpected([REDACTED])"),
        }
    }
}

pub struct PublicItems<'a> {
    api: &'a dyn PublicItemApi,
}

impl<'a> PublicItems<'a> {
    pub fn new(api: &'a dyn PublicItemApi) -> Self {
        Self { api }
    }

    pub fn show(&self, listing_id: &str) -> Result<(PublicItemDetail, Value), AppError> {
        validate_id(listing_id)?;
        let raw = self
            .api
            .item(listing_id)
            .map_err(|error| item_error(error, listing_id))?;
        let detail = normalize_item(&raw, listing_id)?;
        Ok((detail, raw))
    }
}

fn normalize_item(raw: &Value, expected_id: &str) -> Result<PublicItemDetail, AppError> {
    let root = raw
        .as_object()
        .ok_or_else(|| unexpected("item response must be an object"))?;
    let ad = root
        .get("ad")
        .and_then(Value::as_object)
        .or_else(|| root.get("itemData").and_then(Value::as_object))
        .ok_or_else(|| unexpected("item response omitted listing details"))?;
    let meta = root
        .get("meta")
        .and_then(Value::as_object)
        .or_else(|| ad.get("meta").and_then(Value::as_object));
    if let Some(returned_id) = meta
        .and_then(|meta| value_string(meta.get("adId").or_else(|| meta.get("listing_id"))))
        .or_else(|| value_string(ad.get("id")))
        && returned_id != expected_id
    {
        return Err(unexpected("item response returned a different listing ID"));
    }

    let attributes = normalize_attributes(ad.get("extras"));
    let condition = attributes
        .iter()
        .find(|attribute| attribute.machine_id.as_deref() == Some("condition"))
        .cloned()
        .or_else(|| ad.get("condition").and_then(normalize_attribute));
    let published_at = string_path(ad, &["publishedAt", "published_at", "published"])
        .or_else(|| meta.and_then(first_publication));
    let published_at_ms = number_path(ad, &["published_at_ms", "timestamp"])
        .or_else(|| meta.and_then(|meta| number_path(meta, &["published_at_ms", "timestamp"])));
    let canonical_url = string_path(ad, &["canonicalUrl", "canonical_url", "url", "canonical"])
        .or_else(|| string_path(root, &["canonicalUrl", "canonical_url", "url", "canonical"]))
        .or_else(|| meta.and_then(|meta| string_path(meta, &["canonicalUrl", "canonical"])))
        .or_else(|| {
            root.get("jsonLd")
                .and_then(Value::as_object)
                .and_then(|value| string_path(value, &["url"]))
        });

    Ok(PublicItemDetail {
        listing_id: expected_id.to_owned(),
        title: string_path(ad, &["title", "heading"]).unwrap_or_default(),
        description: string_path(ad, &["description", "body"]).unwrap_or_default(),
        price: normalize_price(ad),
        location: normalize_location(ad.get("location")),
        condition,
        seller: normalize_seller(root, ad),
        shipping: normalize_shipping(root, ad),
        images: normalize_images(ad),
        published_at,
        published_at_ms,
        canonical_url,
        trade_type: string_path(ad, &["adViewTypeLabel", "trade_type", "tradeType"]),
        category: normalize_category(ad.get("category")),
        attributes,
    })
}

fn normalize_price(ad: &Map<String, Value>) -> Option<ItemPrice> {
    let price = ad.get("price")?;
    if let Some(object) = price.as_object() {
        let amount = object
            .get("amount")
            .or_else(|| object.get("value"))?
            .clone();
        return Some(ItemPrice {
            amount,
            currency: string_path(object, &["currency", "currencyCode", "currency_code"]),
            display: string_path(object, &["display", "formatted", "text"]),
        });
    }
    if price.is_number() || price.is_string() {
        Some(ItemPrice {
            amount: price.clone(),
            currency: string_path(ad, &["currency", "currencyCode", "currency_code"]),
            display: string_path(ad, &["priceText", "price_text"]),
        })
    } else {
        None
    }
}

fn normalize_location(value: Option<&Value>) -> Option<ItemLocation> {
    match value? {
        Value::String(name) => Some(ItemLocation {
            name: nonempty(name),
            postal_code: None,
            country_code: None,
        }),
        Value::Object(object) => Some(ItemLocation {
            name: string_path(object, &["postalName", "name", "display_name"]),
            postal_code: string_path(object, &["postalCode", "postal_code"]),
            country_code: string_path(object, &["countryCode", "country_code"]),
        }),
        _ => None,
    }
}

fn normalize_attributes(value: Option<&Value>) -> Vec<ItemAttribute> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(normalize_attribute)
        .collect()
}

fn normalize_attribute(value: &Value) -> Option<ItemAttribute> {
    let object = value.as_object()?;
    Some(ItemAttribute {
        value: value_string(object.get("value"))?,
        label: string_path(object, &["label", "display_name", "name"]),
        machine_id: value_string(object.get("id").or_else(|| object.get("valueId"))),
    })
}

fn normalize_category(value: Option<&Value>) -> Vec<ItemAttribute> {
    let mut category = Vec::new();
    let mut current = value;
    while let Some(object) = current.and_then(Value::as_object) {
        if let Some(value) = value_string(object.get("value").or_else(|| object.get("name"))) {
            category.push(ItemAttribute {
                value,
                label: None,
                machine_id: value_string(object.get("id")),
            });
        }
        current = object.get("parent");
    }
    category.reverse();
    category
}

fn normalize_images(ad: &Map<String, Value>) -> Vec<ItemImage> {
    let mut images = Vec::new();
    if let Some(values) = ad.get("images").and_then(Value::as_array) {
        for value in values {
            match value {
                Value::String(url) if !url.is_empty() => images.push(ItemImage {
                    url: url.clone(),
                    width: None,
                    height: None,
                    description: None,
                }),
                Value::Object(object) => {
                    if let Some(url) = string_path(object, &["uri", "url", "src", "image_url"]) {
                        images.push(ItemImage {
                            url,
                            width: object.get("width").and_then(Value::as_u64),
                            height: object.get("height").and_then(Value::as_u64),
                            description: string_path(object, &["description", "alt"]),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    for url in ad
        .get("image_urls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !images.iter().any(|image| image.url == url) {
            images.push(ItemImage {
                url: url.to_owned(),
                width: None,
                height: None,
                description: None,
            });
        }
    }
    images
}

fn normalize_seller(root: &Map<String, Value>, ad: &Map<String, Value>) -> ItemSeller {
    let seller = ["seller", "profileData", "shopProfileData"]
        .into_iter()
        .find_map(|name| root.get(name).and_then(Value::as_object))
        .or_else(|| ad.get("seller").and_then(Value::as_object));
    let seller_type = seller
        .and_then(|seller| string_path(seller, &["type", "sellerType", "seller_type"]))
        .or_else(|| string_path(ad, &["sellerType", "seller_type"]));
    ItemSeller {
        seller_type,
        display_name: seller.and_then(|seller| {
            string_path(seller, &["displayName", "display_name", "name", "label"])
        }),
        profile_url: seller
            .and_then(|seller| string_path(seller, &["profileUrl", "profile_url", "url", "uri"])),
        verified: seller.and_then(|seller| bool_path(seller, &["verified", "isVerified"])),
    }
}

fn normalize_shipping(root: &Map<String, Value>, ad: &Map<String, Value>) -> ItemShipping {
    let shipping = root
        .get("transactableData")
        .or_else(|| root.get("shipping"))
        .or_else(|| ad.get("shippingInfo"))
        .or_else(|| ad.get("shipping"));
    match shipping {
        Some(Value::Bool(available)) => ItemShipping {
            available: Some(*available),
            ..ItemShipping::default()
        },
        Some(Value::Object(object)) => ItemShipping {
            available: bool_path(object, &["available", "shipping", "transactable"]),
            eligible: bool_path(object, &["eligible", "eligibleForShipping"]),
            seller_pays: bool_path(object, &["sellerPays", "sellerPaysShipping"]),
            buy_now: bool_path(object, &["buyNow", "buy_now"]),
            method: string_path(object, &["method", "type", "label"]),
            price: object
                .get("price")
                .or_else(|| object.get("shippingPrice"))
                .cloned(),
        },
        _ => ItemShipping::default(),
    }
}

fn first_publication(meta: &Map<String, Value>) -> Option<String> {
    string_path(meta, &["publishedAt", "published_at", "published"]).or_else(|| {
        meta.get("history")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_object)
            .filter(|entry| {
                string_path(entry, &["mode"]).is_none_or(|mode| mode.eq_ignore_ascii_case("play"))
            })
            .filter_map(|entry| string_path(entry, &["broadcasted", "publishedAt"]))
            .min()
    })
}

fn string_path(object: &Map<String, Value>, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value_string(object.get(*name)))
}

fn number_path(object: &Map<String, Value>, names: &[&str]) -> Option<i64> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_i64))
}

fn bool_path(object: &Map<String, Value>, names: &[&str]) -> Option<bool> {
    names
        .iter()
        .find_map(|name| object.get(*name).and_then(Value::as_bool))
}

fn value_string(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) => nonempty(value),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.to_owned())
}

fn validate_id(listing_id: &str) -> Result<(), AppError> {
    if listing_id.is_empty()
        || listing_id.len() > 128
        || !listing_id.bytes().all(|byte| byte.is_ascii_digit())
    {
        let mut error = AppError::validation(
            "item.invalid_id",
            "listing ID must be the numeric ID returned by public search",
        )
        .with_details(json!({ "listing_id": listing_id }));
        add_search_action(&mut error);
        Err(error)
    } else {
        Ok(())
    }
}

fn item_error(error: PublicItemApiError, listing_id: &str) -> AppError {
    let read = RetryContext::read(OperationMethod::Get);
    let (code, message, exit_class, classification) = match error {
        PublicItemApiError::Invalid => (
            "item.invalid_id",
            "Tori rejected the listing ID; use a numeric ID returned by public search",
            ExitClass::Validation,
            RetryClassification::default(),
        ),
        PublicItemApiError::NotFound => (
            "item.not_found",
            "listing was not found; it may have been removed or expired",
            ExitClass::Validation,
            RetryClassification::default(),
        ),
        PublicItemApiError::Expired => (
            "item.expired",
            "listing has expired and its public details are unavailable",
            ExitClass::Validation,
            RetryClassification::default(),
        ),
        PublicItemApiError::Unexpected(_) => (
            "upstream.unexpected_response",
            "Tori returned an unexpected item response",
            ExitClass::Upstream,
            classify(FailureKind::MalformedSuccess, read),
        ),
        PublicItemApiError::Transport(_) => (
            "upstream.request_failed",
            "the Tori item request failed",
            ExitClass::Upstream,
            classify(FailureKind::Transport, read),
        ),
        PublicItemApiError::Upstream(status) => (
            "upstream.request_failed",
            "the Tori item request failed",
            ExitClass::Upstream,
            classify(FailureKind::HttpStatus(status), read),
        ),
    };
    let mut app_error = AppError::new(code, message, exit_class)
        .with_details(json!({ "listing_id": listing_id }))
        .retry_classification(classification);
    add_search_action(&mut app_error);
    app_error
}

fn add_search_action(error: &mut AppError) {
    error
        .next_actions
        .push(crate::domain::envelope::NextAction {
            command: "flea search".to_owned(),
        });
}

fn unexpected(message: &str) -> AppError {
    AppError::new("upstream.unexpected_response", message, ExitClass::Upstream)
        .retry_classification(classify(
            FailureKind::MalformedSuccess,
            RetryContext::read(OperationMethod::Get),
        ))
}
