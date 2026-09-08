use std::{
    future::Future,
    pin::Pin,
    sync::{Mutex, OnceLock},
};

use serde_json::Value;

use crate::{
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            listing_edit::VintedListingEditApi,
            publication::decode_mutation_response,
            web::VintedWebSession,
            web_publication::{browser_gate_error, decode_browser_response, json_request_script},
        },
    },
};

pub struct VintedWebListingEditApi {
    session: OnceLock<VintedWebSession>,
    csrf_token: Mutex<Option<String>>,
}

impl VintedWebListingEditApi {
    pub const fn new() -> Self {
        Self {
            session: OnceLock::new(),
            csrf_token: Mutex::new(None),
        }
    }

    fn session(&self) -> Result<&VintedWebSession, AppError> {
        if let Some(session) = self.session.get() {
            return Ok(session);
        }
        let session = VintedWebSession::discover(PortalId::Fi)?;
        Ok(self.session.get_or_init(|| session))
    }

    fn csrf_token(&self, session: &VintedWebSession) -> Result<String, AppError> {
        let mut cached = self.csrf_token.lock().map_err(|_| {
            AppError::unexpected("failed to access the Vinted browser security token")
        })?;
        if let Some(token) = cached.as_ref() {
            return Ok(token.clone());
        }
        let token = session.csrf_token()?;
        *cached = Some(token.clone());
        Ok(token)
    }

    async fn update_request(&self, item_id: &str, body: Value) -> Result<Value, AppError> {
        let session = self.session().map_err(mutation_not_attempted)?;
        let csrf_token = self.csrf_token(session).map_err(mutation_not_attempted)?;
        let script = update_script(item_id, &body, &csrf_token).map_err(mutation_not_attempted)?;
        let response = decode_browser_response(session.evaluate(&script)?)?;
        if let Some(error) = browser_gate_error(response.status) {
            return Err(error);
        }
        decode_mutation_response(response)
    }
}

impl Default for VintedWebListingEditApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedListingEditApi for VintedWebListingEditApi {
    fn update<'a>(
        &'a self,
        item_id: &'a str,
        body: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.update_request(item_id, body))
    }
}

fn update_script(item_id: &str, body: &Value, csrf_token: &str) -> Result<String, AppError> {
    json_request_script(
        "PUT",
        &format!("/api/v2/item_upload/items/{item_id}"),
        Some(body),
        Some(csrf_token),
    )
}

fn mutation_not_attempted(mut error: AppError) -> AppError {
    let mut details = match error.details.take() {
        Some(details) => match *details {
            Value::Object(details) => details,
            details => serde_json::Map::from_iter([("error_details".to_owned(), details)]),
        },
        None => serde_json::Map::new(),
    };
    details.insert("mutation_attempted".to_owned(), Value::Bool(false));
    error.details = Some(Box::new(Value::Object(details)));
    error
}

#[cfg(test)]
mod tests {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde_json::json;

    use super::*;

    #[test]
    fn update_uses_exact_native_browser_method_path_and_headers() {
        let body = json!({
            "item": {
                "id": "42",
                "title": "Changed",
                "description": "Preserved",
                "price": "12.5",
                "assigned_photos": [{"id": "9"}],
                "update_photos": 0
            },
            "push_up": false,
            "parcel": null,
            "upload_session_id": "4f557be2-1900-4dd5-ae51-0a4da54bf462"
        });
        let token = "12345678-1234-1234-1234-123456789abc";
        let script = update_script("42", &body, token).unwrap();
        let encoded = STANDARD.encode(serde_json::to_vec(&body).unwrap());

        assert!(script.contains("const method = \"PUT\""));
        assert!(script.contains("/api/v2/item_upload/items/42"));
        assert!(script.contains(&format!("const encodedBody = \"{encoded}\"")));
        assert!(script.contains("credentials: 'include'"));
        assert!(script.contains("headers['x-csrf-token']"));
        assert!(script.contains("headers['x-upload-form'] = 'true'"));
        assert!(script.contains("headers['x-enable-dynamic-attribute-condition'] = 'true'"));
        assert!(script.contains("item.price = Number(item.price)"));
        assert!(!script.contains("Changed"));
    }

    #[test]
    fn preflight_errors_are_annotated_without_losing_details_or_guidance() {
        let mut error = AppError::authentication("vinted.web_csrf_unavailable", "sign in")
            .with_details(json!({"stage": "csrf"}));
        error.safe_to_retry = true;
        error
            .next_actions
            .push(crate::domain::envelope::NextAction {
                command: "flea vinted auth status".to_owned(),
            });

        let error = mutation_not_attempted(error);

        assert_eq!(error.details.as_ref().unwrap()["stage"], "csrf");
        assert_eq!(error.details.as_ref().unwrap()["mutation_attempted"], false);
        assert!(error.safe_to_retry);
        assert_eq!(error.next_actions[0].command, "flea vinted auth status");
    }

    #[test]
    fn evaluated_request_errors_are_not_annotated_as_unattempted() {
        let error = decode_browser_response(json!({"local_error": {
            "code": "vinted.web_browser_request_failed",
            "message": "fetch failed"
        }}))
        .unwrap_err();

        assert_ne!(
            error
                .details
                .as_ref()
                .and_then(|details| details.get("mutation_attempted")),
            Some(&Value::Bool(false))
        );
    }

    #[test]
    fn browser_response_decoder_retains_typed_http_status_for_update_errors() {
        let response = decode_browser_response(json!({
            "status": 422,
            "body": {
                "code": 99,
                "message_code": "validation_error",
                "errors": [{"field": "title", "message": "invalid"}]
            }
        }))
        .unwrap();
        let error = decode_mutation_response(response).unwrap_err();

        assert_eq!(error.code, "vinted.publication_validation_failed");
        assert_eq!(error.details.as_ref().unwrap()["http_status"], 422);
        assert_eq!(error.details.as_ref().unwrap()["response_code"], 99);
        assert!(!error.safe_to_retry);
    }
}
