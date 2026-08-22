pub mod atomic_file;
pub mod auth_flow;
pub mod credentials;

use std::{env, ffi::OsString, io, path::PathBuf};

use atomic_file::secure_directory;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatePaths {
    root: PathBuf,
}

impl StatePaths {
    pub fn discover() -> io::Result<Self> {
        Self::from_environment(env::var_os("XDG_STATE_HOME"), env::var_os("HOME"))
    }

    pub fn from_state_home(state_home: impl Into<PathBuf>) -> Self {
        Self {
            root: state_home.into().join("tori-cli"),
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

        assert_eq!(
            paths.root(),
            std::path::Path::new("/var/lib/example/tori-cli")
        );
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
            std::path::Path::new("/home/example/.local/state/tori-cli")
        );
    }

    #[test]
    fn creates_the_state_tree_with_private_permissions() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(temporary.path().join("tori-cli"));

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
