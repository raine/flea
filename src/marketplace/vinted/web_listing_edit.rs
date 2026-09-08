use std::{future::Future, pin::Pin};

use serde_json::Value;

use crate::{
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            listing_edit::VintedListingEditApi,
            publication::decode_mutation_response,
            web::{VintedWebSession, request_command},
            web_publication::{browser_gate_error, decode_browser_response},
        },
    },
};

pub struct VintedWebListingEditApi;

impl VintedWebListingEditApi {
    pub const fn new() -> Self {
        Self
    }

    async fn update_request(&self, item_id: &str, body: Value) -> Result<Value, AppError> {
        let session = VintedWebSession::discover(PortalId::Fi).map_err(mutation_not_attempted)?;
        let result = session.extension_request(update_command(item_id, &body))?;
        let response = decode_browser_response(result)?;
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

fn update_command(item_id: &str, body: &Value) -> Value {
    request_command(
        "PUT",
        &format!("/api/v2/item_upload/items/{item_id}"),
        Some(body),
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
    use serde_json::json;

    use super::*;

    #[test]
    fn update_uses_defined_extension_command_and_preserves_the_body() {
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
        assert_eq!(
            update_command("42", &body),
            json!({
                "action": "request", "method": "PUT",
                "path": "/api/v2/item_upload/items/42", "body": body
            })
        );
    }

    #[test]
    fn preflight_errors_are_annotated_without_losing_details_or_guidance() {
        let mut error = AppError::usage("run flea extension setup")
            .with_details(json!({"stage": "extension_setup"}));
        error.safe_to_retry = true;
        error
            .next_actions
            .push(crate::domain::envelope::NextAction {
                command: "flea extension setup".to_owned(),
            });

        let error = mutation_not_attempted(error);

        assert_eq!(error.details.as_ref().unwrap()["stage"], "extension_setup");
        assert_eq!(error.details.as_ref().unwrap()["mutation_attempted"], false);
        assert!(error.safe_to_retry);
        assert_eq!(error.next_actions[0].command, "flea extension setup");
    }

    #[test]
    fn invalid_extension_responses_are_not_annotated_as_unattempted() {
        let error =
            decode_browser_response(json!({"body": {"message": "request failed"}})).unwrap_err();

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
