use super::{http::*, normalization::*, types::*, *};

pub(super) fn normalize_delivery_composer(
    source: Value,
    draft_id: &str,
) -> Result<DeliveryComposer, ApiError> {
    normalize_delivery_composer_with_limit(source, draft_id, MAX_OPTIONS_PER_FIELD)
}

pub(super) fn normalize_delivery_composer_with_limit(
    source: Value,
    draft_id: &str,
    option_limit: usize,
) -> Result<DeliveryComposer, ApiError> {
    let root = source.as_object().ok_or_else(|| {
        model_error(
            "delivery_composer",
            "$",
            "delivery composer must be an object",
        )
    })?;
    let context = root
        .get("context")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.context",
                "delivery context is unavailable or unrecognized",
            )
        })?;
    let observed_id = context.get("adId").and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    });
    if observed_id.as_deref() != Some(draft_id) {
        return Err(model_error(
            "delivery_composer",
            "$.context.adId",
            "delivery composer identifies a different draft",
        ));
    }
    let meetup_selected = context
        .get("meetup")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.context.meetup",
                "meetup selection state is unavailable or unrecognized",
            )
        })?;
    let shipping_selected = context
        .get("shipping")
        .and_then(Value::as_bool)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.context.shipping",
                "shipping selection state is unavailable or unrecognized",
            )
        })?;
    let sections = root
        .get("sections")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.sections",
                "delivery sections are unavailable or unrecognized",
            )
        })?;
    if let Some(title) = sections
        .get("head")
        .and_then(Value::as_object)
        .and_then(|head| head.get("title"))
        && title
            .as_str()
            .is_none_or(|title| !safe_display_string(title))
    {
        return Err(model_error(
            "delivery_composer",
            "$.sections.head.title",
            "delivery field label is unavailable or unsafe",
        ));
    }
    let delivery_options = sections
        .get("deliveryOptions")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            model_error(
                "delivery_composer",
                "$.sections.deliveryOptions",
                "delivery options are unavailable or unrecognized",
            )
        })?;

    let mut options = Vec::new();
    if let Some(meetup) = delivery_options.get("meetup") {
        let label = meetup
            .as_object()
            .and_then(|meetup| meetup.get("title"))
            .and_then(Value::as_str)
            .filter(|label| safe_display_string(label))
            .ok_or_else(|| {
                model_error(
                    "delivery_composer",
                    "$.sections.deliveryOptions.meetup.title",
                    "pickup option label is unavailable or unsafe",
                )
            })?;
        options.push(DeliveryOption {
            value: "pickup".to_owned(),
            label: label.to_owned(),
            mode: "pickup".to_owned(),
            package_size: None,
        });
    }
    let mut shipping_options_unavailable = false;
    if let Some(shipping) = delivery_options.get("shipping") {
        shipping
            .as_object()
            .and_then(|shipping| shipping.get("title"))
            .and_then(Value::as_str)
            .filter(|label| safe_display_string(label))
            .ok_or_else(|| {
                model_error(
                    "delivery_composer",
                    "$.sections.deliveryOptions.shipping.title",
                    "shipping option label is unavailable or unsafe",
                )
            })?;
        let mut shipping_options = Vec::new();
        if let Some(package_sizes) = sections
            .get("shipping")
            .and_then(Value::as_object)
            .and_then(|shipping| shipping.get("packageSizes"))
        {
            collect_delivery_package_options(
                package_sizes,
                "$.sections.shipping.packageSizes",
                0,
                &mut shipping_options,
            )?;
        }
        shipping_options.sort_by(|left, right| {
            let rank = |option: &DeliveryOption| match option.package_size.as_deref() {
                Some("SMALL") => 0,
                Some("MEDIUM") => 1,
                Some("LARGE") => 2,
                _ => 3,
            };
            rank(left)
                .cmp(&rank(right))
                .then_with(|| left.value.cmp(&right.value))
        });
        shipping_options_unavailable = shipping_options.is_empty();
        options.extend(shipping_options);
    }

    let mut machine_values = BTreeSet::new();
    for option in &options {
        if !machine_values.insert(option.value.clone()) {
            return Err(model_error(
                "delivery_composer",
                "$.sections",
                "delivery composer contains duplicate machine values",
            ));
        }
    }
    let mut selected = Vec::new();
    if meetup_selected {
        if !machine_values.contains("pickup") {
            return Err(model_error(
                "delivery_composer",
                "$.context.meetup",
                "selected pickup is absent from delivery options",
            ));
        }
        selected.push("pickup".to_owned());
    }
    if shipping_selected {
        let package_size = context
            .get("packageSize")
            .and_then(Value::as_str)
            .filter(|size| safe_machine_identifier(size))
            .ok_or_else(|| {
                model_error(
                    "delivery_composer",
                    "$.context.packageSize",
                    "selected shipping package is unavailable or unsafe",
                )
            })?;
        let value = format!("shipping:{}", package_size.to_ascii_lowercase());
        if !machine_values.contains(&value) {
            return Err(model_error(
                "delivery_composer",
                "$.context.packageSize",
                "selected shipping package is absent from delivery options",
            ));
        }
        selected.push(value);
    }

    let option_count = options.len();
    if option_count > option_limit {
        let selected_options = options
            .iter()
            .filter(|option| selected.contains(&option.value))
            .cloned()
            .collect::<Vec<_>>();
        let unselected_limit = option_limit.saturating_sub(selected_options.len());
        options = options
            .into_iter()
            .filter(|option| !selected.contains(&option.value))
            .take(unselected_limit)
            .chain(selected_options)
            .collect();
    }
    let options_returned = options.len();
    let available = option_count > 0;
    Ok(DeliveryComposer {
        state: DraftDelivery {
            source: "remote_delivery_composer".to_owned(),
            available,
            options,
            option_count,
            options_returned,
            options_truncated: option_count > options_returned,
            selected,
            unavailable_reason: if shipping_options_unavailable {
                Some("Shipping is offered, but package machine values are unavailable".to_owned())
            } else if !available {
                Some("Tori returned no delivery options for this draft".to_owned())
            } else {
                None
            },
        },
        source,
    })
}

fn collect_delivery_package_options(
    value: &Value,
    path: &str,
    depth: usize,
    options: &mut Vec<DeliveryOption>,
) -> Result<(), ApiError> {
    if depth > 8 {
        return Err(model_error(
            "delivery_composer",
            path,
            "shipping package option nesting exceeds the supported limit",
        ));
    }
    match value {
        Value::Object(package) if package.contains_key("size") => {
            let package_size = package
                .get("size")
                .and_then(Value::as_str)
                .filter(|size| safe_machine_identifier(size))
                .ok_or_else(|| {
                    model_error(
                        "delivery_composer",
                        path,
                        "package machine value is unavailable or unsafe",
                    )
                })?;
            let label = package
                .get("title")
                .and_then(Value::as_str)
                .filter(|label| safe_display_string(label))
                .ok_or_else(|| {
                    model_error(
                        "delivery_composer",
                        path,
                        "package option label is unavailable or unsafe",
                    )
                })?;
            options.push(DeliveryOption {
                value: format!("shipping:{}", package_size.to_ascii_lowercase()),
                label: label.to_owned(),
                mode: "shipping".to_owned(),
                package_size: Some(package_size.to_owned()),
            });
        }
        Value::Object(object) => {
            for (index, child) in object.values().enumerate() {
                collect_delivery_package_options(
                    child,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    options,
                )?;
            }
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_delivery_package_options(
                    child,
                    &format!("{path}[{index}]"),
                    depth + 1,
                    options,
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn shipping_products(body: &Value) -> Vec<String> {
    let mut products = body
        .pointer("/sections/shipping/providers/options")
        .or_else(|| body.pointer("/sections/providers/options"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|option| option.get("product").and_then(Value::as_str))
        .filter(|product| !product.trim().is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    products.sort();
    products.dedup();
    products
}

pub(super) fn allowed_delivery_values(state: &DraftDelivery) -> Vec<String> {
    state
        .options
        .iter()
        .map(|option| option.value.clone())
        .collect()
}

pub(super) fn invalid_delivery_api(state: &DraftDelivery, requested: &str) -> ApiError {
    let mut error = ApiError::new(
        "draft.invalid_delivery",
        "The requested delivery value is unavailable for this draft",
    );
    error.details = Some(Box::new(json!({
        "requested_values": [requested],
        "allowed_values": allowed_delivery_values(state),
    })));
    error
}

pub(super) fn shipping_unavailable(state: &DraftDelivery, reason: &str) -> ApiError {
    let mut error = ApiError::new(
        "draft.delivery_options_unavailable",
        "Shipping cannot be configured from the current delivery composer",
    );
    error.details = Some(Box::new(json!({
        "reason": reason,
        "allowed_values": allowed_delivery_values(state),
        "recovery_guidance": "Open the draft delivery composer in Tori and complete the seller address"
    })));
    error
}
