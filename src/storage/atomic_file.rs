use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};

const DIRECTORY_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

pub trait AtomicFileStore: Send + Sync {
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct AtomicFile;

impl AtomicFileStore for AtomicFile {
    fn write(&self, path: &Path, contents: &[u8]) -> io::Result<()> {
        write_atomic(path, contents)
    }
}

pub fn secure_directory(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(DIRECTORY_MODE);
    builder.create(path)?;
    set_mode(path, DIRECTORY_MODE)
}

pub fn write_atomic(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "atomic file path has no parent directory",
        )
    })?;
    secure_directory(parent)?;

    let (temporary_path, mut temporary_file) = create_temporary_file(parent, path)?;
    let result = (|| {
        temporary_file.write_all(contents)?;
        temporary_file.sync_all()?;
        drop(temporary_file);
        fs::rename(&temporary_path, path)?;
        set_mode(path, FILE_MODE)?;
        sync_directory(parent)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

pub(crate) fn sync_directory(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

pub(crate) fn set_private_file_mode(path: &Path) -> io::Result<()> {
    set_mode(path, FILE_MODE)
}

fn create_temporary_file(parent: &Path, destination: &Path) -> io::Result<(PathBuf, File)> {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "invalid file name"))?;

    for _ in 0..128 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temporary_path = parent.join(format!(
            ".{file_name}.tmp.{}.{}",
            std::process::id(),
            sequence
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(FILE_MODE);

        match options.open(&temporary_path) {
            Ok(file) => {
                if let Err(error) = set_private_file_mode(&temporary_path) {
                    let _ = fs::remove_file(&temporary_path);
                    return Err(error);
                }
                return Ok((temporary_path, file));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate an atomic temporary file",
    ))
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    use super::{secure_directory, write_atomic};

    #[test]
    fn creates_private_directories_and_files() {
        let temporary = tempdir().unwrap();
        let directory = temporary.path().join("auth");
        let path = directory.join("credentials.json");

        write_atomic(&path, b"first").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"first");
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn replaces_the_complete_file_and_removes_temporary_files() {
        let temporary = tempdir().unwrap();
        let path = temporary.path().join("record.json");

        write_atomic(&path, b"a much longer old value").unwrap();
        write_atomic(&path, b"new").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"new");
        let entries = fs::read_dir(temporary.path()).unwrap().count();
        assert_eq!(entries, 1);
    }

    #[test]
    fn failed_replacement_preserves_destination_and_cleans_temporary_file() {
        let temporary = tempdir().unwrap();
        let destination = temporary.path().join("record.json");
        fs::create_dir(&destination).unwrap();
        fs::write(destination.join("sentinel"), b"old").unwrap();

        assert!(write_atomic(&destination, b"replacement").is_err());

        assert_eq!(fs::read(destination.join("sentinel")).unwrap(), b"old");
        let entries = fs::read_dir(temporary.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(entries, ["record.json"]);
    }

    #[test]
    fn tightens_existing_directory_permissions() {
        let temporary = tempdir().unwrap();
        let directory = temporary.path().join("state");
        fs::create_dir(&directory).unwrap();
        #[cfg(unix)]
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();

        secure_directory(&directory).unwrap();

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(&directory).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}
