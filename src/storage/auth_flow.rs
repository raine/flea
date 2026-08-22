use std::{fmt, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

use super::{
    StatePaths,
    atomic_file::{AtomicFile, AtomicFileStore, sync_directory},
};

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthFlow {
    pub flow_id: String,
    pub expires_at_unix: u64,
    pub state: String,
    pub nonce: String,
    pub pkce_verifier: String,
    pub device_id: String,
    pub installation_id: String,
    pub ab_test_device_id: String,
}

impl AuthFlow {
    pub fn is_expired(&self, now_unix: u64) -> bool {
        self.expires_at_unix <= now_unix
    }
}

impl fmt::Debug for AuthFlow {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AuthFlow")
            .field("flow_id", &self.flow_id)
            .field("expires_at_unix", &self.expires_at_unix)
            .field("state", &"[REDACTED]")
            .field("nonce", &"[REDACTED]")
            .field("pkce_verifier", &"[REDACTED]")
            .field("device_id", &"[REDACTED]")
            .field("installation_id", &"[REDACTED]")
            .field("ab_test_device_id", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AuthFlowStoreError {
    #[error("invalid OAuth flow identifier")]
    InvalidFlowId,
    #[error("OAuth flow was not found")]
    NotFound,
    #[error("OAuth flow has expired")]
    Expired,
    #[error("OAuth flow storage operation failed")]
    Io(#[source] io::Error),
    #[error("OAuth flow data is invalid")]
    InvalidData(#[source] serde_json::Error),
}

impl From<io::Error> for AuthFlowStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub struct AuthFlowStore<W = AtomicFile> {
    paths: StatePaths,
    writer: W,
}

impl AuthFlowStore<AtomicFile> {
    pub fn new(paths: StatePaths) -> Self {
        Self::with_writer(paths, AtomicFile)
    }
}

impl<W: AtomicFileStore> AuthFlowStore<W> {
    pub fn with_writer(paths: StatePaths, writer: W) -> Self {
        Self { paths, writer }
    }

    pub fn save(&self, flow: &AuthFlow) -> Result<(), AuthFlowStoreError> {
        validate_flow_id(&flow.flow_id)?;
        self.paths.ensure()?;
        let contents = serde_json::to_vec(flow).map_err(AuthFlowStoreError::InvalidData)?;
        self.writer
            .write(&self.flow_path(&flow.flow_id), &contents)?;
        Ok(())
    }

    pub fn load(&self, flow_id: &str, now_unix: u64) -> Result<AuthFlow, AuthFlowStoreError> {
        validate_flow_id(flow_id)?;
        let path = self.flow_path(flow_id);
        reject_symlink(&path)?;
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(AuthFlowStoreError::NotFound);
            }
            Err(error) => return Err(error.into()),
        };
        let flow: AuthFlow =
            serde_json::from_slice(&contents).map_err(AuthFlowStoreError::InvalidData)?;
        if flow.flow_id != flow_id {
            return Err(AuthFlowStoreError::InvalidData(mismatched_flow_id_error()));
        }
        if flow.is_expired(now_unix) {
            self.delete(flow_id)?;
            return Err(AuthFlowStoreError::Expired);
        }
        Ok(flow)
    }

    pub fn delete(&self, flow_id: &str) -> Result<(), AuthFlowStoreError> {
        validate_flow_id(flow_id)?;
        let path = self.flow_path(flow_id);
        match fs::remove_file(path) {
            Ok(()) => sync_directory(&self.paths.flows_dir())?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    fn flow_path(&self, flow_id: &str) -> PathBuf {
        self.paths.flows_dir().join(format!("{flow_id}.json"))
    }
}

fn validate_flow_id(flow_id: &str) -> Result<(), AuthFlowStoreError> {
    if flow_id.is_empty()
        || flow_id.len() > 128
        || !flow_id
            .bytes()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, b'-' | b'_'))
    {
        return Err(AuthFlowStoreError::InvalidFlowId);
    }
    Ok(())
}

fn reject_symlink(path: &std::path::Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "OAuth flow path is a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn mismatched_flow_id_error() -> serde_json::Error {
    <serde_json::Error as serde::de::Error>::custom("stored flow identifier does not match file")
}

#[cfg(test)]
mod tests {
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    use super::{AuthFlow, AuthFlowStore, AuthFlowStoreError};
    use crate::storage::StatePaths;

    fn flow() -> AuthFlow {
        AuthFlow {
            flow_id: "flow-123".to_owned(),
            expires_at_unix: 200,
            state: "secret-state".to_owned(),
            nonce: "secret-nonce".to_owned(),
            pkce_verifier: "secret-verifier".to_owned(),
            device_id: "secret-device".to_owned(),
            installation_id: "secret-installation".to_owned(),
            ab_test_device_id: "secret-ab-device".to_owned(),
        }
    }

    #[test]
    fn persists_loads_and_deletes_flows() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("state"));
        let store = AuthFlowStore::new(paths.clone());

        store.save(&flow()).unwrap();
        let loaded = store.load("flow-123", 100).unwrap();
        assert_eq!(loaded.state, "secret-state");
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(paths.flows_dir().join("flow-123.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );

        store.delete("flow-123").unwrap();
        assert!(matches!(
            store.load("flow-123", 100),
            Err(AuthFlowStoreError::NotFound)
        ));
    }

    #[test]
    fn deletes_expired_flows() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("state"));
        let store = AuthFlowStore::new(paths.clone());
        store.save(&flow()).unwrap();

        assert!(matches!(
            store.load("flow-123", 200),
            Err(AuthFlowStoreError::Expired)
        ));
        assert!(!paths.flows_dir().join("flow-123.json").exists());
    }

    #[test]
    fn rejects_flow_ids_that_can_escape_the_flow_directory() {
        let temporary = tempdir().unwrap();
        let store = AuthFlowStore::new(StatePaths::from_root(temporary.path()));

        assert!(matches!(
            store.load("../credentials", 0),
            Err(AuthFlowStoreError::InvalidFlowId)
        ));
    }

    #[test]
    fn debug_output_redacts_sensitive_material() {
        let output = format!("{:?}", flow());

        assert!(output.contains("flow-123"));
        for secret in [
            "secret-verifier",
            "secret-state",
            "secret-nonce",
            "secret-device",
            "secret-installation",
            "secret-ab-device",
        ] {
            assert!(!output.contains(secret));
        }
    }
}
