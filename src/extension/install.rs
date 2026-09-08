use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{HOST_NAME, failure};
use crate::AppError;

const FILES: [(&str, &[u8]); 3] = [
    (
        "manifest.json",
        include_bytes!("../../extension/manifest.json"),
    ),
    (
        "background.js",
        include_bytes!("../../extension/background.js"),
    ),
    ("content.js", include_bytes!("../../extension/content.js")),
];

pub(super) fn root() -> Result<PathBuf, AppError> {
    if !cfg!(any(target_os = "macos", target_os = "linux")) {
        return Err(AppError::usage(
            "The browser extension supports Chrome on macOS and Linux.",
        ));
    }
    dirs::data_local_dir()
        .map(|path| path.join("flea").join("browser-bridge"))
        .ok_or_else(|| failure("Cannot locate the Flea application support directory."))
}

fn manifest_path() -> Result<PathBuf, AppError> {
    let home = dirs::home_dir().ok_or_else(|| failure("Cannot locate the home directory."))?;
    let directory = if cfg!(target_os = "macos") {
        home.join("Library/Application Support/Google/Chrome/NativeMessagingHosts")
    } else {
        dirs::config_dir()
            .ok_or_else(|| failure("Cannot locate the Chrome configuration directory."))?
            .join("google-chrome/NativeMessagingHosts")
    };
    Ok(directory.join(format!("{HOST_NAME}.json")))
}

pub(super) fn origin() -> Result<String, AppError> {
    let manifest: Value = serde_json::from_slice(FILES[0].1)
        .map_err(|_| failure("The bundled extension manifest is invalid."))?;
    let key = manifest["key"]
        .as_str()
        .ok_or_else(|| failure("The bundled extension manifest has no public key."))?;
    let key = base64::engine::general_purpose::STANDARD
        .decode(key)
        .map_err(|_| failure("The bundled extension public key is invalid."))?;
    let digest = Sha256::digest(key);
    let id: String = digest[..16]
        .iter()
        .flat_map(|byte| {
            [
                char::from(b'a' + (byte >> 4)),
                char::from(b'a' + (byte & 15)),
            ]
        })
        .collect();
    Ok(format!("chrome-extension://{id}/"))
}

pub(super) fn setup() -> Result<Value, AppError> {
    let executable = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|_| failure("Cannot locate the Flea executable."))?;
    install_at(&root()?, &manifest_path()?, &executable)
}

fn install_at(root: &Path, native_manifest: &Path, executable: &Path) -> Result<Value, AppError> {
    private_dir(root).map_err(|_| failure("Cannot prepare the private Flea support directory."))?;
    let extension = root.join("extension");
    private_dir(&extension).map_err(|_| failure("Cannot prepare the extension directory."))?;
    for (name, contents) in FILES {
        atomic_write(&extension.join(name), contents)
            .map_err(|_| failure("Cannot install the bundled extension files."))?;
    }
    let origin = origin()?;
    let manifest = json!({
        "name": HOST_NAME,
        "description": "Flea browser bridge",
        "path": executable,
        "type": "stdio",
        "allowed_origins": [origin],
    });
    let parent = native_manifest
        .parent()
        .ok_or_else(|| failure("Invalid native manifest path."))?;
    fs::create_dir_all(parent)
        .map_err(|_| failure("Cannot create the Chrome native host directory."))?;
    atomic_write(
        native_manifest,
        &serde_json::to_vec_pretty(&manifest)
            .map_err(|_| failure("Cannot encode the native host manifest."))?,
    )
    .map_err(|_| failure("Cannot install the Chrome native host manifest."))?;
    Ok(json!({
        "extension_id": origin.trim_start_matches("chrome-extension://").trim_end_matches('/'),
        "extension_path": extension,
        "native_host_manifest": native_manifest,
        "next_steps": [
            "Open chrome://extensions and enable Developer mode.",
            format!("Choose Load unpacked and select {}.", extension.display()),
            "Open or reload a Vinted tab in Chrome and sign in."
        ]
    }))
}

pub(super) fn configured() -> bool {
    root().is_ok_and(|root| installed_at(&root))
}

fn installed_at(root: &Path) -> bool {
    // Installation remains selected across upgrades and damaged artifacts.
    // A broken installation must not switch to a different browser session.
    !matches!(fs::symlink_metadata(root), Err(error) if error.kind() == io::ErrorKind::NotFound)
}

pub(super) fn owned(metadata: &fs::Metadata) -> bool {
    // geteuid has no pointer arguments and cannot fail.
    metadata.uid() == unsafe { libc::geteuid() }
}

pub(super) fn check_private_dir(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || !owned(&metadata) || metadata.permissions().mode() & 0o777 != 0o700 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bridge directory is not private.",
        ));
    }
    Ok(())
}

pub(super) fn private_dir(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => return check_private_dir(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    check_private_dir(path)
}

fn atomic_write(path: &Path, contents: &[u8]) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("Missing parent directory."))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary
        .as_file()
        .set_permissions(fs::Permissions::from_mode(0o600))?;
    temporary.write_all(contents)?;
    temporary.as_file().sync_all()?;
    temporary.persist(path).map_err(|error| error.error)?;
    Ok(())
}

pub(super) fn lock_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || !owned(&metadata) || metadata.permissions().mode() & 0o777 != 0o600 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Bridge lock is not private.",
        ));
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_key_has_expected_extension_id() {
        assert_eq!(
            origin().unwrap(),
            "chrome-extension://hongnhgapbdanogjpbnpnngkjkkohgjp/"
        );
    }

    #[test]
    fn installs_and_replaces_bundled_files_and_exact_origin() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("support");
        let manifest_path = temporary.path().join("chrome/host.json");
        let executable = std::env::current_exe().unwrap();
        let output = install_at(&root, &manifest_path, &executable).unwrap();
        assert_eq!(output["extension_path"], json!(root.join("extension")));
        assert!(output["next_steps"].is_array());
        check_private_dir(&root).unwrap();
        let manifest: Value = serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(manifest["path"], json!(executable));
        assert_eq!(manifest["allowed_origins"], json!([origin().unwrap()]));
        assert_eq!(
            fs::metadata(&manifest_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::write(root.join("extension/background.js"), b"damaged").unwrap();
        install_at(&root, &manifest_path, &executable).unwrap();
        for (name, contents) in FILES {
            assert_eq!(
                fs::read(root.join("extension").join(name)).unwrap(),
                contents
            );
        }
    }

    #[test]
    fn installation_remains_selected_when_files_are_outdated_or_missing() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("support");
        assert!(!installed_at(&root));
        private_dir(&root).unwrap();
        assert!(installed_at(&root));
        fs::write(root.join("manifest.json"), b"old or damaged manifest").unwrap();
        assert!(installed_at(&root));
    }

    #[test]
    fn refuses_symlink_and_public_directories_and_locks() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("root");
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(private_dir(&root).is_err());
        let link = temporary.path().join("link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(private_dir(&link).is_err());
        assert!(lock_file(&link).is_err());
    }
}
