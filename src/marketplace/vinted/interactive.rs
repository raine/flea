#![allow(clippy::result_large_err)]

use std::io::{self, BufRead, Read};

#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "macos")]
use std::{
    ffi::{CStr, CString},
    fs,
    os::raw::{c_char, c_void},
    path::{Path, PathBuf},
    ptr,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{domain::envelope::NextAction, error::AppError, storage::StatePaths};
#[cfg(target_os = "macos")]
use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
use crate::storage::atomic_file::{AtomicFile, AtomicFileStore, sync_directory};

const MAX_CALLBACK_URL_BYTES: usize = 8 * 1024;
fn read_callback(reader: impl Read) -> Result<String, AppError> {
    let mut reader = io::BufReader::new(reader.take(MAX_CALLBACK_URL_BYTES as u64 + 2));
    let mut callback = Vec::new();
    reader
        .read_until(b'\n', &mut callback)
        .map_err(|error| terminal_error().with_source(error))?;
    if callback.last() == Some(&b'\n') {
        callback.pop();
    }
    if callback.last() == Some(&b'\r') {
        callback.pop();
    }
    if callback.len() > MAX_CALLBACK_URL_BYTES {
        return Err(terminal_error());
    }
    let callback = String::from_utf8(callback).map_err(|_| terminal_error())?;
    if callback.is_empty() {
        return Err(terminal_error());
    }
    Ok(callback)
}

fn terminal_error() -> AppError {
    let mut error = AppError::authentication(
        "vinted_auth.callback_input_failed",
        "the Vinted callback URL could not be read from the terminal",
    );
    error.next_actions.push(retry_action());
    error
}

fn browser_launch_error(
    launcher: &str,
    error: impl std::error::Error + Send + Sync + 'static,
) -> AppError {
    let mut result = AppError::authentication(
        "vinted_auth.browser_launch_failed",
        format!(
            "the default browser could not be opened with {launcher}; verify the launcher and default browser configuration"
        ),
    )
    .with_details(serde_json::json!({ "launcher": launcher }))
    .with_source(error);
    result.next_actions.push(retry_action());
    result
}

fn retry_action() -> NextAction {
    NextAction {
        command: crate::invocation::vinted_fi("auth login"),
    }
}

#[cfg(target_os = "linux")]
pub(super) fn open_and_capture_callback(
    _paths: &StatePaths,
    login_url: &str,
    _expires_at_unix: u64,
) -> Result<String, AppError> {
    let status = std::process::Command::new("xdg-open")
        .arg(login_url)
        .status()
        .map_err(|error| browser_launch_error("xdg-open", error))?;
    if !status.success() {
        return Err(browser_launch_error(
            "xdg-open",
            std::io::Error::other(format!("xdg-open exited with status {status}")),
        ));
    }
    eprintln!("Browser opened for Vinted.");
    eprintln!(
        "Complete sign-in, copy the full vintedfr://auth?... callback URL, and paste it below."
    );
    eprint!("Vinted callback URL: ");
    io::stderr()
        .flush()
        .map_err(|error| terminal_error().with_source(error))?;
    read_callback(io::stdin().lock())
}

#[cfg(target_os = "macos")]
pub(super) fn open_and_capture_callback(
    paths: &StatePaths,
    login_url: &str,
    expires_at_unix: u64,
) -> Result<String, AppError> {
    let mut receiver = MacCallbackReceiver::prepare(paths)?;
    let result = (|| {
        let status = std::process::Command::new("/usr/bin/open")
            .arg(login_url)
            .status()
            .map_err(|error| browser_launch_error("/usr/bin/open", error))?;
        if !status.success() {
            return Err(browser_launch_error(
                "/usr/bin/open",
                std::io::Error::other(format!("open exited with status {status}")),
            ));
        }
        eprintln!("Browser opened. Finish signing in to Vinted.");
        eprintln!("Waiting for Vinted to return to Flea...");
        receiver.wait(expires_at_unix)
    })();
    let cleanup = receiver.cleanup();
    match result {
        Ok(callback) => cleanup.map(|()| callback),
        Err(error) => Err(error),
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) fn open_and_capture_callback(
    _paths: &StatePaths,
    _login_url: &str,
    _expires_at_unix: u64,
) -> Result<String, AppError> {
    let mut error = AppError::authentication(
        "vinted_auth.interactive_login_unsupported",
        "interactive Vinted browser login requires Linux or macOS",
    );
    error.next_actions.push(retry_action());
    Err(error)
}

#[cfg(target_os = "macos")]
const MAC_CALLBACK_BUNDLE_ID: &str = "fi.raine.flea.vinted-auth";

#[cfg(target_os = "macos")]
#[derive(Deserialize, Serialize)]
struct BorrowedUrlHandler {
    previous_handler: Option<String>,
}

#[cfg(target_os = "macos")]
struct MacCallbackReceiver {
    app: PathBuf,
    callback_file: PathBuf,
    recovery_file: PathBuf,
    previous_handler: Option<String>,
    installed: bool,
    _temporary: tempfile::TempDir,
}

#[cfg(target_os = "macos")]
impl MacCallbackReceiver {
    fn prepare(paths: &StatePaths) -> Result<Self, AppError> {
        let temporary = tempfile::Builder::new()
            .prefix("flea-vinted-auth-")
            .tempdir()
            .map_err(callback_receiver_error)?;
        let callback_file = temporary.path().join("callback");
        let applications = dirs::home_dir()
            .ok_or_else(|| {
                callback_receiver_error(io::Error::other("home directory is unavailable"))
            })?
            .join("Applications");
        fs::create_dir_all(&applications).map_err(callback_receiver_error)?;
        let app = applications.join("Flea Vinted Auth.app");
        let recovery_file = paths.auth_dir().join("borrowed-url-handler.json");
        recover_interrupted_receiver(&app, &recovery_file)?;
        let previous_handler = default_url_handler("vintedfr")?;
        paths.ensure().map_err(callback_receiver_error)?;
        let recovery = serde_json::to_vec(&BorrowedUrlHandler {
            previous_handler: previous_handler.clone(),
        })
        .map_err(callback_receiver_error)?;
        AtomicFile
            .write(&recovery_file, &recovery)
            .map_err(callback_receiver_error)?;
        let mut receiver = Self {
            app,
            callback_file,
            recovery_file,
            previous_handler,
            installed: false,
            _temporary: temporary,
        };
        let result = (|| {
            let script = callback_script(&receiver.callback_file);
            checked_command(std::process::Command::new("/usr/bin/osacompile").args([
                "-o",
                path_text(&receiver.app)?,
                "-e",
                &script,
            ]))?;
            let plist = receiver.app.join("Contents/Info.plist");
            checked_command(std::process::Command::new("/usr/bin/plutil").args([
                "-replace",
                "CFBundleName",
                "-string",
                "Flea Vinted Auth",
                path_text(&plist)?,
            ]))?;
            checked_command(std::process::Command::new("/usr/bin/plutil").args([
                "-replace",
                "CFBundleIdentifier",
                "-string",
                MAC_CALLBACK_BUNDLE_ID,
                path_text(&plist)?,
            ]))?;
            checked_command(std::process::Command::new("/usr/bin/plutil").args([
                "-insert",
                "CFBundleURLTypes",
                "-json",
                r#"[{"CFBundleURLName":"Flea Vinted OAuth","CFBundleURLSchemes":["vintedfr"]}]"#,
                path_text(&plist)?,
            ]))?;
            checked_command(std::process::Command::new("/usr/bin/codesign").args([
                "--force",
                "--deep",
                "--sign",
                "-",
                path_text(&receiver.app)?,
            ]))?;
            checked_command(launch_services_command().args(["-f", path_text(&receiver.app)?]))?;
            receiver.installed = true;
            set_default_url_handler("vintedfr", MAC_CALLBACK_BUNDLE_ID)?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = receiver.cleanup();
            return Err(error);
        }
        Ok(receiver)
    }

    fn wait(&self, expires_at_unix: u64) -> Result<String, AppError> {
        loop {
            if self.callback_file.exists() {
                return read_callback_file(&self.callback_file);
            }
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(callback_receiver_error)?
                .as_secs();
            if now >= expires_at_unix {
                let mut error = AppError::authentication(
                    "vinted_auth.flow_expired",
                    "browser sign-in did not finish before the Vinted login flow expired",
                );
                error.next_actions.push(retry_action());
                return Err(error);
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
    }

    fn cleanup(&mut self) -> Result<(), AppError> {
        if !self.installed && !self.app.exists() && !self.recovery_file.exists() {
            return Ok(());
        }
        if let Some(previous_handler) = &self.previous_handler {
            set_default_url_handler("vintedfr", previous_handler)?;
        }
        let mut first_error = None;
        if self.installed && self.app.exists() {
            keep_first_error(
                &mut first_error,
                checked_command(launch_services_command().args(["-u", path_text(&self.app)?])),
            );
        }
        if self.app.exists() {
            keep_first_error(
                &mut first_error,
                fs::remove_dir_all(&self.app).map_err(callback_receiver_error),
            );
        }
        if first_error.is_none() {
            keep_first_error(&mut first_error, remove_private_file(&self.recovery_file));
        }
        self.installed = false;
        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[cfg(target_os = "macos")]
fn keep_first_error(first_error: &mut Option<AppError>, result: Result<(), AppError>) {
    if let Err(error) = result {
        first_error.get_or_insert(error);
    }
}

#[cfg(target_os = "macos")]
impl Drop for MacCallbackReceiver {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(target_os = "macos")]
fn recover_interrupted_receiver(app: &Path, recovery_file: &Path) -> Result<(), AppError> {
    if !recovery_file.exists() {
        return match fs::symlink_metadata(app) {
            Ok(_) => Err(callback_receiver_error(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "the callback receiver application exists without recovery metadata: {}",
                    app.display()
                ),
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(callback_receiver_error(error)),
        };
    }
    let metadata = fs::symlink_metadata(recovery_file).map_err(callback_receiver_error)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() || metadata.len() > 4_096 {
        return Err(callback_receiver_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "the URL handler recovery record is invalid",
        )));
    }
    let record: BorrowedUrlHandler =
        serde_json::from_slice(&fs::read(recovery_file).map_err(callback_receiver_error)?)
            .map_err(callback_receiver_error)?;
    if record.previous_handler.as_deref().is_some_and(|handler| {
        handler.is_empty()
            || handler == MAC_CALLBACK_BUNDLE_ID
            || handler.chars().any(char::is_control)
    }) {
        return Err(callback_receiver_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "the previous URL handler identifier is invalid",
        )));
    }
    if let Some(previous_handler) = &record.previous_handler {
        set_default_url_handler("vintedfr", previous_handler)?;
    }
    if app.exists() {
        verify_owned_callback_app(app)?;
        checked_command(launch_services_command().args(["-u", path_text(app)?]))?;
        fs::remove_dir_all(app).map_err(callback_receiver_error)?;
    }
    remove_private_file(recovery_file)
}

#[cfg(target_os = "macos")]
fn verify_owned_callback_app(app: &Path) -> Result<(), AppError> {
    let plist = app.join("Contents/Info.plist");
    let output = std::process::Command::new("/usr/bin/plutil")
        .args([
            "-extract",
            "CFBundleIdentifier",
            "raw",
            "-o",
            "-",
            path_text(&plist)?,
        ])
        .output()
        .map_err(callback_receiver_error)?;
    let identifier = String::from_utf8_lossy(&output.stdout);
    if output.status.success() && identifier.trim() == MAC_CALLBACK_BUNDLE_ID {
        Ok(())
    } else {
        Err(callback_receiver_error(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "the callback application is not owned by Flea",
        )))
    }
}

#[cfg(target_os = "macos")]
fn remove_private_file(path: &Path) -> Result<(), AppError> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent() {
                sync_directory(parent).map_err(callback_receiver_error)?;
            }
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(callback_receiver_error(error)),
    }
}

#[cfg(target_os = "macos")]
fn callback_script(callback_file: &Path) -> String {
    let path = callback_file.to_string_lossy();
    let path = path.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "on open location theURL\n\tdo shell script \"/usr/bin/printf %s \" & quoted form of theURL & \" > \" & quoted form of \"{path}\" & \" && /bin/chmod 600 \" & quoted form of \"{path}\"\nend open location"
    )
}

#[cfg(target_os = "macos")]
fn read_callback_file(path: &Path) -> Result<String, AppError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        callback_receiver_error(error).with_details(serde_json::json!({
            "stage": "read_callback"
        }))
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.len() > MAX_CALLBACK_URL_BYTES as u64
    {
        return Err(callback_receiver_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "the callback capture file is invalid",
        )));
    }
    let file = fs::File::open(path).map_err(callback_receiver_error)?;
    read_callback(file.take(MAX_CALLBACK_URL_BYTES as u64 + 1))
}

#[cfg(target_os = "macos")]
fn callback_receiver_error(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    let mut result = AppError::authentication(
        "vinted_auth.callback_receiver_failed",
        "the temporary Vinted callback receiver could not be prepared or restored",
    )
    .with_details(serde_json::json!({
        "platform": "macos",
        "required_capability": "custom URL scheme handler"
    }))
    .with_source(error);
    result.next_actions.push(retry_action());
    result
}

#[cfg(target_os = "macos")]
fn checked_command(command: &mut std::process::Command) -> Result<(), AppError> {
    let output = command.output().map_err(callback_receiver_error)?;
    if output.status.success() {
        Ok(())
    } else {
        let message = String::from_utf8_lossy(&output.stderr);
        Err(callback_receiver_error(io::Error::other(
            message.chars().take(512).collect::<String>(),
        )))
    }
}

#[cfg(target_os = "macos")]
fn launch_services_command() -> std::process::Command {
    std::process::Command::new(
        "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
    )
}

#[cfg(target_os = "macos")]
fn path_text(path: &Path) -> Result<&str, AppError> {
    path.to_str().ok_or_else(|| {
        callback_receiver_error(io::Error::new(
            io::ErrorKind::InvalidInput,
            "a callback receiver path is not UTF-8",
        ))
    })
}

#[cfg(target_os = "macos")]
fn default_url_handler(scheme: &str) -> Result<Option<String>, AppError> {
    let scheme = create_cf_string(scheme)?;
    let handler = unsafe { LSCopyDefaultHandlerForURLScheme(scheme) };
    unsafe { CFRelease(scheme) };
    if handler.is_null() {
        return Ok(None);
    }
    let mut buffer = vec![0_i8; 4_096];
    let converted = unsafe {
        CFStringGetCString(
            handler,
            buffer.as_mut_ptr(),
            buffer.len() as isize,
            CF_STRING_ENCODING_UTF8,
        )
    };
    unsafe { CFRelease(handler) };
    if converted == 0 {
        return Err(callback_receiver_error(io::Error::new(
            io::ErrorKind::InvalidData,
            "the existing URL handler identifier is invalid",
        )));
    }
    let handler = unsafe { CStr::from_ptr(buffer.as_ptr()) }
        .to_str()
        .map_err(callback_receiver_error)?
        .to_owned();
    Ok((!handler.is_empty()).then_some(handler))
}

#[cfg(target_os = "macos")]
fn set_default_url_handler(scheme: &str, bundle_id: &str) -> Result<(), AppError> {
    let scheme = create_cf_string(scheme)?;
    let bundle_id = create_cf_string(bundle_id)?;
    let status = unsafe { LSSetDefaultHandlerForURLScheme(scheme, bundle_id) };
    unsafe {
        CFRelease(scheme);
        CFRelease(bundle_id);
    }
    if status == 0 {
        Ok(())
    } else {
        Err(callback_receiver_error(io::Error::other(format!(
            "Launch Services returned status {status}"
        ))))
    }
}

#[cfg(target_os = "macos")]
fn create_cf_string(value: &str) -> Result<CFStringRef, AppError> {
    let value = CString::new(value).map_err(callback_receiver_error)?;
    let string =
        unsafe { CFStringCreateWithCString(ptr::null(), value.as_ptr(), CF_STRING_ENCODING_UTF8) };
    if string.is_null() {
        Err(callback_receiver_error(io::Error::other(
            "a callback receiver identifier could not be allocated",
        )))
    } else {
        Ok(string)
    }
}

#[cfg(target_os = "macos")]
type CFStringRef = *const c_void;
#[cfg(target_os = "macos")]
const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

#[cfg(target_os = "macos")]
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithCString(
        allocator: *const c_void,
        bytes: *const c_char,
        encoding: u32,
    ) -> CFStringRef;
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        buffer_size: isize,
        encoding: u32,
    ) -> u8;
    fn CFRelease(value: *const c_void);
}

#[cfg(target_os = "macos")]
#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn LSCopyDefaultHandlerForURLScheme(scheme: CFStringRef) -> CFStringRef;
    fn LSSetDefaultHandlerForURLScheme(scheme: CFStringRef, handler: CFStringRef) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_input_is_bounded_and_strips_the_terminal_newline() {
        assert_eq!(
            read_callback(&b"vintedfr://auth?code=ok&state=state\r\n"[..]).unwrap(),
            "vintedfr://auth?code=ok&state=state"
        );
        let error = read_callback(&vec![b'x'; MAX_CALLBACK_URL_BYTES + 1][..]).unwrap_err();
        assert_eq!(error.code, "vinted_auth.callback_input_failed");
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi auth login"
        );
    }

    #[test]
    fn browser_errors_use_the_public_vinted_recovery_command() {
        let error = browser_launch_error("fixture", std::io::Error::other("failed"));

        assert_eq!(error.code, "vinted_auth.browser_launch_failed");
        assert_eq!(
            error.next_actions[0].command,
            "flea vinted --portal fi auth login"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn callback_recovery_allows_no_previous_url_handler() {
        let record = BorrowedUrlHandler {
            previous_handler: None,
        };
        let encoded = serde_json::to_vec(&record).unwrap();
        let decoded: BorrowedUrlHandler = serde_json::from_slice(&encoded).unwrap();

        assert!(decoded.previous_handler.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn callback_recovery_accepts_existing_string_records() {
        let decoded: BorrowedUrlHandler =
            serde_json::from_str(r#"{"previous_handler":"com.example.vinted"}"#).unwrap();

        assert_eq!(
            decoded.previous_handler.as_deref(),
            Some("com.example.vinted")
        );
    }
}
