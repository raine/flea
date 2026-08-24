use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            publication::ListingInput,
            publication_discovery::{DiscoveryRequest, VintedPublicationDiscoveryApi},
            search::VintedSearchSession,
        },
    },
};

const NO_BRAND_ID: u64 = 1;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BrandValidationStatus {
    Suggested,
    Searched,
    NoBrand,
    Custom,
    Mismatched,
    Removed,
    Inaccessible,
    Unavailable,
    Invalid,
    Unverifiable,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct BrandValidation {
    pub status: BrandValidationStatus,
    pub valid: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub brand_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supplied_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub canonical_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
}

impl BrandValidation {
    fn valid(
        status: BrandValidationStatus,
        id: Option<u64>,
        supplied_name: Option<&str>,
        canonical_name: Option<String>,
        source: &str,
        message: &str,
    ) -> Self {
        Self {
            status,
            valid: true,
            message: message.into(),
            brand_id: id,
            supplied_name: supplied_name.map(str::to_owned),
            canonical_name,
            source: Some(source.into()),
        }
    }

    fn rejected(
        status: BrandValidationStatus,
        id: Option<u64>,
        supplied_name: Option<&str>,
        canonical_name: Option<String>,
        source: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            valid: false,
            message: message.into(),
            brand_id: id,
            supplied_name: supplied_name.map(str::to_owned),
            canonical_name,
            source: source.map(str::to_owned),
        }
    }

    pub fn normalized_value(&self) -> Option<Value> {
        if !self.valid {
            return None;
        }
        match self.status {
            BrandValidationStatus::NoBrand => Some(json!({"brand_id": NO_BRAND_ID, "brand": ""})),
            BrandValidationStatus::Custom => Some(json!({
                "brand_id": Value::Null,
                "brand": self.canonical_name.as_deref().unwrap_or_default()
            })),
            _ => Some(json!({
                "brand_id": self.brand_id,
                "brand": self.canonical_name.as_deref().unwrap_or_default()
            })),
        }
    }

    pub fn error_code(&self) -> &'static str {
        match self.status {
            BrandValidationStatus::Mismatched => "brand_name_mismatch",
            BrandValidationStatus::Removed => "brand_removed",
            BrandValidationStatus::Inaccessible => "brand_inaccessible",
            BrandValidationStatus::Unavailable => "brand_unavailable",
            BrandValidationStatus::Unverifiable => "brand_unverifiable",
            _ => "invalid_brand",
        }
    }
}

pub fn selected_brand(input: Option<&Map<String, Value>>) -> Option<(Option<u64>, Option<String>)> {
    let input = input?;
    let has_id = input.contains_key("brand_id");
    let has_name = input.contains_key("brand");
    if !has_id && !has_name {
        return None;
    }
    let id = input.get("brand_id").and_then(|value| {
        value
            .as_u64()
            .or_else(|| value.as_str().and_then(|value| value.parse().ok()))
    });
    let name = input
        .get("brand")
        .and_then(Value::as_str)
        .map(str::trim)
        .map(str::to_owned);
    Some((id, name))
}

pub async fn validate_listing_brand(
    portal: PortalId,
    input: &ListingInput,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationDiscoveryApi,
) -> Result<BrandValidation, AppError> {
    let id = input.brand_id;
    let name = input.brand.as_deref();
    if id.is_none() || id == Some(NO_BRAND_ID) {
        return decision_result(decide_brand(id, name, None, None));
    }
    let credentials = session.credentials(portal).await?;
    let defaults = api
        .execute(
            &credentials,
            &DiscoveryRequest::Brands {
                category_id: input.catalog_id,
                keyword: String::new(),
            },
        )
        .await;
    let decision = match defaults {
        Ok(defaults) if find_brand(&defaults, id.expect("checked above")).is_some() => {
            decide_brand(id, name, Some(&defaults), None)
        }
        defaults => {
            let search = match name.filter(|name| !name.trim().is_empty()) {
                Some(name) => {
                    api.execute(
                        &credentials,
                        &DiscoveryRequest::Brands {
                            category_id: input.catalog_id,
                            keyword: name.to_owned(),
                        },
                    )
                    .await
                }
                None => {
                    return decision_result(decide_brand(id, name, defaults.as_ref().ok(), None));
                }
            };
            decide_brand(id, name, defaults.as_ref().ok(), Some(search.as_ref()))
        }
    };
    decision_result(decision)
}

fn decision_result(decision: BrandValidation) -> Result<BrandValidation, AppError> {
    if decision.valid {
        Ok(decision)
    } else {
        let code = format!("vinted.{}", decision.error_code());
        Err(
            AppError::validation(code, decision.message.clone()).with_details(json!({
                "brand_validation": decision
            })),
        )
    }
}

pub fn decide_brand(
    id: Option<u64>,
    name: Option<&str>,
    suggestions: Option<&Value>,
    search: Option<Result<&Value, &AppError>>,
) -> BrandValidation {
    let name = name.map(str::trim);
    if id == Some(NO_BRAND_ID) {
        return if name.is_none_or(str::is_empty) {
            BrandValidation::valid(
                BrandValidationStatus::NoBrand,
                id,
                name,
                Some(String::new()),
                "vinted_no_brand_encoding",
                "Vinted no-brand encoding is valid",
            )
        } else {
            BrandValidation::rejected(
                BrandValidationStatus::Mismatched,
                id,
                name,
                Some(String::new()),
                Some("vinted_no_brand_encoding"),
                "Brand ID 1 represents No brand, so set brand to an empty string",
            )
        };
    }
    if id.is_none() {
        return match name.filter(|name| !name.is_empty()) {
            Some(name) => BrandValidation::valid(
                BrandValidationStatus::Custom,
                None,
                Some(name),
                Some(name.to_owned()),
                "vinted_custom_brand_encoding",
                "A name without a brand ID is a custom brand",
            ),
            None => BrandValidation::rejected(
                BrandValidationStatus::Invalid,
                None,
                name,
                None,
                None,
                "Select No brand with brand_id 1 and an empty brand, or supply a custom brand name",
            ),
        };
    }
    let id = id.expect("checked above");
    let Some(name) = name.filter(|name| !name.is_empty()) else {
        return BrandValidation::rejected(
            BrandValidationStatus::Invalid,
            Some(id),
            name,
            None,
            None,
            "Supply the brand name paired with the selected brand ID",
        );
    };

    if let Some(candidate) = suggestions.and_then(|value| find_brand(value, id)) {
        return decision_from_candidate(id, name, candidate, BrandValidationStatus::Suggested);
    }
    match search {
        Some(Ok(response)) => match find_brand(response, id) {
            Some(candidate) => {
                decision_from_candidate(id, name, candidate, BrandValidationStatus::Searched)
            }
            None => BrandValidation::rejected(
                BrandValidationStatus::Unavailable,
                Some(id),
                Some(name),
                None,
                Some("category_brand_search"),
                "The brand is unavailable for this category. Search category brands again and choose a returned ID",
            ),
        },
        Some(Err(_)) | None => BrandValidation::rejected(
            BrandValidationStatus::Unverifiable,
            Some(id),
            Some(name),
            None,
            Some("category_brand_search"),
            "The selected brand could not be verified. Retry category-scoped brand discovery before publishing",
        ),
    }
}

fn decision_from_candidate(
    id: u64,
    supplied_name: &str,
    candidate: &Map<String, Value>,
    success_status: BrandValidationStatus,
) -> BrandValidation {
    let canonical = label(candidate).unwrap_or_default();
    if flag(candidate, &["is_deleted", "deleted", "removed"]) == Some(true)
        || flag(candidate, &["is_active", "active"]) == Some(false)
    {
        return BrandValidation::rejected(
            BrandValidationStatus::Removed,
            Some(id),
            Some(supplied_name),
            Some(canonical),
            Some("category_brand_search"),
            "The selected brand was removed. Search category brands again and choose an active brand",
        );
    }
    if flag(
        candidate,
        &["is_available", "available", "selectable", "is_selectable"],
    ) == Some(false)
    {
        return BrandValidation::rejected(
            BrandValidationStatus::Inaccessible,
            Some(id),
            Some(supplied_name),
            Some(canonical),
            Some("category_brand_search"),
            "The selected brand is inaccessible for this category or account. Choose an available category brand",
        );
    }
    if canonical != supplied_name {
        return BrandValidation::rejected(
            BrandValidationStatus::Mismatched,
            Some(id),
            Some(supplied_name),
            Some(canonical.clone()),
            Some("category_brand_search"),
            format!(
                "Brand ID {id} is {canonical:?}, not {supplied_name:?}. Use the canonical name returned by category brand discovery"
            ),
        );
    }
    BrandValidation::valid(
        success_status,
        Some(id),
        Some(supplied_name),
        Some(canonical),
        "category_brand_search",
        "The brand ID and name match category-scoped Vinted discovery",
    )
}

fn find_brand(value: &Value, wanted_id: u64) -> Option<&Map<String, Value>> {
    match value {
        Value::Array(values) => values.iter().find_map(|value| find_brand(value, wanted_id)),
        Value::Object(object) => {
            let id = object
                .get("id")
                .or_else(|| object.get("brand_id"))
                .and_then(|value| value.as_u64().or_else(|| value.as_str()?.parse().ok()));
            if id == Some(wanted_id) && label(object).is_some() {
                Some(object)
            } else {
                object
                    .values()
                    .find_map(|value| find_brand(value, wanted_id))
            }
        }
        _ => None,
    }
}

fn label(object: &Map<String, Value>) -> Option<String> {
    ["title", "name", "label", "display_name"]
        .iter()
        .find_map(|key| object.get(*key).and_then(Value::as_str))
        .map(str::to_owned)
}

fn flag(object: &Map<String, Value>, keys: &[&str]) -> Option<bool> {
    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_bool))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distinguishes_all_brand_decisions() {
        let defaults = json!({"brands":[{"id":22,"title":"Abus"}]});
        let searched = json!({"brands":[
            {"id":128186,"title":"Marimekko"},
            {"id":30,"title":"Gone","is_deleted":true},
            {"id":31,"title":"Private","is_available":false}
        ]});
        assert_eq!(
            decide_brand(Some(22), Some("Abus"), Some(&defaults), None).status,
            BrandValidationStatus::Suggested
        );
        assert_eq!(
            decide_brand(
                Some(128186),
                Some("Marimekko"),
                Some(&defaults),
                Some(Ok(&searched))
            )
            .status,
            BrandValidationStatus::Searched
        );
        assert_eq!(
            decide_brand(Some(1), Some(""), Some(&defaults), None).status,
            BrandValidationStatus::NoBrand
        );
        assert_eq!(
            decide_brand(None, Some("My label"), Some(&defaults), None).status,
            BrandValidationStatus::Custom
        );
        assert_eq!(
            decide_brand(
                Some(128186),
                Some("Wrong"),
                Some(&defaults),
                Some(Ok(&searched))
            )
            .status,
            BrandValidationStatus::Mismatched
        );
        assert_eq!(
            decide_brand(Some(30), Some("Gone"), Some(&defaults), Some(Ok(&searched))).status,
            BrandValidationStatus::Removed
        );
        assert_eq!(
            decide_brand(
                Some(31),
                Some("Private"),
                Some(&defaults),
                Some(Ok(&searched))
            )
            .status,
            BrandValidationStatus::Inaccessible
        );
        assert_eq!(
            decide_brand(
                Some(99),
                Some("Missing"),
                Some(&defaults),
                Some(Ok(&searched))
            )
            .status,
            BrandValidationStatus::Unavailable
        );
        assert_eq!(
            decide_brand(Some(99), Some("Missing"), Some(&defaults), None).status,
            BrandValidationStatus::Unverifiable
        );
    }
}
