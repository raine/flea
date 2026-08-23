pub mod atomic_file;
pub mod auth_flow;
pub mod credentials;

use std::{env, ffi::OsString, io, path::PathBuf};

use atomic_file::secure_directory;

use crate::marketplace::MarketplaceContext;

const STATE_DIR: &str = "flea";

pub(crate) fn discover_state_root() -> io::Result<PathBuf> {
    state_root_from_environment(env::var_os("XDG_STATE_HOME"), env::var_os("HOME"))
}

fn state_root_from_environment(
    xdg_state_home: Option<OsString>,
    home: Option<OsString>,
) -> io::Result<PathBuf> {
    let state_home = xdg_state_home
        .filter(|path| PathBuf::from(path).is_absolute())
        .map(PathBuf::from)
        .or_else(|| {
            home.map(PathBuf::from)
                .map(|path| path.join(".local/state"))
        })
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "cannot determine the local state directory",
            )
        })?;
    Ok(state_home.join(STATE_DIR))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatePaths {
    root: PathBuf,
    context: MarketplaceContext,
}

impl StatePaths {
    pub fn discover(context: MarketplaceContext) -> io::Result<Self> {
        let paths = Self::from_root(discover_state_root()?, context);
        paths.remove_unscoped_credentials()?;
        Ok(paths)
    }

    pub fn from_root(root: impl Into<PathBuf>, context: MarketplaceContext) -> Self {
        Self {
            root: root.into(),
            context,
        }
    }

    pub fn ensure(&self) -> io::Result<()> {
        secure_directory(&self.root)?;
        secure_directory(&self.root.join("auth"))?;
        secure_directory(&self.marketplace_auth_dir())?;
        secure_directory(&self.auth_dir())?;
        secure_directory(&self.flows_dir())?;
        secure_directory(&self.accounts_dir())?;
        secure_directory(&self.logs_dir())
    }

    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    fn marketplace_auth_dir(&self) -> PathBuf {
        self.root
            .join("auth")
            .join(self.context.marketplace.to_string())
    }

    pub fn auth_dir(&self) -> PathBuf {
        self.marketplace_auth_dir()
            .join(self.context.portal.to_string())
    }

    pub fn flows_dir(&self) -> PathBuf {
        self.auth_dir().join("flows")
    }

    pub fn accounts_dir(&self) -> PathBuf {
        self.auth_dir().join("accounts")
    }

    pub fn current_account_file(&self) -> PathBuf {
        self.auth_dir().join("current-account")
    }

    pub fn account_credentials_file(&self, account_key: &str) -> PathBuf {
        self.accounts_dir().join(format!("{account_key}.json"))
    }

    pub fn credentials_lock_file(&self) -> PathBuf {
        self.auth_dir().join("credentials.lock")
    }

    pub fn oauth_callback_file(&self) -> PathBuf {
        self.auth_dir().join("oauth-callback")
    }

    pub fn auth_callback_app(&self) -> PathBuf {
        self.auth_dir().join("Flea Auth.app")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    fn remove_unscoped_credentials(&self) -> io::Result<()> {
        let mut paths = vec![self.root.join("auth/credentials.json")];
        if let Some(state_home) = self.root.parent() {
            paths.push(state_home.join("tori-cli/auth/credentials.json"));
        }
        for path in paths {
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    if let Some(parent) = path.parent() {
                        atomic_file::sync_directory(parent)?;
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    use super::{StatePaths, state_root_from_environment};
    use crate::marketplace::MarketplaceContext;

    #[test]
    fn uses_absolute_xdg_state_home() {
        let root = state_root_from_environment(
            Some(OsString::from("/var/lib/example")),
            Some(OsString::from("/home/example")),
        )
        .unwrap();

        assert_eq!(root, std::path::Path::new("/var/lib/example/flea"));
    }

    #[test]
    fn falls_back_to_home_for_relative_xdg_state_home() {
        let root = state_root_from_environment(
            Some(OsString::from("relative")),
            Some(OsString::from("/home/example")),
        )
        .unwrap();

        assert_eq!(
            root,
            std::path::Path::new("/home/example/.local/state/flea")
        );
    }

    #[test]
    fn removes_unscoped_credential_files() {
        let temporary = tempdir().unwrap();
        let root = temporary.path().join("flea");
        let old_flea_auth = root.join("auth");
        let old_tori_auth = temporary.path().join("tori-cli/auth");
        std::fs::create_dir_all(&old_flea_auth).unwrap();
        std::fs::create_dir_all(&old_tori_auth).unwrap();
        std::fs::write(old_flea_auth.join("credentials.json"), b"secret").unwrap();
        std::fs::write(old_tori_auth.join("credentials.json"), b"secret").unwrap();
        let paths = StatePaths::from_root(root, MarketplaceContext::TORI_FI);

        paths.remove_unscoped_credentials().unwrap();

        assert!(!old_flea_auth.join("credentials.json").exists());
        assert!(!old_tori_auth.join("credentials.json").exists());
    }

    #[test]
    fn scopes_authentication_by_marketplace_and_portal() {
        let root = std::path::Path::new("/tmp/flea-state");

        let tori = StatePaths::from_root(root, MarketplaceContext::TORI_FI);
        let vinted = StatePaths::from_root(root, MarketplaceContext::VINTED_FI);

        assert_eq!(tori.auth_dir(), root.join("auth/tori/fi"));
        assert_eq!(vinted.auth_dir(), root.join("auth/vinted/fi"));
        assert_ne!(tori.credentials_lock_file(), vinted.credentials_lock_file());
    }

    #[test]
    fn creates_the_scoped_state_tree_with_private_permissions() {
        let temporary = tempdir().unwrap();
        let paths =
            StatePaths::from_root(temporary.path().join("flea"), MarketplaceContext::VINTED_FI);

        paths.ensure().unwrap();

        for directory in [
            paths.root(),
            paths.auth_dir(),
            paths.flows_dir(),
            paths.accounts_dir(),
            paths.logs_dir(),
        ] {
            assert!(directory.is_dir());
            #[cfg(unix)]
            assert_eq!(
                std::fs::metadata(directory).unwrap().permissions().mode() & 0o777,
                0o700
            );
        }
    }
}
