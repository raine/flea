use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::{
            fs::{FileTypeExt, MetadataExt, PermissionsExt},
            net::{UnixListener, UnixStream},
        },
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde_json::{Value, json};

use super::{failure, install, protocol};
use crate::AppError;

const TIMEOUT: Duration = Duration::from_secs(60);

pub(super) fn request(command: Value) -> Result<Value, AppError> {
    let root = install::root()?;
    request_at(&root, command, TIMEOUT)
}

fn request_at(root: &Path, command: Value, timeout: Duration) -> Result<Value, AppError> {
    install::check_private_dir(root).map_err(|_| disconnected())?;
    let deadline = Instant::now() + timeout;
    let id = uuid::Uuid::new_v4().to_string();
    let bytes = protocol::encode(&json!({"id": id, "command": command})).map_err(|_| {
        AppError::validation(
            "extension.request_too_large",
            "The extension request exceeds the 16 MiB message limit.",
        )
    })?;
    let _lock = acquire_client_lock(root, deadline).map_err(|_| {
        failure("Timed out waiting for another extension request to finish. No request was sent.")
    })?;
    let path = root.join("bridge.sock");
    check_socket(&path).map_err(|_| disconnected())?;
    let mut stream = UnixStream::connect(&path).map_err(|_| disconnected())?;
    stream.set_nonblocking(true).map_err(|_| disconnected())?;
    let mut connection = DeadlineIo {
        inner: &mut stream,
        deadline,
    };
    protocol::write_frame(&mut connection, &bytes).map_err(|_| uncertain())?;
    let reply =
        protocol::read_frame(&mut connection, protocol::MAX_MESSAGE).map_err(|_| uncertain())?;
    let result = protocol::validate_reply(&reply, &id).map_err(|_| uncertain())?;
    if let Some(error) = reply.get("error") {
        if error == "tab_unavailable" {
            return Err(AppError::upstream(
                "extension.tab_unavailable",
                "Open exactly one Vinted tab in Chrome, reload it, and sign in before trying again.",
            ));
        }
        // Browser-provided errors can contain response bodies or credentials.
        return Err(failure(
            "The browser extension could not complete the request. Check the signed-in Vinted tab before repeating the operation.",
        ));
    }
    Ok(result.clone())
}

fn disconnected() -> AppError {
    AppError::upstream(
        "extension.disconnected",
        "The Flea extension is not connected. Enable it in Chrome and open or reload a Vinted tab. If needed, rerun flea extension setup.",
    )
}

fn uncertain() -> AppError {
    AppError::upstream(
        "mutation.uncertain",
        "The browser extension connection failed or timed out. The operation may have completed. Inspect Vinted before repeating it.",
    )
}

fn acquire_client_lock(root: &Path, deadline: Instant) -> io::Result<File> {
    let file = install::lock_file(&root.join("request.lock"))?;
    loop {
        match file.try_lock_exclusive() {
            Ok(()) => return Ok(file),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(timed_out)?;
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
            Err(error) => return Err(error),
        }
    }
}

pub(super) fn serve(origin: &str) -> Result<(), AppError> {
    if origin != install::origin()? {
        return Err(AppError::authentication(
            "extension.invalid_origin",
            "The native host caller is not the Flea extension.",
        ));
    }
    let root = install::root()?;
    install::check_private_dir(&root)
        .map_err(|_| failure("Run flea extension setup before starting the native host."))?;
    let listener = LocalSocket::bind(&root).map_err(|_| failure("Cannot start the private extension socket. Another Chrome profile may already be connected."))?;
    let mut input =
        duplicate_fd(libc::STDIN_FILENO).map_err(|_| failure("Cannot open native input."))?;
    let mut output =
        duplicate_fd(libc::STDOUT_FILENO).map_err(|_| failure("Cannot open native output."))?;
    run(&listener, &mut input, &mut output, TIMEOUT)
        .map_err(|_| failure("The native extension connection closed or timed out."))
}

fn run<R: Read + AsRawFd, W: Write + AsRawFd>(
    socket: &LocalSocket,
    input: &mut R,
    output: &mut W,
    timeout: Duration,
) -> io::Result<()> {
    loop {
        let mut fds = [
            poll_descriptor(socket.listener.as_raw_fd(), libc::POLLIN),
            poll_descriptor(input.as_raw_fd(), libc::POLLIN),
        ];
        poll(&mut fds, None)?;
        if fds[1].revents != 0 {
            let mut byte = [0];
            return match input.read(&mut byte) {
                Ok(0) => Ok(()),
                _ => Err(protocol::invalid()),
            };
        }
        if fds[0].revents == 0 {
            continue;
        }
        let (mut client, _) = match socket.listener.accept() {
            Ok(connection) => connection,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => continue,
            Err(error) => return Err(error),
        };
        client.set_nonblocking(true)?;
        let deadline = Instant::now() + timeout;
        let mut connection = DeadlineIo {
            inner: &mut client,
            deadline,
        };
        let request = match protocol::read_frame(&mut connection, protocol::MAX_MESSAGE) {
            Ok(request)
                if request.get("command").is_some() && protocol::request_id(&request).is_ok() =>
            {
                request
            }
            _ => continue,
        };
        let id = protocol::request_id(&request)?;
        let mut native_output = DeadlineIo {
            inner: &mut *output,
            deadline,
        };
        // Any failure after forwarding ends this host so a late reply cannot
        // be mistaken for the result of a subsequent operation.
        protocol::send_native(&mut native_output, &request)?;
        let mut native_input = DeadlineIo {
            inner: &mut *input,
            deadline,
        };
        let reply = protocol::receive_native(&mut native_input, id)?;
        let bytes = protocol::encode(&reply)?;
        // A departing local client does not invalidate a completed native reply.
        let _ = protocol::write_frame(&mut connection, &bytes);
    }
}

fn duplicate_fd(fd: RawFd) -> io::Result<File> {
    // F_DUPFD_CLOEXEC returns a new owned descriptor without taking ownership
    // of the standard stream or changing its file status flags.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 0) };
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }
    // The descriptor was just allocated and is owned exclusively by this File.
    Ok(unsafe { File::from_raw_fd(duplicate) })
}

fn check_socket(path: &Path) -> io::Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || !install::owned(&metadata)
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bridge socket is not private.",
        ));
    }
    Ok(metadata)
}

struct LocalSocket {
    listener: UnixListener,
    path: PathBuf,
    device: u64,
    inode: u64,
    _lock: File,
}

impl LocalSocket {
    fn bind(root: &Path) -> io::Result<Self> {
        install::check_private_dir(root)?;
        let lock = install::lock_file(&root.join("host.lock"))?;
        lock.try_lock_exclusive()?;
        let path = root.join("bridge.sock");
        match check_socket(&path) {
            Ok(_) => fs::remove_file(&path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        let metadata = check_socket(&path)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            _lock: lock,
        })
    }
}

impl Drop for LocalSocket {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.dev() == self.device
            && metadata.ino() == self.inode
        {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn timed_out() -> io::Error {
    io::Error::new(io::ErrorKind::TimedOut, "Extension request timed out.")
}

fn poll_descriptor(fd: RawFd, events: i16) -> libc::pollfd {
    libc::pollfd {
        fd,
        events,
        revents: 0,
    }
}

fn poll(fds: &mut [libc::pollfd], deadline: Option<Instant>) -> io::Result<()> {
    loop {
        let milliseconds = match deadline {
            Some(deadline) => {
                let remaining = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(timed_out)?;
                remaining.as_millis().max(1).min(i32::MAX as u128) as i32
            }
            None => -1,
        };
        // The pointer addresses the entire initialized pollfd slice.
        let result =
            unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, milliseconds) };
        if result > 0 {
            return Ok(());
        }
        if result == 0 {
            return Err(timed_out());
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

struct DeadlineIo<'a, T> {
    inner: &'a mut T,
    deadline: Instant,
}

impl<T: Read + AsRawFd> Read for DeadlineIo<'_, T> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            poll(
                &mut [poll_descriptor(self.inner.as_raw_fd(), libc::POLLIN)],
                Some(self.deadline),
            )?;
            match self.inner.read(bytes) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
}

impl<T: Write + AsRawFd> Write for DeadlineIo<'_, T> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        loop {
            poll(
                &mut [poll_descriptor(self.inner.as_raw_fd(), libc::POLLOUT)],
                Some(self.deadline),
            )?;
            // A native stdout pipe can accept PIPE_BUF bytes after POLLOUT.
            let count = bytes.len().min(512);
            match self.inner.write(&bytes[..count]) {
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::Interrupted | io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue;
                }
                result => return result,
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory() -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
        directory
    }

    #[test]
    fn socket_is_private_singleton_and_cleaned_on_drop() {
        let directory = directory();
        let socket = LocalSocket::bind(directory.path()).unwrap();
        check_socket(&socket.path).unwrap();
        assert!(LocalSocket::bind(directory.path()).is_err());
        assert!(socket.path.exists());
        let path = socket.path.clone();
        drop(socket);
        assert!(!path.exists());
        LocalSocket::bind(directory.path()).unwrap();
    }

    #[test]
    fn refuses_to_replace_non_socket_or_permissive_socket() {
        let directory = directory();
        let path = directory.path().join("bridge.sock");
        fs::write(&path, "keep").unwrap();
        assert!(LocalSocket::bind(directory.path()).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"keep");
        fs::remove_file(&path).unwrap();
        let _socket = UnixListener::bind(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(LocalSocket::bind(directory.path()).is_err());
    }

    #[test]
    fn partial_frame_has_a_deadline() {
        let (mut server, mut client) = UnixStream::pair().unwrap();
        server.set_nonblocking(true).unwrap();
        client.write_all(&[10, 0, 0, 0, b'{']).unwrap();
        let mut input = DeadlineIo {
            inner: &mut server,
            deadline: Instant::now() + Duration::from_millis(20),
        };
        assert_eq!(
            protocol::read_frame(&mut input, protocol::MAX_MESSAGE)
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
    }

    #[test]
    fn request_lock_times_out_without_sending() {
        let directory = directory();
        let _lock = acquire_client_lock(directory.path(), Instant::now() + TIMEOUT).unwrap();
        let error = request_at(directory.path(), json!({}), Duration::from_millis(20)).unwrap_err();
        assert!(!error.safe_to_retry);
        assert!(error.message.contains("No request was sent"));
    }

    #[test]
    fn forwards_large_requests_and_replies_and_stops_on_native_eof() {
        let directory = directory();
        let socket = LocalSocket::bind(directory.path()).unwrap();
        let (mut native_host, mut extension) = UnixStream::pair().unwrap();
        native_host.set_nonblocking(true).unwrap();
        let mut native_output = native_host.try_clone().unwrap();
        let server = std::thread::spawn(move || {
            run(
                &socket,
                &mut native_host,
                &mut native_output,
                Duration::from_secs(5),
            )
        });
        let browser = std::thread::spawn(move || {
            // The native request envelope has a command, not a result.
            let mut assembled = String::new();
            let id;
            loop {
                let frame = protocol::read_frame(&mut extension, 1024 * 1024).unwrap();
                assembled.push_str(frame["data"].as_str().unwrap());
                if frame["index"].as_u64().unwrap() + 1 == frame["total"].as_u64().unwrap() {
                    id = frame["id"].as_str().unwrap().to_owned();
                    break;
                }
            }
            let request: Value = serde_json::from_str(&assembled).unwrap();
            assert_eq!(request["command"]["photo"].as_str().unwrap().len(), 400_000);
            let reply = json!({"id": id, "result": {"photo": "y".repeat(400_000)}});
            protocol::send_native(&mut extension, &reply).unwrap();
        });
        let reply = request_at(
            directory.path(),
            json!({"photo": "x".repeat(400_000)}),
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(reply["photo"].as_str().unwrap().len(), 400_000);
        browser.join().unwrap();
        server.join().unwrap().unwrap();
        assert!(!directory.path().join("bridge.sock").exists());
    }

    #[test]
    fn rejects_unrecognized_native_origins() {
        assert_eq!(
            serve("chrome-extension://untrusted/").unwrap_err().code,
            "extension.invalid_origin"
        );
        assert_eq!(
            serve("https://vinted.fi/").unwrap_err().code,
            "extension.invalid_origin"
        );
    }

    #[test]
    fn stalled_native_request_times_out_without_retrying() {
        let directory = directory();
        let socket = LocalSocket::bind(directory.path()).unwrap();
        let (mut native_host, mut extension) = UnixStream::pair().unwrap();
        native_host.set_nonblocking(true).unwrap();
        let mut output = native_host.try_clone().unwrap();
        let server = std::thread::spawn(move || {
            run(
                &socket,
                &mut native_host,
                &mut output,
                Duration::from_millis(30),
            )
        });
        let browser = std::thread::spawn(move || {
            let frame = protocol::read_frame(&mut extension, 1024 * 1024).unwrap();
            assert_eq!(frame["total"], 1);
            let mut byte = [0];
            assert_eq!(extension.read(&mut byte).unwrap(), 0);
        });
        let error = request_at(directory.path(), json!({}), Duration::from_secs(2)).unwrap_err();
        assert_eq!(error.code, "mutation.uncertain");
        assert!(!error.safe_to_retry);
        assert_eq!(
            server.join().unwrap().unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        browser.join().unwrap();
    }

    #[test]
    fn browser_errors_do_not_expose_payloads() {
        let directory = directory();
        let socket = LocalSocket::bind(directory.path()).unwrap();
        let server = std::thread::spawn(move || {
            loop {
                if let Ok((mut stream, _)) = socket.listener.accept() {
                    stream.set_nonblocking(false).unwrap();
                    stream
                        .set_read_timeout(Some(Duration::from_secs(1)))
                        .unwrap();
                    let request = protocol::read_frame(&mut stream, protocol::MAX_MESSAGE).unwrap();
                    let reply = json!({"id": request["id"], "error": "secret credential"});
                    protocol::write_frame(&mut stream, &protocol::encode(&reply).unwrap()).unwrap();
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });
        let error = request_at(directory.path(), json!({}), Duration::from_secs(1)).unwrap_err();
        assert!(!error.safe_to_retry);
        assert!(!format!("{error:?}").contains("secret credential"));
        server.join().unwrap();
    }
}
