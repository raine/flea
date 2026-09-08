use std::{collections::HashSet, future::Future, pin::Pin, str::FromStr};

use reqwest::{Method, StatusCode};
use serde_json::{Map, Number, Value};
use url::Url;

use crate::{
    domain::{
        envelope::NextAction,
        search::SearchPrice,
        vinted_listing::{
            VintedConditionIdentity, VintedConditionIdentityStatus, VintedListingCollection,
            VintedListingCondition, VintedListingDetail, VintedListingPhoto, VintedListingShipping,
            VintedListingSize, VintedListingState, VintedListingSummary, VintedListingValue,
        },
    },
    error::{AppError, ExitClass},
    marketplace::{
        PortalId,
        vinted::{
            auth::{VintedAuthentication, VintedCredentialRecord},
            binding::VINTED_FI_BINDING,
            categories::CatalogTree,
            composer::{
                numeric_id, object_label, publication_attribute_definitions,
                publication_attribute_options,
            },
            item::{VintedItemSession, validate_item_id},
            publication_discovery::{DiscoveryRequest, VintedPublicationDiscoveryApi},
        },
    },
    transport::{Transport, TransportError, TransportErrorKind, TransportResponse},
};

const MAX_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const LIST_PAGE_SIZE: usize = 100;
const MAX_LIST_ITEMS: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VintedListingRequest {
    Show { item_id: String },
    List,
}

#[derive(Debug, PartialEq)]
pub enum VintedListingResult {
    Detail(Box<VintedListingDetail>),
    Collection(Box<VintedListingCollection>),
}

#[derive(Clone, Debug, PartialEq)]
pub enum ListingLookup {
    Found(Value),
    Missing,
    Deleted,
}

pub trait VintedListingApi: Send + Sync {
    fn wardrobe_item<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ListingLookup, AppError>> + Send + 'a>>;

    fn item_for_edit<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;

    fn wardrobe_items<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        condition: &'a str,
        page: usize,
        per_page: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>>;
}

pub struct VintedListings<'a> {
    session: &'a dyn VintedItemSession,
    api: &'a dyn VintedListingApi,
    discovery_api: Option<&'a dyn VintedPublicationDiscoveryApi>,
}

impl<'a> VintedListings<'a> {
    pub fn new(session: &'a dyn VintedItemSession, api: &'a dyn VintedListingApi) -> Self {
        Self {
            session,
            api,
            discovery_api: None,
        }
    }

    pub fn with_discovery(mut self, api: &'a dyn VintedPublicationDiscoveryApi) -> Self {
        self.discovery_api = Some(api);
        self
    }

    pub async fn execute(
        &self,
        portal: PortalId,
        request: VintedListingRequest,
    ) -> Result<VintedListingResult, AppError> {
        match request {
            VintedListingRequest::Show { item_id } => {
                validate_item_id(&item_id)?;
                let credentials = self.session.credentials(portal).await?;
                let lookup = self.api.wardrobe_item(&credentials, &item_id).await?;
                let detail = match lookup {
                    ListingLookup::Missing => self
                        .account_item(&credentials, &item_id)
                        .await?
                        .map_or_else(
                            || Ok(absent_detail(item_id.clone(), VintedListingState::Missing)),
                            |item| summary_detail(&item_id, &item),
                        )?,
                    ListingLookup::Deleted => absent_detail(item_id, VintedListingState::Deleted),
                    ListingLookup::Found(wardrobe) => {
                        let item = response_item(&wardrobe)?;
                        let state = normalize_state(item);
                        if state == VintedListingState::Deleted {
                            absent_detail(item_id, state)
                        } else if state == VintedListingState::Moderated {
                            summary_detail(&item_id, &Value::Object(item.clone()))?
                        } else {
                            let edit = self.api.item_for_edit(&credentials, &item_id).await?;
                            let mut detail = normalize_detail(&item_id, &wardrobe, &edit)?;
                            self.normalize_condition_identity(&credentials, &mut detail)
                                .await;
                            detail
                        }
                    }
                };
                Ok(VintedListingResult::Detail(Box::new(detail)))
            }
            VintedListingRequest::List => {
                let credentials = self.session.credentials(portal).await?;
                let collection = self.list(&credentials).await?;
                Ok(VintedListingResult::Collection(Box::new(collection)))
            }
        }
    }

    pub(super) async fn normalize_condition_identity(
        &self,
        credentials: &VintedCredentialRecord,
        detail: &mut VintedListingDetail,
    ) {
        let (Some(api), Some(category_id)) = (
            self.discovery_api,
            detail
                .category
                .as_ref()
                .and_then(|category| category.id.as_deref())
                .and_then(|id| id.parse::<u64>().ok()),
        ) else {
            return;
        };
        if let Ok(response) = api.execute(credentials, &DiscoveryRequest::Catalogs).await
            && let Ok(tree) = CatalogTree::from_response(&response)
            && let Some(node) = tree.get(category_id)
        {
            if let Some(category) = detail.category.as_mut() {
                category.name = Some(node.category.title.clone());
            }
            detail.category_path = Some(node.category.path.clone());
        }
        let request = DiscoveryRequest::Attributes {
            selections: serde_json::json!([{"code": "category", "value": [category_id]}]),
        };
        let Ok(response) = api.execute(credentials, &request).await else {
            return;
        };
        if let Some(condition) = detail.condition.as_mut() {
            resolve_condition_identity(condition, &response);
        }
        if let Some(size) = detail.size.as_mut()
            && let Some(selected) = size.composer_id.as_deref()
        {
            let labels: HashSet<_> = publication_attribute_definitions(&response)
                .into_iter()
                .filter(|(code, _)| *code == "size")
                .flat_map(|(_, definition)| publication_attribute_options(definition))
                .filter_map(|option| {
                    let object = option.as_object()?;
                    (numeric_id(object)?.to_string() == selected)
                        .then(|| object_label(object))
                        .flatten()
                })
                .collect();
            if labels.len() == 1 {
                size.name = labels.into_iter().next();
            }
        }
    }

    async fn account_item(
        &self,
        credentials: &VintedCredentialRecord,
        item_id: &str,
    ) -> Result<Option<Value>, AppError> {
        let mut inspected = 0;
        let mut match_item = None;
        for condition in ["active", "drafts"] {
            let mut page = 1;
            loop {
                let raw = match self
                    .api
                    .wardrobe_items(credentials, condition, page, LIST_PAGE_SIZE)
                    .await
                {
                    Ok(raw) => raw,
                    Err(_) if match_item.is_some() => return Ok(match_item),
                    Err(error) => return Err(error),
                };
                let (items, total_pages) = list_page(&raw)?;
                inspected += items.len();
                if match_item.is_none() {
                    match_item = items
                        .iter()
                        .find(|item| identifier(item.get("id")).as_deref() == Some(item_id))
                        .cloned();
                }
                if match_item.is_some() || page >= total_pages || inspected >= MAX_LIST_ITEMS {
                    break;
                }
                page += 1;
            }
            if inspected >= MAX_LIST_ITEMS {
                break;
            }
        }
        Ok(match_item)
    }

    async fn list(
        &self,
        credentials: &VintedCredentialRecord,
    ) -> Result<VintedListingCollection, AppError> {
        let mut listings = Vec::new();
        let mut seen = HashSet::new();
        let mut truncated = false;
        for condition in ["active", "drafts"] {
            let raw = self
                .api
                .wardrobe_items(credentials, condition, 1, LIST_PAGE_SIZE)
                .await?;
            let (items, total_pages) = list_page(&raw)?;
            truncated |= total_pages > 1;
            for item in items {
                if listings.len() >= MAX_LIST_ITEMS {
                    truncated = true;
                    break;
                }
                let summary = normalize_summary(item)?;
                if seen.insert(summary.listing_id.clone()) {
                    listings.push(summary);
                }
            }
        }
        let active_count = listings
            .iter()
            .filter(|item| item.state != VintedListingState::Draft)
            .count();
        let draft_count = listings.len() - active_count;
        Ok(VintedListingCollection {
            count: listings.len(),
            active_count,
            draft_count,
            listings,
            truncated,
        })
    }
}

pub struct HttpVintedListingApi {
    auth: VintedAuthentication,
    portal_api_base_url: String,
}

impl HttpVintedListingApi {
    pub fn new() -> Self {
        Self {
            auth: VintedAuthentication::new(),
            portal_api_base_url: VINTED_FI_BINDING.portal_api_host.to_owned(),
        }
    }

    fn endpoint(&self, path: &str) -> Result<Url, AppError> {
        let mut url = Url::parse(&self.portal_api_base_url).map_err(|error| {
            AppError::unexpected("Vinted API binding is invalid").with_source(error)
        })?;
        url.set_path(path);
        Ok(url)
    }

    async fn get(
        &self,
        credentials: &VintedCredentialRecord,
        url: Url,
    ) -> Result<TransportResponse, AppError> {
        let request = self.auth.authenticated_request(
            Method::GET,
            url.to_string(),
            credentials,
            MAX_RESPONSE_BYTES,
            transport_error,
        )?;
        self.auth
            .executor()
            .execute(request)
            .await
            .map_err(execution_error)
    }

    async fn wardrobe_item_request(
        &self,
        credentials: &VintedCredentialRecord,
        item_id: &str,
    ) -> Result<ListingLookup, AppError> {
        let response = self
            .get(
                credentials,
                self.endpoint(&format!("/api/v2/wardrobe/items/{item_id}"))?,
            )
            .await?;
        if response.status.is_success() {
            return decode_json(response).map(ListingLookup::Found);
        }
        if matches!(response.status, StatusCode::NOT_FOUND | StatusCode::GONE) {
            return Ok(classify_absence(response.status, &response.body));
        }
        Err(status_error(response.status))
    }

    async fn item_for_edit_request(
        &self,
        credentials: &VintedCredentialRecord,
        item_id: &str,
    ) -> Result<Value, AppError> {
        let response = self
            .get(
                credentials,
                self.endpoint(&format!("/api/v2/item_upload/items/{item_id}"))?,
            )
            .await?;
        if response.status.is_success() {
            decode_json(response)
        } else {
            Err(status_error(response.status))
        }
    }

    async fn wardrobe_items_request(
        &self,
        credentials: &VintedCredentialRecord,
        condition: &str,
        page: usize,
        per_page: usize,
    ) -> Result<Value, AppError> {
        let mut url = self.endpoint(&format!("/api/v2/wardrobe/{}/items", credentials.user_id))?;
        url.query_pairs_mut()
            .append_pair("cond", condition)
            .append_pair("page", &page.to_string())
            .append_pair("per_page", &per_page.to_string())
            .append_pair("order", "newest_first");
        let response = self.get(credentials, url).await?;
        if response.status.is_success() {
            decode_json(response)
        } else {
            Err(status_error(response.status))
        }
    }
}

impl Default for HttpVintedListingApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedListingApi for HttpVintedListingApi {
    fn wardrobe_item<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<ListingLookup, AppError>> + Send + 'a>> {
        Box::pin(self.wardrobe_item_request(credentials, item_id))
    }

    fn item_for_edit<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.item_for_edit_request(credentials, item_id))
    }

    fn wardrobe_items<'a>(
        &'a self,
        credentials: &'a VintedCredentialRecord,
        condition: &'a str,
        page: usize,
        per_page: usize,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.wardrobe_items_request(credentials, condition, page, per_page))
    }
}

fn absent_detail(listing_id: String, state: VintedListingState) -> VintedListingDetail {
    VintedListingDetail {
        listing_id,
        state,
        title: None,
        description: None,
        price: None,
        condition: None,
        size: None,
        is_unisex: None,
        category: None,
        category_path: None,
        brand: None,
        colors: Vec::new(),
        shipping: None,
        photos: Vec::new(),
        canonical_url: None,
    }
}

fn summary_detail(expected_id: &str, item: &Value) -> Result<VintedListingDetail, AppError> {
    let summary = normalize_summary(item)?;
    if summary.listing_id != expected_id {
        return Err(invalid_response(
            "listing response returned a different item ID",
        ));
    }
    Ok(VintedListingDetail {
        listing_id: summary.listing_id,
        state: summary.state,
        title: summary.title,
        description: None,
        price: summary.price,
        condition: None,
        size: None,
        is_unisex: None,
        category: None,
        category_path: None,
        brand: None,
        colors: Vec::new(),
        shipping: None,
        photos: item
            .get("photos")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
            .filter_map(|(order, photo)| normalize_photo(order, photo))
            .collect(),
        canonical_url: summary.canonical_url,
    })
}

pub(super) fn normalize_detail(
    expected_id: &str,
    wardrobe_raw: &Value,
    edit_raw: &Value,
) -> Result<VintedListingDetail, AppError> {
    let wardrobe = response_item(wardrobe_raw)?;
    let edit = response_item(edit_raw)?;
    let returned_id = identifier(edit.get("id")).or_else(|| identifier(wardrobe.get("id")));
    if returned_id.as_deref() != Some(expected_id) {
        return Err(invalid_response(
            "listing response returned a different item ID",
        ));
    }
    let colors = [("color1_id", "color1"), ("color2_id", "color2")]
        .into_iter()
        .filter_map(|(id, name)| listing_value(edit.get(id), edit.get(name)))
        .collect();
    let photos = edit
        .get("photos")
        .or_else(|| wardrobe.get("photos"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(order, photo)| normalize_photo(order, photo))
        .collect();
    let shipment_prices = edit
        .get("shipment_prices")
        .cloned()
        .filter(|value| !value.is_null())
        .or_else(|| {
            let domestic = edit.get("domestic_shipment_price");
            let international = edit.get("international_shipment_price");
            (domestic.is_some() || international.is_some()).then(|| {
                serde_json::json!({
                    "domestic": domestic.cloned().unwrap_or(Value::Null),
                    "international": international.cloned().unwrap_or(Value::Null)
                })
            })
        });
    let package_size_id = identifier(edit.get("package_size_id"));
    let parcel = edit_raw
        .get("parcel")
        .cloned()
        .filter(|value| !value.is_null());
    let shipping = (package_size_id.is_some() || shipment_prices.is_some() || parcel.is_some())
        .then_some(VintedListingShipping {
            package_size_id,
            shipment_prices,
            parcel,
        });
    Ok(VintedListingDetail {
        listing_id: expected_id.to_owned(),
        state: normalize_state(wardrobe),
        title: string(edit.get("title")).or_else(|| string(wardrobe.get("title"))),
        description: string(edit.get("description")),
        price: edit
            .get("price")
            .and_then(|value| normalize_price(value, edit.get("currency")))
            .or_else(|| {
                wardrobe
                    .get("price")
                    .and_then(|value| normalize_price(value, wardrobe.get("currency")))
            }),
        condition: detail_condition(edit),
        size: detail_size(edit),
        is_unisex: edit.get("is_unisex").and_then(Value::as_bool),
        category_path: None,
        category: listing_value(
            edit.get("catalog_id"),
            edit.get("catalog_name")
                .or_else(|| edit.get("catalog_title")),
        ),
        brand: normalize_brand(edit),
        colors,
        shipping,
        photos,
        canonical_url: string(wardrobe.get("url"))
            .or_else(|| string(edit.get("url")))
            .map(|url| absolute_url(&url, expected_id)),
    })
}

fn normalize_summary(value: &Value) -> Result<VintedListingSummary, AppError> {
    let item = value
        .as_object()
        .ok_or_else(|| invalid_response("wardrobe item was not an object"))?;
    let listing_id = identifier(item.get("id"))
        .ok_or_else(|| invalid_response("wardrobe item ID was unavailable"))?;
    Ok(VintedListingSummary {
        state: normalize_state(item),
        title: string(item.get("title")),
        price: item
            .get("price")
            .and_then(|value| normalize_price(value, item.get("currency"))),
        canonical_url: string(item.get("url")).map(|url| absolute_url(&url, &listing_id)),
        listing_id,
    })
}

pub(super) fn normalize_state(item: &Map<String, Value>) -> VintedListingState {
    let status = string(item.get("status"))
        .unwrap_or_default()
        .to_ascii_lowercase();
    let alert = string(item.get("item_alert_type"))
        .or_else(|| {
            item.get("item_alert")
                .and_then(Value::as_object)
                .and_then(|value| string(value.get("type")))
        })
        .unwrap_or_default()
        .to_ascii_lowercase();
    if item.get("is_draft").and_then(Value::as_bool) == Some(true) || status == "draft" {
        VintedListingState::Draft
    } else if status.contains("delet") {
        VintedListingState::Deleted
    } else if alert == "under_review"
        || status == "alert"
        || status == "processing"
        || status.contains("moderat")
        || status.contains("review")
        || item.get("is_processing").and_then(Value::as_bool) == Some(true)
    {
        VintedListingState::Moderated
    } else if item.get("is_hidden").and_then(Value::as_bool) == Some(true) || status == "hidden" {
        VintedListingState::Hidden
    } else if item.get("item_closing_action").and_then(Value::as_str) == Some("sold")
        || matches!(status.as_str(), "sold" | "closed")
        || item.get("is_closed").and_then(Value::as_bool) == Some(true)
    {
        VintedListingState::Sold
    } else {
        VintedListingState::Public
    }
}

fn normalize_brand(item: &Map<String, Value>) -> Option<VintedListingValue> {
    item.get("brand_dto")
        .or_else(|| item.get("brand"))
        .and_then(|brand| match brand {
            Value::Object(brand) => listing_value(
                brand.get("id"),
                brand.get("title").or_else(|| brand.get("name")),
            ),
            Value::String(name) if !name.trim().is_empty() => Some(VintedListingValue {
                id: identifier(item.get("brand_id")),
                name: Some(name.clone()),
            }),
            _ => None,
        })
}

fn normalize_photo(order: usize, value: &Value) -> Option<VintedListingPhoto> {
    let photo = value.as_object()?;
    let url = ["url", "full_size_url", "image_url"]
        .into_iter()
        .find_map(|key| string(photo.get(key)));
    Some(VintedListingPhoto {
        order,
        id: identifier(photo.get("id")),
        url,
        width: photo.get("width").and_then(Value::as_u64),
        height: photo.get("height").and_then(Value::as_u64),
    })
}

fn normalize_price(value: &Value, fallback_currency: Option<&Value>) -> Option<SearchPrice> {
    let object = value.as_object();
    let amount_value = object
        .and_then(|value| value.get("amount"))
        .unwrap_or(value);
    let amount = match amount_value {
        Value::Number(value) => Value::Number(value.clone()),
        Value::String(value) => Value::Number(Number::from_str(value).ok()?),
        _ => return None,
    };
    let currency = object
        .and_then(|value| {
            ["currency_code", "currencyCode", "currency"]
                .into_iter()
                .find_map(|key| string(value.get(key)))
        })
        .or_else(|| string(fallback_currency));
    Some(SearchPrice { amount, currency })
}

fn listing_condition(id: Option<&Value>, name: Option<&Value>) -> Option<VintedListingCondition> {
    let upstream_id = identifier(id);
    let name = string(name);
    if upstream_id.is_none() && name.is_none() {
        return None;
    }
    let status = if upstream_id.is_some() {
        VintedConditionIdentityStatus::UpstreamOnly
    } else {
        VintedConditionIdentityStatus::Unavailable
    };
    Some(VintedListingCondition {
        name,
        identity: VintedConditionIdentity {
            status,
            upstream_id,
            composer_id: None,
        },
    })
}

fn selected_attribute_id(edit: &Map<String, Value>, code: &str) -> Option<String> {
    let mut attributes = edit
        .get("item_attributes")?
        .as_array()?
        .iter()
        .filter(|attribute| attribute.get("code").and_then(Value::as_str) == Some(code));
    let selected = attributes.next()?;
    if attributes.next().is_some() {
        return None;
    }
    let ids = selected.get("ids")?.as_array()?;
    if ids.len() != 1 {
        return None;
    }
    ids[0]
        .as_u64()
        .filter(|id| *id > 0)
        .map(|id| id.to_string())
}

fn detail_size(edit: &Map<String, Value>) -> Option<VintedListingSize> {
    let upstream_id = identifier(edit.get("size_id"));
    let composer_id = selected_attribute_id(edit, "size");
    (upstream_id.is_some() || composer_id.is_some()).then_some(VintedListingSize {
        upstream_id,
        composer_id,
        name: None,
    })
}

fn detail_condition(edit: &Map<String, Value>) -> Option<VintedListingCondition> {
    let mut condition = listing_condition(edit.get("status_id"), edit.get("status"));
    let Some(attributes) = edit.get("item_attributes").and_then(Value::as_array) else {
        return condition;
    };
    let mut attributes = attributes
        .iter()
        .filter(|attribute| attribute.get("code").and_then(Value::as_str) == Some("condition"));
    let selected = attributes.next();
    let composer_id = selected
        .filter(|_| attributes.next().is_none())
        .and_then(|attribute| attribute.get("ids"))
        .and_then(Value::as_array)
        .filter(|ids| ids.len() == 1)
        .and_then(|ids| ids[0].as_u64())
        .filter(|id| *id > 0);
    if let Some(id) = composer_id {
        let condition = condition.get_or_insert(VintedListingCondition {
            name: None,
            identity: VintedConditionIdentity {
                status: VintedConditionIdentityStatus::ComposerMatched,
                upstream_id: None,
                composer_id: None,
            },
        });
        condition.identity.composer_id = Some(id.to_string());
        condition.identity.status = VintedConditionIdentityStatus::ComposerMatched;
    }
    condition
}

fn resolve_condition_identity(condition: &mut VintedListingCondition, response: &Value) {
    let matches: HashSet<_> = publication_attribute_definitions(response)
        .into_iter()
        .filter(|(code, _)| *code == "condition")
        .flat_map(|(_, definition)| publication_attribute_options(definition))
        .filter_map(|option| {
            let object = option.as_object()?;
            let id = numeric_id(object)?.to_string();
            let label = object_label(object)?;
            let matches = match condition.identity.composer_id.as_deref() {
                Some(selected) => selected == id,
                None => condition
                    .name
                    .as_deref()
                    .is_some_and(|name| normalized_label(name) == normalized_label(&label)),
            };
            matches.then_some((id, label))
        })
        .collect();
    if matches.len() == 1 {
        let (id, label) = matches.into_iter().next().unwrap();
        if condition.identity.composer_id.is_some() {
            condition.name = Some(label);
        }
        condition.identity.composer_id = Some(id);
        condition.identity.status = VintedConditionIdentityStatus::ComposerMatched;
    }
}

fn normalized_label(value: &str) -> String {
    value.trim().to_lowercase()
}

fn listing_value(id: Option<&Value>, name: Option<&Value>) -> Option<VintedListingValue> {
    let id = identifier(id);
    let name = string(name);
    (id.is_some() || name.is_some()).then_some(VintedListingValue { id, name })
}

pub(super) fn response_item(raw: &Value) -> Result<&Map<String, Value>, AppError> {
    let body = raw.get("data").unwrap_or(raw);
    body.get("item")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_response("listing item was unavailable"))
}

fn list_page(raw: &Value) -> Result<(&[Value], usize), AppError> {
    let body = raw.get("data").unwrap_or(raw);
    let items = body
        .get("items")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid_response("wardrobe items were unavailable"))?;
    let total_pages = body
        .pointer("/pagination/total_pages")
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(1)
        .max(1);
    Ok((items, total_pages))
}

fn classify_absence(status: StatusCode, body: &[u8]) -> ListingLookup {
    if status == StatusCode::GONE {
        return ListingLookup::Deleted;
    }
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    if text.contains("deleted") || text.contains("removed") {
        ListingLookup::Deleted
    } else {
        ListingLookup::Missing
    }
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

fn identifier(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn string(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
}

fn decode_json(response: TransportResponse) -> Result<Value, AppError> {
    serde_json::from_slice(&response.body)
        .map_err(|error| invalid_response("response was not valid JSON").with_source(error))
}

fn status_error(status: StatusCode) -> AppError {
    if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
        let mut error = AppError::authentication(
            "vinted_listing.authentication_required",
            "Vinted listing inspection requires a valid authenticated session",
        );
        error.next_actions.push(NextAction {
            command: "flea vinted --portal fi auth login".to_owned(),
        });
        return error;
    }
    let mut error = AppError::new(
        "vinted_listing.upstream_failed",
        format!(
            "Vinted listing inspection returned HTTP {}",
            status.as_u16()
        ),
        ExitClass::Upstream,
    );
    error.upstream_transient = status.is_server_error() || status == StatusCode::TOO_MANY_REQUESTS;
    error.safe_to_retry = error.upstream_transient;
    error
}

fn invalid_response(reason: &str) -> AppError {
    AppError::upstream(
        "vinted_listing.unexpected_response",
        "Vinted returned an unsupported listing response",
    )
    .with_details(serde_json::json!({ "reason": reason }))
}

fn transport_error(error: TransportError) -> AppError {
    let mut result = AppError::upstream(
        "vinted_listing.transport_failed",
        "Vinted listing inspection could not be reached",
    )
    .with_source(error);
    result.upstream_transient = true;
    result.safe_to_retry = true;
    result
}

fn execution_error(error: TransportError) -> AppError {
    if error.kind == TransportErrorKind::ResponseTooLarge {
        invalid_response("response exceeded the size limit")
    } else {
        transport_error(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_keeps_legacy_and_composer_namespaces_separate() {
        let edit = serde_json::json!({"size_id": 12,
            "item_attributes": [{"code": "size", "ids": [591]}]});
        let size = detail_size(edit.as_object().unwrap()).unwrap();
        assert_eq!(size.upstream_id.as_deref(), Some("12"));
        assert_eq!(size.composer_id.as_deref(), Some("591"));
        assert_eq!(size.name, None);
        let legacy = serde_json::json!({"size_id": "591"});
        assert_eq!(
            detail_size(legacy.as_object().unwrap())
                .unwrap()
                .composer_id,
            None
        );
    }

    #[test]
    fn malformed_size_selections_and_unknown_unisex_remain_unknown() {
        for attributes in [
            serde_json::json!([{"code": "size", "ids": [591, 592]}]),
            serde_json::json!([{"code": "size", "ids": [591]}, {"code": "size", "ids": [591]}]),
            serde_json::json!([{"code": "size", "ids": ["591"]}]),
            serde_json::json!([{"code": "size", "ids": [0]}]),
            serde_json::json!([]),
        ] {
            let detail = normalize_detail(
                "1",
                &serde_json::json!({"item": {"id": 1}}),
                &serde_json::json!({"item": {"id": 1, "item_attributes": attributes,
                    "description": "Unisex, size 22", "is_unisex": "true"}}),
            )
            .unwrap();
            assert_eq!(detail.size, None);
            assert_eq!(detail.is_unisex, None);
        }
        for value in [
            serde_json::json!(true),
            serde_json::json!(false),
            Value::Null,
        ] {
            let detail = normalize_detail(
                "1",
                &serde_json::json!({"item": {"id": 1}}),
                &serde_json::json!({"item": {"id": 1, "is_unisex": value}}),
            )
            .unwrap();
            assert_eq!(detail.is_unisex, value.as_bool());
        }
    }

    #[test]
    fn absence_classification_distinguishes_missing_and_deleted() {
        assert_eq!(
            classify_absence(
                StatusCode::NOT_FOUND,
                br#"{"message_code":"item_not_found"}"#
            ),
            ListingLookup::Missing
        );
        assert_eq!(
            classify_absence(StatusCode::NOT_FOUND, br#"{"message_code":"item_deleted"}"#),
            ListingLookup::Deleted
        );
        assert_eq!(
            classify_absence(StatusCode::GONE, b""),
            ListingLookup::Deleted
        );
    }

    #[test]
    fn condition_without_any_runtime_identity_is_explicitly_unavailable() {
        let condition = listing_condition(None, Some(&Value::String("Unknown".to_owned())))
            .expect("condition name");

        assert_eq!(
            condition.identity.status,
            VintedConditionIdentityStatus::Unavailable
        );
        assert_eq!(condition.identity.upstream_id, None);
        assert_eq!(condition.identity.composer_id, None);
    }

    #[test]
    fn canonical_condition_resolves_label_without_legacy_status() {
        let edit = serde_json::json!({
            "id": 1,
            "item_attributes": [{"code": "condition", "ids": [3]}]
        });
        let mut condition = normalize_detail(
            "1",
            &serde_json::json!({"item": {"id": 1}}),
            &serde_json::json!({"item": edit}),
        )
        .unwrap()
        .condition
        .unwrap();
        assert_eq!(condition.identity.upstream_id, None);
        assert_eq!(condition.identity.composer_id.as_deref(), Some("3"));
        assert_eq!(condition.name, None);
        resolve_condition_identity(&mut condition, &condition_options());
        assert_eq!(condition.name.as_deref(), Some("Hyvä"));
        assert_eq!(
            condition.identity.status,
            VintedConditionIdentityStatus::ComposerMatched
        );
    }

    fn condition_options() -> Value {
        serde_json::json!({"attributes": [{"code": "condition", "configuration": {
            "groups": [{"options": [{"id": 3, "title": "Hyvä"}, {"id": 6, "title": "Tyydyttävä"}]}]
        }}]})
    }

    #[test]
    fn canonical_condition_rejects_ambiguous_or_invalid_selections() {
        for attributes in [
            serde_json::json!([{"code": "condition", "ids": [3, 6]}]),
            serde_json::json!([{"code": "condition", "ids": [3]}, {"code": "condition", "ids": [6]}]),
            serde_json::json!([{"code": "condition", "ids": []}]),
            serde_json::json!([{"code": "condition", "ids": [0]}]),
            serde_json::json!([{"code": "condition", "ids": ["3"]}]),
        ] {
            let edit = serde_json::json!({"item_attributes": attributes});
            assert_eq!(detail_condition(edit.as_object().unwrap()), None);
        }
    }

    #[test]
    fn legacy_condition_ids_are_not_composer_ids() {
        let edit = serde_json::json!({"status_id": 3});
        let mut condition = detail_condition(edit.as_object().unwrap()).unwrap();
        resolve_condition_identity(&mut condition, &condition_options());
        assert_eq!(condition.identity.upstream_id.as_deref(), Some("3"));
        assert_eq!(condition.identity.composer_id, None);
        assert_eq!(
            condition.identity.status,
            VintedConditionIdentityStatus::UpstreamOnly
        );
        condition.name = Some("Tyydyttävä".to_owned());
        resolve_condition_identity(&mut condition, &condition_options());
        assert_eq!(condition.identity.upstream_id.as_deref(), Some("3"));
        assert_eq!(condition.identity.composer_id.as_deref(), Some("6"));
    }

    #[test]
    fn duplicate_discovery_labels_do_not_guess_composer_identity() {
        let mut condition = listing_condition(None, Some(&serde_json::json!("Hyvä"))).unwrap();
        let response = serde_json::json!({"attributes": [{"code": "condition", "configuration": {
            "groups": [{"options": [{"id": 3, "title": "Hyvä"}, {"id": 7, "title": "Hyvä"}]}]
        }}]});
        resolve_condition_identity(&mut condition, &response);
        assert_eq!(condition.identity.composer_id, None);
        assert_eq!(
            condition.identity.status,
            VintedConditionIdentityStatus::Unavailable
        );
    }

    #[test]
    fn canonical_id_takes_precedence_over_legacy_label() {
        let edit = serde_json::json!({"status_id": 7, "status": "Tyydyttävä",
            "item_attributes": [{"code": "condition", "ids": [3]}]});
        let mut condition = detail_condition(edit.as_object().unwrap()).unwrap();
        resolve_condition_identity(&mut condition, &condition_options());
        assert_eq!(condition.name.as_deref(), Some("Hyvä"));
        assert_eq!(condition.identity.upstream_id.as_deref(), Some("7"));
        assert_eq!(condition.identity.composer_id.as_deref(), Some("3"));
    }

    #[test]
    fn state_precedence_preserves_distinct_account_states() {
        let cases = [
            (
                serde_json::json!({"is_draft": true}),
                VintedListingState::Draft,
            ),
            (
                serde_json::json!({"status": "deleted"}),
                VintedListingState::Deleted,
            ),
            (
                serde_json::json!({"item_alert_type": "under_review"}),
                VintedListingState::Moderated,
            ),
            (
                serde_json::json!({"status": "ALERT"}),
                VintedListingState::Moderated,
            ),
            (
                serde_json::json!({"is_hidden": true}),
                VintedListingState::Hidden,
            ),
            (
                serde_json::json!({"item_closing_action": "sold"}),
                VintedListingState::Sold,
            ),
            (
                serde_json::json!({"status": "CLOSED"}),
                VintedListingState::Sold,
            ),
            (
                serde_json::json!({"status": "active"}),
                VintedListingState::Public,
            ),
        ];
        for (value, expected) in cases {
            assert_eq!(normalize_state(value.as_object().unwrap()), expected);
        }
    }
}
