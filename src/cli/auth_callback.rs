use std::{fs, io::Read, path::Path};

use crate::{error::AppError, storage::StatePaths};

const MAX_CAPTURED_CALLBACK_BYTES: u64 = 8 * 1024;

pub fn prepare(paths: &StatePaths) -> Result<(), AppError> {
    paths.ensure().map_err(callback_receiver_error)?;
    clear(paths)?;
    if std::env::var_os("TORI_AUTH_CALLBACK_RECEIVER").is_some_and(|value| value == "disabled") {
        return Ok(());
    }
    prepare_platform_receiver(paths)
}

pub fn clear(paths: &StatePaths) -> Result<(), AppError> {
    match fs::remove_file(paths.oauth_callback_file()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(callback_capture_error().with_source(error)),
    }
}

pub fn read(paths: &StatePaths) -> Result<String, AppError> {
    let path = paths.oauth_callback_file();
    let metadata =
        fs::symlink_metadata(&path).map_err(|error| callback_capture_error().with_source(error))?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_CAPTURED_CALLBACK_BYTES
    {
        return Err(callback_capture_error());
    }

    let mut callback = String::new();
    fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_CAPTURED_CALLBACK_BYTES + 1)
                .read_to_string(&mut callback)
        })
        .map_err(|error| callback_capture_error().with_source(error))?;
    if callback.len() as u64 > MAX_CAPTURED_CALLBACK_BYTES {
        return Err(callback_capture_error());
    }
    let callback = callback.trim();
    if callback.is_empty() {
        return Err(callback_capture_error());
    }
    Ok(callback.to_owned())
}

fn callback_capture_error() -> AppError {
    AppError::authentication(
        "auth.callback_not_captured",
        "finish browser sign-in and allow the browser to open Tori CLI Auth, then retry the completion command",
    )
}

fn callback_receiver_error(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    AppError::authentication(
        "auth.callback_receiver_failed",
        "the Tori CLI browser callback receiver could not be prepared",
    )
    .with_source(error)
}

#[cfg(not(target_os = "macos"))]
fn prepare_platform_receiver(_paths: &StatePaths) -> Result<(), AppError> {
    Ok(())
}

#[cfg(target_os = "macos")]
fn prepare_platform_receiver(paths: &StatePaths) -> Result<(), AppError> {
    use std::process::Command;

    const SCHEME: &str = "fi.tori.www.6079834b9b0b741812e7e91f";
    const BUNDLE_ID: &str = "fi.raine.tori-cli.auth-callback";

    let app = paths.auth_callback_app();
    let temporary = paths
        .auth_dir()
        .join(format!(".Tori CLI Auth.{}.app", uuid::Uuid::new_v4()));
    let callback_path = paths.oauth_callback_file();
    let script = callback_script(&callback_path);

    let result = (|| {
        checked_command(Command::new("/usr/bin/osacompile").args([
            "-o",
            path_text(&temporary)?,
            "-e",
            &script,
        ]))?;
        let plist = temporary.join("Contents/Info.plist");
        checked_command(Command::new("/usr/bin/plutil").args([
            "-replace",
            "CFBundleName",
            "-string",
            "Tori CLI Auth",
            path_text(&plist)?,
        ]))?;
        checked_command(Command::new("/usr/bin/plutil").args([
            "-replace",
            "CFBundleIdentifier",
            "-string",
            BUNDLE_ID,
            path_text(&plist)?,
        ]))?;
        checked_command(Command::new("/usr/bin/plutil").args([
            "-insert",
            "CFBundleURLTypes",
            "-json",
            &format!(
                r#"[{{"CFBundleURLName":"Tori CLI OAuth","CFBundleURLSchemes":["{SCHEME}"]}}]"#
            ),
            path_text(&plist)?,
        ]))?;
        checked_command(Command::new("/usr/bin/codesign").args([
            "--force",
            "--deep",
            "--sign",
            "-",
            path_text(&temporary)?,
        ]))?;

        if app.exists() {
            fs::remove_dir_all(&app).map_err(callback_receiver_error)?;
        }
        fs::rename(&temporary, &app).map_err(callback_receiver_error)?;

        checked_command(Command::new(
            "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
        ).args(["-f", path_text(&app)?]))?;
        set_default_url_handler(SCHEME, BUNDLE_ID)?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

#[cfg(target_os = "macos")]
fn callback_script(callback_path: &Path) -> String {
    let path = callback_path.to_string_lossy();
    let path = path.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "on open location theURL\n\tdo shell script \"/usr/bin/printf %s \" & quoted form of theURL & \" > \" & quoted form of \"{path}\" & \" && /bin/chmod 600 \" & quoted form of \"{path}\"\nend open location"
    )
}

#[cfg(target_os = "macos")]
fn path_text(path: &Path) -> Result<&str, AppError> {
    path.to_str().ok_or_else(|| {
        callback_receiver_error(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "callback receiver path is not UTF-8",
        ))
    })
}

#[cfg(target_os = "macos")]
fn checked_command(command: &mut std::process::Command) -> Result<(), AppError> {
    let output = command.output().map_err(callback_receiver_error)?;
    if output.status.success() {
        Ok(())
    } else {
        let message = String::from_utf8_lossy(&output.stderr);
        Err(callback_receiver_error(std::io::Error::other(
            message.chars().take(512).collect::<String>(),
        )))
    }
}

#[cfg(target_os = "macos")]
fn set_default_url_handler(scheme: &str, bundle_id: &str) -> Result<(), AppError> {
    use std::{ffi::CString, os::raw::c_void, ptr};

    type CFStringRef = *const c_void;
    const UTF8: u32 = 0x0800_0100;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFStringCreateWithCString(
            allocator: *const c_void,
            bytes: *const std::os::raw::c_char,
            encoding: u32,
        ) -> CFStringRef;
        fn CFRelease(value: *const c_void);
    }
    #[link(name = "ApplicationServices", kind = "framework")]
    unsafe extern "C" {
        fn LSSetDefaultHandlerForURLScheme(scheme: CFStringRef, handler: CFStringRef) -> i32;
    }

    let scheme = CString::new(scheme).map_err(callback_receiver_error)?;
    let bundle_id = CString::new(bundle_id).map_err(callback_receiver_error)?;
    unsafe {
        let scheme_ref = CFStringCreateWithCString(ptr::null(), scheme.as_ptr(), UTF8);
        let bundle_ref = CFStringCreateWithCString(ptr::null(), bundle_id.as_ptr(), UTF8);
        if scheme_ref.is_null() || bundle_ref.is_null() {
            if !scheme_ref.is_null() {
                CFRelease(scheme_ref);
            }
            if !bundle_ref.is_null() {
                CFRelease(bundle_ref);
            }
            return Err(callback_receiver_error(std::io::Error::other(
                "could not create callback receiver identifiers",
            )));
        }
        let status = LSSetDefaultHandlerForURLScheme(scheme_ref, bundle_ref);
        CFRelease(scheme_ref);
        CFRelease(bundle_ref);
        if status != 0 {
            return Err(callback_receiver_error(std::io::Error::other(format!(
                "Launch Services returned status {status}"
            ))));
        }
    }
    Ok(())
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn callback_script_shell_quotes_the_url_and_private_destination() {
        let script = callback_script(Path::new("/tmp/path with spaces/callback"));
        assert!(script.contains("quoted form of theURL"));
        assert!(script.contains("quoted form of \"/tmp/path with spaces/callback\""));
        assert!(script.contains("chmod 600"));
    }
}
