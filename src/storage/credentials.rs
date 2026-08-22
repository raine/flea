use std::{fmt, fs, io, path::Path};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::{
    StatePaths,
    atomic_file::{AtomicFile, AtomicFileStore, set_private_file_mode, sync_directory},
};

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct CredentialRecord {
    pub user_id: String,
    pub refresh_token: String,
    pub bearer_token: String,
    pub bearer_expires_at_unix: u64,
    pub device_id: String,
    pub installation_id: String,
    pub ab_test_device_id: String,
}

impl CredentialRecord {
    pub fn bearer_is_valid_at(&self, now_unix: u64, minimum_remaining_seconds: u64) -> bool {
        self.bearer_expires_at_unix.saturating_sub(now_unix) > minimum_remaining_seconds
    }
}

impl fmt::Debug for CredentialRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CredentialRecord")
            .field("user_id", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("bearer_token", &"[REDACTED]")
            .field("bearer_expires_at_unix", &self.bearer_expires_at_unix)
            .field("device_id", &"[REDACTED]")
            .field("installation_id", &"[REDACTED]")
            .field("ab_test_device_id", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    #[error("credential storage operation failed")]
    Io(#[source] io::Error),
    #[error("credential data is invalid")]
    InvalidData(#[source] serde_json::Error),
}

impl From<io::Error> for CredentialStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct CredentialStore<W = AtomicFile> {
    paths: StatePaths,
    writer: W,
}

impl CredentialStore<AtomicFile> {
    pub fn new(paths: StatePaths) -> Self {
        Self::with_writer(paths, AtomicFile)
    }
}

impl<W: AtomicFileStore> CredentialStore<W> {
    pub fn with_writer(paths: StatePaths, writer: W) -> Self {
        Self { paths, writer }
    }

    pub fn lock(&self) -> Result<LockedCredentials<'_, W>, CredentialStoreError> {
        self.paths.ensure()?;
        let lock_path = self.paths.credentials_lock_file();
        let lock_file = open_lock_file(&lock_path)?;
        lock_file.lock_exclusive()?;
        Ok(LockedCredentials {
            store: self,
            lock_file,
        })
    }

    pub fn load(&self) -> Result<Option<CredentialRecord>, CredentialStoreError> {
        self.lock()?.load()
    }

    pub fn save(&self, credentials: &CredentialRecord) -> Result<(), CredentialStoreError> {
        self.lock()?.save(credentials)
    }

    pub fn delete(&self) -> Result<(), CredentialStoreError> {
        self.lock()?.delete()
    }

    pub fn with_locked<T>(
        &self,
        operation: impl FnOnce(&LockedCredentials<'_, W>) -> Result<T, CredentialStoreError>,
    ) -> Result<T, CredentialStoreError> {
        let locked = self.lock()?;
        operation(&locked)
    }

    pub fn rotate(
        &self,
        operation: impl FnOnce(
            Option<CredentialRecord>,
        ) -> Result<CredentialRecord, CredentialStoreError>,
    ) -> Result<(), CredentialStoreError> {
        self.with_locked(|locked| {
            let latest = locked.load()?;
            let replacement = operation(latest)?;
            locked.save(&replacement)
        })
    }
}

pub struct LockedCredentials<'a, W: AtomicFileStore> {
    store: &'a CredentialStore<W>,
    lock_file: fs::File,
}

impl<W: AtomicFileStore> LockedCredentials<'_, W> {
    pub fn load(&self) -> Result<Option<CredentialRecord>, CredentialStoreError> {
        match fs::read(self.store.paths.credentials_file()) {
            Ok(contents) => serde_json::from_slice(&contents)
                .map(Some)
                .map_err(CredentialStoreError::InvalidData),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn save(&self, credentials: &CredentialRecord) -> Result<(), CredentialStoreError> {
        let contents =
            serde_json::to_vec(credentials).map_err(CredentialStoreError::InvalidData)?;
        self.store
            .writer
            .write(&self.store.paths.credentials_file(), &contents)?;
        Ok(())
    }

    pub fn delete(&self) -> Result<(), CredentialStoreError> {
        match fs::remove_file(self.store.paths.credentials_file()) {
            Ok(()) => sync_directory(&self.store.paths.auth_dir())?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
}

impl<W: AtomicFileStore> Drop for LockedCredentials<'_, W> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock_file);
    }
}

fn open_lock_file(path: &Path) -> io::Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    set_private_file_mode(path)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::Path,
        sync::{Arc, Barrier},
        thread,
        time::Duration,
    };

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    use super::{CredentialRecord, CredentialStore, CredentialStoreError};
    use crate::storage::{StatePaths, atomic_file::AtomicFileStore};

    struct FailingWriter;

    impl AtomicFileStore for FailingWriter {
        fn write(&self, _path: &Path, _contents: &[u8]) -> io::Result<()> {
            Err(io::Error::other("injected write failure"))
        }
    }

    fn credentials(refresh_token: &str) -> CredentialRecord {
        CredentialRecord {
            user_id: "user-secret".to_owned(),
            refresh_token: refresh_token.to_owned(),
            bearer_token: "bearer-secret".to_owned(),
            bearer_expires_at_unix: 500,
            device_id: "device-secret".to_owned(),
            installation_id: "installation-secret".to_owned(),
            ab_test_device_id: "ab-secret".to_owned(),
        }
    }

    #[test]
    fn saves_private_credentials_and_lock_file() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("state"));
        let store = CredentialStore::new(paths.clone());

        store.save(&credentials("refresh-secret")).unwrap();
        let loaded = store.load().unwrap().unwrap();
        assert_eq!(loaded.refresh_token, "refresh-secret");

        #[cfg(unix)]
        for path in [paths.credentials_file(), paths.credentials_lock_file()] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn rotation_reads_the_latest_record_while_holding_the_lock() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("state"));
        let store = CredentialStore::new(paths.clone());
        store.save(&credentials("first")).unwrap();

        store
            .rotate(|latest| {
                assert_eq!(latest.unwrap().refresh_token, "first");
                Ok(credentials("second"))
            })
            .unwrap();

        assert_eq!(store.load().unwrap().unwrap().refresh_token, "second");
    }

    #[test]
    fn failed_rotation_preserves_the_previous_record() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("state"));
        CredentialStore::new(paths.clone())
            .save(&credentials("first"))
            .unwrap();
        let failing_store = CredentialStore::with_writer(paths.clone(), FailingWriter);

        let result = failing_store.rotate(|_| Ok(credentials("second")));

        assert!(matches!(result, Err(CredentialStoreError::Io(_))));
        assert_eq!(
            CredentialStore::new(paths)
                .load()
                .unwrap()
                .unwrap()
                .refresh_token,
            "first"
        );
    }

    #[test]
    fn process_lock_serializes_credential_access() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("state"));
        let first = CredentialStore::new(paths.clone());
        let second = CredentialStore::new(paths);
        let barrier = Arc::new(Barrier::new(2));
        let held = first.lock().unwrap();
        let worker_barrier = Arc::clone(&barrier);

        let worker = thread::spawn(move || {
            worker_barrier.wait();
            second.save(&credentials("second")).unwrap();
        });
        barrier.wait();
        thread::sleep(Duration::from_millis(50));
        assert!(!worker.is_finished());

        drop(held);
        worker.join().unwrap();
    }

    #[test]
    fn debug_output_redacts_all_identifiers_and_tokens() {
        let output = format!("{:?}", credentials("refresh-secret"));

        for secret in [
            "refresh-secret",
            "bearer-secret",
            "user-secret",
            "device-secret",
            "installation-secret",
            "ab-secret",
        ] {
            assert!(!output.contains(secret));
        }
    }

    #[test]
    fn bearer_validity_reserves_the_requested_margin() {
        let mut record = credentials("refresh");
        record.bearer_expires_at_unix = 200;

        assert!(record.bearer_is_valid_at(100, 99));
        assert!(!record.bearer_is_valid_at(100, 100));
        assert!(!record.bearer_is_valid_at(300, 0));
    }
}
