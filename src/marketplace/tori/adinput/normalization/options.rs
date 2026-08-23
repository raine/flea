use super::*;

pub(super) fn normalize_field_type(
    widget: &Map<String, Value>,
    upstream_type: &str,
    path: &str,
) -> Result<FieldType, ApiError> {
    let subtype = match widget.get("sub-type") {
        Some(Value::String(subtype)) if safe_machine_identifier(subtype) => Some(subtype.as_str()),
        Some(_) => {
            return Err(model_error(
                "listing_composer",
                &format!("{path}.sub-type"),
                "field subtype is unavailable or unsafe",
            ));
        }
        None => None,
    };
    Ok(match upstream_type {
        "simple" => match subtype {
            None | Some("string") => FieldType::String,
            Some("multiline") => FieldType::Text,
            Some("number" | "decimal") => FieldType::Decimal,
            Some("integer") => FieldType::Integer,
            Some("boolean") => FieldType::Boolean,
            Some(subtype) => FieldType::Unknown(format!("simple:{subtype}")),
        },
        "html" => FieldType::Text,
        "post-code" => FieldType::String,
        "checkbox" => FieldType::Boolean,
        "select" if widget.get("multiple").and_then(Value::as_bool) == Some(true) => {
            FieldType::MultiSelect
        }
        "select" | "tree-select" | "managed" => FieldType::Select,
        "multi-select" => FieldType::MultiSelect,
        "date" => FieldType::Date,
        value => FieldType::Unknown(value.to_owned()),
    })
}

pub(super) fn normalize_widget_options(
    widget: &Map<String, Value>,
    upstream_type: &str,
    field: &str,
    selected: Option<&Value>,
    path: &str,
    option_limit: usize,
) -> Result<NormalizedOptions, ApiError> {
    let selected = match selected {
        Some(Value::Array(values)) => values.clone(),
        Some(value) => vec![value.clone()],
        None => Vec::new(),
    };
    let mut result = NormalizedOptions {
        options: Vec::new(),
        validation_options: Vec::new(),
        total: 0,
        selected,
        selected_options: Vec::new(),
    };
    match upstream_type {
        "select" | "multi-select" => {
            let items = widget
                .get("items")
                .or_else(|| widget.get("options"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    model_error(
                        "listing_composer",
                        &format!("{path}.items"),
                        "select options are unavailable or unrecognized",
                    )
                })?;
            for (index, item) in items.iter().enumerate() {
                normalize_flat_option(
                    item,
                    field,
                    &format!("{path}.items[{index}]"),
                    &mut result,
                    option_limit,
                )?;
            }
        }
        "managed" => {
            let nodes = widget
                .get("value-nodes")
                .or_else(|| widget.get("valueNodes"))
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    model_error(
                        "listing_composer",
                        &format!("{path}.value-nodes"),
                        "managed options are unavailable or unrecognized",
                    )
                })?;
            for (index, node) in nodes.iter().enumerate() {
                normalize_option_node(
                    node,
                    field,
                    &format!("{path}.value-nodes[{index}]"),
                    &mut result,
                    option_limit,
                )?;
            }
        }
        "tree-select" => {
            let root = widget.get("value").ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.value"),
                    "tree options are unavailable",
                )
            })?;
            normalize_option_node(
                root,
                field,
                &format!("{path}.value"),
                &mut result,
                option_limit,
            )?;
        }
        _ => {}
    }
    for selected in std::mem::take(&mut result.selected_options) {
        if result
            .options
            .iter()
            .any(|option| values_semantically_equal(&option.value, &selected.value))
        {
            continue;
        }
        if result.options.len() == option_limit {
            result.options.pop();
        }
        result.options.push(selected);
    }
    Ok(result)
}

fn normalize_flat_option(
    option: &Value,
    field: &str,
    path: &str,
    result: &mut NormalizedOptions,
    option_limit: usize,
) -> Result<(), ApiError> {
    let option = option
        .as_object()
        .ok_or_else(|| model_error("listing_composer", path, "option must be an object"))?;
    let value = option
        .get("value")
        .filter(|value| is_machine_value(value))
        .cloned()
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.value"),
                "option machine value is unavailable or unsafe",
            )
        })?;
    let label = option
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| safe_display_string(label))
        .ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.label"),
                "option label is unavailable",
            )
        })?;
    push_option(result, field, value, label, option_limit);
    Ok(())
}

fn normalize_option_node(
    node: &Value,
    field: &str,
    path: &str,
    result: &mut NormalizedOptions,
    option_limit: usize,
) -> Result<(), ApiError> {
    let node = node
        .as_object()
        .ok_or_else(|| model_error("listing_composer", path, "option node must be an object"))?;
    let children = match node.get("children") {
        Some(children) => Some(children.as_array().ok_or_else(|| {
            model_error(
                "listing_composer",
                &format!("{path}.children"),
                "option node children must be an array",
            )
        })?),
        None => None,
    };
    let persistable = node
        .get("persistable")
        .and_then(Value::as_bool)
        .unwrap_or(children.is_none_or(|children| children.is_empty()));
    if persistable {
        let value = node
            .get("id")
            .or_else(|| node.get("value"))
            .filter(|value| is_machine_value(value))
            .cloned()
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.id"),
                    "option node machine value is unavailable or unsafe",
                )
            })?;
        let label = node
            .get("label")
            .and_then(Value::as_str)
            .filter(|label| safe_display_string(label))
            .ok_or_else(|| {
                model_error(
                    "listing_composer",
                    &format!("{path}.label"),
                    "option node label is unavailable",
                )
            })?;
        push_option(result, field, value, label, option_limit);
    }
    if let Some(children) = children {
        for (index, child) in children.iter().enumerate() {
            normalize_option_node(
                child,
                field,
                &format!("{path}.children[{index}]"),
                result,
                option_limit,
            )?;
        }
    }
    Ok(())
}

fn push_option(
    result: &mut NormalizedOptions,
    field: &str,
    value: Value,
    label: &str,
    option_limit: usize,
) {
    result.total += 1;
    result.validation_options.push(value.clone());
    let option = FieldOption {
        field: field.to_owned(),
        value,
        label: label.to_owned(),
    };
    if result
        .selected
        .iter()
        .any(|selected| select_values_equal(field, selected, &option.value))
    {
        result.selected_options.push(option.clone());
    }
    if result.options.len() < option_limit {
        result.options.push(option);
    }
}

fn is_machine_value(value: &Value) -> bool {
    match value {
        Value::String(value) => {
            !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
        }
        Value::Number(_) | Value::Bool(_) => true,
        _ => false,
    }
}

pub(super) fn model_field_value(values: &Map<String, Value>, field: &str) -> Option<Value> {
    if let Some(value) = values.get(field) {
        return Some(value.clone());
    }
    match field {
        "price_amount" => values.get("price").cloned(),
        "price_max" => values.get("max_price").cloned(),
        "postal-code" => values
            .get("location")
            .and_then(Value::as_array)
            .and_then(|locations| locations.first())
            .and_then(Value::as_object)
            .and_then(|location| {
                location
                    .get("postal-code")
                    .or_else(|| location.get("postal_code"))
            })
            .cloned(),
        _ => None,
    }
}

pub(super) fn value_is_present(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::String(value) => !value.trim().is_empty(),
        Value::Array(values) => !values.is_empty() && values.iter().all(value_is_present),
        Value::Object(values) => !values.is_empty() && values.values().all(value_is_present),
        _ => true,
    }
}
