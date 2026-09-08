#![cfg(unix)]

use std::{
    fs::File,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tempfile::TempDir;
use tungstenite::Message;
use wait_timeout::ChildExt;

const LIMIT: Duration = Duration::from_secs(20);

#[derive(Default)]
struct Observed {
    handshakes: usize,
    closed: usize,
    methods: Vec<String>,
    sessions: Vec<String>,
    max_sessions: usize,
    evaluations: usize,
}

struct Chrome {
    url: String,
    observed: Arc<Mutex<Observed>>,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Chrome {
    fn start(approval: bool, reject: bool, fail_evaluation: bool) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        listener.set_nonblocking(true).unwrap();
        let observed = Arc::new(Mutex::new(Observed::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let state = observed.clone();
        let shutdown = stop.clone();
        let worker = thread::spawn(move || {
            let mut peers = Vec::new();
            while !shutdown.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let state = state.clone();
                        let shutdown = shutdown.clone();
                        peers.push(thread::spawn(move || {
                            serve(
                                stream,
                                &address.to_string(),
                                state,
                                shutdown,
                                approval,
                                reject,
                                fail_evaluation,
                            );
                        }));
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept: {error}"),
                }
            }
            for peer in peers {
                peer.join().unwrap();
            }
        });
        Self {
            url: format!("http://{address}"),
            observed,
            stop,
            worker: Some(worker),
        }
    }

    fn wait_for(&self, predicate: impl Fn(&Observed) -> bool) {
        let deadline = Instant::now() + LIMIT;
        while !predicate(&self.observed.lock().unwrap()) {
            assert!(Instant::now() < deadline, "mock Chrome condition timed out");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Chrome {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn serve(
    mut stream: TcpStream,
    address: &str,
    state: Arc<Mutex<Observed>>,
    stop: Arc<AtomicBool>,
    approval: bool,
    reject: bool,
    fail_evaluation: bool,
) {
    stream
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    stream
        .set_write_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut header = [0; 8192];
    let size = loop {
        if stop.load(Ordering::Relaxed) || Instant::now() > deadline {
            return;
        }
        match stream.peek(&mut header) {
            Ok(0) => return,
            Ok(size) if header[..size].windows(4).any(|s| s == b"\r\n\r\n") => break size,
            Ok(_) => thread::sleep(Duration::from_millis(5)),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(_) => return,
        }
    };
    let header = String::from_utf8_lossy(&header[..size]);
    if !header.to_ascii_lowercase().contains("upgrade: websocket") {
        let version = header.starts_with("GET /json/version ");
        let body = json!({"webSocketDebuggerUrl":format!("ws://{address}/devtools/browser/mock")})
            .to_string();
        let (status, body) = if version && !approval {
            ("200 OK", body)
        } else {
            ("404 Not Found", String::new())
        };
        let mut consumed = vec![0; size];
        stream.read_exact(&mut consumed).unwrap();
        let _ = write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        return;
    }
    assert!(header.starts_with(if approval {
        "GET /devtools/browser "
    } else {
        "GET /devtools/browser/mock "
    }));
    state.lock().unwrap().handshakes += 1;
    if reject {
        let _ = stream
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return;
    }
    let Ok(mut socket) = tungstenite::accept(stream) else {
        return;
    };
    while !stop.load(Ordering::Relaxed) {
        let message = match socket.read() {
            Ok(Message::Text(text)) => text,
            Ok(Message::Close(_)) => {
                let _ = socket.flush();
                break;
            }
            Ok(_) => continue,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => break,
        };
        let call: Value = serde_json::from_str(&message).unwrap();
        let method = call["method"].as_str().unwrap();
        let mut observed = state.lock().unwrap();
        observed.methods.push(method.to_owned());
        let result = match method {
            "Target.getTargets" => {
                json!({"targetInfos":[{"targetId":"vinted", "type":"page", "url":"https://www.vinted.fi/items/new"}]})
            }
            "Target.attachToTarget" => {
                let session = format!("session-{}", observed.methods.len());
                observed.sessions.push(session.clone());
                observed.max_sessions = observed.max_sessions.max(observed.sessions.len());
                json!({"sessionId":session})
            }
            "Target.detachFromTarget" => {
                let session = call["params"]["sessionId"].as_str().unwrap();
                assert!(observed.sessions.iter().any(|s| s == session));
                observed.sessions.retain(|s| s != session);
                json!({})
            }
            "Runtime.evaluate" => {
                let expression = call["params"]["expression"].as_str().unwrap();
                if expression.contains("document.readyState") {
                    json!({"result":{"value":true}})
                } else {
                    observed.evaluations += 1;
                    if fail_evaluation {
                        break;
                    }
                    // Keep the lease occupied long enough to expose overlapping clients.
                    drop(observed);
                    thread::sleep(Duration::from_millis(100));
                    observed = state.lock().unwrap();
                    json!({"result":{"value":{"status":200,"authenticated":true}}})
                }
            }
            "Browser.getVersion" => json!({"product":"Chrome/mock", "protocolVersion":"1.3"}),
            other => panic!("unexpected CDP method: {other}"),
        };
        drop(observed);
        let mut reply = json!({"id":call["id"],"result":result});
        if let Some(session) = call.get("sessionId") {
            reply["sessionId"] = session.clone();
        }
        if socket
            .send(Message::Text(reply.to_string().into()))
            .is_err()
        {
            break;
        }
    }
    state.lock().unwrap().closed += 1;
}

struct Invocation {
    child: Child,
    stdout: File,
    stderr: File,
}

impl Invocation {
    fn finish(mut self) -> (bool, String) {
        let status = self
            .child
            .wait_timeout(LIMIT)
            .unwrap()
            .expect("CLI timed out");
        let mut output = String::new();
        // Separate file handles share offsets, so reopen through the stored file via seek.
        use std::io::{Seek, SeekFrom};
        self.stdout.seek(SeekFrom::Start(0)).unwrap();
        self.stdout.read_to_string(&mut output).unwrap();
        if !status.success() {
            self.stderr.seek(SeekFrom::Start(0)).unwrap();
            let mut diagnostic = String::new();
            self.stderr.read_to_string(&mut diagnostic).unwrap();
            eprintln!("CLI stderr: {diagnostic}");
        }
        (status.success(), output)
    }
}

impl Drop for Invocation {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Session {
    home: TempDir,
    state: TempDir,
    url: String,
}

impl Session {
    fn new(chrome: &Chrome) -> Self {
        Self {
            home: tempfile::tempdir().unwrap(),
            state: tempfile::tempdir().unwrap(),
            url: chrome.url.clone(),
        }
    }

    fn spawn(&self, args: &[&str]) -> Invocation {
        let stdout = tempfile::tempfile().unwrap();
        let stderr = tempfile::tempfile().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_flea"))
            .args(["--format", "json", "--browser-url", &self.url])
            .args(args)
            .env("HOME", self.home.path())
            .env("XDG_STATE_HOME", self.state.path())
            .stdin(Stdio::null())
            .stdout(stdout.try_clone().unwrap())
            .stderr(stderr.try_clone().unwrap())
            .spawn()
            .unwrap();
        Invocation {
            child,
            stdout,
            stderr,
        }
    }

    fn status(&self) -> Invocation {
        self.spawn(&["vinted", "auth", "status", "--browser"])
    }

    fn disconnect(&self, expected: bool) {
        let (ok, output) = self.spawn(&["browser", "disconnect"]).finish();
        assert!(ok, "{output}");
        let value: Value = serde_json::from_str(&output).unwrap();
        assert_eq!(value["data"]["disconnected"], expected, "{output}");
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Cleanup must also run after assertions fail, without a second panic.
        let _ = std::panic::catch_unwind(|| self.spawn(&["browser", "disconnect"]).finish());
    }
}

fn assert_authenticated(invocation: Invocation) {
    let (ok, output) = invocation.finish();
    assert!(ok, "{output}");
    let value: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(value["data"]["authenticated"], true, "{output}");
}

#[test]
fn separate_commands_share_approval_and_disconnect_only_the_helper() {
    let chrome = Chrome::start(true, false, false);
    let session = Session::new(&chrome);
    session.disconnect(false);
    assert_eq!(chrome.observed.lock().unwrap().handshakes, 0);
    for _ in 0..2 {
        assert_authenticated(session.status());
        chrome.wait_for(|s| s.sessions.is_empty());
    }
    assert_eq!(chrome.observed.lock().unwrap().handshakes, 1);
    session.disconnect(true);
    chrome.wait_for(|s| s.closed == 1);
    session.disconnect(false);
    assert_authenticated(session.status());
    assert_eq!(chrome.observed.lock().unwrap().handshakes, 2);
    session.disconnect(true);
    chrome.wait_for(|s| s.closed == 2);
    assert!(
        !chrome
            .observed
            .lock()
            .unwrap()
            .methods
            .iter()
            .any(|m| m == "Browser.close")
    );
}

#[test]
fn concurrent_startup_uses_one_connection_and_serializes_leases() {
    let chrome = Chrome::start(false, false, false);
    let session = Session::new(&chrome);
    let first = session.status();
    let second = session.status();
    assert_authenticated(first);
    assert_authenticated(second);
    chrome.wait_for(|s| s.sessions.is_empty());
    let observed = chrome.observed.lock().unwrap();
    assert_eq!(observed.handshakes, 1);
    assert_eq!(observed.evaluations, 2);
    assert_eq!(observed.max_sessions, 1);
}

#[test]
fn lost_evaluation_response_is_not_replayed() {
    let chrome = Chrome::start(false, false, true);
    let session = Session::new(&chrome);
    let (ok, output) = session.status().finish();
    assert!(!ok, "{output}");
    let observed = chrome.observed.lock().unwrap();
    assert_eq!(observed.evaluations, 1);
    assert_eq!(observed.handshakes, 1);
}

#[test]
fn rejected_approval_is_not_retried_within_a_command() {
    let chrome = Chrome::start(true, true, false);
    let session = Session::new(&chrome);
    let (ok, output) = session.status().finish();
    assert!(!ok, "{output}");
    assert_eq!(chrome.observed.lock().unwrap().handshakes, 1);
    assert_eq!(chrome.observed.lock().unwrap().evaluations, 0);
}

#[test]
fn closing_chrome_ends_the_idle_helper() {
    let mut chrome = Chrome::start(false, false, false);
    let session = Session::new(&chrome);
    assert_authenticated(session.status());
    chrome.wait_for(|observed| observed.sessions.is_empty());
    let sockets: Vec<_> = std::fs::read_dir(session.state.path().join("flea/browser"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "sock")
        })
        .collect();
    assert_eq!(sockets.len(), 1);
    chrome.stop.store(true, Ordering::Relaxed);
    chrome.worker.take().unwrap().join().unwrap();
    let deadline = Instant::now() + LIMIT;
    while sockets[0].exists() {
        assert!(
            Instant::now() < deadline,
            "helper did not detect Chrome closing"
        );
        thread::sleep(Duration::from_millis(25));
    }
    session.disconnect(false);
}
