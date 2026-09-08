use std::{future::Future, pin::Pin};

use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::{StatusCode, header::HeaderMap};
use serde_json::{Value, json};

use crate::{
    domain::envelope::NextAction,
    error::AppError,
    marketplace::vinted::{
        publication::{
            PreparedImage, PublicationOperation, UploadedPhoto, VintedPublicationApi,
            decode_draft_response, decode_mutation_response, decode_photo_response,
            operation_endpoint,
        },
        web::{VintedWebSession, request_command},
    },
    transport::TransportResponse,
};

const MAX_BROWSER_BODY_BYTES: usize = 4 * 1024 * 1024;

pub struct VintedWebPublicationApi;

impl VintedWebPublicationApi {
    pub const fn new() -> Self {
        Self
    }

    async fn configuration_request(&self) -> Result<Value, AppError> {
        let session = VintedWebSession::discover(crate::marketplace::PortalId::Fi)?;
        session.ready()?;
        Ok(json!({ "upload_session_id": uuid::Uuid::new_v4().to_string() }))
    }

    async fn fetch_item_request(&self, item_id: &str) -> Result<Value, AppError> {
        let session = VintedWebSession::discover(crate::marketplace::PortalId::Fi)?;
        let response = json_request(
            &session,
            "GET",
            &format!("/api/v2/item_upload/items/{item_id}"),
            None,
        )?;
        if let Some(error) = browser_gate_error(response.status) {
            return Err(error);
        }
        decode_draft_response(response)
    }

    async fn upload_photo_request(
        &self,
        upload_session_id: &str,
        image: PreparedImage,
    ) -> Result<UploadedPhoto, AppError> {
        let session = VintedWebSession::discover(crate::marketplace::PortalId::Fi)?;
        let result = session.extension_request(photo_command(upload_session_id, &image))?;
        let response = decode_browser_response(result)?;
        if let Some(error) = browser_gate_error(response.status) {
            return Err(error);
        }
        decode_photo_response(response)
    }

    async fn mutation_request(
        &self,
        operation: &PublicationOperation,
        body: Option<Value>,
    ) -> Result<Value, AppError> {
        let session = VintedWebSession::discover(crate::marketplace::PortalId::Fi)?;
        let (method, path) = operation_endpoint(operation);
        let response = json_request(
            &session,
            method.as_str(),
            &format!("/api/v2/{path}"),
            body.as_ref(),
        )?;
        if let Some(error) = browser_gate_error(response.status) {
            return Err(error);
        }
        decode_mutation_response(response)
    }
}

impl Default for VintedWebPublicationApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedPublicationApi for VintedWebPublicationApi {
    fn configuration<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.configuration_request())
    }

    fn fetch_item<'a>(
        &'a self,
        item_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.fetch_item_request(item_id))
    }

    fn upload_photo<'a>(
        &'a self,
        upload_session_id: &'a str,
        image: PreparedImage,
    ) -> Pin<Box<dyn Future<Output = Result<UploadedPhoto, AppError>> + Send + 'a>> {
        Box::pin(self.upload_photo_request(upload_session_id, image))
    }

    fn mutate<'a>(
        &'a self,
        operation: &'a PublicationOperation,
        body: Option<Value>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.mutation_request(operation, body))
    }
}

fn json_request(
    session: &VintedWebSession,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<TransportResponse, AppError> {
    decode_browser_response(session.extension_request(request_command(method, path, body))?)
}

fn photo_command(upload_session_id: &str, image: &PreparedImage) -> Value {
    json!({
        "action": "photo", "upload_session_id": upload_session_id,
        "file_name": image.file_name, "media_type": image.media_type,
        "bytes": STANDARD.encode(&image.bytes),
    })
}

pub(super) fn decode_browser_response(value: Value) -> Result<TransportResponse, AppError> {
    let status = value.get("status").and_then(Value::as_u64).ok_or_else(|| {
        AppError::upstream(
            "vinted.web_browser_invalid_response",
            "the browser API request returned no HTTP status",
        )
    })?;
    let status = u16::try_from(status)
        .ok()
        .and_then(|status| StatusCode::from_u16(status).ok())
        .ok_or_else(|| {
            AppError::upstream(
                "vinted.web_browser_invalid_response",
                "the browser API request returned an invalid HTTP status",
            )
        })?;
    let body = value.get("body").cloned().unwrap_or_else(|| json!({}));
    let body = serde_json::to_vec(&body).map_err(|error| {
        AppError::unexpected("failed to decode Vinted browser response").with_source(error)
    })?;
    if body.len() > MAX_BROWSER_BODY_BYTES {
        return Err(AppError::upstream(
            "vinted.web_browser_response_too_large",
            "the Vinted browser API response exceeded its safety limit",
        ));
    }
    Ok(TransportResponse {
        status,
        headers: HeaderMap::new(),
        body,
    })
}

pub(super) fn browser_gate_error(status: StatusCode) -> Option<AppError> {
    let (code, message, user_action) = match status {
        StatusCode::UNAUTHORIZED => (
            "vinted.web_authentication_required",
            "the Vinted web session is not signed in",
            "Sign in to Vinted in the browser window, then retry the unchanged command.",
        ),
        StatusCode::FORBIDDEN => (
            "vinted.web_verification_required",
            "Vinted requires browser verification before publication can continue",
            "Complete the human verification shown in the Vinted browser window, then retry the unchanged command.",
        ),
        _ => return None,
    };
    let mut error = AppError::authentication(code, message).with_details(json!({
        "http_status": status.as_u16(),
        "browser_open": true,
        "user_action": user_action
    }));
    error.safe_to_retry = true;
    error.next_actions.push(NextAction {
        command: "flea vinted --portal fi auth status --browser".to_owned(),
    });
    Some(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutation_command_preserves_unicode_and_leaves_web_fields_to_the_extension() {
        let body = json!({
            "item": {
                "title": "Käytetty tuoli 🪑",
                "description": "säädettävät, Ελληνικά, 日本語, e\u{301}",
                "manufacturer": "Møbelfabrik",
                "price": "5.00"
            },
            "push_up": null,
            "upload_session_id": "4f557be2-1900-4dd5-ae51-0a4da54bf462"
        });
        assert_eq!(
            request_command("POST", "/api/v2/item_upload/items", Some(&body)),
            json!({
                "action": "request", "method": "POST",
                "path": "/api/v2/item_upload/items", "body": body
            })
        );
    }

    #[test]
    fn delete_command_has_no_body() {
        assert_eq!(
            request_command("DELETE", "/api/v2/item_upload/drafts/42", None),
            json!({
                "action": "request", "method": "DELETE",
                "path": "/api/v2/item_upload/drafts/42", "body": null
            })
        );
    }

    #[test]
    fn photo_command_transfers_image_bytes_and_metadata() {
        let image = PreparedImage {
            file_name: "image.jpg",
            media_type: "image/jpeg",
            bytes: vec![0, 128, 255],
        };
        assert_eq!(
            photo_command("4f557be2-1900-4dd5-ae51-0a4da54bf462", &image),
            json!({
                "action": "photo", "upload_session_id": "4f557be2-1900-4dd5-ae51-0a4da54bf462",
                "file_name": "image.jpg", "media_type": "image/jpeg",
                "bytes": "AID/"
            })
        );
    }

    #[test]
    fn rejects_missing_and_invalid_http_status_without_exposing_payloads() {
        for status in [
            Value::Null,
            json!("secret"),
            json!(-1),
            json!(99),
            json!(65536),
        ] {
            let error = decode_browser_response(json!({
                "status": status, "body": {"secret": "private-cookie-value"}
            }))
            .unwrap_err();
            assert_eq!(error.code, "vinted.web_browser_invalid_response");
            assert!(!error.safe_to_retry);
            assert!(!format!("{error:?}").contains("private-cookie-value"));
        }
    }

    #[test]
    fn rejects_oversized_response_bodies() {
        let error = decode_browser_response(json!({
            "status": 200, "body": "x".repeat(MAX_BROWSER_BODY_BYTES)
        }))
        .unwrap_err();
        assert_eq!(error.code, "vinted.web_browser_response_too_large");
        assert!(!error.safe_to_retry);
    }

    #[test]
    fn unauthorized_responses_keep_sign_in_guidance_and_http_status() {
        let error = browser_gate_error(StatusCode::UNAUTHORIZED).unwrap();
        assert_eq!(error.code, "vinted.web_authentication_required");
        assert_eq!(error.details.as_ref().unwrap()["http_status"], 401);
        assert!(error.safe_to_retry);
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi auth status --browser"
        );
        assert!(browser_gate_error(StatusCode::INTERNAL_SERVER_ERROR).is_none());
    }

    #[test]
    fn browser_gate_requires_interactive_verification_for_forbidden_responses() {
        let error = browser_gate_error(StatusCode::FORBIDDEN).unwrap();
        assert_eq!(error.code, "vinted.web_verification_required");
        assert!(error.safe_to_retry);
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi auth status --browser"
        );
    }

    #[test]
    fn decodes_bounded_browser_http_response() {
        let response = decode_browser_response(json!({
            "status": 200,
            "body": {"item": {"id": 42}}
        }))
        .unwrap();
        assert_eq!(response.status, StatusCode::OK);
        assert_eq!(
            serde_json::from_slice::<Value>(&response.body).unwrap()["item"]["id"],
            42
        );
    }
}
