use super::normalization::values_semantically_equal;
use super::types::{
    CategoryValidation, ComposerModelStatus, DraftImage, DraftState, ImageState,
    PublicationCategory, PublicationRequirement, PublicationValidation,
};
use crate::domain::commerce::TradeType;
use crate::domain::commerce::normalize_trade_type;
use crate::domain::commerce::select_values_equal;
use crate::domain::field::FieldStatus;
use crate::domain::field::FieldType;
use crate::domain::field::Requirement;
use serde_json::Value;
use std::collections::BTreeMap;

pub fn evaluate_publication(
    state: &DraftState,
    categories: Option<&[PublicationCategory]>,
    composer_model: ComposerModelStatus,
    delivery_verifiable: bool,
) -> PublicationValidation {
    let mut report = PublicationValidation {
        draft_id: state.draft_id.clone(),
        revision: state.revision.clone().unwrap_or_default(),
        ready: false,
        category_validation: None,
        missing: Vec::new(),
        invalid: Vec::new(),
        pending: Vec::new(),
        unverifiable: Vec::new(),
        evidence_failures: Vec::new(),
    };
    validate_publication_core(
        state,
        categories,
        composer_model,
        delivery_verifiable,
        &mut report,
    );
    validate_publication_composer(state, composer_model, &mut report);
    validate_publication_images(state, &mut report);
    for requirements in [
        &mut report.missing,
        &mut report.invalid,
        &mut report.pending,
        &mut report.unverifiable,
    ] {
        requirements.sort();
        requirements.dedup();
    }
    report.ready = report.missing.is_empty()
        && report.invalid.is_empty()
        && report.pending.is_empty()
        && report.unverifiable.is_empty();
    report
}

fn validate_publication_core(
    state: &DraftState,
    categories: Option<&[PublicationCategory]>,
    composer_model: ComposerModelStatus,
    delivery_verifiable: bool,
    report: &mut PublicationValidation,
) {
    validate_publication_category(state, categories, composer_model, report);

    validate_publication_text(state, "title", report);
    validate_publication_text(state, "description", report);

    let trade_type = publication_field_value(state, "trade_type");
    let trade_type = if trade_type.is_none_or(publication_value_missing) {
        report.missing.push(publication_core_issue(
            state,
            "trade_type",
            "a trade type is required for publication",
        ));
        None
    } else {
        match normalize_trade_type(trade_type) {
            TradeType::Sell => Some("sell"),
            TradeType::GiveAway => Some("give_away"),
            TradeType::Wanted => Some("wanted"),
            TradeType::Unknown => {
                report.invalid.push(publication_core_issue(
                    state,
                    "trade_type",
                    "the trade type must identify a sale, give-away, or wanted listing",
                ));
                None
            }
        }
    };

    let price = publication_field_value(state, "price");
    match trade_type {
        Some("sell") => match price.and_then(publication_numeric_value) {
            None if price.is_none_or(publication_value_missing) => report.missing.push(
                publication_core_issue(state, "price", "sale listings require a price"),
            ),
            Some(price) if price > 0.0 => {}
            _ => report.invalid.push(publication_core_issue(
                state,
                "price",
                "a sale price must be a positive number",
            )),
        },
        Some("give_away") if price.is_some_and(|price| !publication_value_missing(price)) => {
            report.invalid.push(publication_core_issue(
                state,
                "price",
                "give-away listings cannot include a sale price",
            ));
        }
        Some("wanted") if price.is_some_and(|price| publication_numeric_value(price).is_none()) => {
            report.invalid.push(publication_core_issue(
                state,
                "price",
                "the price must be numeric when supplied",
            ));
        }
        _ => {}
    }

    match publication_field_value(state, "postal_code") {
        None => report.missing.push(publication_core_issue(
            state,
            "postal_code",
            "a postal location is required for publication",
        )),
        Some(Value::String(postal_code))
            if postal_code.len() == 5
                && postal_code
                    .bytes()
                    .all(|character| character.is_ascii_digit()) => {}
        Some(_) => report.invalid.push(publication_core_issue(
            state,
            "postal_code",
            "the postal location must contain a five-digit postal code",
        )),
    }

    if !delivery_verifiable {
        report.unverifiable.push(publication_issue(
            "delivery",
            "delivery configuration could not be verified",
            "delivery_composer",
            format!("flea tori draft show {}", state.draft_id),
        ));
    } else {
        match state.delivery.as_ref() {
            Some(delivery) if delivery.selected.is_empty() => {
                report.missing.push(publication_core_issue(
                    state,
                    "delivery",
                    "explicit delivery intent is required for publication",
                ))
            }
            Some(delivery)
                if delivery.selected.len() == 1
                    && delivery
                        .options
                        .iter()
                        .any(|option| option.value == delivery.selected[0]) => {}
            Some(_) => report.invalid.push(publication_issue(
                "delivery",
                "the selected delivery value is unavailable or ambiguous",
                "delivery_composer",
                format!("flea tori draft show {}", state.draft_id),
            )),
            None => report.unverifiable.push(publication_issue(
                "delivery",
                "delivery configuration could not be verified",
                "delivery_composer",
                format!("flea tori draft show {}", state.draft_id),
            )),
        }
    }
}

fn validate_publication_category(
    state: &DraftState,
    categories: Option<&[PublicationCategory]>,
    composer_model: ComposerModelStatus,
    report: &mut PublicationValidation,
) {
    let category_value = publication_field_value(state, "category");
    let Some(category_id) = category_value.and_then(publication_scalar_string) else {
        if category_value.is_none_or(publication_value_missing) {
            report.missing.push(publication_issue(
                "category",
                "a category is required for publication",
                "publication_invariant",
                format!("flea tori draft update {} --category VALUE", state.draft_id),
            ));
        } else {
            report.invalid.push(publication_issue(
                "category",
                "the category must be a non-empty machine value",
                "publication_invariant",
                format!("flea tori draft update {} --category VALUE", state.draft_id),
            ));
        }
        return;
    };

    let taxonomy_category = categories.and_then(|categories| {
        categories
            .iter()
            .find(|category| category.category_id == category_id)
    });
    let taxonomy_available = categories.is_some();
    let category_field = state
        .fields
        .iter()
        .find(|field| publication_field_name(&field.key) == "category");
    let composer_option = state.options.iter().find(|option| {
        publication_field_name(&option.field) == "category"
            && values_semantically_equal(category_value.expect("category value"), &option.value)
    });
    let compatible = match (composer_model, category_field) {
        (ComposerModelStatus::Available, Some(field)) if field.status == FieldStatus::Invalid => {
            Some(false)
        }
        (ComposerModelStatus::Available, Some(field)) if !field.validation_options.is_empty() => {
            Some(field.validation_options.iter().any(|option| {
                values_semantically_equal(category_value.expect("category value"), option)
            }))
        }
        (ComposerModelStatus::Available, Some(_)) if composer_option.is_some() => Some(true),
        (ComposerModelStatus::Available, Some(field))
            if field.option_count > 0 && !field.options_truncated =>
        {
            Some(false)
        }
        (ComposerModelStatus::Available, None) => Some(true),
        _ => None,
    };

    report.category_validation = Some(CategoryValidation {
        value: category_id.clone(),
        label: taxonomy_category
            .map(|category| category.label.clone())
            .or_else(|| composer_option.map(|option| option.label.clone())),
        exists: taxonomy_available.then_some(taxonomy_category.is_some()),
        selectable: taxonomy_category.map(|category| category.selectable),
        compatible,
        existence_source: taxonomy_available.then(|| "category_taxonomy".to_owned()),
        selectability_source: taxonomy_category.map(|_| "category_taxonomy".to_owned()),
        compatibility_source: compatible.map(|_| "listing_composer".to_owned()),
    });

    match (taxonomy_available, taxonomy_category) {
        (false, _) => report.unverifiable.push(publication_issue(
            "category",
            "category existence and selectability could not be verified",
            "category_taxonomy",
            "flea tori category list".to_owned(),
        )),
        (true, None) => report.invalid.push(publication_issue(
            "category",
            "the selected category is absent from or inaccessible in the current taxonomy",
            "category_taxonomy",
            format!("flea tori category search {category_id}"),
        )),
        (true, Some(category)) if !category.selectable => report.invalid.push(publication_issue(
            "category",
            "the selected category cannot contain listings",
            "category_taxonomy",
            format!("flea tori category search {category_id}"),
        )),
        _ => {}
    }

    match compatible {
        Some(true) => {}
        Some(false) => report.invalid.push(publication_issue(
            "category",
            "the selected category is incompatible with the current listing-composer schema",
            "listing_composer",
            format!("flea tori draft show {}", state.draft_id),
        )),
        None if composer_model == ComposerModelStatus::Available => {
            report.unverifiable.push(publication_issue(
                "category",
                "category compatibility could not be verified from the listing composer",
                "listing_composer",
                format!("flea tori draft show {}", state.draft_id),
            ));
        }
        None => {}
    }
}

fn validate_publication_text(state: &DraftState, field: &str, report: &mut PublicationValidation) {
    match publication_field_value(state, field) {
        None => report.missing.push(publication_core_issue(
            state,
            field,
            &format!("a {field} is required for publication"),
        )),
        Some(Value::String(value)) if !value.trim().is_empty() => {}
        Some(Value::String(_)) => report.missing.push(publication_core_issue(
            state,
            field,
            &format!("a {field} is required for publication"),
        )),
        Some(_) => report.invalid.push(publication_core_issue(
            state,
            field,
            &format!("the {field} must be text"),
        )),
    }
}

fn validate_publication_composer(
    state: &DraftState,
    composer_model: ComposerModelStatus,
    report: &mut PublicationValidation,
) {
    if composer_model != ComposerModelStatus::Available {
        let reason = match composer_model {
            ComposerModelStatus::Unavailable => "listing-composer requirements are unavailable",
            ComposerModelStatus::Malformed => "listing-composer requirements are malformed",
            ComposerModelStatus::Available => unreachable!(),
        };
        report.unverifiable.push(publication_issue(
            "composer_model",
            reason,
            "listing_composer",
            format!("flea tori draft validate {}", state.draft_id),
        ));
        return;
    }

    for field in &state.fields {
        let publication_field = publication_field_name(&field.key);
        if publication_field == "category" {
            continue;
        }
        if field.requirement == Requirement::Required
            && !publication_report_contains(report, publication_field)
            && publication_field_value(state, publication_field)
                .is_none_or(publication_value_missing)
        {
            report.missing.push(publication_issue(
                publication_field,
                "the selected category requires this field",
                "listing_composer",
                format!("flea tori draft show {}", state.draft_id),
            ));
            continue;
        }
        if field.status == FieldStatus::Invalid
            && !publication_report_contains(report, publication_field)
        {
            report.invalid.push(publication_issue(
                publication_field,
                field
                    .validation_message
                    .as_deref()
                    .unwrap_or("the listing composer rejected this value"),
                "listing_composer",
                format!("flea tori draft show {}", state.draft_id),
            ));
            continue;
        }
        if field.requirement == Requirement::Required
            && matches!(field.field_type, FieldType::Unknown(_))
            && !publication_report_contains(report, publication_field)
        {
            report.unverifiable.push(publication_issue(
                publication_field,
                "the required listing-composer field has an unknown type",
                "listing_composer",
                format!("flea tori draft show {}", state.draft_id),
            ));
        }
    }

    for field in &state.fields {
        let publication_field = publication_field_name(&field.key);
        if publication_field == "category" || publication_report_contains(report, publication_field)
        {
            continue;
        }
        let options = state
            .options
            .iter()
            .filter(|option| option.field == field.key)
            .collect::<Vec<_>>();
        if options.is_empty() {
            continue;
        }
        let Some(value) = publication_field_value(state, publication_field) else {
            continue;
        };
        let valid = match value {
            Value::Array(values) => values.iter().all(|value| {
                options
                    .iter()
                    .any(|option| select_values_equal(&field.key, value, &option.value))
            }),
            value => options
                .iter()
                .any(|option| select_values_equal(&field.key, value, &option.value)),
        };
        if !valid && field.options_truncated {
            report.unverifiable.push(publication_issue(
                publication_field,
                "the listing-composer options are truncated",
                "listing_composer",
                format!("flea tori draft show {}", state.draft_id),
            ));
        } else if !valid {
            report.invalid.push(publication_issue(
                publication_field,
                "the value is not an option in the current listing composer",
                "listing_composer",
                format!("flea tori draft show {}", state.draft_id),
            ));
        }
    }
}

fn validate_publication_images(state: &DraftState, report: &mut PublicationValidation) {
    if state.images.is_empty() {
        report.missing.push(publication_issue(
            "images",
            "at least one image is required for publication",
            "publication_invariant",
            format!("flea tori draft image add {} PATH", state.draft_id),
        ));
        return;
    }
    let mut images = state.images.iter().collect::<Vec<_>>();
    images.sort_by(|left, right| {
        left.position
            .cmp(&right.position)
            .then_with(|| left.image_id.cmp(&right.image_id))
    });
    let pending = images
        .iter()
        .filter(|image| image.state == ImageState::Processing)
        .map(|image| image.image_id.as_str())
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        report.pending.push(publication_issue(
            "images",
            format!("image processing is pending: {}", pending.join(", ")),
            "image_processing",
            format!("flea tori draft validate {}", state.draft_id),
        ));
    }
    let failed = images
        .iter()
        .filter(|image| image.state == ImageState::Failed)
        .map(|image| image.image_id.as_str())
        .collect::<Vec<_>>();
    if !failed.is_empty() {
        report.invalid.push(publication_issue(
            "images",
            format!("image processing rejected: {}", failed.join(", ")),
            "image_processing",
            format!(
                "flea tori draft image remove {} {}",
                state.draft_id,
                failed.join(" ")
            ),
        ));
    }
}

fn publication_field_value<'a>(state: &'a DraftState, field: &str) -> Option<&'a Value> {
    let aliases: &[&str] = match field {
        "category" => &["category", "category_id", "categoryId"],
        "title" => &["title", "subject", "heading"],
        "description" => &["description", "body", "text"],
        "trade_type" => &["trade_type", "trade-type", "tradeType"],
        "price" => &["price", "price_amount", "price_max"],
        "postal_code" => &[
            "postal_code",
            "postal-code",
            "post-code",
            "postcode",
            "postalCode",
        ],
        "images" => &["multi_image", "multi-image", "image", "images"],
        "delivery" => &["delivery"],
        _ => &[],
    };
    if let Some(value) = aliases.iter().find_map(|alias| state.values.get(*alias)) {
        return Some(value);
    }
    if let Some(value) = state.values.get(field) {
        return Some(value);
    }
    if field == "postal_code"
        && let Some(value) = publication_postal_value(state.values.get("location")?)
    {
        return Some(value);
    }
    state
        .fields
        .iter()
        .find(|model_field| publication_field_name(&model_field.key) == field)
        .and_then(|model_field| model_field.value.as_ref())
}

fn publication_postal_value(location: &Value) -> Option<&Value> {
    let location = match location {
        Value::Array(locations) => locations.first()?,
        location => location,
    };
    let location = location.as_object()?;
    ["postal_code", "postal-code", "postalCode"]
        .into_iter()
        .find_map(|key| location.get(key))
}

fn publication_field_name(field: &str) -> &str {
    match field {
        "subject" | "heading" => "title",
        "body" | "text" => "description",
        "categoryId" | "category_id" => "category",
        "tradeType" | "trade-type" => "trade_type",
        "price_amount" | "price_max" => "price",
        "postalCode" | "postal-code" | "post-code" | "postcode" => "postal_code",
        "multi_image" | "multi-image" | "image" => "images",
        field => field,
    }
}

pub(super) fn publication_scalar_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if !value.trim().is_empty() => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn publication_value_missing(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(value) => value.trim().is_empty(),
        Value::Array(values) => values.is_empty(),
        Value::Object(values) => values.is_empty(),
        _ => false,
    }
}

fn publication_numeric_value(value: &Value) -> Option<f64> {
    let value = match value {
        Value::Number(value) => value.as_f64()?,
        Value::String(value) => value.parse().ok()?,
        _ => return None,
    };
    value.is_finite().then_some(value)
}

fn publication_issue(
    field: impl Into<String>,
    reason: impl Into<String>,
    source: impl Into<String>,
    command: String,
) -> PublicationRequirement {
    PublicationRequirement {
        field: field.into(),
        reason: reason.into(),
        source: source.into(),
        command,
    }
}

fn publication_core_issue(state: &DraftState, field: &str, reason: &str) -> PublicationRequirement {
    let option = match field {
        "category" => "--category VALUE",
        "title" => "--title VALUE",
        "description" => "--description VALUE",
        "trade_type" => "--trade-type VALUE",
        "price" => "--price VALUE",
        "postal_code" => "--postal-code VALUE",
        "delivery" => "--delivery VALUE",
        _ => "--input PATH",
    };
    publication_issue(
        field,
        reason,
        "publication_invariant",
        format!("flea tori draft update {} {option}", state.draft_id),
    )
}

fn publication_report_contains(report: &PublicationValidation, field: &str) -> bool {
    report
        .missing
        .iter()
        .chain(&report.invalid)
        .chain(&report.pending)
        .chain(&report.unverifiable)
        .any(|requirement| requirement.field == field)
}

pub(super) fn delivery_values(delivery: &Value) -> Option<Vec<String>> {
    match delivery {
        Value::String(value) if !value.trim().is_empty() => Some(vec![value.clone()]),
        Value::Array(values) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .filter(|value| !value.trim().is_empty())
                    .map(str::to_owned)
            })
            .collect(),
        _ => None,
    }
}

pub fn ordered_image_states(images: &[DraftImage]) -> BTreeMap<usize, (&str, &ImageState)> {
    images
        .iter()
        .map(|image| (image.position, (image.image_id.as_str(), &image.state)))
        .collect()
}
