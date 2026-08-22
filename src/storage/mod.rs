pub mod atomic_file;
pub mod auth_flow;
pub mod credentials;

use std::{env, ffi::OsString, io, path::PathBuf};

use atomic_file::secure_directory;

const STATE_DIR: &str = "flea";
const LEGACY_STATE_DIR: &str = "tori-cli";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    pub fn discover() -> io::Result<Self> {
        Self::from_environment(env::var_os("XDG_STATE_HOME"), env::var_os("HOME"))
    }

    pub fn from_state_home(state_home: impl Into<PathBuf>) -> Self {
        let state_home = state_home.into();
        let root = state_home.join(STATE_DIR);
        let legacy_root = state_home.join(LEGACY_STATE_DIR);
        if path_exists(&root) || !is_directory(&legacy_root) {
            Self { root }
        } else {
            Self { root: legacy_root }
        }
    }

    pub fn from_root(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn ensure(&self) -> io::Result<()> {
        secure_directory(&self.root)?;
        secure_directory(&self.auth_dir())?;
        secure_directory(&self.flows_dir())?;
        secure_directory(&self.logs_dir())
    }

    pub fn root(&self) -> PathBuf {
        self.root.clone()
    }

    pub fn auth_dir(&self) -> PathBuf {
        self.root.join("auth")
    }

    pub fn flows_dir(&self) -> PathBuf {
        self.auth_dir().join("flows")
    }

    pub fn credentials_file(&self) -> PathBuf {
        self.auth_dir().join("credentials.json")
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

    fn from_environment(
        xdg_state_home: Option<OsString>,
        home: Option<OsString>,
    ) -> io::Result<Self> {
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
        Ok(Self::from_state_home(state_home))
    }
}

fn path_exists(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn is_directory(path: &std::path::Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.is_dir())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    use super::StatePaths;

    #[test]
    fn uses_absolute_xdg_state_home() {
        let paths = StatePaths::from_environment(
            Some(OsString::from("/var/lib/example")),
            Some(OsString::from("/home/example")),
        )
        .unwrap();

        assert_eq!(paths.root(), std::path::Path::new("/var/lib/example/flea"));
    }

    #[test]
    fn falls_back_to_home_for_relative_xdg_state_home() {
        let paths = StatePaths::from_environment(
            Some(OsString::from("relative")),
            Some(OsString::from("/home/example")),
        )
        .unwrap();

        assert_eq!(
            paths.root(),
            std::path::Path::new("/home/example/.local/state/flea")
        );
    }

    #[test]
    fn uses_existing_legacy_state_when_flea_state_is_absent() {
        let temporary = tempdir().unwrap();
        let legacy = temporary.path().join("tori-cli");
        std::fs::create_dir(&legacy).unwrap();

        let paths = StatePaths::from_state_home(temporary.path());

        assert_eq!(paths.root(), legacy);
    }

    #[test]
    fn prefers_flea_state_when_both_state_directories_exist() {
        let temporary = tempdir().unwrap();
        let flea = temporary.path().join("flea");
        std::fs::create_dir(&flea).unwrap();
        std::fs::create_dir(temporary.path().join("tori-cli")).unwrap();

        let paths = StatePaths::from_state_home(temporary.path());

        assert_eq!(paths.root(), flea);
    }

    #[test]
    fn creates_the_state_tree_with_private_permissions() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("flea"));

        paths.ensure().unwrap();

        for directory in [
            paths.root(),
            paths.auth_dir(),
            paths.flows_dir(),
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
