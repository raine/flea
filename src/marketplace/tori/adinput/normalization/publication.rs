use super::*;

pub(in crate::marketplace::tori::adinput) fn normalize_publication_draft(
    body: Value,
    response_etag: Option<&str>,
) -> Result<PublicationDraftState, ApiError> {
    normalize_publication_draft_with_limit(body, response_etag, MAX_OPTIONS_PER_FIELD)
}

pub(in crate::marketplace::tori::adinput) fn normalize_publication_draft_with_limit(
    body: Value,
    response_etag: Option<&str>,
    option_limit: usize,
) -> Result<PublicationDraftState, ApiError> {
    let composer_model = publication_composer_status(&body);
    match normalize_authoritative_draft_state_with_limit(body.clone(), response_etag, option_limit)
    {
        Ok(draft) => Ok(PublicationDraftState {
            draft,
            composer_model,
        }),
        Err(error)
            if composer_model != ComposerModelStatus::Available
                || error
                    .details
                    .as_deref()
                    .and_then(|details| details.get("path"))
                    .and_then(Value::as_str)
                    .is_some_and(|path| path.starts_with("$.model")) =>
        {
            publication_draft_without_model(body, response_etag).map(|draft| {
                PublicationDraftState {
                    draft,
                    composer_model: if composer_model == ComposerModelStatus::Available {
                        ComposerModelStatus::Malformed
                    } else {
                        composer_model
                    },
                }
            })
        }
        Err(error) => Err(error),
    }
}

fn publication_composer_status(body: &Value) -> ComposerModelStatus {
    if body.get("draft_id").is_some()
        && ["fields", "options", "required_fields"]
            .into_iter()
            .all(|key| body.get(key).and_then(Value::as_array).is_some())
    {
        return ComposerModelStatus::Available;
    }
    let Some(model) = body.get("model").filter(|model| !model.is_null()) else {
        return ComposerModelStatus::Unavailable;
    };
    let Some(model) = model.as_object() else {
        return ComposerModelStatus::Malformed;
    };
    match model.get("sections") {
        Some(Value::Array(sections)) if !sections.is_empty() => ComposerModelStatus::Available,
        Some(Value::Array(_)) | None | Some(Value::Null) => ComposerModelStatus::Unavailable,
        Some(_) => ComposerModelStatus::Malformed,
    }
}

fn publication_draft_without_model(
    body: Value,
    response_etag: Option<&str>,
) -> Result<DraftState, ApiError> {
    if body.get("draft_id").is_some() {
        return normalize_draft_state(body, response_etag);
    }
    let draft_id = draft_id_from_body(&body).ok_or_else(|| {
        model_error(
            "publication_validation",
            "$",
            "draft response did not contain an authoritative identity",
        )
    })?;
    let ad = body
        .get("ad")
        .and_then(Value::as_object)
        .ok_or_else(|| model_error("publication_validation", "$.ad", "ad data is unavailable"))?;
    let values = normalize_draft_values(
        ad.get("values")
            .and_then(Value::as_object)
            .cloned()
            .ok_or_else(|| {
                model_error(
                    "publication_validation",
                    "$.ad.values",
                    "draft values are unavailable or unrecognized",
                )
            })?,
    )?;
    let etag = response_etag
        .or_else(|| ad.get("etag").and_then(Value::as_str))
        .unwrap_or_default()
        .to_owned();
    let revision = extract_revision(ad, &values, &etag).ok();
    let images = normalize_draft_images(&values)?;
    Ok(DraftState {
        draft_id,
        etag,
        revision,
        values,
        fields: Vec::new(),
        options: Vec::new(),
        required_fields: Vec::new(),
        images,
        cleared_fields: Vec::new(),
        predictions: Vec::new(),
        delivery: None,
    })
}

pub(in crate::marketplace::tori::adinput) fn normalize_publication_categories(
    body: &Value,
) -> Result<Vec<PublicationCategory>, ApiError> {
    let roots = body
        .get("categories")
        .and_then(Value::as_array)
        .ok_or_else(category_model_error)?;
    let mut categories = Vec::new();
    let mut seen = BTreeSet::new();
    for root in roots {
        normalize_publication_category(root, &mut seen, &mut categories)?;
    }
    if categories.is_empty() {
        return Err(category_model_error());
    }
    categories.sort_by(|left, right| left.category_id.cmp(&right.category_id));
    Ok(categories)
}

fn normalize_publication_category(
    category: &Value,
    seen: &mut BTreeSet<String>,
    output: &mut Vec<PublicationCategory>,
) -> Result<(), ApiError> {
    let category = category.as_object().ok_or_else(category_model_error)?;
    let category_id = category
        .get("id")
        .or_else(|| category.get("category_id"))
        .and_then(publication_scalar_string)
        .ok_or_else(category_model_error)?;
    if !seen.insert(category_id.clone()) {
        return Err(category_model_error());
    }
    let label = category
        .get("label")
        .and_then(Value::as_str)
        .filter(|label| safe_display_string(label))
        .ok_or_else(category_model_error)?
        .to_owned();
    let children: &[Value] = match category.get("children") {
        Some(children) => children.as_array().ok_or_else(category_model_error)?,
        None => &[],
    };
    let selectable = match category
        .get("selectable")
        .or_else(|| category.get("isSelectable"))
    {
        Some(Value::Bool(selectable)) => *selectable,
        Some(_) => return Err(category_model_error()),
        None => children.is_empty(),
    };
    output.push(PublicationCategory {
        category_id,
        label,
        selectable,
    });
    for child in children {
        normalize_publication_category(child, seen, output)?;
    }
    Ok(())
}

fn category_model_error() -> ApiError {
    malformed_read_response(
        "publication_category_taxonomy",
        ObservationSource::DraftService,
    )
}
