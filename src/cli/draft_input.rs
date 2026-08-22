use std::{collections::BTreeMap, path::PathBuf};

use serde_json::{Map, Value, json};

use crate::{
    api::{
        adinput::{PreparedImage, normalize_category, prepare_image},
        listings::{ListingsApi, ListingsApiError, UpstreamCategory},
    },
    error::{AppError, ExitClass},
};

use super::draft::CollectedInput;

const TITLE_MAX_CHARS: usize = 100;
const DESCRIPTION_MAX_CHARS: usize = 10_000;
const ATTRIBUTES_MAX_BYTES: usize = 32 * 1024;
const DELIVERY_MAX_VALUES: usize = 8;
const IMAGE_MAX_COUNT: usize = 12;

pub struct NormalizedInput {
    pub values: Map<String, Value>,
    pub images: Vec<PreparedImage>,
    image_paths: Vec<PathBuf>,
}

pub fn normalize(input: CollectedInput, process_images: bool) -> Result<NormalizedInput, AppError> {
    let values = normalize_values(input.values)?;
    if input.image_paths.len() > IMAGE_MAX_COUNT {
        return Err(validation_error(BTreeMap::from([(
            "image".to_owned(),
            format!("at most {IMAGE_MAX_COUNT} images are accepted"),
        )])));
    }

    let images = if process_images {
        prepare_images(&input.image_paths)?
    } else {
        Vec::new()
    };
    Ok(NormalizedInput {
        values,
        images,
        image_paths: input.image_paths,
    })
}

pub fn preview(
    input: CollectedInput,
    verify_category: bool,
    taxonomy: Option<&dyn ListingsApi>,
) -> Result<Value, AppError> {
    let normalized = normalize(input, true)?;
    let category_machine_value = category_machine_value(normalized.values.get("category"));
    if verify_category && category_machine_value.is_none() {
        return Err(validation_error(BTreeMap::from([(
            "category".to_owned(),
            "--verify-category requires a category machine value".to_owned(),
        )])));
    }

    let mut unverifiable = vec![
        "category-specific fields and allowed values require an authoritative remote draft model"
            .to_owned(),
        "postal-code existence is not established by the local five-digit shape check".to_owned(),
        "publication policy, moderation, and server-side image processing are not evaluated"
            .to_owned(),
    ];
    let mut warnings = Vec::new();
    let remote_verification = verify_remote_category(
        verify_category,
        taxonomy,
        category_machine_value.as_deref(),
        &mut unverifiable,
        &mut warnings,
    )?;

    let image_plan = normalized
        .images
        .iter()
        .zip(&normalized.image_paths)
        .enumerate()
        .map(|(position, (image, path))| {
            json!({
                "position": position,
                "file_name": safe_image_name(path, position),
                "source_format": image.source_format(),
                "upload_format": image.output_format(),
                "width": image.width(),
                "height": image.height(),
                "byte_size": image.byte_len(),
                "processing": if image.source_format() == "heic" {
                    "convert_to_jpeg_and_reencode_pixels"
                } else {
                    "reencode_pixels"
                },
                "metadata": if image.metadata_stripped() { "stripped" } else { "unverified" },
                "upload": false,
            })
        })
        .collect::<Vec<_>>();

    let mut create_input = normalized.values.clone();
    if !normalized.image_paths.is_empty() {
        create_input.insert(
            "image".to_owned(),
            Value::Array(
                normalized
                    .image_paths
                    .iter()
                    .enumerate()
                    .map(|(position, path)| Value::String(safe_image_name(path, position)))
                    .collect(),
            ),
        );
    }
    let create_input_bytes = serde_json::to_vec(&create_input)
        .map_err(|error| AppError::output("failed to serialize draft preview").with_source(error))?
        .len();

    let description_summary = normalized
        .values
        .get("description")
        .and_then(Value::as_str)
        .map(|description| {
            json!({
                "characters": description.chars().count(),
                "lines": description.lines().count().max(1),
                "excerpt": excerpt(description, 120),
            })
        });
    let structured_price = normalized.values.get("price").map(|amount| {
        json!({
            "amount": amount,
            "currency": "EUR",
        })
    });

    Ok(json!({
        "mode": "local_draft_preview",
        "remote_mutation": "none",
        "local_validation": {
            "status": "passed",
            "scope": [
                "input_shape",
                "text_bounds",
                "price_syntax",
                "postal_code_shape",
                "delivery_values",
                "image_files",
                "image_formats",
                "image_dimensions",
                "metadata_processing",
            ],
        },
        "normalized": {
            "title": normalized.values.get("title"),
            "description_summary": description_summary,
            "price": structured_price,
            "trade_type": normalized.values.get("trade_type"),
            "postal_code": normalized.values.get("postal_code"),
            "delivery_intent": delivery_methods(normalized.values.get("delivery")),
            "category_machine_value": category_machine_value,
        },
        "image_plan": image_plan,
        "remote_verification": remote_verification,
        "assumptions": [
            "prices use euros",
            "text limits are generic local safety bounds rather than category-specific publication rules",
            "image metadata is removed by decoding and re-encoding pixels before any upload",
            "preview creates no remote draft and does not claim publication readiness",
        ],
        "unverifiable_requirements": unverifiable,
        "create": {
            "command": "flea draft create --input listing.json",
            "input_file_name": "listing.json",
            "input_byte_length": create_input_bytes,
            "input": create_input,
            "path_policy": "image paths contain file names only; replace them if the images are stored elsewhere",
        },
        "warnings": warnings,
    }))
}

fn normalize_values(mut values: Map<String, Value>) -> Result<Map<String, Value>, AppError> {
    let allowed = [
        "attributes",
        "category",
        "delivery",
        "description",
        "postal_code",
        "price",
        "title",
        "trade_type",
    ];
    let mut issues = BTreeMap::new();
    for key in values.keys() {
        if !allowed.contains(&key.as_str()) {
            issues.insert(key.clone(), "unknown draft input field".to_owned());
        }
    }

    normalize_string(&mut values, "title", TITLE_MAX_CHARS, false, &mut issues);
    normalize_string(
        &mut values,
        "description",
        DESCRIPTION_MAX_CHARS,
        true,
        &mut issues,
    );

    if let Some(category) = values.get_mut("category") {
        match category {
            Value::String(value) => {
                *value = value.trim().to_owned();
                if value.is_empty()
                    || value.len() > 128
                    || !value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'/')
                    })
                {
                    issues.insert(
                        "category".to_owned(),
                        "expected a non-empty category machine value".to_owned(),
                    );
                }
            }
            Value::Number(number) if number.as_u64().is_some() => {}
            _ => {
                issues.insert(
                    "category".to_owned(),
                    "expected a string or unsigned numeric machine value".to_owned(),
                );
            }
        }
        *category = normalize_category(category.clone());
    }

    if let Some(price) = values.get("price")
        && price
            .as_f64()
            .is_none_or(|price| !price.is_finite() || price < 0.0)
    {
        issues.insert(
            "price".to_owned(),
            "expected a non-negative JSON number".to_owned(),
        );
    }

    if let Some(trade_type) = values.get_mut("trade_type") {
        match trade_type.as_str() {
            Some("sell" | "give_away" | "wanted") => {}
            _ => {
                issues.insert(
                    "trade_type".to_owned(),
                    "expected one of: sell, give_away, wanted".to_owned(),
                );
            }
        }
    }

    if let Some(postal_code) = values.get_mut("postal_code") {
        match postal_code.as_str() {
            Some(value) if value.len() == 5 && value.bytes().all(|byte| byte.is_ascii_digit()) => {}
            _ => {
                issues.insert(
                    "postal_code".to_owned(),
                    "expected exactly five ASCII digits".to_owned(),
                );
            }
        }
    }

    if let Some(delivery) = values.get_mut("delivery")
        && let Err(message) = normalize_delivery(delivery)
    {
        issues.insert("delivery".to_owned(), message);
    }

    if let Some(attributes) = values.get("attributes") {
        match attributes {
            Value::Object(_) => {
                let byte_len =
                    serde_json::to_vec(attributes).map_or(usize::MAX, |value| value.len());
                if byte_len > ATTRIBUTES_MAX_BYTES {
                    issues.insert(
                        "attributes".to_owned(),
                        "attribute data exceeds the 32 KiB local limit".to_owned(),
                    );
                } else if contains_unsafe_metadata(attributes) {
                    issues.insert(
                        "attributes".to_owned(),
                        "attribute keys must not contain credentials or private metadata"
                            .to_owned(),
                    );
                }
            }
            _ => {
                issues.insert("attributes".to_owned(), "expected a JSON object".to_owned());
            }
        }
    }

    if issues.is_empty() {
        Ok(values)
    } else {
        Err(validation_error(issues))
    }
}

fn normalize_string(
    values: &mut Map<String, Value>,
    field: &str,
    maximum: usize,
    multiline: bool,
    issues: &mut BTreeMap<String, String>,
) {
    let Some(value) = values.get_mut(field) else {
        return;
    };
    let Value::String(text) = value else {
        issues.insert(field.to_owned(), "expected a string".to_owned());
        return;
    };
    *text = text.trim().to_owned();
    let length = text.chars().count();
    if length == 0 || length > maximum {
        issues.insert(
            field.to_owned(),
            format!("expected 1 through {maximum} characters"),
        );
        return;
    }
    if text.chars().any(|character| {
        character.is_control() && (!multiline || !matches!(character, '\n' | '\r' | '\t'))
    }) {
        issues.insert(
            field.to_owned(),
            "contains unsupported control characters".to_owned(),
        );
    }
}

fn normalize_delivery(delivery: &mut Value) -> Result<(), String> {
    let methods = match delivery {
        Value::String(method) => vec![method.trim().to_owned()],
        Value::Array(methods) => methods
            .iter()
            .map(|method| {
                method
                    .as_str()
                    .map(str::trim)
                    .map(str::to_owned)
                    .ok_or_else(|| "expected delivery machine-value strings".to_owned())
            })
            .collect::<Result<Vec<_>, _>>()?,
        Value::Object(object) => {
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "methods" | "shipping"))
            {
                return Err("delivery object contains an unknown field".to_owned());
            }
            let methods = object
                .get("methods")
                .and_then(Value::as_array)
                .ok_or_else(|| "delivery object requires a methods array".to_owned())?
                .iter()
                .map(|method| {
                    method
                        .as_str()
                        .map(str::trim)
                        .map(str::to_owned)
                        .ok_or_else(|| "expected delivery machine-value strings".to_owned())
                })
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(shipping) = object.get("shipping") {
                let shipping = shipping.as_object().ok_or_else(|| {
                    "delivery shipping configuration must be an object".to_owned()
                })?;
                if shipping
                    .keys()
                    .any(|key| !matches!(key.as_str(), "price" | "provider" | "size"))
                {
                    return Err(
                        "delivery shipping configuration contains an unknown field".to_owned()
                    );
                }
                for key in ["provider", "size"] {
                    if let Some(value) = shipping.get(key)
                        && value.as_str().is_none_or(|value| {
                            value.is_empty() || value.len() > 64 || !semantic_machine_value(value)
                        })
                    {
                        return Err(format!(
                            "delivery shipping {key} must be a lowercase machine value"
                        ));
                    }
                }
                if let Some(price) = shipping.get("price")
                    && price
                        .as_f64()
                        .is_none_or(|price| !price.is_finite() || price < 0.0)
                {
                    return Err("delivery shipping price must be non-negative".to_owned());
                }
            }
            methods
        }
        _ => return Err("expected a string, array, or delivery object".to_owned()),
    };

    if methods.is_empty() || methods.len() > DELIVERY_MAX_VALUES {
        return Err(format!(
            "expected 1 through {DELIVERY_MAX_VALUES} delivery values"
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for method in &methods {
        if !semantic_machine_value(method) {
            return Err("expected non-empty lowercase delivery machine values".to_owned());
        }
        if !seen.insert(method.clone()) {
            return Err(format!("duplicate delivery value `{method}`"));
        }
    }

    match delivery {
        Value::Object(object) => {
            object.insert(
                "methods".to_owned(),
                Value::Array(methods.into_iter().map(Value::String).collect()),
            );
        }
        _ => {
            *delivery = Value::Array(methods.into_iter().map(Value::String).collect());
        }
    }
    Ok(())
}

fn semantic_machine_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b':')
        })
}

fn contains_unsafe_metadata(value: &Value) -> bool {
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
            matches!(
                normalized.as_str(),
                "accesstoken"
                    | "authorization"
                    | "cookie"
                    | "exif"
                    | "gps"
                    | "idtoken"
                    | "latitude"
                    | "longitude"
                    | "password"
                    | "refreshtoken"
                    | "secret"
                    | "xmp"
            ) || contains_unsafe_metadata(value)
        }),
        Value::Array(values) => values.iter().any(contains_unsafe_metadata),
        _ => false,
    }
}

fn prepare_images(paths: &[PathBuf]) -> Result<Vec<PreparedImage>, AppError> {
    let mut images = Vec::with_capacity(paths.len());
    let mut issues = BTreeMap::new();
    for (position, path) in paths.iter().enumerate() {
        match prepare_image(path) {
            Ok(image) => images.push(image),
            Err(error) => {
                issues.insert(format!("image[{position}]"), error.message);
            }
        }
    }
    if issues.is_empty() {
        Ok(images)
    } else {
        Err(validation_error(issues))
    }
}

fn verify_remote_category(
    requested: bool,
    taxonomy: Option<&dyn ListingsApi>,
    category: Option<&str>,
    unverifiable: &mut Vec<String>,
    warnings: &mut Vec<String>,
) -> Result<Value, AppError> {
    if !requested {
        return Ok(json!({
            "requested": false,
            "status": "not_requested",
            "source": "local_only",
            "verified_constraints": [],
        }));
    }
    let Some(taxonomy) = taxonomy else {
        unverifiable.push("category taxonomy could not be queried".to_owned());
        warnings.push("authenticated category verification was unavailable".to_owned());
        return Ok(json!({
            "requested": true,
            "status": "unavailable",
            "source": "authenticated_taxonomy",
            "verified_constraints": [],
        }));
    };
    let categories = match taxonomy.categories() {
        Ok(categories) => categories,
        Err(error) => {
            unverifiable.push("category taxonomy could not be queried".to_owned());
            warnings.push("authenticated category verification was unavailable".to_owned());
            return Ok(json!({
                "requested": true,
                "status": "unavailable",
                "source": "authenticated_taxonomy",
                "reason": taxonomy_error_kind(&error),
                "verified_constraints": [],
            }));
        }
    };
    let category = category.expect("requested verification requires a category");
    let Some(found) = find_category(&categories, category) else {
        return Err(validation_error(BTreeMap::from([(
            "category".to_owned(),
            "machine value was not found in the authenticated taxonomy".to_owned(),
        )])));
    };
    if found.selectable == Some(false) {
        return Err(validation_error(BTreeMap::from([(
            "category".to_owned(),
            "authenticated taxonomy identifies this category as non-selectable".to_owned(),
        )])));
    }
    let (status, verified_constraints) = if found.selectable == Some(true) {
        (
            "verified",
            json!(["category_exists", "category_selectable"]),
        )
    } else {
        unverifiable
            .push("category selectability was absent from the authenticated taxonomy".to_owned());
        warnings
            .push("category existence was verified but selectability was unavailable".to_owned());
        ("partially_verified", json!(["category_exists"]))
    };
    Ok(json!({
        "requested": true,
        "status": status,
        "source": "authenticated_taxonomy",
        "category": {
            "machine_value": found.id,
            "label": found.label,
            "selectable": found.selectable,
        },
        "verified_constraints": verified_constraints,
    }))
}

fn find_category<'a>(categories: &'a [UpstreamCategory], id: &str) -> Option<&'a UpstreamCategory> {
    categories.iter().find_map(|category| {
        if category.id == id {
            Some(category)
        } else {
            find_category(&category.children, id)
        }
    })
}

fn taxonomy_error_kind(error: &ListingsApiError) -> &'static str {
    match error {
        ListingsApiError::Authentication => "authentication_failed",
        ListingsApiError::NotFound => "endpoint_unavailable",
        ListingsApiError::Conflict => "unexpected_conflict",
        ListingsApiError::Validation { .. } => "upstream_validation_failed",
        ListingsApiError::Upstream(_) => "upstream_unavailable",
        ListingsApiError::UnexpectedResponse(_) => "protocol_unrecognized",
    }
}

fn category_machine_value(value: Option<&Value>) -> Option<String> {
    value.and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) if value.as_u64().is_some() => Some(value.to_string()),
        _ => None,
    })
}

fn delivery_methods(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::Array(values)) => values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        Some(Value::Object(object)) => object
            .get("methods")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn safe_image_name(path: &std::path::Path, position: usize) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && name.len() <= 128)
        .map(str::to_owned)
        .unwrap_or_else(|| format!("image-{}", position + 1))
}

fn excerpt(value: &str, maximum: usize) -> String {
    let mut excerpt = value.chars().take(maximum).collect::<String>();
    if value.chars().count() > maximum {
        excerpt.push_str("...");
    }
    excerpt
}

fn validation_error(fields: BTreeMap<String, String>) -> AppError {
    let mut error = AppError::new(
        "draft.input_invalid",
        "draft input is invalid",
        ExitClass::Validation,
    );
    error.details = Some(Box::new(json!({ "fields": fields })));
    error
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use image::DynamicImage;

    use super::*;

    #[test]
    fn complete_finnish_listing_is_normalized_for_preview() {
        let temporary = tempfile::tempdir().unwrap();
        let image_path = temporary.path().join("tuoli.png");
        DynamicImage::new_rgb8(40, 60).save(&image_path).unwrap();
        let input = CollectedInput {
            values: serde_json::from_value(json!({
                "category": "258",
                "title": "  Koivutuoli  ",
                "description": "Hyväkuntoinen suomalainen koivutuoli. Nouto Helsingistä.",
                "price": 45.50,
                "trade_type": "sell",
                "postal_code": "00100",
                "delivery": ["pickup", "shipping"],
                "attributes": {"condition": "good"}
            }))
            .unwrap(),
            image_paths: vec![image_path],
        };

        let output = preview(input, false, None).unwrap();

        assert_eq!(output["mode"], "local_draft_preview");
        assert_eq!(output["remote_mutation"], "none");
        assert_eq!(output["normalized"]["title"], "Koivutuoli");
        assert_eq!(output["normalized"]["price"]["amount"], 45.50);
        assert_eq!(output["normalized"]["price"]["currency"], "EUR");
        assert_eq!(output["normalized"]["trade_type"], "sell");
        assert_eq!(output["normalized"]["postal_code"], "00100");
        assert_eq!(
            output["normalized"]["delivery_intent"],
            json!(["pickup", "shipping"])
        );
        assert_eq!(output["normalized"]["category_machine_value"], "258");
        assert_eq!(output["image_plan"][0]["width"], 40);
        assert_eq!(output["image_plan"][0]["height"], 60);
        assert_eq!(output["image_plan"][0]["metadata"], "stripped");
        assert_eq!(output["remote_verification"]["status"], "not_requested");
        assert_eq!(
            output["create"]["command"],
            "flea draft create --input listing.json"
        );
        assert!(
            !output
                .to_string()
                .contains(temporary.path().to_str().unwrap())
        );
    }

    #[test]
    fn discovered_shipping_package_is_a_valid_delivery_machine_value() {
        let input = CollectedInput {
            values: serde_json::from_value(json!({
                "delivery": ["shipping:small"]
            }))
            .unwrap(),
            image_paths: Vec::new(),
        };

        let normalized = normalize(input, false).unwrap();

        assert_eq!(normalized.values["delivery"], json!(["shipping:small"]));
    }

    #[test]
    fn invalid_fields_are_reported_together() {
        let input = CollectedInput {
            values: serde_json::from_value(json!({
                "title": " ",
                "description": "bad\u{0000}",
                "price": -1,
                "trade_type": "auction",
                "postal_code": "Helsinki",
                "delivery": ["pickup", "pickup"]
            }))
            .unwrap(),
            image_paths: Vec::new(),
        };

        let error = preview(input, false, None).unwrap_err();
        let fields = &error.details.as_deref().unwrap()["fields"];

        assert_eq!(error.code, "draft.input_invalid");
        for field in [
            "title",
            "description",
            "price",
            "trade_type",
            "postal_code",
            "delivery",
        ] {
            assert!(fields.get(field).is_some(), "missing {field}: {fields}");
        }
    }

    #[test]
    fn missing_images_fail_before_any_remote_work() {
        let input = CollectedInput {
            values: Map::new(),
            image_paths: vec![PathBuf::from("missing-preview-image.jpg")],
        };

        let error = preview(input, false, None).unwrap_err();

        assert_eq!(error.code, "draft.input_invalid");
        assert!(
            error.details.as_deref().unwrap()["fields"]["image[0]"]
                .as_str()
                .unwrap()
                .contains("does not exist")
        );
    }

    #[test]
    fn description_summary_is_bounded_on_character_boundaries() {
        let summary = excerpt(&"ä".repeat(130), 120);
        assert_eq!(summary.chars().count(), 123);
        assert!(summary.ends_with("..."));

        let mut png = Cursor::new(Vec::new());
        DynamicImage::new_rgb8(1, 1)
            .write_to(&mut png, image::ImageFormat::Png)
            .unwrap();
        assert!(!png.into_inner().is_empty());
    }
}
