//! A private, per-endpoint helper retains Chrome's approved connection.

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use url::Url;

use super::{AppError, COMMAND_TIMEOUT, DirectCdp, MAX_MESSAGE_BYTES, connect_remote};
use crate::storage::{atomic_file::secure_directory, discover_state_root};

const PROTOCOL: u32 = 1;
const QUEUE_TIMEOUT: Duration = Duration::from_secs(300);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const HEARTBEAT_TIMEOUT: Duration = Duration::from_secs(3);
const SPAWN_TIMEOUT: Duration = Duration::from_secs(10);

struct Paths {
    socket: PathBuf,
    startup: PathBuf,
    owner: PathBuf,
}

impl Paths {
    fn discover(endpoint: &Url) -> Result<Self, AppError> {
        Self::in_root(&discover_state_root().map_err(local_error)?, endpoint)
    }

    fn in_root(root: &Path, endpoint: &Url) -> Result<Self, AppError> {
        let directory = root.join("browser");
        secure_directory(&directory).map_err(local_error)?;
        let key = format!(
            "{:x}",
            Sha256::digest(endpoint.as_str().trim_end_matches('/').as_bytes())
        );
        let base = directory.join(&key[..24]);
        Ok(Self {
            socket: base.with_extension("sock"),
            startup: base.with_extension("start"),
            owner: base.with_extension("lock"),
        })
    }
}

fn lock_file(path: &Path) -> Result<File, AppError> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(local_error)
}

fn startup_lock(paths: &Paths) -> Result<File, AppError> {
    let lock = lock_file(&paths.startup)?;
    let deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        match FileExt::try_lock_exclusive(&lock) {
            Ok(()) => return Ok(lock),
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
            {
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(local_error(error)),
        }
    }
}

fn absent(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Request {
    Acquire {
        protocol: u32,
    },
    Call {
        session: Option<String>,
        method: String,
        params: Value,
        timeout_ms: u64,
    },
    Release,
    Disconnect {
        protocol: u32,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Reply {
    Ready,
    Result { value: Value },
    Error { code: String, message: String },
    Disconnected,
}

impl Reply {
    fn error(error: &AppError) -> Self {
        Self::Error {
            code: error.code.clone(),
            message: error.message.clone(),
        }
    }

    fn into_error(self) -> AppError {
        match self {
            Self::Error { code, message } => AppError::upstream(code, message),
            _ => protocol_error(),
        }
    }
}

struct Wire {
    stream: UnixStream,
    deadline: Instant,
}

impl Wire {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            deadline: Instant::now() + QUEUE_TIMEOUT,
        }
    }

    fn remaining(&self) -> io::Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|duration| !duration.is_zero())
            .ok_or_else(|| io::Error::from(io::ErrorKind::TimedOut))
    }

    fn send(&mut self, value: &impl Serialize) -> Result<(), AppError> {
        let bytes = serde_json::to_vec(value).map_err(|_| protocol_error())?;
        if bytes.len() > MAX_MESSAGE_BYTES {
            return Err(protocol_error());
        }
        self.write_all(&(bytes.len() as u32).to_be_bytes())
            .map_err(local_error)?;
        self.write_all(&bytes).map_err(local_error)
    }

    fn receive<T: DeserializeOwned>(&mut self) -> Result<T, AppError> {
        let mut header = [0; 4];
        self.read_exact(&mut header).map_err(local_error)?;
        let length = u32::from_be_bytes(header) as usize;
        if length > MAX_MESSAGE_BYTES {
            return Err(protocol_error());
        }
        let mut body = vec![0; length];
        self.read_exact(&mut body).map_err(local_error)?;
        serde_json::from_slice(&body).map_err(|_| protocol_error())
    }
}

impl Read for Wire {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if let Err(error) = self.stream.set_read_timeout(Some(self.remaining()?)) {
            // macOS rejects SO_RCVTIMEO after the peer closes, even with unread data.
            // Reading a closed socket drains those bytes and then returns EOF.
            if !cfg!(target_os = "macos") || error.raw_os_error() != Some(libc::EINVAL) {
                return Err(error);
            }
        }
        self.stream.read(buffer)
    }
}

impl Write for Wire {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.stream.set_write_timeout(Some(self.remaining()?))?;
        self.stream.write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.stream.flush()
    }
}

pub(super) struct Client {
    wire: Wire,
    healthy: bool,
}

impl Client {
    pub(super) fn connect(endpoint: &Url) -> Result<Self, AppError> {
        let paths = Paths::discover(endpoint)?;
        let lock = startup_lock(&paths)?;
        let stream = match UnixStream::connect(&paths.socket) {
            Ok(stream) => stream,
            Err(error) if absent(&error) => {
                let mut child = Command::new(std::env::current_exe().map_err(local_error)?)
                    .arg("__browser-session")
                    .arg("--browser-url")
                    .arg(endpoint.as_str())
                    .current_dir("/")
                    .process_group(0)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(local_error)?;
                let deadline = Instant::now() + SPAWN_TIMEOUT;
                loop {
                    match UnixStream::connect(&paths.socket) {
                        Ok(stream) => break stream,
                        Err(error) if absent(&error) && Instant::now() < deadline => {
                            if child.try_wait().map_err(local_error)?.is_some() {
                                return Err(helper_error("the browser helper could not start"));
                            }
                            thread::sleep(Duration::from_millis(25));
                        }
                        Err(error) => {
                            let _ = child.kill();
                            let _ = child.wait();
                            return Err(local_error(error));
                        }
                    }
                }
            }
            Err(error) => return Err(local_error(error)),
        };
        // A queued command must not hold the helper-startup lock.
        drop(lock);
        Self::acquire(stream)
    }

    fn acquire(stream: UnixStream) -> Result<Self, AppError> {
        let mut wire = Wire::new(stream);
        wire.send(&Request::Acquire { protocol: PROTOCOL })?;
        match wire.receive::<Reply>()? {
            Reply::Ready => Ok(Self {
                wire,
                healthy: true,
            }),
            reply => Err(reply.into_error()),
        }
    }

    pub(super) fn call_until(
        &mut self,
        session: Option<&str>,
        method: &str,
        params: Value,
        deadline: Instant,
    ) -> Result<Value, AppError> {
        if !self.healthy {
            return Err(helper_error(
                "the browser session failed; inspect remote state before retrying",
            ));
        }
        self.wire.deadline = deadline;
        let timeout_ms = deadline
            .saturating_duration_since(Instant::now())
            .min(COMMAND_TIMEOUT)
            .as_millis() as u64;
        let result = (|| {
            self.wire.send(&Request::Call {
                session: session.map(str::to_owned),
                method: method.to_owned(),
                params,
                timeout_ms,
            })?;
            match self.wire.receive::<Reply>()? {
                Reply::Result { value } => Ok(value),
                reply => Err(reply.into_error()),
            }
        })();
        if result
            .as_ref()
            .is_err_and(|error| error.code != "vinted.web_browser_command_rejected")
        {
            self.healthy = false;
            let _ = self.wire.stream.shutdown(Shutdown::Both);
        }
        result
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        if self.healthy {
            self.wire.deadline = Instant::now() + Duration::from_secs(1);
            let _ = self.wire.send(&Request::Release);
        }
        let _ = self.wire.stream.shutdown(Shutdown::Both);
    }
}

pub(crate) fn disconnect(endpoint: &Url) -> Result<bool, AppError> {
    let paths = Paths::discover(endpoint)?;
    let lock = startup_lock(&paths)?;
    let stream = match UnixStream::connect(&paths.socket) {
        Ok(stream) => stream,
        Err(error) if absent(&error) => return Ok(false),
        Err(error) => return Err(local_error(error)),
    };
    let mut wire = Wire::new(stream);
    wire.send(&Request::Disconnect { protocol: PROTOCOL })?;
    match wire.receive::<Reply>()? {
        Reply::Disconnected => {
            // Wait for the owner to close Chrome and remove its socket before a new startup.
            let owner = lock_file(&paths.owner)?;
            let deadline = Instant::now() + SPAWN_TIMEOUT;
            loop {
                match FileExt::try_lock_exclusive(&owner) {
                    Ok(()) => break,
                    Err(error)
                        if error.kind() == io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        thread::sleep(Duration::from_millis(25))
                    }
                    Err(error) => return Err(local_error(error)),
                }
            }
            drop(lock);
            Ok(true)
        }
        reply => Err(reply.into_error()),
    }
}

struct Binding {
    listener: UnixListener,
    paths: Paths,
    _owner: File,
}

impl Binding {
    fn bind(paths: Paths) -> Result<Self, AppError> {
        let owner = lock_file(&paths.owner)?;
        FileExt::try_lock_exclusive(&owner).map_err(local_error)?;
        match fs::remove_file(&paths.socket) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(local_error(error)),
        }
        let listener = UnixListener::bind(&paths.socket).map_err(local_error)?;
        fs::set_permissions(&paths.socket, fs::Permissions::from_mode(0o600))
            .map_err(local_error)?;
        listener.set_nonblocking(true).map_err(local_error)?;
        Ok(Self {
            listener,
            paths,
            _owner: owner,
        })
    }
}

impl Drop for Binding {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.paths.socket);
    }
}

pub(crate) fn serve(endpoint: &Url) -> Result<(), AppError> {
    let binding = Binding::bind(Paths::discover(endpoint)?)?;
    serve_bound(binding, endpoint)
}

fn serve_bound(binding: Binding, endpoint: &Url) -> Result<(), AppError> {
    let mut cdp: Option<DirectCdp> = None;
    let mut heartbeat = Instant::now();
    let startup_deadline = Instant::now() + SPAWN_TIMEOUT;
    loop {
        match binding.listener.accept() {
            Ok((stream, _)) => {
                // Accepted sockets inherit nonblocking mode on macOS.
                stream.set_nonblocking(false).map_err(local_error)?;
                let mut wire = Wire::new(stream);
                wire.deadline = Instant::now() + Duration::from_secs(5);
                let request = match wire.receive::<Request>() {
                    Ok(request) => request,
                    Err(_) => continue,
                };
                wire.deadline = Instant::now() + QUEUE_TIMEOUT;
                match request {
                    Request::Disconnect { protocol: PROTOCOL } => {
                        cdp.take();
                        wire.send(&Reply::Disconnected)?;
                        return Ok(());
                    }
                    Request::Acquire { protocol: PROTOCOL } => {
                        if cdp.is_none() {
                            match connect_remote(endpoint) {
                                Ok(connection) => cdp = Some(connection),
                                Err(error) => {
                                    let _ = wire.send(&Reply::error(&error));
                                    return Err(error);
                                }
                            }
                        }
                        let connection = cdp.as_mut().expect("connection initialized");
                        if let Err(error) = probe(connection) {
                            let _ = wire.send(&Reply::error(&error));
                            return Err(error);
                        }
                        if wire.send(&Reply::Ready).is_ok() {
                            serve_client(&mut wire, connection)?;
                        }
                        heartbeat = Instant::now();
                    }
                    _ => {
                        let _ = wire.send(&Reply::error(&protocol_error()));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if heartbeat.elapsed() >= HEARTBEAT_INTERVAL {
                    if let Some(connection) = cdp.as_mut() {
                        probe(connection)?;
                    }
                    heartbeat = Instant::now();
                }
                if cdp.is_none() && Instant::now() >= startup_deadline {
                    return Ok(());
                }
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(local_error(error)),
        }
    }
}

fn probe(cdp: &mut DirectCdp) -> Result<(), AppError> {
    cdp.call_until(
        None,
        "Browser.getVersion",
        json!({}),
        Instant::now() + HEARTBEAT_TIMEOUT,
    )
    .map(|_| ())
}

fn serve_client(wire: &mut Wire, cdp: &mut DirectCdp) -> Result<(), AppError> {
    let mut sessions = Vec::new();
    loop {
        wire.deadline = Instant::now() + QUEUE_TIMEOUT;
        let request = match wire.receive::<Request>() {
            Ok(request) => request,
            Err(_) => break,
        };
        let Request::Call {
            session,
            method,
            params,
            timeout_ms,
        } = request
        else {
            break;
        };
        let timeout = Duration::from_millis(timeout_ms).min(COMMAND_TIMEOUT);
        let result = cdp.call_until(
            session.as_deref(),
            &method,
            params,
            Instant::now() + timeout,
        );
        let reply = match result {
            Ok(value) => {
                if method == "Target.attachToTarget"
                    && let Some(session) = value.get("sessionId").and_then(Value::as_str)
                {
                    sessions.push(session.to_owned());
                }
                Reply::Result { value }
            }
            Err(error) => {
                let _ = wire.send(&Reply::error(&error));
                if error.code != "vinted.web_browser_command_rejected" {
                    // Never reconnect or replay a request whose outcome may be uncertain.
                    return Err(error);
                }
                continue;
            }
        };
        if wire.send(&reply).is_err() {
            break;
        }
    }
    for session in sessions {
        if let Err(error) = cdp.call_until(
            None,
            "Target.detachFromTarget",
            json!({ "sessionId": session }),
            Instant::now() + HEARTBEAT_TIMEOUT,
        ) && error.code != "vinted.web_browser_command_rejected"
        {
            return Err(error);
        }
    }
    Ok(())
}

fn helper_error(message: &'static str) -> AppError {
    AppError::upstream("vinted.web_browser_session_failed", message)
}

fn local_error(error: io::Error) -> AppError {
    helper_error(
        "the local browser session failed or timed out; inspect remote state before retrying",
    )
    .with_source(error)
}

fn protocol_error() -> AppError {
    helper_error(
        "the browser session protocol is invalid or incompatible; disconnect the session before retrying",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn wire_drains_a_complete_frame_after_the_peer_closes() {
        let (sender, receiver) = UnixStream::pair().unwrap();
        let mut sender = Wire::new(sender);
        sender.send(&Reply::Disconnected).unwrap();
        drop(sender);
        assert!(matches!(
            Wire::new(receiver).receive::<Reply>().unwrap(),
            Reply::Disconnected
        ));
    }

    #[test]
    fn wire_bounds_frames_and_trickling_reads() {
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        sender
            .write_all(&((MAX_MESSAGE_BYTES + 1) as u32).to_be_bytes())
            .unwrap();
        assert!(Wire::new(receiver).receive::<Reply>().is_err());

        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let (release, wait) = mpsc::channel();
        let peer = thread::spawn(move || {
            sender.write_all(&2_u32.to_be_bytes()).unwrap();
            sender.write_all(b"{").unwrap();
            let _ = wait.recv_timeout(Duration::from_secs(2));
        });
        let mut wire = Wire::new(receiver);
        wire.deadline = Instant::now() + Duration::from_millis(50);
        assert!(wire.receive::<Reply>().is_err());
        release.send(()).unwrap();
        peer.join().unwrap();
    }

    #[test]
    fn binding_is_private_endpoint_scoped_and_exclusive() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = Url::parse("http://localhost:9222").unwrap();
        let paths = Paths::in_root(root.path(), &endpoint).unwrap();
        let socket = paths.socket.clone();
        let directory = socket.parent().unwrap();
        assert_eq!(
            fs::metadata(directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let binding = Binding::bind(paths).unwrap();
        assert_eq!(
            fs::metadata(&socket).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert!(Binding::bind(Paths::in_root(root.path(), &endpoint).unwrap()).is_err());
        assert!(socket.exists());
        let other = Url::parse("http://localhost:9223").unwrap();
        assert_ne!(socket, Paths::in_root(root.path(), &other).unwrap().socket);
        assert_eq!(
            socket,
            Paths::in_root(root.path(), &Url::parse("http://localhost:9222/").unwrap())
                .unwrap()
                .socket
        );
        drop(binding);
        assert!(!socket.exists());
    }

    #[test]
    fn binding_recovers_a_socket_left_by_an_exited_helper() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = Url::parse("http://localhost:9222").unwrap();
        let paths = Paths::in_root(root.path(), &endpoint).unwrap();
        drop(UnixListener::bind(&paths.socket).unwrap());
        let binding = Binding::bind(paths).unwrap();
        assert!(UnixStream::connect(&binding.paths.socket).is_ok());
    }

    #[test]
    fn incompatible_clients_do_not_connect_to_chrome() {
        let root = tempfile::tempdir().unwrap();
        let endpoint = Url::parse("http://127.0.0.1:1").unwrap();
        let paths = Paths::in_root(root.path(), &endpoint).unwrap();
        let socket = paths.socket.clone();
        let binding = Binding::bind(paths).unwrap();
        let helper = thread::spawn(move || serve_bound(binding, &endpoint));
        let mut wire = Wire::new(UnixStream::connect(&socket).unwrap());
        wire.send(&Request::Acquire {
            protocol: PROTOCOL + 1,
        })
        .unwrap();
        assert!(matches!(
            wire.receive::<Reply>().unwrap(),
            Reply::Error { .. }
        ));
        let mut wire = Wire::new(UnixStream::connect(&socket).unwrap());
        wire.send(&Request::Disconnect { protocol: PROTOCOL })
            .unwrap();
        assert!(matches!(
            wire.receive::<Reply>().unwrap(),
            Reply::Disconnected
        ));
        helper.join().unwrap().unwrap();
        assert!(!socket.exists());
    }

    #[test]
    fn lost_mutation_response_ends_the_session_without_replay() {
        let (page, peer) = crate::browser::tests::peer(|socket| {
            let call = crate::browser::tests::request(socket, "Runtime.evaluate");
            assert_eq!(call["params"]["expression"], "fixtureMutation()");
            // Closing after receipt models an operation with an unknown remote outcome.
        });
        let super::super::Cdp::Direct(mut cdp) = page.cdp else {
            panic!("direct fixture expected")
        };
        let (sender, receiver) = UnixStream::pair().unwrap();
        let helper = thread::spawn(move || serve_client(&mut Wire::new(receiver), &mut cdp));
        let mut client = Client {
            wire: Wire::new(sender),
            healthy: true,
        };
        let error = client
            .call_until(
                Some("page-session"),
                "Runtime.evaluate",
                json!({ "expression": "fixtureMutation()" }),
                Instant::now() + COMMAND_TIMEOUT,
            )
            .unwrap_err();
        assert!(!error.safe_to_retry);
        assert!(!client.healthy);
        assert!(
            client
                .call_until(
                    None,
                    "Runtime.evaluate",
                    json!({}),
                    Instant::now() + COMMAND_TIMEOUT
                )
                .is_err()
        );
        assert!(helper.join().unwrap().is_err());
        peer.join().unwrap();
    }
}
