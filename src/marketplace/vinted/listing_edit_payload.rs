use std::collections::HashSet;

use serde_json::{Map, Value, json};

use crate::{domain::vinted_listing_changes::VintedListingChanges, error::AppError};

pub(super) struct PreparedListingUpdate {
    pub body: Value,
    pub expected: Value,
}

pub(super) fn project_editable(item_id: &str, raw: &Value) -> Result<Value, AppError> {
    let body = raw.get("data").unwrap_or(raw);
    let source = body
        .get("item")
        .and_then(Value::as_object)
        .ok_or_else(|| unsupported("item", "editable item object is unavailable"))?;

    let returned_id = required_id(source, "id")?;
    if returned_id != item_id {
        return Err(AppError::conflict(
            "vinted_listing_edit.item_identity_mismatch",
            format!("Vinted returned a different item while preparing item {item_id}"),
        ));
    }
    if source.get("is_draft").and_then(Value::as_bool) != Some(false) {
        return Err(unsupported(
            "is_draft",
            "editable source did not identify a published item",
        ));
    }

    reject_unknown_active_fields(source)?;
    reject_unsupported_active_fields(source)?;
    reject_unmapped_legacy_attributes(source)?;

    let title = required_string(source, "title")?;
    let description = required_string(source, "description")?;
    let catalog_id = required_id(source, "catalog_id")?;
    let (price, currency) = project_price(source)?;
    let assigned_photos = project_photos(source)?;
    let item_attributes = project_item_attributes(source)?;
    let (brand_id, brand) = project_brand(source)?;
    let color_ids = project_colors(source)?;
    let package_size_id = optional_id_field(source, "package_size_id")?;
    let shipment_prices = project_shipment_prices(source)?;
    let model = project_model(required_field(source, "model")?)?;

    let mut item = Map::new();
    item.insert("id".to_owned(), Value::String(returned_id));
    item.insert("title".to_owned(), Value::String(title));
    item.insert("description".to_owned(), Value::String(description));
    item.insert("catalog_id".to_owned(), Value::String(catalog_id));
    item.insert("brand_id".to_owned(), brand_id);
    item.insert("brand".to_owned(), brand);
    item.insert("price".to_owned(), Value::String(price));
    item.insert("currency".to_owned(), Value::String(currency));
    item.insert("package_size_id".to_owned(), package_size_id);
    item.insert("shipment_prices".to_owned(), shipment_prices);
    item.insert("color_ids".to_owned(), Value::Array(color_ids));
    item.insert("assigned_photos".to_owned(), Value::Array(assigned_photos));
    item.insert("item_attributes".to_owned(), Value::Array(item_attributes));
    for field in [
        "isbn",
        "measurement_length",
        "measurement_width",
        "manufacturer",
        "manufacturer_labelling",
    ] {
        item.insert(field.to_owned(), project_nullable_scalar(source, field)?);
    }
    item.insert(
        "is_unisex".to_owned(),
        Value::Bool(required_bool(source, "is_unisex")?),
    );
    item.insert(
        "ai_photo".to_owned(),
        Value::Bool(required_bool(source, "ai_photo")?),
    );
    item.insert("model".to_owned(), model);

    let parcel = project_parcel(required_field(
        body.as_object()
            .ok_or_else(|| unsupported("response", "editable response is not an object"))?,
        "parcel",
    )?)?;

    Ok(json!({
        "item": Value::Object(item),
        "parcel": parcel
    }))
}

pub(super) fn prepare_update(
    item_id: &str,
    raw: &Value,
    changes: &VintedListingChanges,
) -> Result<PreparedListingUpdate, AppError> {
    let mut expected = project_editable(item_id, raw)?;
    let item = expected
        .get_mut("item")
        .and_then(Value::as_object_mut)
        .expect("projected editable item is an object");
    if let Some(title) = &changes.title {
        item.insert("title".to_owned(), Value::String(title.clone()));
    }
    if let Some(description) = &changes.description {
        item.insert("description".to_owned(), Value::String(description.clone()));
    }
    if let Some(price) = &changes.price {
        item.insert(
            "price".to_owned(),
            Value::String(canonical_amount(price, "price", true)?),
        );
    }

    let mut body = expected.clone();
    let body_item = body
        .get_mut("item")
        .and_then(Value::as_object_mut)
        .expect("projected editable item is an object");
    body_item.insert("id".to_owned(), Value::String(item_id.to_owned()));
    body_item.insert("update_photos".to_owned(), json!(0));
    let body = body
        .as_object_mut()
        .expect("projected update body is an object");
    body.insert("push_up".to_owned(), Value::Bool(false));
    body.insert(
        "upload_session_id".to_owned(),
        Value::String(uuid::Uuid::new_v4().to_string()),
    );

    Ok(PreparedListingUpdate {
        body: Value::Object(body.clone()),
        expected,
    })
}

fn project_price(source: &Map<String, Value>) -> Result<(String, String), AppError> {
    let price = required_field(source, "price")?;
    let amount = price
        .as_object()
        .and_then(|price| price.get("amount"))
        .unwrap_or(price);
    let amount = amount_as_string(amount, "price", true)?;
    let currency = price
        .as_object()
        .and_then(|price| {
            ["currency_code", "currencyCode", "currency"]
                .into_iter()
                .find_map(|field| price.get(field).and_then(Value::as_str))
        })
        .or_else(|| source.get("currency").and_then(Value::as_str))
        .filter(|currency| !currency.trim().is_empty())
        .ok_or_else(|| unsupported("currency", "price currency is unavailable"))?;
    Ok((amount, currency.to_owned()))
}

fn project_photos(source: &Map<String, Value>) -> Result<Vec<Value>, AppError> {
    let photos = required_field(source, "photos")?
        .as_array()
        .ok_or_else(|| unsupported("photos", "photo sequence is not an array"))?;
    if photos.is_empty() {
        return Err(unsupported(
            "photos",
            "published item has no photos to preserve",
        ));
    }
    let mut seen = HashSet::new();
    let mut main_index = None;
    let projected = photos
        .iter()
        .enumerate()
        .map(|(index, photo)| {
            let photo = photo
                .as_object()
                .ok_or_else(|| unsupported("photos", "photo entry is not an object"))?;
            reject_unknown_active_photo_fields(photo)?;
            let id = required_id(photo, "id")?;
            if !seen.insert(id.clone()) {
                return Err(unsupported("photos", "photo IDs are not unique"));
            }
            if photo
                .get("is_main")
                .is_some_and(|value| !value.is_null() && !value.is_boolean())
            {
                return Err(unsupported("photos", "main photo marker is invalid"));
            }
            if photo.get("is_main").and_then(Value::as_bool) == Some(true)
                && main_index.replace(index).is_some()
            {
                return Err(unsupported("photos", "photo order is ambiguous"));
            }
            project_photo_assignment(photo, id)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if main_index.is_some_and(|index| index != 0) {
        return Err(unsupported("photos", "main photo is not first"));
    }
    Ok(projected)
}

fn reject_unknown_active_photo_fields(photo: &Map<String, Value>) -> Result<(), AppError> {
    const KNOWN: &[&str] = &[
        "id",
        "orientation",
        "ai_detected",
        "digital_source_type",
        "c2pa_read_error",
        "is_main",
        "image_no",
        "extra",
        "url",
        "width",
        "height",
        "thumbnails",
        "dominant_color",
        "dominant_color_opaque",
        "is_suspicious",
        "full_size_url",
        "is_hidden",
        "high_resolution",
    ];
    for (field, value) in photo {
        if !KNOWN.contains(&field.as_str()) && !value.is_null() {
            return Err(unsupported(
                "photos",
                format!("active photo field `{field}` has no verified assignment projection"),
            ));
        }
    }
    Ok(())
}

fn project_photo_assignment(photo: &Map<String, Value>, id: String) -> Result<Value, AppError> {
    let mut assignment = Map::from_iter([("id".to_owned(), Value::String(id))]);
    if let Some(value) = photo.get("orientation").filter(|value| !value.is_null()) {
        if value
            .as_u64()
            .and_then(|value| u16::try_from(value).ok())
            .is_none()
        {
            return Err(unsupported("photos", "photo orientation is invalid"));
        }
        assignment.insert("orientation".to_owned(), value.clone());
    }
    if let Some(value) = photo.get("ai_detected").filter(|value| !value.is_null()) {
        if !value.is_boolean() {
            return Err(unsupported("photos", "photo AI marker is invalid"));
        }
        assignment.insert("ai_detected".to_owned(), value.clone());
    }
    if let Some(value) = photo
        .get("digital_source_type")
        .filter(|value| !value.is_null())
    {
        let valid = value.as_array().is_some_and(|codes| {
            codes
                .iter()
                .all(|code| code.as_str().is_some_and(|code| !code.trim().is_empty()))
        });
        if !valid {
            return Err(unsupported(
                "photos",
                "photo digital source types are invalid",
            ));
        }
        assignment.insert("digital_source_type".to_owned(), value.clone());
    }
    if let Some(value) = photo
        .get("c2pa_read_error")
        .filter(|value| !value.is_null())
    {
        if !matches!(value.as_str(), Some("noManifest" | "inAppCamera" | "other")) {
            return Err(unsupported("photos", "photo C2PA read error is invalid"));
        }
        assignment.insert("c2pa_read_error".to_owned(), value.clone());
    }
    Ok(Value::Object(assignment))
}

fn project_item_attributes(source: &Map<String, Value>) -> Result<Vec<Value>, AppError> {
    let attributes = required_field(source, "item_attributes")?
        .as_array()
        .ok_or_else(|| unsupported("item_attributes", "dynamic attributes are not an array"))?;
    let mut codes = HashSet::new();
    attributes
        .iter()
        .map(|attribute| {
            let attribute = attribute.as_object().ok_or_else(|| {
                unsupported("item_attributes", "dynamic attribute is not an object")
            })?;
            let code = required_string(attribute, "code")?;
            if !codes.insert(code.clone()) {
                return Err(unsupported(
                    "item_attributes",
                    "dynamic attribute codes are not unique",
                ));
            }
            let ids = required_field(attribute, "ids")?
                .as_array()
                .ok_or_else(|| unsupported("item_attributes", "attribute IDs are not an array"))?;
            let ids = ids
                .iter()
                .map(|id| {
                    id.as_u64()
                        .and_then(|id| u32::try_from(id).ok())
                        .filter(|id| *id != 0)
                        .map(|id| json!(id))
                        .ok_or_else(|| unsupported("item_attributes", "attribute ID is invalid"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({ "code": code, "ids": ids }))
        })
        .collect()
}

fn project_brand(source: &Map<String, Value>) -> Result<(Value, Value), AppError> {
    let brand = required_field(source, "brand_dto")?;
    if brand.is_null() {
        if source.get("brand_id").is_some_and(|value| !value.is_null()) {
            return Err(unsupported(
                "brand_dto",
                "brand ID exists without canonical brand metadata",
            ));
        }
        return Ok((Value::Null, Value::Null));
    }
    let brand = brand
        .as_object()
        .ok_or_else(|| unsupported("brand_dto", "brand metadata is not an object"))?;
    let is_custom = match brand.get("is_custom_brand") {
        Some(Value::Bool(value)) => *value,
        Some(Value::Null) | None => false,
        Some(_) => return Err(unsupported("brand_dto", "custom brand marker is invalid")),
    };
    if is_custom {
        if brand.get("id").is_some_and(|value| !value.is_null())
            || source.get("brand_id").is_some_and(|value| !value.is_null())
        {
            return Err(unsupported("brand_id", "custom brand identity is not null"));
        }
        return Ok((Value::Null, Value::String(required_string(brand, "title")?)));
    }

    let id = required_id(brand, "id")?;
    if let Some(source_id) = source.get("brand_id").filter(|value| !value.is_null())
        && id_from_value(source_id).as_deref() != Some(id.as_str())
    {
        return Err(unsupported("brand_id", "brand identities disagree"));
    }
    let title = required_field(brand, "title")?
        .as_str()
        .ok_or_else(|| unsupported("brand_dto", "brand title is invalid"))?;
    if id == "1" {
        Ok((Value::String(id), Value::String(String::new())))
    } else if title.trim().is_empty() {
        Err(unsupported("brand_dto", "brand title is unavailable"))
    } else {
        Ok((Value::String(id), Value::String(title.to_owned())))
    }
}

fn project_colors(source: &Map<String, Value>) -> Result<Vec<Value>, AppError> {
    ["color1_id", "color2_id"]
        .into_iter()
        .filter_map(|field| match optional_id_field(source, field) {
            Ok(Value::Null) => None,
            Ok(value) => Some(Ok(value)),
            Err(error) => Some(Err(error)),
        })
        .collect()
}

fn project_shipment_prices(source: &Map<String, Value>) -> Result<Value, AppError> {
    let prices = required_field(source, "shipment_prices")?;
    if prices.is_null() {
        if ["domestic_shipment_price", "international_shipment_price"]
            .into_iter()
            .any(|field| source.get(field).is_some_and(|value| !value.is_null()))
        {
            return Err(unsupported(
                "shipment_prices",
                "legacy shipment prices cannot be projected canonically",
            ));
        }
        return Ok(Value::Null);
    }
    let prices = prices
        .as_object()
        .ok_or_else(|| unsupported("shipment_prices", "shipment prices are not an object"))?;
    let mut projected = Map::new();
    for field in ["domestic", "international"] {
        let value = required_field(prices, field)?;
        projected.insert(
            field.to_owned(),
            if value.is_null() {
                Value::Null
            } else {
                Value::String(amount_as_string(value, "shipment_prices", false)?)
            },
        );
    }
    Ok(Value::Object(projected))
}

fn project_model(value: &Value) -> Result<Value, AppError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let model = value
        .as_object()
        .ok_or_else(|| unsupported("model", "model selection is not an object"))?;
    let model_type = required_string(model, "type")?;
    let metadata = required_field(model, "metadata")?;
    if !metadata.is_null() && !metadata.is_object() {
        return Err(unsupported("model", "model metadata is not an object"));
    }
    let suggestion = if model_type.eq_ignore_ascii_case("other") {
        Value::String(required_string(model, "name")?)
    } else {
        Value::Null
    };
    Ok(json!({ "metadata": metadata.clone(), "suggestion": suggestion }))
}

fn project_parcel(value: &Value) -> Result<Value, AppError> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let parcel = value
        .as_object()
        .ok_or_else(|| unsupported("parcel", "parcel is not an object"))?;
    let mut projected = Map::new();
    for field in ["width", "height", "length", "weight"] {
        let value = required_field(parcel, field)?;
        if value
            .as_f64()
            .is_none_or(|value| !value.is_finite() || value <= 0.0)
        {
            return Err(unsupported("parcel", "parcel measurement is invalid"));
        }
        projected.insert(field.to_owned(), value.clone());
    }
    Ok(Value::Object(projected))
}

fn project_nullable_scalar(
    source: &Map<String, Value>,
    field: &'static str,
) -> Result<Value, AppError> {
    let value = required_field(source, field)?;
    let valid = match field {
        "measurement_length" | "measurement_width" => {
            value.is_null() || value.as_u64().is_some_and(|value| value > 0)
        }
        _ => value.is_null() || value.as_str().is_some(),
    };
    valid
        .then(|| value.clone())
        .ok_or_else(|| unsupported(field, "editable value has an unsupported type"))
}

fn reject_unsupported_active_fields(source: &Map<String, Value>) -> Result<(), AppError> {
    // Active ontology selections have no canonical update projection and must
    // not be discarded. Null means there is no selection to preserve.
    for field in ["ontology_collection_id", "ontology_model_id"] {
        if source.get(field).is_some_and(|value| !value.is_null()) {
            return Err(unsupported(
                field,
                "active ontology selection has no verified update projection",
            ));
        }
    }
    Ok(())
}

fn reject_unmapped_legacy_attributes(source: &Map<String, Value>) -> Result<(), AppError> {
    let attributes = source
        .get("item_attributes")
        .and_then(Value::as_array)
        .ok_or_else(|| unsupported("item_attributes", "dynamic attributes are unavailable"))?;
    let codes = attributes
        .iter()
        .filter_map(|attribute| attribute.get("code").and_then(Value::as_str))
        .collect::<HashSet<_>>();
    for (field, code) in [
        ("status_id", "condition"),
        ("size_id", "size"),
        ("material_id", "material"),
        ("video_game_rating_id", "video_game_ratings"),
    ] {
        if source.get(field).is_some_and(|value| !value.is_null()) && !codes.contains(code) {
            return Err(unsupported(
                field,
                "legacy attribute identity has no canonical dynamic attribute",
            ));
        }
    }
    Ok(())
}

fn reject_unknown_active_fields(source: &Map<String, Value>) -> Result<(), AppError> {
    const KNOWN: &[&str] = &[
        "id",
        "title",
        "description",
        "catalog_id",
        "catalog_name",
        "catalog_title",
        "brand_dto",
        "brand_id",
        "color1",
        "color1_id",
        "color2",
        "color2_id",
        "package_size_id",
        "status",
        "status_id",
        "size_id",
        "material_id",
        "price",
        "currency",
        "shipment_prices",
        "domestic_shipment_price",
        "international_shipment_price",
        "photos",
        "item_attributes",
        "is_draft",
        "is_unisex",
        "isbn",
        "author",
        "book_title",
        "measurement_width",
        "measurement_length",
        "measurement_unit",
        "video_game_rating",
        "video_game_rating_id",
        "manufacturer",
        "manufacturer_labelling",
        "model",
        "ai_photo",
        "ontology_collection_id",
        "ontology_model_id",
        "can_push_up",
        "promoted",
        "item_alert",
        "url",
    ];
    for (field, value) in source {
        if !KNOWN.contains(&field.as_str()) && !value.is_null() {
            return Err(unsupported(
                "editable_state",
                format!("active field `{field}` has no verified update projection"),
            ));
        }
    }
    Ok(())
}

fn required_field<'a>(
    source: &'a Map<String, Value>,
    field: &'static str,
) -> Result<&'a Value, AppError> {
    source
        .get(field)
        .ok_or_else(|| unsupported(field, "editable value is unavailable"))
}

fn required_string(source: &Map<String, Value>, field: &'static str) -> Result<String, AppError> {
    required_field(source, field)?
        .as_str()
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| unsupported(field, "editable text is unavailable"))
}

fn required_id(source: &Map<String, Value>, field: &'static str) -> Result<String, AppError> {
    required_field(source, field)
        .and_then(|value| id_from_value(value).ok_or_else(|| unsupported(field, "ID is invalid")))
}

fn optional_id_field(source: &Map<String, Value>, field: &'static str) -> Result<Value, AppError> {
    let value = required_field(source, field)?;
    if value.is_null() {
        Ok(Value::Null)
    } else {
        id_from_value(value)
            .map(Value::String)
            .ok_or_else(|| unsupported(field, "ID is invalid"))
    }
}

fn id_from_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
        .map(ToOwned::to_owned)
        .or_else(|| {
            value
                .as_u64()
                .filter(|value| *value != 0)
                .map(|value| value.to_string())
        })
}

fn required_bool(source: &Map<String, Value>, field: &'static str) -> Result<bool, AppError> {
    required_field(source, field)?
        .as_bool()
        .ok_or_else(|| unsupported(field, "editable boolean is unavailable"))
}

fn amount_as_string(
    value: &Value,
    field: &'static str,
    positive: bool,
) -> Result<String, AppError> {
    let value = match value {
        Value::String(value) => value.as_str(),
        Value::Number(value) => return canonical_amount(&value.to_string(), field, positive),
        Value::Object(value) => {
            return value
                .get("amount")
                .ok_or_else(|| unsupported(field, "money amount is unavailable"))
                .and_then(|value| amount_as_string(value, field, positive));
        }
        _ => return Err(unsupported(field, "money amount has an unsupported type")),
    };
    canonical_amount(value, field, positive)
}

fn canonical_amount(value: &str, field: &'static str, positive: bool) -> Result<String, AppError> {
    let parsed = value.parse::<f64>().ok().filter(|value| value.is_finite());
    let parsed = parsed
        .filter(|value| !positive || *value > 0.0)
        .ok_or_else(|| unsupported(field, "money amount is not a valid finite decimal"))?;
    serde_json::Number::from_f64(parsed)
        .map(|value| value.to_string())
        .ok_or_else(|| unsupported(field, "money amount cannot be represented safely"))
}

fn unsupported(field: &'static str, reason: impl Into<String>) -> AppError {
    AppError::validation(
        "vinted_listing_edit.unsupported_editable_state",
        "Vinted listing cannot be updated without risking loss of existing fields",
    )
    .with_details(json!({ "field": field, "reason": reason.into() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn editable() -> Value {
        json!({
            "item": {
                "id": "9001",
                "title": "Bicycle lock",
                "description": "Two keys",
                "catalog_id": "3412",
                "brand_dto": {"id": "77", "title": "Kryptonite"},
                "brand_id": "77",
                "color1_id": "1",
                "color2_id": "3",
                "package_size_id": "2",
                "price": {"amount": "12.50", "currency_code": "EUR"},
                "currency": "EUR",
                "shipment_prices": {"domestic": null, "international": null},
                "domestic_shipment_price": null,
                "international_shipment_price": null,
                "photos": [{"id": 41, "orientation": 0}, {"id": "42", "orientation": 0}],
                "item_attributes": [{"code": "condition", "ids": [6]}],
                "is_draft": false,
                "is_unisex": false,
                "isbn": null,
                "measurement_length": null,
                "measurement_width": null,
                "manufacturer": null,
                "manufacturer_labelling": null,
                "model": null,
                "ai_photo": false
            },
            "parcel": {"width": 10.0, "height": 5.0, "length": 15.0, "weight": 0.4},
            "code": 0
        })
    }

    #[test]
    fn projects_complete_canonical_state_without_response_only_fields() {
        let projected = project_editable("9001", &editable()).unwrap();

        assert_eq!(projected["item"]["price"], "12.5");
        assert_eq!(
            projected["item"]["assigned_photos"],
            json!([{"id":"41","orientation":0},{"id":"42","orientation":0}])
        );
        assert_eq!(
            projected["item"]["item_attributes"],
            json!([{"code":"condition","ids":[6]}])
        );
        assert_eq!(projected["item"]["brand"], "Kryptonite");
        assert!(projected["item"].get("photos").is_none());
        assert!(projected.get("code").is_none());
        assert!(projected.get("upload_session_id").is_none());
    }

    #[test]
    fn response_photo_metadata_does_not_change_assignment_order() {
        let mut raw = editable();
        raw["item"]["photos"][0]["image_no"] = json!(1);
        raw["item"]["photos"][1]["image_no"] = json!(2);
        raw["item"]["photos"][0]["extra"] = json!({});

        assert_eq!(
            project_editable("9001", &raw).unwrap(),
            project_editable("9001", &editable()).unwrap()
        );
    }

    #[test]
    fn partial_changes_produce_a_complete_non_sparse_update() {
        let changes = VintedListingChanges::new(Some("Safer lock".to_owned()), None, None).unwrap();
        let prepared = prepare_update("9001", &editable(), &changes).unwrap();

        assert_eq!(prepared.expected["item"]["title"], "Safer lock");
        assert_eq!(prepared.expected["item"]["description"], "Two keys");
        assert_eq!(
            prepared.body["item"]["assigned_photos"],
            json!([{"id":"41","orientation":0},{"id":"42","orientation":0}])
        );
        assert_eq!(prepared.body["item"]["update_photos"], 0);
        assert_eq!(prepared.body["push_up"], false);
        assert_eq!(prepared.body["parcel"], editable()["parcel"]);
        assert!(prepared.body["upload_session_id"].as_str().is_some());
        assert!(prepared.expected.get("upload_session_id").is_none());
        assert!(prepared.expected["item"].get("update_photos").is_none());
    }

    #[test]
    fn preserves_valid_photo_orientation_and_c2pa_assignment_metadata() {
        let mut raw = editable();
        raw["item"]["photos"] = json!([
            {
                "id": 41,
                "orientation": 90,
                "is_main": true,
                "ai_detected": true,
                "digital_source_type": ["trainedAlgorithmicMedia"],
                "url": "https://images.example/41.jpg",
                "width": 1200,
                "height": 900
            },
            {
                "id": 42,
                "orientation": 180,
                "c2pa_read_error": "noManifest"
            }
        ]);

        let projected = project_editable("9001", &raw).unwrap();
        assert_eq!(
            projected["item"]["assigned_photos"],
            json!([
                {
                    "id": "41",
                    "orientation": 90,
                    "ai_detected": true,
                    "digital_source_type": ["trainedAlgorithmicMedia"]
                },
                {"id": "42", "orientation": 180, "c2pa_read_error": "noManifest"}
            ])
        );
    }

    #[test]
    fn rejects_invalid_or_unknown_active_photo_assignment_metadata() {
        let mut invalid_orientation = editable();
        invalid_orientation["item"]["photos"][0]["orientation"] = json!(70000);
        let error = project_editable("9001", &invalid_orientation).unwrap_err();
        assert_eq!(error.details.unwrap()["field"], "photos");

        let mut unknown = editable();
        unknown["item"]["photos"][0]["future_assignment"] = json!(true);
        let error = project_editable("9001", &unknown).unwrap_err();
        assert_eq!(error.details.unwrap()["field"], "photos");
    }

    #[test]
    fn projects_custom_and_no_brand_encodings() {
        let mut custom = editable();
        custom["item"]["brand_dto"] =
            json!({"id": null, "title": "Workshop", "is_custom_brand": true});
        custom["item"]["brand_id"] = Value::Null;
        let projected = project_editable("9001", &custom).unwrap();
        assert_eq!(projected["item"]["brand_id"], Value::Null);
        assert_eq!(projected["item"]["brand"], "Workshop");

        let mut no_brand = editable();
        no_brand["item"]["brand_dto"] = json!({"id": "1", "title": ""});
        no_brand["item"]["brand_id"] = json!(1);
        let projected = project_editable("9001", &no_brand).unwrap();
        assert_eq!(projected["item"]["brand_id"], "1");
        assert_eq!(projected["item"]["brand"], "");

        no_brand["item"]["brand_id"] = json!(77);
        let error = project_editable("9001", &no_brand).unwrap_err();
        assert_eq!(error.details.unwrap()["field"], "brand_id");
    }

    #[test]
    fn price_overlay_is_normalized_for_post_update_comparison() {
        let changes = VintedListingChanges::new(None, None, Some("15.00".to_owned())).unwrap();
        let prepared = prepare_update("9001", &editable(), &changes).unwrap();

        assert_eq!(prepared.body["item"]["price"], "15.0");
        assert_eq!(prepared.expected["item"]["price"], "15.0");
        assert_eq!(prepared.body["item"]["currency"], "EUR");
    }

    #[test]
    fn fails_closed_when_canonical_state_is_missing_or_unknown() {
        for field in ["item_attributes", "photos", "shipment_prices", "model"] {
            let mut raw = editable();
            raw["item"].as_object_mut().unwrap().remove(field);
            let error = project_editable("9001", &raw).unwrap_err();
            assert_eq!(error.code, "vinted_listing_edit.unsupported_editable_state");
        }

        let mut raw = editable();
        raw["item"]["ontology_model_id"] = json!(44);
        let error = project_editable("9001", &raw).unwrap_err();
        assert_eq!(error.details.unwrap()["field"], "ontology_model_id");
    }

    #[test]
    fn fails_closed_instead_of_converting_legacy_condition_id() {
        let mut raw = editable();
        raw["item"]["item_attributes"] = json!([]);
        raw["item"]["status_id"] = json!(3);

        let error = project_editable("9001", &raw).unwrap_err();
        assert_eq!(error.details.unwrap()["field"], "status_id");
    }

    #[test]
    fn rejects_identity_and_photo_order_ambiguity() {
        let identity = project_editable("9002", &editable()).unwrap_err();
        assert_eq!(identity.code, "vinted_listing_edit.item_identity_mismatch");

        let mut duplicate = editable();
        duplicate["item"]["photos"] = json!([{"id":41},{"id":"41"}]);
        let error = project_editable("9001", &duplicate).unwrap_err();
        assert_eq!(error.details.unwrap()["field"], "photos");

        let mut conflicting_main = editable();
        conflicting_main["item"]["photos"] =
            json!([{"id":41,"is_main":false},{"id":42,"is_main":true}]);
        let error = project_editable("9001", &conflicting_main).unwrap_err();
        assert_eq!(error.details.unwrap()["field"], "photos");
    }
}
