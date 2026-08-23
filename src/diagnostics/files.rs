use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use fs2::FileExt;
use tracing_subscriber::fmt::MakeWriter;
use uuid::Uuid;

use super::redaction::redact_json_line;

const LOG_FILE: &str = "flea.jsonl";
const LOG_PREFIX: &str = "flea";
const LOG_SUFFIX: &str = ".jsonl";

#[derive(Clone, Copy, Debug)]
pub(super) struct RetentionPolicy {
    pub(super) max_age: Duration,
    pub(super) max_total_bytes: u64,
    pub(super) max_active_bytes: u64,
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

pub(super) fn active_log_path(state_dir: &Path) -> PathBuf {
    state_dir.join("logs").join(LOG_FILE)
}

pub(super) fn prepare_log(
    log_path: &Path,
    retention: RetentionPolicy,
) -> io::Result<RedactingMakeWriter> {
    let log_dir = log_path
        .parent()
        .expect("the active log path always has a parent");
    let state_dir = log_dir
        .parent()
        .expect("the log directory always has a state directory");
    create_private_dir(state_dir)?;
    create_private_dir(log_dir)?;
    roll_active_log(log_dir, retention.max_active_bytes)?;
    enforce_retention(log_dir, retention)?;
    open_private_log(log_path).map(RedactingMakeWriter::new)
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
pub(super) struct RedactingMakeWriter {
    file: Arc<Mutex<File>>,
}

impl RedactingMakeWriter {
    pub(super) fn new(file: File) -> Self {
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

pub(super) struct RedactingWriter {
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

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;

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
        enforce_retention(&logs_dir, policy).expect("retention should succeed");

        assert!(auth_dir.join("credentials.json").exists());
        assert!(logs_dir.join("unrelated.txt").exists());
        assert!(logs_dir.join("flea.1.old.jsonl").exists());
        assert!(!archive.exists());
    }

    #[test]
    fn active_writer_lock_prevents_concurrent_rotation() {
        let state = tempdir().expect("temporary state directory");
        let logs_dir = state.path().join("logs");
        fs::create_dir_all(&logs_dir).expect("logs directory");
        let active = logs_dir.join(LOG_FILE);
        fs::write(&active, "large enough").expect("active log fixture");
        let _writer = open_private_log(&active).expect("active writer");

        roll_active_log(&logs_dir, 1).expect("rotation should succeed");

        let archives = fs::read_dir(&logs_dir)
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
}
