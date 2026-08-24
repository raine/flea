use std::{
    collections::HashSet,
    ffi::OsStr,
    fs,
    io::{Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    domain::envelope::NextAction,
    error::AppError,
    marketplace::{MarketplaceContext, PortalId},
    storage::{StatePaths, atomic_file::secure_directory},
};

use super::binding::VINTED_FI_BINDING;

const AGENT_BROWSER: &str = "agent-browser";
const CHROME_CDP_PORT: u16 = 9222;
const CHROME_START_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BROWSER_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

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

#[derive(Clone, Debug)]
pub struct AgentBrowserSession {
    session: String,
    profile: PathBuf,
}

impl AgentBrowserSession {
    pub fn discover(portal: PortalId) -> Result<Self, AppError> {
        let context = match portal {
            PortalId::Fi => MarketplaceContext::VINTED_FI,
        };
        let paths = StatePaths::discover(context).map_err(state_error)?;
        paths.ensure().map_err(state_error)?;
        let profile = paths.auth_dir().join("web-profile");
        secure_directory(&profile).map_err(state_error)?;
        Ok(Self {
            session: format!("flea-vinted-{portal}-web"),
            profile,
        })
    }

    pub fn open(&self) -> Result<(), AppError> {
        if self.evaluate("location.origin === 'https://www.vinted.fi'")? == Value::Bool(true) {
            return Ok(());
        }
        let output = self.run(["--json", "open", web_publication_url()], None)?;
        decode_success(&output).map(|_| ())
    }

    pub fn evaluate(&self, script: &str) -> Result<Value, AppError> {
        let output = self.run(["--json", "eval", "--stdin"], Some(script))?;
        let data = decode_success(&output)?;
        data.get("result").cloned().ok_or_else(|| {
            AppError::upstream(
                "vinted.web_browser_invalid_response",
                "the browser transport returned no evaluation result",
            )
        })
    }

    pub fn csrf_token(&self) -> Result<String, AppError> {
        if let Some(token) = self
            .evaluate("window.__fleaCsrfToken || localStorage.getItem('__fleaCsrfToken') || null")?
            .as_str()
            .filter(|token| valid_csrf_token(token))
        {
            return Ok(token.to_owned());
        }
        let output = self.run(
            [
                "--json",
                "network",
                "requests",
                "--filter",
                "_next/static/chunks/",
            ],
            None,
        )?;
        let data = decode_success(&output)?;
        let mut seen_urls = HashSet::new();
        let request_ids = data
            .get("requests")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .rev()
            .filter_map(|request| {
                let url = request.get("url").and_then(Value::as_str)?;
                let request_id = request.get("requestId").and_then(Value::as_str)?;
                (url.contains(".js") && seen_urls.insert(url.to_owned()))
                    .then(|| request_id.to_owned())
            })
            .collect::<Vec<_>>();

        for request_ids in request_ids.chunks(4) {
            let commands = request_ids
                .iter()
                .map(|request_id| json!(["network", "request", request_id]))
                .collect::<Vec<_>>();
            let input = serde_json::to_string(&commands).expect("browser batch serializes");
            let output = self.run(["batch", "--json"], Some(&input))?;
            let results: Value = serde_json::from_slice(&output).map_err(|error| {
                AppError::upstream(
                    "vinted.web_browser_invalid_response",
                    "the browser transport returned an invalid network response",
                )
                .with_source(error)
            })?;
            for result in results.as_array().into_iter().flatten() {
                if let Some(token) = result
                    .pointer("/result/responseBody")
                    .and_then(Value::as_str)
                    .and_then(extract_csrf_token)
                {
                    let token = token.to_owned();
                    let encoded = serde_json::to_string(&token).expect("CSRF token serializes");
                    self.evaluate(&format!(
                        "window.__fleaCsrfToken = {encoded}; localStorage.setItem('__fleaCsrfToken', {encoded}); true"
                    ))?;
                    return Ok(token);
                }
            }
        }

        Err(AppError::upstream(
            "vinted.web_csrf_unavailable",
            "the Vinted browser session did not expose a publication security token",
        ))
    }

    pub fn close(&self) -> Result<(), AppError> {
        let output = self.run(["--json", "close"], None)?;
        decode_success(&output).map(|_| ())
    }

    pub fn clear(&self) -> Result<(), AppError> {
        let output = self.run(["--json", "cookies", "clear"], None)?;
        decode_success(&output)?;
        self.evaluate(
            "(() => { try { localStorage.clear(); sessionStorage.clear(); } catch (_) {} return { cleared: true }; })()",
        )?;
        self.close()
    }

    fn run<I, S>(&self, arguments: I, stdin: Option<&str>) -> Result<Vec<u8>, AppError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let cdp_endpoint = self.cdp_endpoint()?;
        let mut command = Command::new(AGENT_BROWSER);
        command
            .arg("--session")
            .arg(&self.session)
            .arg("--cdp")
            .arg(cdp_endpoint)
            .args(arguments)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if stdin.is_some() {
            command.stdin(Stdio::piped());
        }
        let mut child = command.spawn().map_err(browser_launch_error)?;
        if let Some(script) = stdin {
            child
                .stdin
                .take()
                .expect("piped browser stdin")
                .write_all(script.as_bytes())
                .map_err(browser_execution_error)?;
        }
        let output = child.wait_with_output().map_err(browser_execution_error)?;
        if output.stdout.len() > MAX_BROWSER_OUTPUT_BYTES {
            return Err(AppError::upstream(
                "vinted.web_browser_response_too_large",
                "the browser transport response exceeded its safety limit",
            ));
        }
        if !output.status.success() {
            return Err(AppError::upstream(
                "vinted.web_browser_failed",
                "the browser transport command failed",
            )
            .with_details(json!({ "status": output.status.code() })));
        }
        Ok(output.stdout)
    }

    fn cdp_endpoint(&self) -> Result<String, AppError> {
        if let Some(endpoint) = active_cdp_endpoint(&self.profile) {
            return Ok(endpoint);
        }
        remove_stale_cdp_endpoint(&self.profile)?;
        launch_chrome(&self.profile)?;
        let deadline = Instant::now() + CHROME_START_TIMEOUT;
        while Instant::now() < deadline {
            if let Some(endpoint) = active_cdp_endpoint(&self.profile) {
                return Ok(endpoint);
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(AppError::upstream(
            "vinted.web_chrome_start_timeout",
            "Google Chrome did not expose its local debugging connection in time",
        )
        .with_details(json!({ "cdp_port": CHROME_CDP_PORT })))
    }
}

pub fn login(portal: PortalId) -> Result<VintedWebAuthStatus, AppError> {
    let session = AgentBrowserSession::discover(portal)?;
    session.open()?;
    let status = status_with_session(&session)?;
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
    error.next_actions.push(web_status_action(portal));
    Err(error)
}

pub fn status(portal: PortalId) -> Result<VintedWebAuthStatus, AppError> {
    let session = AgentBrowserSession::discover(portal)?;
    session.open()?;
    status_with_session(&session)
}

pub fn logout(portal: PortalId) -> Result<VintedWebLogoutOutput, AppError> {
    AgentBrowserSession::discover(portal)?.clear()?;
    Ok(VintedWebLogoutOutput {
        authenticated: false,
        cleared: true,
    })
}

fn status_with_session(session: &AgentBrowserSession) -> Result<VintedWebAuthStatus, AppError> {
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

fn decode_success(output: &[u8]) -> Result<Value, AppError> {
    let value: Value = serde_json::from_slice(output).map_err(|error| {
        AppError::upstream(
            "vinted.web_browser_invalid_response",
            "the browser transport returned invalid structured output",
        )
        .with_source(error)
    })?;
    if value.get("success").and_then(Value::as_bool) != Some(true) {
        return Err(AppError::upstream(
            "vinted.web_browser_failed",
            "the browser transport could not complete the requested action",
        ));
    }
    value.get("data").cloned().ok_or_else(|| {
        AppError::upstream(
            "vinted.web_browser_invalid_response",
            "the browser transport returned no result data",
        )
    })
}

fn extract_csrf_token(body: &str) -> Option<&str> {
    let marker = "X-CSRF-Token\",\"";
    let start = body.find(marker)? + marker.len();
    let token = body.get(start..)?.split('"').next()?;
    valid_csrf_token(token).then_some(token)
}

fn valid_csrf_token(token: &str) -> bool {
    token.len() == 36
        && token
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
}

fn active_cdp_endpoint(_profile: &Path) -> Option<String> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, CHROME_CDP_PORT);
    let mut stream =
        TcpStream::connect_timeout(&address.into(), Duration::from_millis(200)).ok()?;
    stream
        .set_read_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    stream
        .set_write_timeout(Some(Duration::from_millis(500)))
        .ok()?;
    stream
        .write_all(
            format!(
                "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1:{CHROME_CDP_PORT}\r\nUser-Agent: flea\r\nAccept: application/json\r\nConnection: close\r\n\r\n"
            )
            .as_bytes(),
        )
        .ok()?;
    let mut response = Vec::new();
    let mut buffer = [0_u8; 4096];
    while response.len() < 64 * 1024 {
        match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&buffer[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(_) => return None,
        }
    }
    decode_cdp_endpoint(&response)
}

fn decode_cdp_endpoint(response: &[u8]) -> Option<String> {
    let body = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .and_then(|index| response.get(index + 4..))?;
    let value: Value = serde_json::from_slice(body).ok()?;
    let endpoint = value.get("webSocketDebuggerUrl")?.as_str()?;
    endpoint
        .starts_with(&format!(
            "ws://127.0.0.1:{CHROME_CDP_PORT}/devtools/browser/"
        ))
        .then(|| endpoint.to_owned())
}

fn remove_stale_cdp_endpoint(profile: &Path) -> Result<(), AppError> {
    match fs::remove_file(profile.join("DevToolsActivePort")) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(state_error(error)),
    }
}

fn launch_chrome(profile: &Path) -> Result<(), AppError> {
    let port = format!("--remote-debugging-port={CHROME_CDP_PORT}");
    let profile = format!("--user-data-dir={}", profile.display());
    for executable in chrome_executables() {
        match Command::new(executable)
            .args([port.as_str(), profile.as_str(), "about:blank"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(chrome_launch_error(error)),
        }
    }
    Err(
        AppError::usage("Vinted web publication requires Google Chrome or Chromium")
            .with_details(json!({ "dependency": "Google Chrome" })),
    )
}

#[cfg(target_os = "macos")]
fn chrome_executables() -> &'static [&'static str] {
    &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"]
}

#[cfg(target_os = "linux")]
fn chrome_executables() -> &'static [&'static str] {
    &["google-chrome", "chromium", "chromium-browser"]
}

#[cfg(target_os = "windows")]
fn chrome_executables() -> &'static [&'static str] {
    &["chrome.exe"]
}

#[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
fn chrome_executables() -> &'static [&'static str] {
    &[]
}

fn web_publication_url() -> &'static str {
    match VINTED_FI_BINDING.context.portal {
        PortalId::Fi => "https://www.vinted.fi/items/new",
    }
}

pub fn login_action(portal: PortalId) -> NextAction {
    NextAction {
        command: format!("flea vinted --portal {portal} auth web login"),
    }
}

fn web_status_action(portal: PortalId) -> NextAction {
    NextAction {
        command: format!("flea vinted --portal {portal} auth web status"),
    }
}

fn browser_launch_error(error: std::io::Error) -> AppError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return AppError::usage(
            "Vinted web publication requires the `agent-browser` executable in PATH",
        )
        .with_details(json!({
            "dependency": AGENT_BROWSER,
            "install_url": "https://github.com/vercel-labs/agent-browser"
        }));
    }
    AppError::upstream(
        "vinted.web_browser_client_failed",
        "the Vinted publication browser client could not be started",
    )
    .with_source(error)
}

fn chrome_launch_error(error: std::io::Error) -> AppError {
    AppError::upstream(
        "vinted.web_chrome_launch_failed",
        "Google Chrome could not be started for Vinted web publication",
    )
    .with_source(error)
}

fn browser_execution_error(error: std::io::Error) -> AppError {
    AppError::upstream(
        "vinted.web_browser_failed",
        "the Vinted publication browser could not complete its command",
    )
    .with_source(error)
}

fn state_error(error: std::io::Error) -> AppError {
    AppError::unexpected("failed to manage the Vinted web browser profile").with_source(error)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_agent_browser_results_without_exposing_error_payloads() {
        let value =
            decode_success(br#"{"success":true,"data":{"result":{"status":200}},"error":null}"#)
                .unwrap();
        assert_eq!(value["result"]["status"], 200);

        let error = decode_success(
            br#"{"success":false,"data":null,"error":{"message":"private cookie"}}"#,
        )
        .unwrap_err();
        assert_eq!(error.code, "vinted.web_browser_failed");
        assert!(!error.message.contains("private cookie"));
    }

    #[test]
    fn web_actions_are_portal_scoped() {
        assert_eq!(
            web_status_action(PortalId::Fi).command,
            "flea vinted --portal fi auth web status"
        );
        assert_eq!(
            login_action(PortalId::Fi).command,
            "flea vinted --portal fi auth web login"
        );
    }

    #[test]
    fn extracts_csrf_token_from_browser_bundle() {
        let body = r#"before X-CSRF-Token","12345678-1234-1234-1234-123456789abc" after"#;
        assert_eq!(
            extract_csrf_token(body),
            Some("12345678-1234-1234-1234-123456789abc")
        );
        assert_eq!(extract_csrf_token(r#"X-CSRF-Token","private""#), None);
    }

    #[test]
    fn decodes_local_chrome_debugging_endpoint() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"webSocketDebuggerUrl\":\"ws://127.0.0.1:9222/devtools/browser/browser-id\"}";
        assert_eq!(
            decode_cdp_endpoint(response).as_deref(),
            Some("ws://127.0.0.1:9222/devtools/browser/browser-id")
        );
    }
}
