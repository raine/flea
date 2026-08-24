use std::{
    future::Future,
    pin::Pin,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

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
        web::AgentBrowserSession,
    },
    transport::TransportResponse,
};

const MAX_BROWSER_BODY_BYTES: usize = 4 * 1024 * 1024;

pub struct AgentBrowserVintedPublicationApi {
    opened: AtomicBool,
    csrf_token: Mutex<Option<String>>,
}

impl AgentBrowserVintedPublicationApi {
    pub const fn new() -> Self {
        Self {
            opened: AtomicBool::new(false),
            csrf_token: Mutex::new(None),
        }
    }

    fn session(&self) -> Result<AgentBrowserSession, AppError> {
        AgentBrowserSession::discover(crate::marketplace::PortalId::Fi)
    }

    fn ensure_open(&self, session: &AgentBrowserSession) -> Result<(), AppError> {
        if self.opened.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        if let Err(error) = session.open() {
            self.opened.store(false, Ordering::Release);
            return Err(error);
        }
        Ok(())
    }

    fn csrf_token(&self, session: &AgentBrowserSession) -> Result<String, AppError> {
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

    async fn configuration_request(&self) -> Result<Value, AppError> {
        let session = self.session()?;
        self.ensure_open(&session)?;
        Ok(json!({ "upload_session_id": uuid::Uuid::new_v4().to_string() }))
    }

    async fn fetch_draft_request(&self, draft_id: &str) -> Result<Value, AppError> {
        let session = self.session()?;
        self.ensure_open(&session)?;
        let response = self.json_request(
            &session,
            "GET",
            &format!("/api/v2/item_upload/items/{draft_id}"),
            None,
            false,
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
        let session = self.session()?;
        self.ensure_open(&session)?;
        let csrf_token = self.csrf_token(&session)?;
        let script = photo_upload_script(upload_session_id, &image, &csrf_token);
        let response = decode_browser_response(session.evaluate(&script)?)?;
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
        let session = self.session()?;
        self.ensure_open(&session)?;
        let (method, path) = operation_endpoint(operation);
        let response = self.json_request(
            &session,
            method.as_str(),
            &format!("/api/v2/{path}"),
            body.as_ref(),
            true,
        )?;
        if let Some(error) = browser_gate_error(response.status) {
            return Err(error);
        }
        decode_mutation_response(response)
    }

    fn json_request(
        &self,
        session: &AgentBrowserSession,
        method: &str,
        path: &str,
        body: Option<&Value>,
        csrf_required: bool,
    ) -> Result<TransportResponse, AppError> {
        let csrf_token = csrf_required
            .then(|| self.csrf_token(session))
            .transpose()?;
        let script = json_request_script(method, path, body, csrf_token.as_deref())?;
        decode_browser_response(session.evaluate(&script)?)
    }
}

impl Default for AgentBrowserVintedPublicationApi {
    fn default() -> Self {
        Self::new()
    }
}

impl VintedPublicationApi for AgentBrowserVintedPublicationApi {
    fn configuration<'a>(
        &'a self,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.configuration_request())
    }

    fn fetch_draft<'a>(
        &'a self,
        draft_id: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, AppError>> + Send + 'a>> {
        Box::pin(self.fetch_draft_request(draft_id))
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
        Box::pin(async move { self.mutation_request(operation, body).await })
    }
}

fn json_request_script(
    method: &str,
    path: &str,
    body: Option<&Value>,
    csrf_token: Option<&str>,
) -> Result<String, AppError> {
    let method = serde_json::to_string(method).expect("HTTP method serializes");
    let path = serde_json::to_string(path).expect("API path serializes");
    let csrf_token = serde_json::to_string(&csrf_token).expect("CSRF token serializes");
    let body = body
        .map(serde_json::to_vec)
        .transpose()
        .map_err(|error| {
            AppError::unexpected("failed to serialize Vinted browser request").with_source(error)
        })?
        .map(|bytes| STANDARD.encode(bytes));
    let body = body
        .as_deref()
        .map(serde_json::to_string)
        .transpose()
        .expect("base64 body serializes")
        .unwrap_or_else(|| "null".to_owned());
    Ok(format!(
        r#"(async () => {{
            {response_helper}
            const method = {method};
            const encodedBody = {body};
            const csrfToken = {csrf_token};
            if (csrfToken !== null) {{
                window.__fleaCsrfToken = csrfToken;
                localStorage.setItem('__fleaCsrfToken', csrfToken);
            }}
            const headers = {{ accept: 'application/json,text/plain,*/*,image/webp' }};
            if (csrfToken !== null) headers['x-csrf-token'] = csrfToken;
            let requestBody;
            if (encodedBody !== null) {{
                headers['content-type'] = 'application/json';
                headers['x-upload-form'] = 'true';
                headers['x-enable-dynamic-attribute-condition'] = 'true';
                headers['x-enable-dynamic-attribute-size'] = 'true';
                headers['x-enable-dynamic-attribute-video-game-rating'] = 'true';
                const value = JSON.parse(atob(encodedBody));
                const item = value.item || value.draft;
                if (item && typeof item.price === 'string') item.price = Number(item.price);
                if (item && value.upload_session_id) item.temp_uuid = value.upload_session_id;
                if (value.push_up == null) value.push_up = false;
                requestBody = JSON.stringify(value);
            }}
            const response = await fetch({path}, {{
                method,
                credentials: 'include',
                headers,
                body: requestBody
            }});
            return await fleaResponse(response);
        }})()"#,
        response_helper = browser_response_helper(),
    ))
}

fn photo_upload_script(upload_session_id: &str, image: &PreparedImage, csrf_token: &str) -> String {
    let session = serde_json::to_string(upload_session_id).expect("upload session serializes");
    let csrf_token = serde_json::to_string(csrf_token).expect("CSRF token serializes");
    let file_name = serde_json::to_string(image.file_name).expect("file name serializes");
    let media_type = serde_json::to_string(image.media_type).expect("media type serializes");
    let bytes = serde_json::to_string(&STANDARD.encode(&image.bytes)).expect("image serializes");
    format!(
        r#"(async () => {{
            {response_helper}
            const csrfToken = {csrf_token};
            window.__fleaCsrfToken = csrfToken;
            localStorage.setItem('__fleaCsrfToken', csrfToken);
            const raw = atob({bytes});
            const content = new Uint8Array(raw.length);
            for (let index = 0; index < raw.length; index++) content[index] = raw.charCodeAt(index);
            const form = new FormData();
            form.append('photo[type]', 'item');
            form.append('upload_session_id', {session});
            form.append('photo[file]', new File([content], {file_name}, {{ type: {media_type} }}));
            const response = await fetch('/api/v2/photos', {{
                method: 'POST',
                credentials: 'include',
                headers: {{
                    accept: 'application/json,text/plain,*/*,image/webp',
                    'x-csrf-token': csrfToken
                }},
                body: form
            }});
            return await fleaResponse(response);
        }})()"#,
        response_helper = browser_response_helper(),
    )
}

fn browser_response_helper() -> &'static str {
    r#"
            async function fleaResponse(response) {
                const text = await response.text();
                let body = {};
                if (text) {
                    try { body = JSON.parse(text); }
                    catch (_) { body = { message: 'Vinted returned a non-JSON browser response' }; }
                }
                return { status: response.status, body };
            }
    "#
}

fn decode_browser_response(value: Value) -> Result<TransportResponse, AppError> {
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

fn browser_gate_error(status: StatusCode) -> Option<AppError> {
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
        command: "flea vinted --portal fi auth web status".to_owned(),
    });
    Some(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_script_uses_browser_credentials_and_normalizes_web_fields() {
        let script = json_request_script(
            "POST",
            "/api/v2/item_upload/items",
            Some(&json!({"item":{"price":"5.00"},"push_up":null})),
            Some("12345678-1234-1234-1234-123456789abc"),
        )
        .unwrap();
        assert!(script.contains("credentials: 'include'"));
        assert!(script.contains("headers['x-csrf-token']"));
        assert!(script.contains("item.price = Number(item.price)"));
        assert!(script.contains("item.temp_uuid = value.upload_session_id"));
        assert!(script.contains("value.push_up = false"));
        assert!(!script.contains("5.00"));
    }

    #[test]
    fn request_script_sends_csrf_token_without_a_body() {
        let script = json_request_script(
            "DELETE",
            "/api/v2/item_upload/drafts/42",
            None,
            Some("12345678-1234-1234-1234-123456789abc"),
        )
        .unwrap();
        assert!(script.contains("if (csrfToken !== null)"));
        assert!(script.contains("12345678-1234-1234-1234-123456789abc"));
        assert!(script.contains("const encodedBody = null"));
    }

    #[test]
    fn browser_gate_requires_interactive_verification_for_forbidden_responses() {
        let error = browser_gate_error(StatusCode::FORBIDDEN).unwrap();
        assert_eq!(error.code, "vinted.web_verification_required");
        assert!(error.safe_to_retry);
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi auth web status"
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
