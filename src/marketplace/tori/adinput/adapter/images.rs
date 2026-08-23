use super::*;

impl<T: AdInputProtocol> DraftImages for HttpAdInputApi<T> {
    async fn upload_image(
        &self,
        draft_id: &str,
        file_name: &str,
        bytes: Vec<u8>,
        width: u32,
        height: u32,
    ) -> Result<UploadedImage, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mime_type = if file_name.ends_with(".png") {
            "image/png"
        } else {
            "image/jpeg"
        };
        let response = self
            .json(HttpRequest::mutation(
                ObservationSource::DraftService,
                Method::Post,
                format!("/adinput/ad/recommerce/{draft_id}/upload"),
                RequestBody::Image {
                    bytes,
                    file_name: file_name.to_owned(),
                    mime_type: mime_type.to_owned(),
                    width,
                    height,
                },
            ))
            .await?;
        let location = response
            .location
            .as_deref()
            .or_else(|| response.body.get("location").and_then(Value::as_str))
            .and_then(valid_image_location)
            .ok_or_else(|| {
                let mut error = unexpected_representation("upload_image", &response);
                error.code = "mutation.uncertain".to_owned();
                error.message =
                    "Image upload succeeded without an authoritative image location".to_owned();
                error
            })?;
        Ok(UploadedImage {
            image_id: location.clone(),
            state: ImageState::Processing,
            url: Some(location),
            width,
            height,
            mime_type: Some(mime_type.to_owned()),
        })
    }
    async fn set_images(
        &self,
        draft_id: &str,
        etag: &str,
        values: &Map<String, Value>,
        images: &[UploadedImage],
    ) -> Result<DraftState, ApiError> {
        validate_resource_id(draft_id, "draft")?;
        let mut values = composer_values(values)?;
        let mut image = Vec::with_capacity(images.len());
        let mut multi_image = Vec::with_capacity(images.len());
        for uploaded in images {
            let url = uploaded
                .url
                .as_deref()
                .and_then(valid_image_location)
                .ok_or_else(|| {
                    ApiError::new("draft.invalid_image", "Tori image location is invalid")
                })?;
            let path = url
                .strip_prefix("https://img.tori.net/dynamic/default/")
                .expect("validated image location has the canonical prefix");
            let mime_type = uploaded.mime_type.as_deref().unwrap_or("image/jpeg");
            image.push(json!({
                "height": uploaded.height.to_string(),
                "type": mime_type,
                "uri": path,
                "width": uploaded.width.to_string()
            }));
            multi_image.push(json!({
                "description": "",
                "height": uploaded.height,
                "path": path,
                "type": mime_type,
                "url": url,
                "width": uploaded.width
            }));
        }
        values.insert("image".to_owned(), Value::Array(image));
        values.insert("multi_image".to_owned(), Value::Array(multi_image));
        let mut request = HttpRequest::mutation(
            ObservationSource::DraftService,
            Method::Put,
            format!("/adinput/ad/recommerce/{draft_id}/update"),
            RequestBody::Json(Value::Object(values)),
        );
        request.if_match = Some(etag.to_owned());
        self.draft_request(request, false).await
    }
}
