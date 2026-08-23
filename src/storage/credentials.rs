use std::{fs, io, marker::PhantomData, path::Path};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use fs2::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

use super::{
    StatePaths,
    atomic_file::{AtomicFile, AtomicFileStore, set_private_file_mode, sync_directory},
};

pub trait StoredCredential: Serialize + DeserializeOwned {
    fn account_id(&self) -> &str;
    fn validate(&self) -> Result<(), CredentialStoreError>;
}

#[derive(Debug, thiserror::Error)]
pub enum CredentialStoreError {
    #[error("credential storage operation failed")]
    Io(#[source] io::Error),
    #[error("credential data is invalid")]
    InvalidData(#[source] serde_json::Error),
    #[error("credential data is missing a required value")]
    MissingRequiredValue,
    #[error("credential account selection is invalid")]
    InvalidAccountSelection,
    #[error("credential account does not match its storage key")]
    AccountMismatch,
}

impl From<io::Error> for CredentialStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct TypedCredentialStore<R, W = AtomicFile> {
    paths: StatePaths,
    writer: W,
    marker: PhantomData<R>,
}

impl<R: StoredCredential> TypedCredentialStore<R, AtomicFile> {
    pub fn new(paths: StatePaths) -> Self {
        Self::with_writer(paths, AtomicFile)
    }
}

impl<R: StoredCredential, W: AtomicFileStore> TypedCredentialStore<R, W> {
    pub fn with_writer(paths: StatePaths, writer: W) -> Self {
        Self {
            paths,
            writer,
            marker: PhantomData,
        }
    }

    pub fn lock(&self) -> Result<LockedCredentials<'_, R, W>, CredentialStoreError> {
        self.paths.ensure()?;
        let lock_path = self.paths.credentials_lock_file();
        let lock_file = open_lock_file(&lock_path)?;
        lock_file.lock_exclusive()?;
        Ok(LockedCredentials {
            store: self,
            lock_file,
        })
    }

    pub fn load(&self) -> Result<Option<R>, CredentialStoreError> {
        self.lock()?.load()
    }

    pub fn save(&self, credentials: &R) -> Result<(), CredentialStoreError> {
        self.lock()?.save(credentials)
    }

    pub fn delete(&self) -> Result<(), CredentialStoreError> {
        self.lock()?.delete()
    }

    #[cfg(test)]
    pub fn with_locked<T>(
        &self,
        operation: impl FnOnce(&LockedCredentials<'_, R, W>) -> Result<T, CredentialStoreError>,
    ) -> Result<T, CredentialStoreError> {
        let locked = self.lock()?;
        operation(&locked)
    }

    #[cfg(test)]
    pub fn rotate(
        &self,
        operation: impl FnOnce(Option<R>) -> Result<R, CredentialStoreError>,
    ) -> Result<(), CredentialStoreError> {
        self.with_locked(|locked| {
            let latest = locked.load()?;
            let replacement = operation(latest)?;
            locked.save(&replacement)
        })
    }
}

pub struct LockedCredentials<'a, R, W: AtomicFileStore> {
    store: &'a TypedCredentialStore<R, W>,
    lock_file: fs::File,
}

impl<R: StoredCredential, W: AtomicFileStore> LockedCredentials<'_, R, W> {
    pub fn load(&self) -> Result<Option<R>, CredentialStoreError> {
        let Some(account_key) = read_current_account(&self.store.paths)? else {
            return Ok(None);
        };
        let path = self.store.paths.account_credentials_file(&account_key);
        reject_symlink(&path)?;
        let contents = match fs::read(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                remove_if_present(&self.store.paths.current_account_file())?;
                sync_directory(&self.store.paths.auth_dir())?;
                return Ok(None);
            }
            Err(error) => return Err(error.into()),
        };
        let record: R =
            serde_json::from_slice(&contents).map_err(CredentialStoreError::InvalidData)?;
        record.validate()?;
        if account_storage_key(record.account_id()) != account_key {
            return Err(CredentialStoreError::AccountMismatch);
        }
        Ok(Some(record))
    }

    pub fn save(&self, credentials: &R) -> Result<(), CredentialStoreError> {
        credentials.validate()?;
        let account_key = account_storage_key(credentials.account_id());
        let contents =
            serde_json::to_vec(credentials).map_err(CredentialStoreError::InvalidData)?;
        self.store.writer.write(
            &self.store.paths.account_credentials_file(&account_key),
            &contents,
        )?;
        self.store.writer.write(
            &self.store.paths.current_account_file(),
            account_key.as_bytes(),
        )?;
        Ok(())
    }

    pub fn delete(&self) -> Result<(), CredentialStoreError> {
        let account_key = match read_current_account(&self.store.paths) {
            Ok(Some(account_key)) => account_key,
            Ok(None) => return Ok(()),
            Err(CredentialStoreError::InvalidAccountSelection) => {
                remove_if_present(&self.store.paths.current_account_file())?;
                sync_directory(&self.store.paths.auth_dir())?;
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        remove_if_present(&self.store.paths.current_account_file())?;
        sync_directory(&self.store.paths.auth_dir())?;
        remove_if_present(&self.store.paths.account_credentials_file(&account_key))?;
        sync_directory(&self.store.paths.accounts_dir())?;
        Ok(())
    }
}

impl<R, W: AtomicFileStore> Drop for LockedCredentials<'_, R, W> {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.lock_file);
    }
}

fn read_current_account(paths: &StatePaths) -> Result<Option<String>, CredentialStoreError> {
    let path = paths.current_account_file();
    reject_symlink(&path)?;
    let contents = match fs::read(path) {
        Ok(contents) => contents,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let account_key = std::str::from_utf8(&contents)
        .ok()
        .filter(|value| valid_account_key(value))
        .ok_or(CredentialStoreError::InvalidAccountSelection)?;
    Ok(Some(account_key.to_owned()))
}

fn account_storage_key(account_id: &str) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(account_id.as_bytes()))
}

fn valid_account_key(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn remove_if_present(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn open_lock_file(path: &Path) -> io::Result<fs::File> {
    reject_symlink(path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options.open(path)?;
    set_private_file_mode(path)?;
    Ok(file)
}

fn reject_symlink(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "authentication state path is a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
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

    use super::*;
    use crate::{
        marketplace::{
            MarketplaceContext, PortalId, tori::session::CredentialRecord,
            vinted::auth::VintedCredentialRecord,
        },
        storage::atomic_file::{AtomicFile, AtomicFileStore},
    };

    type CredentialStore<W = AtomicFile> = TypedCredentialStore<CredentialRecord, W>;
    type VintedCredentialStore<W = AtomicFile> = TypedCredentialStore<VintedCredentialRecord, W>;

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
            id_token: Some("id-secret".to_owned()),
            bearer_expires_at_unix: 500,
            device_id: "device-secret".to_owned(),
            installation_id: "installation-secret".to_owned(),
            ab_test_device_id: "ab-secret".to_owned(),
        }
    }

    fn vinted_credentials() -> VintedCredentialRecord {
        VintedCredentialRecord {
            portal: PortalId::Fi,
            user_id: "vinted-user".to_owned(),
            login: Some("login-secret".to_owned()),
            access_token: "access-secret".to_owned(),
            refresh_token: "refresh-secret".to_owned(),
            access_expires_at_unix: 500,
            device_uuid: "device-secret".to_owned(),
            anonymous_id: "anonymous-secret".to_owned(),
            user_device_token: Some("udt-secret".to_owned()),
        }
    }

    #[test]
    fn saves_private_account_credentials_and_pointer() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        let store = CredentialStore::new(paths.clone());

        store.save(&credentials("refresh-secret")).unwrap();
        assert_eq!(
            store.load().unwrap().unwrap().refresh_token,
            "refresh-secret"
        );

        #[cfg(unix)]
        for path in [
            paths.current_account_file(),
            paths.credentials_lock_file(),
            paths.account_credentials_file(&account_storage_key("user-secret")),
        ] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn marketplace_scopes_do_not_share_credentials_or_locks() {
        let temporary = tempdir().unwrap();
        let tori_paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        let vinted_paths = StatePaths::from_root(
            temporary.path().join("state"),
            MarketplaceContext::VINTED_FI,
        );

        CredentialStore::new(tori_paths.clone())
            .save(&credentials("tori-refresh"))
            .unwrap();
        VintedCredentialStore::new(vinted_paths.clone())
            .save(&vinted_credentials())
            .unwrap();

        assert_eq!(
            CredentialStore::new(tori_paths.clone())
                .load()
                .unwrap()
                .unwrap()
                .refresh_token,
            "tori-refresh"
        );
        assert_eq!(
            VintedCredentialStore::new(vinted_paths.clone())
                .load()
                .unwrap()
                .unwrap()
                .refresh_token,
            "refresh-secret"
        );
        assert_ne!(
            tori_paths.credentials_lock_file(),
            vinted_paths.credentials_lock_file()
        );
    }

    #[test]
    fn rotation_reads_the_latest_record_while_holding_the_lock() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        let store = CredentialStore::new(paths);
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
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        CredentialStore::new(paths.clone())
            .save(&credentials("first"))
            .unwrap();
        let failing_store: CredentialStore<FailingWriter> =
            TypedCredentialStore::with_writer(paths.clone(), FailingWriter);

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
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
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
    fn rejects_invalid_current_account_pointer() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        paths.ensure().unwrap();
        fs::write(paths.current_account_file(), b"../escape").unwrap();

        assert!(matches!(
            CredentialStore::new(paths).load(),
            Err(CredentialStoreError::InvalidAccountSelection)
        ));
    }

    #[test]
    fn delete_recovers_from_an_invalid_current_account_pointer() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        paths.ensure().unwrap();
        fs::write(paths.current_account_file(), b"../escape").unwrap();

        CredentialStore::new(paths.clone()).delete().unwrap();

        assert!(!paths.current_account_file().exists());
    }

    #[test]
    fn missing_selected_account_is_treated_as_logged_out() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        let record = credentials("account");
        let store = CredentialStore::new(paths.clone());
        store.save(&record).unwrap();
        fs::remove_file(paths.account_credentials_file(&account_storage_key(record.account_id())))
            .unwrap();

        assert!(store.load().unwrap().is_none());
        assert!(!paths.current_account_file().exists());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_links_for_account_files_and_lock_files() {
        use std::os::unix::fs::symlink;

        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        paths.ensure().unwrap();
        let target = temporary.path().join("target");
        fs::write(&target, b"secret").unwrap();
        let account_key = account_storage_key("user-secret");
        fs::write(paths.current_account_file(), account_key.as_bytes()).unwrap();
        symlink(&target, paths.account_credentials_file(&account_key)).unwrap();

        assert!(matches!(
            CredentialStore::new(paths.clone()).load(),
            Err(CredentialStoreError::Io(_))
        ));

        fs::remove_file(paths.account_credentials_file(&account_key)).unwrap();
        fs::remove_file(paths.credentials_lock_file()).unwrap();
        symlink(&target, paths.credentials_lock_file()).unwrap();
        assert!(matches!(
            CredentialStore::new(paths).lock(),
            Err(CredentialStoreError::Io(_))
        ));
        assert_eq!(fs::read(target).unwrap(), b"secret");
    }

    #[test]
    fn debug_output_redacts_tori_and_vinted_secrets() {
        let output = format!(
            "{:?} {:?}",
            credentials("refresh-secret"),
            vinted_credentials()
        );

        for secret in [
            "refresh-secret",
            "bearer-secret",
            "id-secret",
            "user-secret",
            "device-secret",
            "installation-secret",
            "ab-secret",
            "vinted-user",
            "login-secret",
            "access-secret",
            "anonymous-secret",
            "udt-secret",
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
