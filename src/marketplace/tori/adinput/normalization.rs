use super::adapter::{
    draft_id_from_body, malformed_read_response, valid_image_location, validate_price,
};
use super::http::ApiError;
use super::types::{
    ComposerModelStatus, DeliveryComposer, DraftImage, DraftModel, DraftState, FieldOption,
    ImageState, PublicationCategory, PublicationDraftState, model_error,
};
use super::validation::publication_scalar_string;
use crate::domain::commerce::select_values_equal;
use crate::domain::field::Field;
use crate::domain::field::FieldType;
use crate::domain::field::Requirement;
use crate::domain::observation::ObservationSource;
use serde_json::Map;
use serde_json::Value;
use serde_json::json;
use std::collections::BTreeSet;

mod images;
mod options;
mod publication;

use images::*;
use options::*;
pub(super) use publication::{
    normalize_publication_categories, normalize_publication_draft,
    normalize_publication_draft_with_limit,
};

pub(super) const MAX_OPTIONS_PER_FIELD: usize = 50;

fn normalize_draft_values(mut values: Map<String, Value>) -> Result<Map<String, Value>, ApiError> {
    let Some(price) = values.remove("price") else {
        return Ok(values);
    };
    let normalized = if price.is_number() {
        price
    } else {
        let entries = price.as_array().ok_or_else(invalid_source_price)?;
        let [entry] = entries.as_slice() else {
            return Err(invalid_source_price());
        };
        let object = entry.as_object().ok_or_else(invalid_source_price)?;
        if object.len() != 1 {
            return Err(invalid_source_price());
        }
        let amount = object
            .get("price_amount")
            .or_else(|| object.get("price_max"))
            .and_then(Value::as_str)
            .ok_or_else(invalid_source_price)?;
        serde_json::from_str::<Value>(amount).map_err(|_| invalid_source_price())?
    };
    if validate_price(&normalized).is_err() {
        return Err(invalid_source_price());
    }
    values.insert("price".to_owned(), normalized);
    Ok(values)
}

fn invalid_source_price() -> ApiError {
    let mut error = ApiError::new(
        "upstream.unexpected_response",
        "Tori returned an unsupported price representation",
    );
    error.details = Some(Box::new(json!({ "stage": "normalize_price" })));
    error
}

pub(super) fn normalize_draft_state(
    body: Value,
    response_etag: Option<&str>,
) -> Result<DraftState, ApiError> {
    if let Ok(mut normalized) = serde_json::from_value::<DraftState>(body.clone()) {
        if let Some(etag) = response_etag {
            normalized.etag = etag.to_owned();
        }
        normalized.values = normalize_draft_values(normalized.values)?;
        if normalized.revision.is_none() {
            normalized.revision = normalized
                .values
                .get("revision")
                .and_then(revision_value)
                .or_else(|| revision_from_etag(&normalized.etag));
        }
        return Ok(normalized);
    }

    normalize_source_draft_state(body, response_etag)
}

pub(super) fn normalize_authoritative_draft_state(
    body: Value,
    response_etag: Option<&str>,
) -> Result<DraftState, ApiError> {
    normalize_authoritative_draft_state_with_limit(body, response_etag, MAX_OPTIONS_PER_FIELD)
}

fn normalize_authoritative_draft_state_with_limit(
    body: Value,
    response_etag: Option<&str>,
    option_limit: usize,
) -> Result<DraftState, ApiError> {
    if body.get("model").is_some() {
        return normalize_source_draft_state_with_limit(body, response_etag, option_limit);
    }
    let root = body.as_object().ok_or_else(|| {
        model_error(
            "listing_composer",
            "$",
            "authoritative draft state must be an object",
        )
    })?;
    for key in ["fields", "options", "required_fields"] {
        if root.get(key).and_then(Value::as_array).is_none() {
            return Err(model_error(
                "listing_composer",
                &format!("$.{key}"),
                "authoritative normalized model data is unavailable or unrecognized",
            ));
        }
    }
    normalize_draft_state(body, response_etag)
}

pub(super) fn normalize_source_draft_state(
    body: Value,
    response_etag: Option<&str>,
) -> Result<DraftState, ApiError> {
    normalize_source_draft_state_with_limit(body, response_etag, MAX_OPTIONS_PER_FIELD)
}

fn normalize_source_draft_state_with_limit(
    body: Value,
    response_etag: Option<&str>,
    option_limit: usize,
) -> Result<DraftState, ApiError> {
    let draft_id = draft_id_from_body(&body).ok_or_else(|| {
        model_error(
            "listing_composer",
            "$",
            "draft response did not contain an authoritative identity",
        )
    })?;
    let ad = body
        .get("ad")
        .and_then(Value::as_object)
        .ok_or_else(|| model_error("listing_composer", "$.ad", "ad data is unavailable"))?;
    let values = normalize_draft_values(
        ad.get("values")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    "$.ad.values",
                    "draft values are unavailable or unrecognized",
                )
            })?,
    )?;
    let etag = response_etag
        .or_else(|| ad.get("etag").and_then(Value::as_str))
        .filter(|etag| !etag.is_empty())
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.ad.etag",
                "draft revision metadata is unavailable",
            )
        })?
        .to_owned();
    let revision = extract_revision(ad, &values, &etag)?;
    let model = body
        .get("model")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.model",
                "listing composer model is unavailable or unrecognized",
            )
        })?;
    let normalized_model = normalize_listing_model(model, &values, option_limit)?;
    let images = normalize_draft_images(&values)?;
    let DraftModel {
        fields,
        options,
        required_fields,
        values: normalized_values,
    } = normalized_model;
    let mut values = values;
    values.extend(normalized_values);

    Ok(DraftState {
        draft_id,
        etag,
        revision: Some(revision),
        values,
        fields,
        options,
        required_fields,
        images,
        cleared_fields: Vec::new(),
        predictions: Vec::new(),
        delivery: None,
    })
}

fn normalize_listing_model(
    model: &Map<String, Value>,
    values: &Map<String, Value>,
    option_limit: usize,
) -> Result<DraftModel, ApiError> {
    let sections = model
        .get("sections")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.model.sections",
                "composer sections are unavailable or unrecognized",
            )
        })?;
    let mut normalized = DraftModel::default();
    let mut field_names = BTreeSet::new();
    for (section_index, section) in sections.iter().enumerate() {
        let path = format!("$.model.sections[{section_index}]");
        let section = section
            .as_object()
            .ok_or_else(|| model_error("listing_composer", &path, "section must be an object"))?;
        let section_name = match section.get("type") {
            Some(Value::String(name)) if safe_machine_identifier(name) => name.clone(),
            Some(_) => {
                return Err(model_error(
                    "listing_composer",
                    &format!("{path}.type"),
                    "section type is unavailable or unsafe",
                ));
            }
            None => format!("section_{section_index}"),
        };
        let content = section
            .get("content")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.content"),
                    "section content is unavailable or unrecognized",
                )
            })?;
        for (widget_index, widget) in content.iter().enumerate() {
            normalize_widget(
                widget,
                &format!("{path}.content[{widget_index}]"),
                &section_name,
                values,
                &mut field_names,
                &mut normalized,
                option_limit,
            )?;
        }
    }
    normalized.required_fields = normalized
        .fields
        .iter()
        .filter(|field| field.requirement == Requirement::Required)
        .map(|field| field.key.clone())
        .collect();
    Ok(normalized)
}

fn normalize_widget(
    widget: &Value,
    path: &str,
    section: &str,
    values: &Map<String, Value>,
    field_names: &mut BTreeSet<String>,
    normalized: &mut DraftModel,
    option_limit: usize,
) -> Result<(), ApiError> {
    let widget = widget
        .as_object()
        .ok_or_else(|| model_error("listing_composer", path, "widget must be an object"))?;
    let id = required_model_string(widget, "id", path)?;
    let upstream_type = required_model_string(widget, "type", path)?;
    if !widget_is_applicable(widget, values, path)? {
        return Ok(());
    }

    if upstream_type == "complex" {
        let children = widget
            .get("children")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.children"),
                    "complex widget children are unavailable or unrecognized",
                )
            })?;
        for (child_index, child) in children.iter().enumerate() {
            normalize_widget(
                child,
                &format!("{path}.children[{child_index}]"),
                section,
                values,
                field_names,
                normalized,
                option_limit,
            )?;
        }
        return Ok(());
    }

    if matches!(
        upstream_type.as_str(),
        "multi-image"
            | "image"
            | "static"
            | "info-text"
            | "attention"
            | "context-attention"
            | "section-title"
            | "proceed"
    ) {
        return Ok(());
    }

    if !field_names.insert(id.clone()) {
        return Err(model_error(
            "listing_composer",
            path,
            "composer contains duplicate applicable field names",
        ));
    }
    let label = match widget.get("label") {
        Some(Value::String(label)) if safe_display_string(label) => label.clone(),
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.label"),
                "field label is unavailable or unsafe",
            ));
        }
        None => id.clone(),
    };
    let mandatory = has_mandatory_rule(widget, path)?;
    let requirement = match widget.get("required") {
        Some(Value::Bool(true)) => Requirement::Required,
        Some(Value::Bool(false)) if mandatory => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.required"),
                "required state conflicts with mandatory validation",
            ));
        }
        Some(Value::Bool(false)) => Requirement::Optional,
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.required"),
                "required state must be a boolean",
            ));
        }
        None if mandatory => Requirement::Required,
        None => Requirement::Unknown,
    };
    let field_type = normalize_field_type(widget, &upstream_type, path)?;
    let value = model_field_value(values, &id);
    let option_result = normalize_widget_options(
        widget,
        &upstream_type,
        &id,
        value.as_ref(),
        path,
        option_limit,
    )?;
    let mut field = Field::new(
        id.clone(),
        label,
        field_type.clone(),
        requirement,
        value,
        section,
    );
    field.option_count = option_result.total;
    field.options_returned = option_result.options.len();
    field.options_truncated = option_result.total > option_result.options.len();
    field.validation_options = option_result.validation_options;
    if matches!(field_type, FieldType::Unknown(_)) {
        field.raw = Some(json!({
            "type": upstream_type,
            "sub_type": widget.get("sub-type").and_then(Value::as_str),
            "has_children": widget.get("children").is_some(),
            "has_options": widget.get("items").is_some() || widget.get("options").is_some()
        }));
    }
    normalized.options.extend(option_result.options);
    normalized.fields.push(field);
    Ok(())
}

fn required_model_string(
    object: &Map<String, Value>,
    key: &str,
    path: &str,
) -> Result<String, ApiError> {
    let value = object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| safe_machine_identifier(value))
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.{key}"),
                "machine identifier is unavailable or unsafe",
            )
        })?;
    Ok(value.to_owned())
}

pub(super) fn safe_machine_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

pub(super) fn safe_display_string(value: &str) -> bool {
    !value.is_empty() && value.len() <= 1024 && !value.chars().any(char::is_control)
}

fn widget_is_applicable(
    widget: &Map<String, Value>,
    values: &Map<String, Value>,
    path: &str,
) -> Result<bool, ApiError> {
    match widget.get("hidden") {
        Some(Value::Bool(true)) => return Ok(false),
        Some(Value::Bool(false)) | None => {}
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.hidden"),
                "hidden state must be a boolean",
            ));
        }
    }
    if let Some(dependencies) = widget.get("dependencies") {
        let dependencies = dependencies.as_array().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.dependencies"),
                "dependencies must be an array",
            )
        })?;
        for dependency in dependencies {
            let dependency = dependency.as_str().ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.dependencies"),
                    "dependency names must be strings",
                )
            })?;
            if model_field_value(values, dependency)
                .as_ref()
                .is_none_or(|value| !value_is_present(value))
            {
                return Ok(false);
            }
        }
    }
    let Some(exclusive) = widget.get("exclusive-dependencies") else {
        return Ok(true);
    };
    let exclusive = exclusive.as_object().ok_or_else(|| {
        model_error(
            "listing_composer",
            &format!("{path}.exclusive-dependencies"),
            "exclusive dependencies must be an object",
        )
    })?;
    for (dependency, allowed) in exclusive {
        let allowed = allowed.as_array().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.exclusive-dependencies.{dependency}"),
                "exclusive dependency values must be an array",
            )
        })?;
        let Some(selected) = model_field_value(values, dependency) else {
            return Ok(false);
        };
        if !allowed
            .iter()
            .any(|allowed| values_semantically_equal(&selected, allowed))
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn values_semantically_equal(left: &Value, right: &Value) -> bool {
    if left == right {
        return true;
    }
    match (left, right) {
        (Value::Array(values), right) | (right, Value::Array(values)) => values
            .iter()
            .any(|value| values_semantically_equal(value, right)),
        (Value::String(left), Value::Number(right)) => left == &right.to_string(),
        (Value::Number(left), Value::String(right)) => &left.to_string() == right,
        _ => false,
    }
}

fn has_mandatory_rule(widget: &Map<String, Value>, path: &str) -> Result<bool, ApiError> {
    let Some(rules) = widget.get("validation-rules") else {
        return Ok(false);
    };
    let rules = rules.as_array().ok_or_else(|| {
        model_error(
            "listing_composer",
            &format!("{path}.validation-rules"),
            "validation rules must be an array",
        )
    })?;
    for (index, rule) in rules.iter().enumerate() {
        let rule = rule.as_object().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.validation-rules[{index}]"),
                "validation rule must be an object",
            )
        })?;
        if rule.get("type").and_then(Value::as_str) == Some("MANDATORY") {
            return Ok(true);
        }
    }
    Ok(false)
}

struct NormalizedOptions {
    options: Vec<FieldOption>,
    validation_options: Vec<Value>,
    total: usize,
    selected: Vec<Value>,
    selected_options: Vec<FieldOption>,
}

fn extract_revision(
    ad: &Map<String, Value>,
    values: &Map<String, Value>,
    etag: &str,
) -> Result<String, ApiError> {
    let mut revisions = Vec::new();
    for key in ["checkout-url", "product-context-url"] {
        let Some(url) = ad.get(key) else {
            continue;
        };
        let url = url.as_str().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("$.ad.{key}"),
                "revision URL must be a string",
            )
        })?;
        let revision = revision_from_url(url).ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("$.ad.{key}"),
                "revision URL did not contain a safe revision",
            )
        })?;
        revisions.push(revision);
    }
    if let Some(ad_etag) = ad.get("etag") {
        let ad_etag = ad_etag.as_str().ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.ad.etag",
                "draft ETag must be a string",
            )
        })?;
        let revision = revision_from_etag(ad_etag).ok_or_else(|| {
            model_error(
                "listing_composer",
                "$.ad.etag",
                "draft ETag did not contain a safe revision",
            )
        })?;
        revisions.push(revision);
    }
    if let Some(revision) = values.get("revision").and_then(revision_value) {
        revisions.push(revision);
    }
    let response_revision = revision_from_etag(etag).ok_or_else(|| {
        model_error(
            "listing_composer",
            "$.headers.etag",
            "response ETag did not contain a safe revision",
        )
    })?;
    revisions.push(response_revision);
    revisions.sort();
    revisions.dedup();
    match revisions.as_slice() {
        [revision] => Ok(revision.clone()),
        [] => Err(model_error(
            "listing_composer",
            "$.ad",
            "draft revision is unavailable",
        )),
        _ => Err(model_error(
            "listing_composer",
            "$.ad",
            "draft revision sources disagree",
        )),
    }
}

fn revision_from_url(value: &str) -> Option<String> {
    let query = value
        .split_once('?')?
        .1
        .split('#')
        .next()
        .unwrap_or_default();
    url::form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "adRevision")
        .map(|(_, value)| value.into_owned())
        .filter(|value| safe_revision(value))
}

fn revision_from_etag(etag: &str) -> Option<String> {
    let revision = etag
        .strip_prefix("W/\"")
        .and_then(|value| value.strip_suffix('"'))
        .or_else(|| {
            etag.strip_prefix('"')
                .and_then(|value| value.strip_suffix('"'))
        })
        .unwrap_or(etag);
    safe_revision(revision).then(|| revision.to_owned())
}

fn safe_revision(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
}

fn revision_value(value: &Value) -> Option<String> {
    match value {
        Value::String(value) if safe_revision(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    }
}

fn delivery_draft_model(composer: &DeliveryComposer) -> DraftModel {
    let label = composer
        .source
        .pointer("/sections/head/title")
        .and_then(Value::as_str)
        .unwrap_or("Delivery");
    let selected = Value::Array(
        composer
            .state
            .selected
            .iter()
            .cloned()
            .map(Value::String)
            .collect(),
    );
    let options = composer
        .state
        .options
        .iter()
        .map(|option| FieldOption {
            field: "delivery".to_owned(),
            value: Value::String(option.value.clone()),
            label: option.label.clone(),
        })
        .collect::<Vec<_>>();
    let mut field = Field::new(
        "delivery",
        label,
        FieldType::MultiSelect,
        Requirement::Required,
        Some(selected.clone()),
        "delivery",
    );
    field.option_count = composer.state.option_count;
    field.options_returned = composer.state.options_returned;
    field.options_truncated = composer.state.options_truncated;
    let mut values = Map::new();
    values.insert("delivery".to_owned(), selected);
    DraftModel {
        fields: vec![field],
        options,
        required_fields: vec!["delivery".to_owned()],
        values,
    }
}

pub(super) fn attach_delivery_model(
    state: &mut DraftState,
    composer: &DeliveryComposer,
) -> Result<(), ApiError> {
    state.merge_model(delivery_draft_model(composer))?;
    state.delivery = Some(composer.state.clone());
    Ok(())
}

pub(super) fn validate_resource_id(value: &str, resource: &str) -> Result<(), ApiError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(ApiError::new(
            format!("{resource}.invalid_id"),
            format!("The {resource} ID is invalid"),
        ));
    }
    Ok(())
}
