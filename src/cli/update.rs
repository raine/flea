use std::{fs, path::Path, process::Command, time::Duration};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::outcome::{CommandData, CommandOutcome};
use crate::AppError;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPO: &str = "https://github.com/raine/flea/releases/download";

fn platform(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Ok("darwin-arm64"),
        ("macos", "x86_64") => Ok("darwin-amd64"),
        ("linux", "aarch64") => Ok("linux-arm64"),
        ("linux", "x86_64") => Ok("linux-amd64"),
        _ => Err(
            format!("Unsupported platform: {os}/{arch}. Build flea from source instead.").into(),
        ),
    }
}

fn check_install(path: &Path) -> Result<()> {
    if path.components().any(|part| part.as_os_str() == "Cellar") {
        return Err("Homebrew installation: run `brew upgrade raine/flea/flea` instead.".into());
    }
    if path.starts_with("/nix/store") {
        return Err(
            "Nix installation: update flea through your Nix configuration or profile instead."
                .into(),
        );
    }
    Ok(())
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

fn release_version(tag: &str) -> Result<&str> {
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if version.is_empty()
        || !version.starts_with(|c: char| c.is_ascii_digit())
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ".-+".contains(c))
    {
        return Err("Invalid release tag in GitHub response".into());
    }
    Ok(version)
}

async fn fetch(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    Ok(client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?
        .to_vec())
}

fn verify_checksum(bytes: &[u8], checksum: &str, archive: &str) -> Result<()> {
    let fields: Vec<_> = checksum.split_whitespace().collect();
    if fields.len() != 2 || fields[1].trim_start_matches('*') != archive {
        return Err("Invalid release checksum file".into());
    }
    if fields[0] != format!("{:x}", Sha256::digest(bytes)) {
        return Err("Checksum mismatch: downloaded archive was not installed".into());
    }
    Ok(())
}

fn install(archive: &[u8], destination: &Path) -> Result<()> {
    let temp = tempfile::tempdir()?;
    let archive_path = temp.path().join("release.tar.gz");
    fs::write(&archive_path, archive)?;
    let output = Command::new("tar")
        .arg("-xzf")
        .arg(&archive_path)
        .arg("-C")
        .arg(temp.path())
        .arg("flea")
        .output()?;
    if !output.status.success() {
        return Err("Failed to extract flea from release archive (tar is required)".into());
    }
    replace_binary(&temp.path().join("flea"), destination)
}

fn replace_binary(source: &Path, destination: &Path) -> Result<()> {
    if !fs::symlink_metadata(source)?.file_type().is_file() {
        return Err("Release archive does not contain a regular flea binary".into());
    }
    let parent = destination
        .parent()
        .ok_or("Could not determine install directory")?;
    // Staging on the same filesystem makes replacement atomic, preserving the old
    // executable if copying or renaming fails.
    let staged = tempfile::NamedTempFile::new_in(parent)?;
    fs::copy(source, staged.path())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(staged.path(), fs::Permissions::from_mode(0o755))?;
    }
    staged.as_file().sync_all()?;
    staged.persist(destination)?;
    Ok(())
}

fn outcome(latest: &str, updated: bool) -> CommandOutcome {
    let message = if updated {
        format!("Updated flea v{VERSION} -> v{latest}")
    } else {
        format!("Already up to date (v{VERSION})")
    };
    let mut result = CommandOutcome::new(CommandData::Raw(serde_json::json!({
        "updated": updated, "previous_version": VERSION, "version": latest,
    })));
    result.presentation = super::outcome::CommandPresentation::Plain(
        super::outcome::PlainOutput::UpdateDocument(format!("✔ {message}\n")),
    );
    result
}

async fn update(destination: &Path) -> Result<CommandOutcome> {
    check_install(destination)?;
    let suffix = platform(std::env::consts::OS, std::env::consts::ARCH)?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("flea/", env!("CARGO_PKG_VERSION")))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .build()?;
    eprintln!("Checking for updates...");
    let release: Release = serde_json::from_slice(
        &fetch(
            &client,
            "https://api.github.com/repos/raine/flea/releases/latest",
        )
        .await?,
    )?;
    let latest = release_version(&release.tag_name)?;
    if latest == VERSION {
        return Ok(outcome(latest, false));
    }
    eprintln!("Downloading v{latest}...");
    let artifact = format!("flea-{suffix}");
    let archive_name = format!("{artifact}.tar.gz");
    let base = format!("{REPO}/{}", release.tag_name);
    let archive = fetch(&client, &format!("{base}/{archive_name}")).await?;
    let checksum = fetch(&client, &format!("{base}/{artifact}.sha256")).await?;
    eprintln!("Verifying checksum...");
    verify_checksum(&archive, std::str::from_utf8(&checksum)?, &archive_name)?;
    eprintln!("Installing...");
    install(&archive, destination)?;
    Ok(outcome(latest, true))
}

pub async fn run() -> std::result::Result<CommandOutcome, AppError> {
    let result = match std::env::current_exe().and_then(fs::canonicalize) {
        Ok(path) => update(&path).await,
        Err(error) => Err(error.into()),
    };
    result.map_err(|error| {
        eprintln!("✘ Update failed");
        AppError::upstream("update.failed", format!("{error}"))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn success_presentation_respects_explicit_format() {
        for updated in [false, true] {
            let result = outcome(VERSION, updated);
            let plain = crate::output::render_plain(
                &result.presentation,
                crate::output::OutputFormat::Toon,
                false,
            )
            .unwrap()
            .unwrap();
            assert!(plain.contains(if updated {
                "Updated flea"
            } else {
                "Already up to date"
            }));
            assert!(
                crate::output::render_plain(
                    &result.presentation,
                    crate::output::OutputFormat::Json,
                    true,
                )
                .unwrap()
                .is_none()
            );
            assert_eq!(
                serde_json::to_value(result.data).unwrap()["updated"],
                updated
            );
        }
    }

    #[test]
    fn packaged_binary_is_extracted_and_installed() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("flea");
        fs::write(&source, b"release binary").unwrap();
        let archive = dir.path().join("release.tar.gz");
        assert!(
            Command::new("tar")
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(dir.path())
                .arg("flea")
                .status()
                .unwrap()
                .success()
        );
        let destination = dir.path().join("installed");
        fs::write(&destination, b"old").unwrap();
        install(&fs::read(archive).unwrap(), &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"release binary");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(destination).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
    }

    #[test]
    fn invalid_archive_does_not_replace_binary() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("flea");
        fs::write(&destination, b"old").unwrap();
        assert!(install(b"invalid archive", &destination).is_err());
        assert_eq!(fs::read(destination).unwrap(), b"old");
    }

    #[test]
    fn release_platforms_match_packaging() {
        for (os, arch, expected) in [
            ("macos", "aarch64", "darwin-arm64"),
            ("macos", "x86_64", "darwin-amd64"),
            ("linux", "aarch64", "linux-arm64"),
            ("linux", "x86_64", "linux-amd64"),
        ] {
            assert_eq!(platform(os, arch).unwrap(), expected);
        }
        assert!(platform("windows", "x86_64").is_err());
    }

    #[test]
    fn managed_installations_are_not_replaced() {
        assert!(check_install(Path::new("/opt/homebrew/Cellar/flea/1/bin/flea")).is_err());
        assert!(check_install(Path::new("/nix/store/hash-flea/bin/flea")).is_err());
        assert!(check_install(Path::new("/home/me/.cargo/bin/flea")).is_ok());
    }

    #[test]
    fn validate_release_metadata_and_checksum() {
        assert_eq!(release_version("v1.2.3").unwrap(), "1.2.3");
        for tag in ["", "v", "../bad", "v1/other"] {
            assert!(release_version(tag).is_err());
        }
        let checksum = format!("{:x}  flea.tar.gz\n", Sha256::digest(b"archive"));
        assert!(verify_checksum(b"archive", &checksum, "flea.tar.gz").is_ok());
        assert!(verify_checksum(b"corrupt", &checksum, "flea.tar.gz").is_err());
        assert!(verify_checksum(b"archive", &checksum, "other.tar.gz").is_err());
        assert!(verify_checksum(b"archive", "", "flea.tar.gz").is_err());
    }

    #[test]
    fn replacement_preserves_old_binary_on_failure() {
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("flea");
        fs::write(&destination, b"old").unwrap();
        assert!(replace_binary(&dir.path().join("missing"), &destination).is_err());
        assert_eq!(fs::read(&destination).unwrap(), b"old");
        let source = dir.path().join("new");
        fs::write(&source, b"new").unwrap();
        replace_binary(&source, &destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"new");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }
}
