use super::*;

pub(super) fn normalize_draft_images(
    values: &Map<String, Value>,
) -> Result<Vec<DraftImage>, ApiError> {
    let Some(images) = values.get("multi_image") else {
        return Ok(Vec::new());
    };
    let images = images.as_array().ok_or_else(|| {
        model_error(
            "listing_composer",
            "$.ad.values.multi_image",
            "draft images must be an array",
        )
    })?;
    images
        .iter()
        .enumerate()
        .map(|(position, value)| {
            let path = format!("$.ad.values.multi_image[{position}]");
            let object = value.as_object().ok_or_else(|| {
                model_error("listing_composer", &path, "draft image must be an object")
            })?;
            let url = object
                .get("url")
                .and_then(Value::as_str)
                .and_then(valid_image_location)
                .ok_or_else(|| {
                    model_error(
                        "listing_composer",
                        &format!("{path}.url"),
                        "draft image URL is unavailable or unsafe",
                    )
                })?;
            Ok(DraftImage {
                image_id: url.clone(),
                position,
                state: ImageState::Ready,
                url: Some(url),
                width: object
                    .get("width")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
                height: object
                    .get("height")
                    .and_then(Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok())
                    .unwrap_or_default(),
                mime_type: object
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
                failure: None,
            })
        })
        .collect()
}
