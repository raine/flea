use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use serde_json::Value;
use tracing::{Subscriber, info, info_span};
use tracing_subscriber::{EnvFilter, Layer, fmt::MakeWriter, layer::SubscriberExt};
use uuid::Uuid;

use crate::{
    domain::envelope::Diagnostics,
    error::{AppError, ExitClass},
    storage::StatePaths,
};

const LOG_FILE: &str = "flea.jsonl";
const LOG_PREFIX: &str = "flea";
const LOG_SUFFIX: &str = ".jsonl";
const REDACTED: &str = "[REDACTED]";
pub const UPSTREAM_BODY_LIMIT: usize = 4 * 1024;

#[derive(Clone, Copy, Debug)]
pub struct RetentionPolicy {
    pub max_age: Duration,
    pub max_total_bytes: u64,
    pub max_active_bytes: u64,
}

impl Default for RetentionPolicy {
    fn default() -> Self {
        Self {
            max_age: Duration::from_secs(30 * 24 * 60 * 60),
            max_total_bytes: 50 * 1024 * 1024,
            max_active_bytes: 5 * 1024 * 1024,
        }
    }
}

#[derive(Clone, Debug)]
pub struct DiagnosticsContext {
    pub trace_id: String,
    pub correlation_id: String,
    pub log_path: PathBuf,
}

impl DiagnosticsContext {
    pub fn envelope(&self) -> Diagnostics {
        Diagnostics {
            trace_id: self.trace_id.clone(),
            correlation_id: self.correlation_id.clone(),
            log_path: self.log_path.to_string_lossy().into_owned(),
        }
    }
}

pub struct DiagnosticsSession {
    context: DiagnosticsContext,
    subscriber: Arc<dyn Subscriber + Send + Sync>,
}

impl std::fmt::Debug for DiagnosticsSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DiagnosticsSession")
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

impl DiagnosticsSession {
    pub fn initialize() -> Result<Self, DiagnosticsInitError> {
        let state_dir = state_dir().map_err(DiagnosticsInitError::without_path)?;
        Self::initialize_at(&state_dir, RetentionPolicy::default())
    }

    pub fn initialize_at(
        state_dir: &Path,
        retention: RetentionPolicy,
    ) -> Result<Self, DiagnosticsInitError> {
        let context = DiagnosticsContext {
            trace_id: Uuid::new_v4().to_string(),
            correlation_id: Uuid::new_v4().to_string(),
            log_path: state_dir.join("logs").join(LOG_FILE),
        };
        initialize_with_context(context, retention)
    }

    pub fn context(&self) -> &DiagnosticsContext {
        &self.context
    }

    pub fn run<T>(&self, command: &str, operation: impl FnOnce() -> (T, u8)) -> T {
        tracing::subscriber::with_default(Arc::clone(&self.subscriber), || {
            let span = info_span!(
                "command",
                command,
                trace_id = %self.context.trace_id,
                correlation_id = %self.context.correlation_id
            );
            let _entered = span.enter();
            let started = Instant::now();
            info!(event = "command.started");
            let (result, exit_code) = operation();
            info!(
                event = "command.finished",
                status = if exit_code == 0 { "success" } else { "failure" },
                exit_code,
                duration_ms = started.elapsed().as_millis() as u64
            );
            result
        })
    }
}

#[derive(Debug)]
pub struct DiagnosticsInitError {
    context: Option<DiagnosticsContext>,
    source: io::Error,
}

impl DiagnosticsInitError {
    fn without_path(source: io::Error) -> Self {
        Self {
            context: None,
            source,
        }
    }

    pub fn into_app_error(self) -> AppError {
        let mut error = AppError::new(
            "diagnostics.initialization_failed",
            "failed to initialize diagnostics",
            ExitClass::Upstream,
        )
        .with_source(self.source);
        if let Some(context) = self.context {
            error.diagnostics = Some(Box::new(context.envelope()));
        }
        error
    }
}

fn initialize_with_context(
    context: DiagnosticsContext,
    retention: RetentionPolicy,
) -> Result<DiagnosticsSession, DiagnosticsInitError> {
    let log_dir = context
        .log_path
        .parent()
        .expect("the active log path always has a parent")
        .to_owned();
    let state_dir = log_dir
        .parent()
        .expect("the log directory always has a state directory")
        .to_owned();
    let error_context = context.clone();
    let result = (|| {
        create_private_dir(&state_dir)?;
        create_private_dir(&log_dir)?;
        roll_active_log(&log_dir, retention.max_active_bytes)?;
        enforce_retention(&log_dir, retention)?;
        let file = open_private_log(&context.log_path)?;
        let writer = RedactingMakeWriter::new(file);
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("flea=info"));
        let subscriber = tracing_subscriber::registry().with(
            tracing_subscriber::fmt::layer()
                .json()
                .flatten_event(true)
                .with_current_span(true)
                .with_span_list(false)
                .with_ansi(false)
                .with_writer(writer)
                .with_filter(filter),
        );
        Ok(DiagnosticsSession {
            context,
            subscriber: Arc::new(subscriber),
        })
    })();

    result.map_err(|source| DiagnosticsInitError {
        context: Some(error_context),
        source,
    })
}

fn state_dir() -> io::Result<PathBuf> {
    StatePaths::discover().map(|paths| paths.root())
}

fn create_private_dir(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    set_mode(path, 0o700)
}

fn open_private_log(path: &Path) -> io::Result<File> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "active log path is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    set_mode(path, 0o600)?;
    file.lock_shared()?;
    Ok(file)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

fn roll_active_log(log_dir: &Path, max_bytes: u64) -> io::Result<()> {
    let active = log_dir.join(LOG_FILE);
    let metadata = match fs::symlink_metadata(&active) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() < max_bytes {
        return Ok(());
    }
    let file = match OpenOptions::new().read(true).write(true).open(&active) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    match file.try_lock_exclusive() {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
        Err(error) => return Err(error),
    }
    if file.metadata()?.len() < max_bytes {
        return Ok(());
    }
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let archive = log_dir.join(format!("{LOG_PREFIX}.{timestamp}.{}.jsonl", Uuid::new_v4()));
    match fs::rename(active, archive) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn enforce_retention(log_dir: &Path, policy: RetentionPolicy) -> io::Result<()> {
    let now = SystemTime::now();
    let mut files = log_files(log_dir)?;
    for file in &files {
        if file.path.file_name().is_some_and(|name| name == LOG_FILE) {
            continue;
        }
        if now
            .duration_since(file.modified)
            .is_ok_and(|age| age > policy.max_age)
        {
            remove_file_if_present(&file.path)?;
        }
    }

    files = log_files(log_dir)?;
    files.sort_by_key(|file| file.modified);
    let mut total: u64 = files.iter().map(|file| file.size).sum();
    for file in files {
        if total <= policy.max_total_bytes {
            break;
        }
        if file.path.file_name().is_some_and(|name| name == LOG_FILE) {
            continue;
        }
        remove_file_if_present(&file.path)?;
        total = total.saturating_sub(file.size);
    }
    Ok(())
}

#[derive(Debug)]
struct LogFile {
    path: PathBuf,
    modified: SystemTime,
    size: u64,
}

fn log_files(log_dir: &Path) -> io::Result<Vec<LogFile>> {
    let mut files = Vec::new();
    for entry in fs::read_dir(log_dir)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() || !is_log_file_name(name) {
            continue;
        }
        files.push(LogFile {
            path: entry.path(),
            modified: metadata.modified().unwrap_or(UNIX_EPOCH),
            size: metadata.len(),
        });
    }
    Ok(files)
}

fn is_log_file_name(name: &str) -> bool {
    if name == LOG_FILE {
        return true;
    }
    let Some(stem) = name
        .strip_prefix("flea.")
        .and_then(|name| name.strip_suffix(LOG_SUFFIX))
    else {
        return false;
    };
    let Some((timestamp, id)) = stem.split_once('.') else {
        return false;
    };
    !timestamp.is_empty()
        && timestamp.bytes().all(|byte| byte.is_ascii_digit())
        && Uuid::parse_str(id).is_ok()
}

fn remove_file_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[derive(Clone)]
struct RedactingMakeWriter {
    file: Arc<Mutex<File>>,
}

impl RedactingMakeWriter {
    fn new(file: File) -> Self {
        Self {
            file: Arc::new(Mutex::new(file)),
        }
    }
}

impl<'a> MakeWriter<'a> for RedactingMakeWriter {
    type Writer = RedactingWriter;

    fn make_writer(&'a self) -> Self::Writer {
        RedactingWriter {
            file: Arc::clone(&self.file),
            buffer: Vec::new(),
        }
    }
}

struct RedactingWriter {
    file: Arc<Mutex<File>>,
    buffer: Vec<u8>,
}

impl Write for RedactingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.buffer.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Drop for RedactingWriter {
    fn drop(&mut self) {
        let output = redact_json_line(&self.buffer);
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(&output);
            let _ = file.flush();
        }
    }
}

fn redact_json_line(line: &[u8]) -> Vec<u8> {
    match serde_json::from_slice::<Value>(line) {
        Ok(mut value) => {
            redact_value(&mut value);
            let mut output = serde_json::to_vec(&value).unwrap_or_else(|_| b"{}".to_vec());
            output.push(b'\n');
            output
        }
        Err(_) => format!(
            "{{\"message\":{}}}\n",
            json_string(&redact_text(&String::from_utf8_lossy(line)))
        )
        .into_bytes(),
    }
}

fn json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("\"{REDACTED}\""))
}

pub fn redact_value(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for (key, value) in object {
                if is_secret_key(key) {
                    *value = Value::String(REDACTED.to_owned());
                } else {
                    redact_value(value);
                }
            }
        }
        Value::Array(values) => values.iter_mut().for_each(redact_value),
        Value::String(text) => *text = redact_text(text),
        _ => {}
    }
}

fn is_secret_key(key: &str) -> bool {
    let normalized: String = key
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    normalized.ends_with("authorization")
        || normalized.ends_with("token")
        || normalized.ends_with("cookie")
        || normalized.ends_with("signature")
        || matches!(
            normalized.as_str(),
            "bearer"
                | "hmac"
                | "oauthcode"
                | "authorizationcode"
                | "spidcode"
                | "callbackurl"
                | "loginurl"
                | "authorizationurl"
                | "redirecturi"
                | "pkce"
                | "pkceverifier"
                | "pkcechallenge"
                | "codeverifier"
                | "codechallenge"
                | "rawimage"
                | "imagedata"
                | "imagebytes"
        )
}

pub fn redact_text(text: &str) -> String {
    let mut redacted = redact_callback_urls(text);
    for marker in ["Bearer ", "Basic "] {
        redacted = redact_after_marker(&redacted, marker);
    }
    for key in [
        "access_token",
        "refresh_token",
        "id_token",
        "authorization",
        "cookie",
        "set-cookie",
        "signature",
        "hmac",
        "oauth_code",
        "authorization_code",
        "code_verifier",
        "code_challenge",
        "pkce_verifier",
        "pkce_challenge",
        "callback_url",
        "login_url",
        "authorization_url",
        "redirect_uri",
    ] {
        redacted = redact_assignment(&redacted, key);
    }
    redacted = redact_data_images(&redacted);
    redacted
}

fn redact_callback_urls(text: &str) -> String {
    text.split_inclusive(char::is_whitespace)
        .map(|part| {
            let lower = part.to_ascii_lowercase();
            if lower.contains("://")
                && (lower.contains("code=")
                    || lower.contains("/callback")
                    || lower.contains("oauth"))
            {
                let whitespace = part
                    .chars()
                    .rev()
                    .take_while(|character| character.is_whitespace())
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>();
                format!("{REDACTED}{whitespace}")
            } else {
                part.to_owned()
            }
        })
        .collect()
}

fn redact_after_marker(text: &str, marker: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remainder = text;
    while let Some(index) = remainder
        .to_ascii_lowercase()
        .find(&marker.to_ascii_lowercase())
    {
        let value_start = index + marker.len();
        result.push_str(&remainder[..value_start]);
        result.push_str(REDACTED);
        let end = remainder[value_start..]
            .find(|character: char| {
                character.is_whitespace() || matches!(character, ',' | ';' | '"')
            })
            .map_or(remainder.len(), |offset| value_start + offset);
        remainder = &remainder[end..];
    }
    result.push_str(remainder);
    result
}

fn redact_assignment(text: &str, key: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remainder = text;
    let lower_key = key.to_ascii_lowercase();
    loop {
        let lower = remainder.to_ascii_lowercase();
        let Some(index) = lower.find(&lower_key) else {
            result.push_str(remainder);
            break;
        };
        result.push_str(&remainder[..index + key.len()]);
        let after_key = &remainder[index + key.len()..];
        let separator_len = after_key
            .char_indices()
            .take_while(|(_, character)| {
                character.is_whitespace() || matches!(character, '=' | ':' | '"')
            })
            .last()
            .map_or(0, |(index, character)| index + character.len_utf8());
        if separator_len == 0 {
            remainder = after_key;
            continue;
        }
        result.push_str(&after_key[..separator_len]);
        result.push_str(REDACTED);
        let value = &after_key[separator_len..];
        let end = value
            .find(|character: char| {
                character.is_whitespace() || matches!(character, '&' | ',' | ';' | '"')
            })
            .unwrap_or(value.len());
        remainder = &value[end..];
    }
    result
}

fn redact_data_images(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut remainder = text;
    while let Some(index) = remainder.to_ascii_lowercase().find("data:image/") {
        result.push_str(&remainder[..index]);
        result.push_str(REDACTED);
        let tail = &remainder[index..];
        let end = tail
            .find(|character: char| character.is_whitespace() || matches!(character, '"' | '\''))
            .unwrap_or(tail.len());
        remainder = &tail[end..];
    }
    result.push_str(remainder);
    result
}

pub fn sanitized_upstream_body(body: &[u8]) -> String {
    let mut sanitized = match serde_json::from_slice::<Value>(body) {
        Ok(mut value) => {
            redact_value(&mut value);
            serde_json::to_string(&value).unwrap_or_else(|_| REDACTED.to_owned())
        }
        Err(_) => match std::str::from_utf8(body) {
            Ok(text) => redact_text(text),
            Err(_) => "[REDACTED_BINARY_BODY]".to_owned(),
        },
    };
    if sanitized.len() > UPSTREAM_BODY_LIMIT {
        let mut boundary = UPSTREAM_BODY_LIMIT;
        while !sanitized.is_char_boundary(boundary) {
            boundary -= 1;
        }
        sanitized.truncate(boundary);
        sanitized.push_str("...[truncated]");
    }
    sanitized
}

#[derive(Debug, Default)]
pub struct WorkflowContext<'a> {
    pub workflow: &'a str,
    pub step: &'a str,
    pub draft_id: Option<&'a str>,
    pub listing_id: Option<&'a str>,
}

pub fn workflow_step(context: &WorkflowContext<'_>, status: &str) {
    info!(
        event = "workflow.step",
        workflow = context.workflow,
        step = context.step,
        draft_id = context.draft_id,
        listing_id = context.listing_id,
        status
    );
}

#[derive(Debug)]
pub struct HttpContext<'a> {
    pub method: &'a str,
    pub service: &'a str,
    pub path: &'a str,
    pub status: Option<u16>,
    pub latency: Duration,
    pub retry_count: u32,
    pub upstream_body: Option<&'a [u8]>,
}

pub fn http_event(context: &HttpContext<'_>) {
    let upstream_body = context.upstream_body.map(sanitized_upstream_body);
    info!(
        event = "http.request",
        http.method = context.method,
        http.service = context.service,
        http.path = context.path,
        http.status = context.status,
        http.latency_ms = context.latency.as_millis() as u64,
        http.retry_count = context.retry_count,
        upstream.body = upstream_body
    );
}

pub fn command_name(args: &[OsString]) -> String {
    let arguments: Vec<&str> = args
        .iter()
        .skip(1)
        .filter_map(|argument| argument.to_str())
        .collect();
    let Some(root_index) = arguments.iter().position(|argument| {
        matches!(
            *argument,
            "auth" | "category" | "draft" | "item" | "listing" | "search" | "location"
        )
    }) else {
        return "unknown".to_owned();
    };
    let root = arguments[root_index];
    let leaves: &[&str] = match root {
        "auth" => &["start", "complete", "status", "logout"],
        "category" => &["search", "list"],
        "draft" => &["create", "show", "update", "publish", "delete", "image"],
        "item" => &["show"],
        "listing" => &["list", "show", "update", "dispose", "delete"],
        "location" => &["search"],
        "search" => &[],
        _ => unreachable!("the root command is matched above"),
    };
    let leaf = arguments[root_index + 1..]
        .iter()
        .copied()
        .find(|argument| leaves.contains(argument));
    let Some(leaf) = leaf else {
        return root.to_owned();
    };
    if root == "draft" && leaf == "image" {
        let operation = arguments[root_index + 1..]
            .iter()
            .copied()
            .find(|argument| matches!(*argument, "add" | "remove"));
        return operation.map_or_else(
            || "draft image".to_owned(),
            |operation| format!("draft image {operation}"),
        );
    }
    format!("{root} {leaf}")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tempfile::tempdir;
    use tracing::info;

    use super::*;

    #[test]
    fn redacts_every_secret_class() {
        let mut value = json!({
            "authorization": "Bearer secret-auth-value",
            "session_token": "secret-token-value",
            "cookie": "secret-cookie-value",
            "hmac_signature": "secret-signature-value",
            "oauth_code": "secret-code-value",
            "callback_url": "flea://secret-callback-value",
            "login_url": "https://login.vend.fi/oauth/authorize?state=secret-login-state",
            "code_verifier": "secret-verifier-value",
            "raw_image": "secret-image-value",
            "message": "Bearer secret-text-auth access_token=secret-text-token flea://oauth/callback?code=secret-callback-code data:image/png;base64,secret-image-data"
        });
        redact_value(&mut value);
        let encoded = value.to_string();
        for secret in [
            "secret-auth-value",
            "secret-token-value",
            "secret-cookie-value",
            "secret-signature-value",
            "secret-code-value",
            "secret-callback-value",
            "secret-login-state",
            "secret-verifier-value",
            "secret-image-value",
            "secret-text-auth",
            "secret-text-token",
            "secret-callback-code",
            "secret-image-data",
        ] {
            assert!(!encoded.contains(secret), "secret leaked: {secret}");
        }
    }

    #[test]
    fn subscriber_writes_redacted_jsonl_with_correlation_fields() {
        let state = tempdir().expect("temporary state directory");
        let session = DiagnosticsSession::initialize_at(state.path(), RetentionPolicy::default())
            .expect("diagnostics should initialize");
        session.run("draft show", || {
            info!(authorization = "Bearer top-secret", "request");
            ((), 0)
        });
        let contents = fs::read_to_string(&session.context().log_path).expect("log should exist");
        assert!(!contents.contains("top-secret"));
        let events: Vec<Value> = contents
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid JSONL event"))
            .collect();
        assert!(
            events
                .iter()
                .all(|event| event["span"]["trace_id"] == session.context().trace_id)
        );
        assert!(
            events
                .iter()
                .all(|event| event["span"]["correlation_id"] == session.context().correlation_id)
        );
        assert!(events.iter().all(|event| {
            event["timestamp"].is_string()
                && event["level"].is_string()
                && event["target"].is_string()
                && event["span"]["command"] == "draft show"
        }));
        assert!(
            events
                .iter()
                .any(|event| { event["event"] == "command.started" })
        );
        assert!(events.iter().any(|event| {
            event["event"] == "command.finished"
                && event["status"] == "success"
                && event["exit_code"] == 0
                && event["duration_ms"].is_number()
        }));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&session.context().log_path)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn retention_only_removes_recognized_files_inside_logs() {
        let state = tempdir().expect("temporary state directory");
        let auth_dir = state.path().join("auth");
        let logs_dir = state.path().join("logs");
        fs::create_dir_all(&auth_dir).expect("auth directory");
        fs::create_dir_all(&logs_dir).expect("logs directory");
        fs::write(auth_dir.join("credentials.json"), "keep").expect("credential fixture");
        fs::write(logs_dir.join("unrelated.txt"), "keep").expect("unrelated fixture");
        fs::write(logs_dir.join("flea.1.old.jsonl"), "keep").expect("lookalike fixture");
        let archive = logs_dir.join("flea.1.00000000-0000-4000-8000-000000000000.jsonl");
        fs::write(&archive, vec![0; 32]).expect("log fixture");

        let policy = RetentionPolicy {
            max_age: Duration::MAX,
            max_total_bytes: 1,
            max_active_bytes: 1024,
        };
        let _session = DiagnosticsSession::initialize_at(state.path(), policy)
            .expect("diagnostics should initialize");

        assert!(auth_dir.join("credentials.json").exists());
        assert!(logs_dir.join("unrelated.txt").exists());
        assert!(logs_dir.join("flea.1.old.jsonl").exists());
        assert!(!archive.exists());
    }

    #[test]
    fn active_writer_lock_prevents_concurrent_rotation() {
        let state = tempdir().expect("temporary state directory");
        let policy = RetentionPolicy {
            max_age: Duration::MAX,
            max_total_bytes: u64::MAX,
            max_active_bytes: 1,
        };
        let first = DiagnosticsSession::initialize_at(state.path(), policy)
            .expect("first diagnostics session");
        first.run("draft show", || ((), 0));

        let second = DiagnosticsSession::initialize_at(state.path(), policy)
            .expect("second diagnostics session");
        second.run("listing show", || ((), 0));

        let archives = fs::read_dir(state.path().join("logs"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                name != LOG_FILE && is_log_file_name(&name)
            })
            .count();
        assert_eq!(archives, 0);
    }

    #[test]
    fn command_name_excludes_values_and_callback_urls() {
        let args = [
            OsString::from("flea"),
            OsString::from("auth"),
            OsString::from("complete"),
            OsString::from("flow-secret"),
            OsString::from("flea://oauth/callback?code=secret"),
        ];
        assert_eq!(command_name(&args), "auth complete");

        let item = [
            OsString::from("flea"),
            OsString::from("item"),
            OsString::from("show"),
            OsString::from("42346404"),
        ];
        assert_eq!(command_name(&item), "item show");
    }

    #[test]
    fn search_command_name_excludes_query_and_coordinates() {
        let args = [
            OsString::from("flea"),
            OsString::from("search"),
            OsString::from("private query"),
            OsString::from("--latitude"),
            OsString::from("60.1699"),
        ];
        assert_eq!(command_name(&args), "search");
    }

    #[test]
    fn upstream_body_is_bounded_and_redacted() {
        let body = format!(
            "access_token=secret {}",
            "x".repeat(UPSTREAM_BODY_LIMIT * 2)
        );
        let sanitized = sanitized_upstream_body(body.as_bytes());
        assert!(!sanitized.contains("secret"));
        assert!(sanitized.len() <= UPSTREAM_BODY_LIMIT + "...[truncated]".len());
        assert!(sanitized.ends_with("...[truncated]"));
        assert_eq!(
            sanitized_upstream_body(&[0xff, 0xd8, 0xff, 0x00]),
            "[REDACTED_BINARY_BODY]"
        );
    }
}
