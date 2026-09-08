use std::{path::PathBuf, sync::Mutex};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    browser::ChromePage,
    domain::envelope::NextAction,
    error::AppError,
    marketplace::{MarketplaceContext, PortalId},
    storage::{StatePaths, atomic_file::secure_directory},
};

use super::binding::VINTED_FI_BINDING;

const VINTED_ORIGIN: &str = "https://www.vinted.fi";

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

pub struct VintedWebSession {
    profile: PathBuf,
    browser_url: Option<url::Url>,
    page: Mutex<Option<ChromePage>>,
}

impl VintedWebSession {
    pub fn discover(portal: PortalId) -> Result<Self, AppError> {
        Self::discover_with_browser_url(portal, None)
    }

    pub fn discover_with_browser_url(
        portal: PortalId,
        browser_url: Option<&url::Url>,
    ) -> Result<Self, AppError> {
        if let Some(browser_url) = browser_url {
            return Ok(Self {
                profile: PathBuf::new(),
                browser_url: Some(browser_url.clone()),
                page: Mutex::new(None),
            });
        }
        let context = match portal {
            PortalId::Fi => MarketplaceContext::VINTED_FI,
        };
        let paths = StatePaths::discover(context).map_err(state_error)?;
        paths.ensure().map_err(state_error)?;
        let profile = paths.auth_dir().join("web-profile");
        secure_directory(&profile).map_err(state_error)?;
        Ok(Self {
            profile,
            browser_url: None,
            page: Mutex::new(None),
        })
    }

    fn with_page<T>(
        &self,
        action: impl FnOnce(&mut ChromePage) -> Result<T, AppError>,
    ) -> Result<T, AppError> {
        let mut page = self
            .page
            .lock()
            .map_err(|_| AppError::unexpected("failed to access the Vinted browser"))?;
        if page.is_none() {
            *page = Some(match &self.browser_url {
                Some(endpoint) => ChromePage::open_remote(endpoint, web_publication_url())?,
                None => ChromePage::open(&self.profile, web_publication_url())?,
            });
        }
        action(page.as_mut().expect("page initialized"))
    }

    pub fn open_without_debugging(&self) -> Result<(), AppError> {
        crate::browser::open_without_debugging(&self.profile)
    }

    pub fn open(&self) -> Result<(), AppError> {
        self.with_page(|_| Ok(()))
    }

    pub fn evaluate(&self, script: &str) -> Result<Value, AppError> {
        self.with_page(|page| page.evaluate(script))
    }

    pub fn csrf_token(&self) -> Result<String, AppError> {
        self.with_page(read_csrf_token)
    }

    pub fn clear(&self) -> Result<(), AppError> {
        match &self.browser_url {
            Some(endpoint) => ChromePage::clear_remote(endpoint, VINTED_ORIGIN),
            None => ChromePage::clear_profile(&self.profile, VINTED_ORIGIN),
        }
    }
}

// Vinted embeds CSRF_TOKEN in JSON-encoded inline bootstrap data. Inspect text only;
// bootstrap script bodies are never evaluated or returned to the client.
const CSRF_TOKEN_SCRIPT: &str = r#"(() => {
    const tokens = new Set();
    for (const script of document.scripts) {
        if (script.src) continue;
        for (const match of script.textContent.matchAll(/"CSRF_TOKEN\\?":\\?"([a-f0-9-]{36})\\?"/gi)) {
            tokens.add(match[1]);
        }
    }
    return tokens.size === 1 ? [...tokens][0] : null;
})()"#;

fn read_csrf_token(page: &mut ChromePage) -> Result<String, AppError> {
    page.evaluate(CSRF_TOKEN_SCRIPT)?
        .as_str()
        .filter(|token| valid_csrf_token(token))
        .map(str::to_owned)
        .ok_or_else(|| {
            AppError::upstream(
                "vinted.web_csrf_unavailable",
                "the Vinted page did not expose an unambiguous publication security token",
            )
        })
}

pub fn begin_login_with_browser_url(
    portal: PortalId,
    browser_url: Option<&url::Url>,
) -> Result<VintedWebAuthStatus, AppError> {
    let session = VintedWebSession::discover_with_browser_url(portal, browser_url)?;
    status_with_session(&session)
}

pub fn login_with_browser_url(
    portal: PortalId,
    browser_url: Option<&url::Url>,
) -> Result<VintedWebAuthStatus, AppError> {
    let status = begin_login_with_browser_url(portal, browser_url)?;
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

pub fn status_with_browser_url(
    portal: PortalId,
    browser_url: Option<&url::Url>,
) -> Result<VintedWebAuthStatus, AppError> {
    begin_login_with_browser_url(portal, browser_url)
}

pub fn logout_with_browser_url(
    portal: PortalId,
    browser_url: Option<&url::Url>,
) -> Result<VintedWebLogoutOutput, AppError> {
    VintedWebSession::discover_with_browser_url(portal, browser_url)?.clear()?;
    Ok(VintedWebLogoutOutput {
        authenticated: false,
        cleared: true,
    })
}

fn status_with_session(session: &VintedWebSession) -> Result<VintedWebAuthStatus, AppError> {
    let result = session.evaluate(
        r#"(async () => {
            const response = await fetch('/api/v2/users/current', {
                credentials: 'include',
                headers: { accept: 'application/json' }
            });
            const body = await response.json().catch(() => ({}));
            const user = body.user || body;
            return { status: response.status, authenticated: Boolean(response.ok && user.id) };
        })()"#,
    )?;
    let status = result
        .get("status")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            AppError::upstream(
                "vinted.web_browser_invalid_response",
                "the browser authentication check returned an invalid response",
            )
        })?;
    let authenticated = result
        .get("authenticated")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    Ok(VintedWebAuthStatus {
        authenticated: (200..300).contains(&status) && authenticated,
        browser_open: true,
        validation: "online_current_user",
    })
}

fn valid_csrf_token(token: &str) -> bool {
    token.len() == 36
        && token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

fn web_publication_url() -> &'static str {
    match VINTED_FI_BINDING.context.portal {
        PortalId::Fi => "https://www.vinted.fi/items/new",
    }
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

fn state_error(error: std::io::Error) -> AppError {
    AppError::unexpected("failed to manage the Vinted web browser profile").with_source(error)
}

#[cfg(test)]
pub(crate) fn verify_token_fixture(page: &mut ChromePage) -> Result<(), AppError> {
    let token = "12345678-1234-1234-1234-123456789abc";
    assert!(read_csrf_token(page).is_err());
    // Exercise the actual script on both JSON and JSON-encoded bootstrap payloads.
    for bootstrap in [
        json!({"CSRF_TOKEN":token}).to_string(),
        serde_json::to_string(&json!({"CSRF_TOKEN":token}).to_string()).unwrap(),
    ] {
        page.evaluate(&format!("document.querySelector('#token-fixture')?.remove(); var s=document.createElement('script'); s.id='token-fixture'; s.type='application/json'; s.textContent={}; document.head.append(s); true", serde_json::to_string(&bootstrap).unwrap()))?;
        assert_eq!(read_csrf_token(page)?, token);
    }
    page.evaluate(r#"document.querySelector('#token-fixture').textContent=JSON.stringify({a:{CSRF_TOKEN:'12345678-1234-1234-1234-123456789abc'},b:{CSRF_TOKEN:'abcdefab-1234-1234-1234-123456789abc'}}); true"#)?;
    assert!(read_csrf_token(page).is_err());
    page.evaluate("document.querySelector('#token-fixture').remove(); true")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_values_are_validated_without_exposing_script_errors() {
        use crate::browser::tests::{peer, reply, request};
        for value in [
            Value::Null,
            json!("private-cookie-value"),
            json!("12345678-1234-1234-1234-123456789abc"),
        ] {
            let expected = value.as_str().is_some_and(valid_csrf_token);
            let (mut page, join) = peer(move |socket| {
                let call = request(socket, "Runtime.evaluate");
                assert!(
                    call["params"]["expression"]
                        .as_str()
                        .unwrap()
                        .ends_with(CSRF_TOKEN_SCRIPT)
                );
                reply(socket, &call, json!({"result":{"value":value}}));
            });
            let result = read_csrf_token(&mut page);
            assert_eq!(result.is_ok(), expected);
            if let Err(error) = result {
                assert!(
                    !error
                        .internal_chain()
                        .join(" ")
                        .contains("private-cookie-value")
                );
            }
            join.join().unwrap();
        }
    }

    #[test]
    #[ignore = "requires a signed-in Vinted Chrome profile; read-only"]
    fn live_browser_read_only_probe() {
        let session = VintedWebSession::discover(PortalId::Fi).unwrap();
        assert!(status_with_session(&session).unwrap().authenticated);
        session.csrf_token().unwrap();
        eprintln!("authenticated browser and uncached token discovery succeeded (token redacted)");
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
