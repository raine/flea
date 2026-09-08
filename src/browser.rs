use std::{
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, Shutdown, SocketAddr, TcpStream, ToSocketAddrs},
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde_json::{Value, json};
use tungstenite::{Message, WebSocket, client::client_with_config, protocol::WebSocketConfig};
use url::Url;

use crate::error::AppError;

const START_TIMEOUT: Duration = Duration::from_secs(15);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
const MAX_MESSAGE_BYTES: usize = 8 * 1024 * 1024;

pub(crate) fn parse_browser_url(value: &str) -> Result<Url, String> {
    let url = Url::parse(value).map_err(|_| "expected a Chrome debugging HTTP URL".to_owned())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(
            "expected an HTTP(S) browser URL without credentials, query, or fragment".to_owned(),
        );
    }
    Ok(url)
}

fn connect_remote(endpoint: &Url) -> Result<Cdp, AppError> {
    let mut endpoint = endpoint.clone();
    endpoint.set_path(&format!(
        "{}/json/version",
        endpoint.path().trim_end_matches('/')
    ));
    // Reqwest's runtime must not nest inside the command's Tokio runtime.
    let websocket_url = thread::spawn(move || -> Result<String, AppError> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| remote_connection_error())?
            .block_on(async {
                let client = reqwest::Client::builder()
                    .timeout(START_TIMEOUT)
                    .redirect(reqwest::redirect::Policy::none())
                    .no_proxy()
                    .build()
                    .map_err(|_| remote_connection_error())?;
                let mut response = client
                    .get(endpoint)
                    .send()
                    .await
                    .and_then(reqwest::Response::error_for_status)
                    .map_err(|_| remote_connection_error())?;
                let mut body = Vec::new();
                while let Some(chunk) = response
                    .chunk()
                    .await
                    .map_err(|_| remote_connection_error())?
                {
                    if body.len() + chunk.len() > MAX_MESSAGE_BYTES {
                        return Err(remote_connection_error());
                    }
                    body.extend_from_slice(&chunk);
                }
                let value: Value =
                    serde_json::from_slice(&body).map_err(|_| remote_connection_error())?;
                value
                    .get("webSocketDebuggerUrl")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(remote_connection_error)
            })
    })
    .join()
    .map_err(|_| remote_connection_error())??;
    let url = Url::parse(&websocket_url).map_err(|_| remote_connection_error())?;
    if url.scheme() != "ws" || !url.username().is_empty() || url.password().is_some() {
        return Err(remote_connection_error());
    }
    let host = url.host_str().ok_or_else(remote_connection_error)?;
    let port = url
        .port_or_known_default()
        .ok_or_else(remote_connection_error)?;
    let addresses = (host.trim_matches(['[', ']']), port)
        .to_socket_addrs()
        .map_err(|_| remote_connection_error())?;
    let deadline = Instant::now() + START_TIMEOUT;
    for address in addresses {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(remote_connection_error)?;
        let Ok(stream) = TcpStream::connect_timeout(&address, remaining) else {
            continue;
        };
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        if let Ok((socket, _)) = client_with_config(
            url.as_str(),
            DeadlineStream { stream, deadline },
            Some(config),
        ) {
            return Ok(Cdp { socket, next_id: 0 });
        }
    }
    Err(remote_connection_error())
}

fn remote_connection_error() -> AppError {
    browser_error(
        "could not connect to the supplied Chrome debugging URL; verify its /json/version endpoint and ws debugger address are reachable",
    )
}

// A deadline on each underlying read bounds fragmented frames and trickling peers too.
struct DeadlineStream {
    stream: TcpStream,
    deadline: Instant,
}

impl DeadlineStream {
    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
    }
}

impl Read for DeadlineStream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.stream.set_read_timeout(Some(self.remaining()?))?;
        self.stream.read(buffer)
    }
}

impl Write for DeadlineStream {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

struct Cdp {
    socket: WebSocket<DeadlineStream>,
    next_id: u64,
}

impl Cdp {
    fn connect(port: u16, path: &str, deadline: Instant) -> Option<Self> {
        for ip in [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V6(Ipv6Addr::LOCALHOST),
        ] {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let address = SocketAddr::new(ip, port);
            let Ok(stream) =
                TcpStream::connect_timeout(&address, remaining.min(Duration::from_millis(200)))
            else {
                continue;
            };
            let stream = DeadlineStream { stream, deadline };
            let config = WebSocketConfig::default()
                .max_message_size(Some(MAX_MESSAGE_BYTES))
                .max_frame_size(Some(MAX_MESSAGE_BYTES));
            // The profile's opaque browser path, not a global port, identifies Chrome.
            // No Origin header or permissive remote-allow-origins flag is needed.
            if let Ok((socket, _)) =
                client_with_config(format!("ws://{address}{path}"), stream, Some(config))
            {
                return Some(Self { socket, next_id: 0 });
            }
        }
        None
    }

    fn call(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, AppError> {
        self.call_until(session, method, params, Instant::now() + COMMAND_TIMEOUT)
    }

    fn call_until(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, AppError> {
        self.socket.get_mut().deadline = deadline;
        self.next_id += 1;
        let id = self.next_id;
        let mut request = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            request["sessionId"] = json!(session);
        }
        let result = (|| {
            self.socket
                .send(Message::Text(request.to_string().into()))
                .map_err(cdp_io_error)?;
            loop {
                if Instant::now() >= deadline {
                    return Err(browser_error("the browser command timed out"));
                }
                let message = self.socket.read().map_err(cdp_io_error)?;
                match message {
                    Message::Text(text) => {
                        let value: Value =
                            serde_json::from_str(&text).map_err(|_| invalid_response())?;
                        if value.get("id").and_then(Value::as_u64) != Some(id) {
                            continue;
                        }
                        if value.get("sessionId").and_then(Value::as_str) != session {
                            return Err(invalid_response());
                        }
                        if value.get("error").is_some() {
                            return Err(AppError::upstream(
                                "vinted.web_browser_command_rejected",
                                "Chrome could not complete the browser command",
                            ));
                        }
                        return value.get("result").cloned().ok_or_else(invalid_response);
                    }
                    Message::Ping(_) | Message::Pong(_) => {}
                    _ => return Err(browser_error("the browser connection closed unexpectedly")),
                }
            }
        })();
        if result
            .as_ref()
            .is_err_and(|error| error.code != "vinted.web_browser_command_rejected")
        {
            // A failed command may already have mutated remote state. Never replay it.
            let _ = self.socket.get_mut().stream.shutdown(Shutdown::Both);
        }
        result
    }
}

pub(crate) struct ChromePage {
    cdp: Cdp,
    session: String,
    target: String,
    origin: String,
}

impl ChromePage {
    pub(crate) fn open_remote(endpoint: &Url, url: &str) -> Result<Self, AppError> {
        let mut page = Self::attach(connect_remote(endpoint)?, url, url)?;
        page.wait_ready()?;
        Ok(page)
    }

    pub(crate) fn clear_remote(endpoint: &Url, origin: &str) -> Result<(), AppError> {
        Self::attach(connect_remote(endpoint)?, origin, "about:blank")?.clear()
    }

    pub(crate) fn open(profile: &Path, url: &str) -> Result<Self, AppError> {
        let mut page = Self::connect(profile, url, url)?;
        page.wait_ready()?;
        Ok(page)
    }

    pub(crate) fn clear_profile(profile: &Path, origin: &str) -> Result<(), AppError> {
        Self::connect(profile, origin, "about:blank")?.clear()
    }

    fn connect(profile: &Path, url: &str, initial_url: &str) -> Result<Self, AppError> {
        let deadline = Instant::now() + START_TIMEOUT;
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(profile.with_extension("lock"))
            .map_err(profile_error)?;
        loop {
            match FileExt::try_lock_exclusive(&lock) {
                Ok(()) => break,
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(profile_error(error)),
            }
        }
        let mut cdp = connect_profile(profile, deadline);
        if cdp.is_none() {
            launch_chrome(profile)?;
            while Instant::now() < deadline {
                cdp = connect_profile(profile, deadline);
                if cdp.is_some() {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
        let cdp = cdp.ok_or_else(|| {
            AppError::upstream(
                "vinted.web_chrome_start_timeout",
                "Chrome did not expose its profile debugging connection; close the Flea Chrome window and retry",
            )
        })?;
        let page = Self::attach(cdp, url, initial_url)?;
        drop(lock);
        Ok(page)
    }

    fn attach(mut cdp: Cdp, url: &str, initial_url: &str) -> Result<Self, AppError> {
        let origin = Url::parse(url)
            .map_err(|_| invalid_response())?
            .origin()
            .ascii_serialization();
        let targets = cdp.call(None, "Target.getTargets", json!({}))?;
        let target = match matching_target(&targets, &origin) {
            Some(target) => target.to_owned(),
            None => {
                let result =
                    cdp.call(None, "Target.createTarget", json!({ "url": initial_url }))?;
                string_field(&result, "targetId")?.to_owned()
            }
        };
        let result = cdp.call(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target, "flatten": true }),
        )?;
        let session = string_field(&result, "sessionId")?.to_owned();
        Ok(Self {
            cdp,
            session,
            target,
            origin,
        })
    }

    fn wait_ready(&mut self) -> Result<(), AppError> {
        let deadline = Instant::now() + START_TIMEOUT;
        let expression = format!(
            "location.origin === {} && document.readyState !== 'loading'",
            serde_json::to_string(&self.origin).expect("origin serializes"),
        );
        while Instant::now() < deadline {
            // Navigation may replace the execution context between readiness probes.
            let result = self.cdp.call_until(
                Some(&self.session),
                "Runtime.evaluate",
                json!({
                    "expression": expression, "returnByValue": true,
                }),
                deadline,
            );
            match result {
                Ok(result) if result.pointer("/result/value") == Some(&Value::Bool(true)) => {
                    return Ok(());
                }
                Ok(_) => {}
                Err(error) if error.code == "vinted.web_browser_command_rejected" => {}
                Err(error) => return Err(error),
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(browser_error(
            "the browser page did not become ready; finish any sign-in or verification in Chrome and retry",
        ))
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, AppError> {
        self.cdp.call(Some(&self.session), method, params)
    }

    pub(crate) fn evaluate(&mut self, expression: &str) -> Result<Value, AppError> {
        // Check in the same execution task as the action, not in a preceding probe.
        let expression = format!(
            "if (location.origin !== {}) {{ throw new Error('Unexpected page origin'); }}\n{expression}",
            serde_json::to_string(&self.origin).expect("origin serializes"),
        );
        let result = self.call(
            "Runtime.evaluate",
            json!({
                "expression": expression,
                "awaitPromise": true,
                "returnByValue": true,
                "timeout": COMMAND_TIMEOUT.as_millis() as u64,
            }),
        )?;
        if result.get("exceptionDetails").is_some() {
            return Err(browser_error(
                "the browser could not evaluate the requested action",
            ));
        }
        if result.pointer("/result/type").and_then(Value::as_str) == Some("undefined") {
            return Ok(Value::Null);
        }
        result
            .pointer("/result/value")
            .cloned()
            .ok_or_else(invalid_response)
    }

    fn clear(&mut self) -> Result<(), AppError> {
        self.call("Network.clearBrowserCookies", json!({}))?;
        self.call(
            "Storage.clearDataForOrigin",
            json!({ "origin": self.origin, "storageTypes": "local_storage" }),
        )?;
        self.cdp.call(
            None,
            "Target.closeTarget",
            json!({ "targetId": self.target }),
        )?;
        Ok(())
    }
}

fn matching_target<'a>(targets: &'a Value, origin: &str) -> Option<&'a str> {
    targets
        .get("targetInfos")?
        .as_array()?
        .iter()
        .find_map(|target| {
            if target.get("type")?.as_str()? != "page" {
                return None;
            }
            let url = Url::parse(target.get("url")?.as_str()?).ok()?;
            (url.origin().ascii_serialization() == origin)
                .then(|| target.get("targetId")?.as_str())
                .flatten()
        })
}

fn connect_profile(profile: &Path, deadline: Instant) -> Option<Cdp> {
    let mut content = String::new();
    File::open(profile.join("DevToolsActivePort"))
        .ok()?
        .take(1025)
        .read_to_string(&mut content)
        .ok()?;
    let (port, path) = parse_endpoint(&content)?;
    Cdp::connect(
        port,
        path,
        deadline.min(Instant::now() + Duration::from_secs(1)),
    )
}

fn parse_endpoint(content: &str) -> Option<(u16, &str)> {
    if content.len() > 1024 {
        return None;
    }
    let mut lines = content.lines();
    let port = lines
        .next()?
        .parse::<u16>()
        .ok()
        .filter(|port| *port != 0)?;
    let path = lines.next()?;
    let id = path.strip_prefix("/devtools/browser/")?;
    if id.is_empty()
        || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        || lines.next().is_some()
    {
        return None;
    }
    Some((port, path))
}

pub(crate) fn open_without_debugging(profile: &Path) -> Result<(), AppError> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(profile.with_extension("lock"))
        .map_err(profile_error)?;
    FileExt::lock_exclusive(&lock).map_err(profile_error)?;
    if connect_profile(profile, Instant::now() + START_TIMEOUT).is_some() {
        return Err(AppError::usage(
            "close the debugging-enabled Flea Chrome window before running flea browser",
        ));
    }
    launch_chrome_with_debugging(profile, false)
}

fn launch_chrome(profile: &Path) -> Result<(), AppError> {
    launch_chrome_with_debugging(profile, true)
}

fn chrome_command(executable: &str, profile: &Path, debugging: bool) -> Command {
    let mut command = Command::new(executable);
    if debugging {
        command
            .arg("--remote-debugging-port=0")
            .arg("--remote-debugging-address=127.0.0.1");
    }
    command
        .arg(format!("--user-data-dir={}", profile.display()))
        .arg("about:blank")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

fn launch_chrome_with_debugging(profile: &Path, debugging: bool) -> Result<(), AppError> {
    for executable in chrome_executables() {
        match chrome_command(executable, profile, debugging).spawn() {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(AppError::upstream(
                    "vinted.web_chrome_launch_failed",
                    "Google Chrome could not be started for Vinted web publication",
                )
                .with_source(error));
            }
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

fn string_field<'a>(value: &'a Value, key: &str) -> Result<&'a str, AppError> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(invalid_response)
}

fn cdp_io_error(error: tungstenite::Error) -> AppError {
    if matches!(error, tungstenite::Error::Capacity(_)) {
        return AppError::upstream(
            "vinted.web_browser_response_too_large",
            "the browser transport response exceeded its safety limit",
        );
    }
    browser_error(
        "the browser connection failed or timed out; inspect remote state before retrying",
    )
}

fn browser_error(message: &'static str) -> AppError {
    AppError::upstream("vinted.web_browser_failed", message)
}

fn invalid_response() -> AppError {
    AppError::upstream(
        "vinted.web_browser_invalid_response",
        "the browser transport returned an invalid response",
    )
}

fn profile_error(error: io::Error) -> AppError {
    AppError::unexpected("failed to manage the browser profile").with_source(error)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::{net::TcpListener, sync::mpsc};

    #[allow(
        clippy::result_large_err,
        reason = "tungstenite fixes the handshake callback error type"
    )]
    pub(crate) fn peer(
        handler: impl FnOnce(&mut WebSocket<TcpStream>) + Send + 'static,
    ) -> (ChromePage, thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let join = thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .set_write_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut socket = tungstenite::accept_hdr(
                stream,
                |request: &tungstenite::handshake::server::Request, response| {
                    assert_eq!(request.uri().path(), "/devtools/browser/test-id");
                    assert!(!request.headers().contains_key("origin"));
                    Ok(response)
                },
            )
            .unwrap();
            handler(&mut socket);
        });
        let cdp = Cdp::connect(
            port,
            "/devtools/browser/test-id",
            Instant::now() + Duration::from_secs(5),
        )
        .unwrap();
        (
            ChromePage {
                cdp,
                session: "page-session".into(),
                target: "page-target".into(),
                origin: "https://www.vinted.fi".into(),
            },
            join,
        )
    }

    pub(crate) fn request(socket: &mut WebSocket<TcpStream>, method: &str) -> Value {
        let message = socket.read().unwrap();
        let value: Value = serde_json::from_str(message.to_text().unwrap()).unwrap();
        assert_eq!(value["method"], method);
        value
    }

    pub(crate) fn reply(socket: &mut WebSocket<TcpStream>, request: &Value, result: Value) {
        let mut response = json!({ "id": request["id"], "result": result });
        if let Some(session) = request.get("sessionId") {
            response["sessionId"] = session.clone();
        }
        socket
            .send(Message::Text(response.to_string().into()))
            .unwrap();
    }

    pub(crate) fn reject(socket: &mut WebSocket<TcpStream>, request: &Value) {
        let mut response =
            json!({ "id": request["id"], "error": { "message": "private-cookie-value" } });
        if let Some(session) = request.get("sessionId") {
            response["sessionId"] = session.clone();
        }
        socket
            .send(Message::Text(response.to_string().into()))
            .unwrap();
    }

    #[test]
    fn manual_browser_has_no_debugging_flags() {
        for debugging in [false, true] {
            let command = chrome_command("chrome", Path::new("/tmp/flea-profile"), debugging);
            let args: Vec<_> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(args.contains(&"--user-data-dir=/tmp/flea-profile".to_owned()));
            assert_eq!(
                args.iter().any(|arg| arg.starts_with("--remote-debugging")),
                debugging
            );
        }
    }

    #[test]
    fn remote_browser_discovers_endpoint_and_attaches() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let join = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut header = Vec::new();
            while !header.ends_with(b"\r\n\r\n") {
                let mut byte = [0];
                stream.read_exact(&mut byte).unwrap();
                header.push(byte[0]);
            }
            assert!(
                String::from_utf8(header)
                    .unwrap()
                    .starts_with("GET /json/version HTTP/1.1")
            );
            let body =
                json!({"webSocketDebuggerUrl": format!("ws://{address}/devtools/browser/remote")})
                    .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            )
            .unwrap();
            drop(stream);
            let (stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            let call = request(&mut socket, "Target.getTargets");
            reply(
                &mut socket,
                &call,
                json!({"targetInfos": [{"type": "page", "url": "https://www.vinted.fi/items/new", "targetId": "existing"}]}),
            );
            let call = request(&mut socket, "Target.attachToTarget");
            assert_eq!(call["params"]["targetId"], "existing");
            reply(&mut socket, &call, json!({"sessionId": "session"}));
            let call = request(&mut socket, "Runtime.evaluate");
            reply(&mut socket, &call, json!({"result": {"value": true}}));
        });
        let endpoint = parse_browser_url(&format!("http://{address}/")).unwrap();
        // Remote attachment does not require a profile directory or Chrome executable.
        let page = ChromePage::open_remote(&endpoint, "https://www.vinted.fi/items/new").unwrap();
        assert_eq!(page.target, "existing");
        join.join().unwrap();
    }

    #[test]
    fn unavailable_remote_browser_returns_error_without_fallback() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let endpoint =
            parse_browser_url(&format!("http://{}", listener.local_addr().unwrap())).unwrap();
        drop(listener);
        assert!(connect_remote(&endpoint).is_err());
    }

    #[test]
    fn endpoint_file_is_bounded_and_cannot_select_a_remote_host_or_arbitrary_path() {
        assert_eq!(
            parse_endpoint("12345\n/devtools/browser/opaque-id\n"),
            Some((12345, "/devtools/browser/opaque-id"))
        );
        for invalid in [
            "",
            "0\n/devtools/browser/id",
            "65536\n/devtools/browser/id",
            "9222",
            "9222\n",
            "9222\n/devtools/browser/",
            "9222\nws://evil.test/devtools/browser/id",
            "9222\n/devtools/browser/../id",
            "9222\n/devtools/browser/id?query",
            "9222\n/devtools/browser/id\nextra",
        ] {
            assert_eq!(parse_endpoint(invalid), None, "{invalid}");
        }
        assert_eq!(
            parse_endpoint(&format!("1234\n/devtools/browser/{}", "x".repeat(1024))),
            None
        );
    }

    #[test]
    fn targets_require_an_exact_origin_and_page_type() {
        let targets = json!({ "targetInfos": [
            { "type": "service_worker", "url": "https://www.vinted.fi/", "targetId": "worker" },
            { "type": "page", "url": "https://www.vinted.fi.evil.test/", "targetId": "other" },
            { "type": "page", "url": "http://www.vinted.fi/", "targetId": "insecure" },
            { "type": "page", "url": "https://www.vinted.fi/items/new", "targetId": "selected" }
        ] });
        assert_eq!(
            matching_target(&targets, "https://www.vinted.fi"),
            Some("selected")
        );
        assert_eq!(matching_target(&targets, "https://example.test"), None);
    }

    #[test]
    fn evaluation_correlates_responses_and_awaits_json_promises() {
        let (mut page, join) = peer(|socket| {
            let call = request(socket, "Runtime.evaluate");
            assert_eq!(call["params"]["awaitPromise"], true);
            assert_eq!(call["params"]["returnByValue"], true);
            socket
                .send(Message::Text(
                    json!({"method":"Page.loadEventFired","params":{}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            socket
                .send(Message::Text(
                    json!({"id":999,"result":{}}).to_string().into(),
                ))
                .unwrap();
            reply(
                socket,
                &call,
                json!({ "result": { "type": "object", "value": { "text": "Käytetty", "price": 5 } } }),
            );
        });
        assert_eq!(
            page.evaluate("Promise.resolve({text: 'Käytetty', price: 5})")
                .unwrap(),
            json!({ "text": "Käytetty", "price": 5 })
        );
        join.join().unwrap();
    }

    #[test]
    fn command_rejections_are_redacted_without_breaking_later_reads() {
        let (mut page, join) = peer(|socket| {
            let call = request(socket, "Page.getResourceContent");
            reject(socket, &call);
            let call = request(socket, "Runtime.evaluate");
            reply(
                socket,
                &call,
                json!({ "result": { "type": "boolean", "value": true } }),
            );
        });
        let error = page.call("Page.getResourceContent", json!({})).unwrap_err();
        assert_eq!(error.code, "vinted.web_browser_command_rejected");
        assert!(!error.safe_to_retry);
        assert!(
            !error
                .internal_chain()
                .join(" ")
                .contains("private-cookie-value")
        );
        assert_eq!(page.evaluate("true").unwrap(), true);
        join.join().unwrap();
    }

    #[test]
    fn javascript_exceptions_are_not_exposed_or_replayed() {
        let (mut page, join) = peer(|socket| {
            let call = request(socket, "Runtime.evaluate");
            reply(
                socket,
                &call,
                json!({ "exceptionDetails": { "text": "private-cookie-value" }, "result": { "type": "object" } }),
            );
        });
        let error = page
            .evaluate("throw new Error('private-cookie-value')")
            .unwrap_err();
        assert_eq!(error.code, "vinted.web_browser_failed");
        assert!(!error.safe_to_retry);
        assert!(
            !error
                .internal_chain()
                .join(" ")
                .contains("private-cookie-value")
        );
        join.join().unwrap();
    }

    #[test]
    fn responses_from_another_session_are_rejected() {
        let (mut page, join) = peer(|socket| {
            let call = request(socket, "Runtime.evaluate");
            socket
                .send(Message::Text(
                    json!({"id":call["id"],"sessionId":"other","result":{}})
                        .to_string()
                        .into(),
                ))
                .unwrap();
        });
        assert_eq!(
            page.evaluate("true").unwrap_err().code,
            "vinted.web_browser_invalid_response"
        );
        join.join().unwrap();
    }

    #[test]
    fn deadline_disconnect_and_response_size_fail_without_replay() {
        let (release, wait) = mpsc::channel();
        let (mut page, join) = peer(move |socket| {
            request(socket, "Runtime.evaluate");
            wait.recv_timeout(Duration::from_secs(5)).unwrap();
        });
        let error = page
            .cdp
            .call_until(
                Some(&page.session),
                "Runtime.evaluate",
                json!({}),
                Instant::now() + Duration::from_millis(50),
            )
            .unwrap_err();
        assert!(!error.safe_to_retry);
        release.send(()).unwrap();
        join.join().unwrap();

        let (mut page, join) = peer(|socket| {
            request(socket, "Runtime.evaluate");
        });
        assert_eq!(
            page.evaluate("true").unwrap_err().code,
            "vinted.web_browser_failed"
        );
        join.join().unwrap();

        let (mut page, join) = peer(|socket| {
            request(socket, "Runtime.evaluate");
            let _ = socket.send(Message::Text("x".repeat(MAX_MESSAGE_BYTES + 1).into()));
        });
        assert_eq!(
            page.evaluate("true").unwrap_err().code,
            "vinted.web_browser_response_too_large"
        );
        join.join().unwrap();
    }

    #[test]
    fn readiness_retries_only_rejected_read_only_probes() {
        let (mut page, join) = peer(|socket| {
            let call = request(socket, "Runtime.evaluate");
            reject(socket, &call);
            let call = request(socket, "Runtime.evaluate");
            reply(socket, &call, json!({ "result": { "value": true } }));
        });
        page.wait_ready().unwrap();
        join.join().unwrap();
    }

    #[test]
    fn profile_discovery_uses_its_browser_path_and_ignores_missing_files() {
        let profile = tempfile::tempdir().unwrap();
        assert!(connect_profile(profile.path(), Instant::now() + Duration::from_secs(1)).is_none());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::fs::write(
            profile.path().join("DevToolsActivePort"),
            format!("{port}\n/devtools/browser/profile-id\n"),
        )
        .unwrap();
        let join = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut request = [0; 4096];
            let count = stream.read(&mut request).unwrap();
            assert!(
                String::from_utf8_lossy(&request[..count])
                    .starts_with("GET /devtools/browser/profile-id HTTP/1.1")
            );
            stream
                .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
                .unwrap();
        });
        assert!(connect_profile(profile.path(), Instant::now() + Duration::from_secs(2)).is_none());
        join.join().unwrap();
        assert!(profile.path().join("DevToolsActivePort").exists());
    }

    #[test]
    #[ignore = "launches system Chrome with an isolated profile and local HTTP fixture"]
    fn live_chrome_profile_lifecycle() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let occupied = TcpListener::bind((Ipv4Addr::LOCALHOST, 9222)).ok();
        eprintln!("port 9222 occupied by fixture: {}", occupied.is_some());
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let offline = Arc::new(AtomicBool::new(false));
        let offline_server = offline.clone();
        let server = thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(500)))
                        .unwrap();
                    let mut buffer = [0; 4096];
                    let _ = stream.read(&mut buffer);
                    if offline_server.load(Ordering::Relaxed) {
                        continue;
                    }
                    let body = "<!doctype html><title>Flea CDP fixture</title>Local fixture";
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                } else {
                    thread::sleep(Duration::from_millis(10));
                }
            }
        });
        let profile = tempfile::tempdir().unwrap();
        let url = format!("http://{address}/");
        let mut page = ChromePage::open(profile.path(), &url).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(
            || -> Result<(), AppError> {
                let endpoint =
                    std::fs::read_to_string(profile.path().join("DevToolsActivePort")).unwrap();
                assert_ne!(parse_endpoint(&endpoint).unwrap().0, 9222);
                assert_eq!(
                    page.evaluate("Promise.resolve({text:'Käytetty', ok:true})")?,
                    json!({"text":"Käytetty","ok":true})
                );
                crate::marketplace::vinted::web::verify_token_fixture(&mut page)?;
                page.evaluate("document.cookie='fixture=present; path=/'; localStorage.setItem('fixture','present'); sessionStorage.setItem('fixture','present'); true")?;
                eprintln!("fixture state seeded");
                let target = page.target.clone();
                let mut reconnect = ChromePage::open(profile.path(), &url)?;
                assert_eq!(reconnect.target, target);
                assert_eq!(
                    reconnect.evaluate("localStorage.getItem('fixture')")?,
                    "present"
                );
                let unrelated =
                    page.cdp
                        .call(None, "Target.createTarget", json!({ "url": "about:blank" }))?;
                let unrelated = unrelated["targetId"].as_str().unwrap().to_owned();
                // Remove the origin tab and make HTTP unavailable before clearing.
                page.cdp
                    .call(None, "Target.closeTarget", json!({ "targetId": target }))?;
                offline.store(true, Ordering::Relaxed);
                eprintln!("clearing profile offline");
                ChromePage::clear_profile(profile.path(), &url)?;
                eprintln!("offline clear passed");
                let targets = page.cdp.call(None, "Target.getTargets", json!({}))?;
                assert!(targets["targetInfos"].as_array().unwrap().iter().any(
                    |target| target["targetId"] == unrelated && target["url"] == "about:blank"
                ));
                let cookies = page.cdp.call(None, "Storage.getCookies", json!({}))?;
                assert!(
                    !cookies["cookies"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .any(|cookie| cookie["name"] == "fixture")
                );
                offline.store(false, Ordering::Relaxed);
                let mut cleared = ChromePage::open(profile.path(), &url)?;
                assert_eq!(
                    cleared
                        .evaluate("({local:localStorage.length, session:sessionStorage.length})")?,
                    json!({"local":0,"session":0})
                );
                eprintln!(
                    "ephemeral launch, promise evaluation, reconnect, offline logout and unrelated-tab preservation passed"
                );
                Ok(())
            },
        ));
        stop.store(true, Ordering::Relaxed);
        server.join().unwrap();
        let _ = page.cdp.call(None, "Browser.close", json!({}));
        result.unwrap().unwrap();
    }
}
