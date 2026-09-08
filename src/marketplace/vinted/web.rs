use serde::Serialize;
use serde_json::{Value, json};

use crate::{domain::envelope::NextAction, error::AppError, marketplace::PortalId};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VintedWebAuthStatus {
    pub authenticated: bool,
    pub browser_open: bool,
    pub validation: &'static str,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VintedWebLogoutOutput {
    pub authenticated: bool,
    pub cleared: bool,
}

pub(super) struct VintedWebSession;

impl VintedWebSession {
    pub fn discover(_portal: PortalId) -> Result<Self, AppError> {
        Self::from_configuration(crate::extension::configured())
    }

    fn from_configuration(configured: bool) -> Result<Self, AppError> {
        if !configured {
            let mut error = AppError::usage(
                "Vinted browser access requires the Flea Chrome extension. Run flea extension setup, follow its installation instructions, then open or reload a Vinted tab in Chrome and sign in.",
            );
            error.next_actions.push(NextAction {
                command: "flea extension setup".to_owned(),
            });
            return Err(error);
        }
        Ok(Self)
    }

    pub fn ready(&self) -> Result<(), AppError> {
        self.extension_request(json!({ "action": "ready" }))
            .map(|_| ())
    }

    pub fn extension_request(&self, command: Value) -> Result<Value, AppError> {
        crate::extension::request(command)
    }
}

pub(super) fn request_command(method: &str, path: &str, body: Option<&Value>) -> Value {
    json!({ "action": "request", "method": method, "path": path, "body": body })
}

pub fn begin_login(portal: PortalId) -> Result<VintedWebAuthStatus, AppError> {
    let session = VintedWebSession::discover(portal)?;
    status_with_session(&session)
}

pub fn login(portal: PortalId) -> Result<VintedWebAuthStatus, AppError> {
    let status = begin_login(portal)?;
    if status.authenticated {
        return Ok(status);
    }
    let mut error = AppError::authentication(
        "vinted.web_authentication_pending",
        "finish signing in to Vinted in the browser window",
    )
    .with_details(json!({
        "browser_open": true,
        "user_action": "Sign in to Vinted and complete any human verification shown in the browser."
    }));
    error.safe_to_retry = true;
    error.next_actions.push(status_action(portal));
    Err(error)
}

pub fn status(portal: PortalId) -> Result<VintedWebAuthStatus, AppError> {
    begin_login(portal)
}

pub fn logout(portal: PortalId) -> Result<VintedWebLogoutOutput, AppError> {
    VintedWebSession::discover(portal)?;
    Err(AppError::usage(
        "sign out on the Vinted website to clear your normal Chrome session; Flea does not clear shared browser cookies",
    ))
}

fn status_with_session(session: &VintedWebSession) -> Result<VintedWebAuthStatus, AppError> {
    decode_auth_response(session.extension_request(request_command(
        "GET",
        "/api/v2/users/current",
        None,
    ))?)
}

fn decode_auth_response(response: Value) -> Result<VintedWebAuthStatus, AppError> {
    let status = response
        .get("status")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            AppError::upstream(
                "vinted.web_browser_invalid_response",
                "the browser authentication check returned an invalid response",
            )
        })?;
    let body = response.get("body").unwrap_or(&Value::Null);
    let user = body.get("user").unwrap_or(body);
    let authenticated = user.get("id").is_some_and(|id| !id.is_null());
    Ok(VintedWebAuthStatus {
        authenticated: (200..300).contains(&status) && authenticated,
        browser_open: true,
        validation: "online_current_user",
    })
}

pub fn login_action(portal: PortalId) -> NextAction {
    NextAction {
        command: format!("flea vinted --portal {portal} auth login --browser"),
    }
}

pub fn status_action(portal: PortalId) -> NextAction {
    NextAction {
        command: format!("flea vinted --portal {portal} auth status --browser"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_extension_setup_has_actionable_guidance() {
        let error = VintedWebSession::from_configuration(false).err().unwrap();
        assert!(error.message.contains("Flea Chrome extension"));
        assert!(error.message.contains("flea extension setup"));
        assert_eq!(error.next_actions[0].command, "flea extension setup");
        assert!(VintedWebSession::from_configuration(true).is_ok());
    }

    #[test]
    fn authentication_requests_the_current_user_through_a_defined_command() {
        assert_eq!(
            request_command("GET", "/api/v2/users/current", None),
            json!({
                "action": "request", "method": "GET",
                "path": "/api/v2/users/current", "body": null
            })
        );
    }

    #[test]
    fn authentication_requires_success_and_a_current_user() {
        for (status, body, authenticated) in [
            (200, json!({"user": {"id": 42}}), true),
            (200, json!({"id": 42}), true),
            (200, json!({"user": {"id": null}}), false),
            (200, json!({}), false),
            (401, json!({"user": {"id": 42}}), false),
            (403, json!({"message": "verification required"}), false),
        ] {
            let response = decode_auth_response(json!({"status": status, "body": body})).unwrap();
            assert_eq!(response.authenticated, authenticated);
            assert!(response.browser_open);
            assert_eq!(response.validation, "online_current_user");
        }
    }

    #[test]
    fn invalid_auth_responses_do_not_expose_browser_payloads() {
        for status in [Value::Null, json!("private-cookie-value"), json!(-1)] {
            let error = decode_auth_response(json!({"status": status})).unwrap_err();
            assert_eq!(error.code, "vinted.web_browser_invalid_response");
            assert!(!format!("{error:?}").contains("private-cookie-value"));
        }
    }

    #[test]
    #[ignore = "requires a signed-in Vinted Chrome extension; read-only"]
    fn live_extension_read_only_probe() {
        assert!(status(PortalId::Fi).unwrap().authenticated);
    }

    #[test]
    fn web_actions_are_portal_scoped() {
        assert_eq!(
            status_action(PortalId::Fi).command,
            "flea vinted --portal fi auth status --browser"
        );
        assert_eq!(
            login_action(PortalId::Fi).command,
            "flea vinted --portal fi auth login --browser"
        );
    }
}
