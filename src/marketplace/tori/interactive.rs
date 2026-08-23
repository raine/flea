use std::{fs, io::Read, path::Path};

use serde::Serialize;
use serde_json::json;
use url::Url;

use crate::{
    domain::envelope::NextAction,
    error::AppError,
    storage::{StatePaths, atomic_file::write_atomic},
};

const MAX_CAPTURED_CALLBACK_BYTES: u64 = 8 * 1024;
const SCHEME: &str = "fi.tori.www.6079834b9b0b741812e7e91f";

#[derive(Debug, Serialize)]
pub struct CallbackCapture {
    captured: bool,
}

pub fn prepare(paths: &StatePaths) -> Result<(), AppError> {
    paths.ensure().map_err(callback_receiver_error)?;
    clear(paths)?;
    if ["FLEA_AUTH_CALLBACK_RECEIVER", "TORI_AUTH_CALLBACK_RECEIVER"]
        .into_iter()
        .filter_map(std::env::var_os)
        .any(|value| value == "disabled")
    {
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

pub fn open_and_wait(
    paths: &StatePaths,
    login_url: &str,
    expires_at_unix: u64,
) -> Result<String, AppError> {
    open_browser(login_url)?;
    eprintln!("Browser opened. Finish signing in and choose Open Flea Auth when asked.");
    eprintln!("Waiting for Tori to return to the CLI...");

    loop {
        if paths.oauth_callback_file().exists() {
            return read(paths);
        }
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(callback_receiver_error)?
            .as_secs();
        if now >= expires_at_unix {
            return Err(retry_login_error(
                "auth.flow_expired",
                "browser sign-in did not finish before the authentication flow expired; retry flea tori auth login",
            ));
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

pub fn capture(paths: &StatePaths, callback_url: &str) -> Result<CallbackCapture, AppError> {
    let valid_scheme = callback_url.len() <= MAX_CAPTURED_CALLBACK_BYTES as usize
        && Url::parse(callback_url).is_ok_and(|url| url.scheme() == SCHEME);
    if !valid_scheme {
        return Err(callback_capture_error());
    }
    paths.ensure().map_err(callback_receiver_error)?;
    write_atomic(
        paths.oauth_callback_file().as_path(),
        callback_url.as_bytes(),
    )
    .map_err(|error| callback_capture_error().with_source(error))?;
    Ok(CallbackCapture { captured: true })
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
    retry_login_error(
        "auth.callback_not_captured",
        "the browser callback could not be captured; allow the browser to open Flea Auth, then run `flea tori auth login` again",
    )
}

fn callback_receiver_error(error: impl std::error::Error + Send + Sync + 'static) -> AppError {
    retry_login_error(
        "auth.callback_receiver_failed",
        "the browser callback receiver could not be prepared; check state directory permissions and install the operating system's desktop URL handler tools, then retry flea tori auth login",
    )
    .with_details(json!({
        "platform": std::env::consts::OS,
        "required_capability": "custom URL scheme handler"
    }))
    .with_source(error)
}

fn browser_launch_error(
    launcher: &str,
    error: impl std::error::Error + Send + Sync + 'static,
) -> AppError {
    retry_login_error(
        "auth.browser_launch_failed",
        format!("the default browser could not be opened with {launcher}; verify the launcher is installed and a default browser is configured, then retry flea tori auth login"),
    )
    .with_details(json!({ "launcher": launcher }))
    .with_source(error)
}

fn retry_login_error(code: &str, message: impl Into<String>) -> AppError {
    let mut error = AppError::authentication(code, message);
    error.next_actions.push(NextAction {
        command: crate::invocation::tori("auth login"),
    });
    error
}

#[cfg(target_os = "linux")]
fn prepare_platform_receiver(paths: &StatePaths) -> Result<(), AppError> {
    const DESKTOP_FILE: &str = "flea-auth-callback.desktop";

    let executable = std::env::current_exe().map_err(callback_receiver_error)?;
    let data_home = dirs::data_dir().ok_or_else(|| {
        callback_receiver_error(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "XDG data directory is unavailable",
        ))
    })?;
    install_linux_receiver(paths, &executable, &data_home, DESKTOP_FILE)?;

    let status = std::process::Command::new("xdg-mime")
        .args([
            "default",
            DESKTOP_FILE,
            &format!("x-scheme-handler/{SCHEME}"),
        ])
        .status()
        .map_err(callback_receiver_error)?;
    if status.success() {
        Ok(())
    } else {
        Err(callback_receiver_error(std::io::Error::other(format!(
            "xdg-mime exited with status {status}"
        ))))
    }
}

#[cfg(target_os = "linux")]
fn install_linux_receiver(
    paths: &StatePaths,
    executable: &Path,
    data_home: &Path,
    desktop_file: &str,
) -> Result<(), AppError> {
    let desktop_path = |path: &Path| {
        path.to_str()
            .filter(|path| {
                !path.is_empty() && !path.chars().any(|character| character.is_control())
            })
            .map(desktop_exec_quote)
            .ok_or_else(|| {
                callback_receiver_error(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "a callback receiver path is not valid desktop entry text",
                ))
            })
    };
    let executable = desktop_path(executable)?;
    let state_root = desktop_path(&paths.root())?;
    let contents = format!(
        "[Desktop Entry]\nType=Application\nName=Flea Auth\nNoDisplay=true\nTerminal=false\nExec={executable} tori auth callback --state-root {state_root} %u\nMimeType=x-scheme-handler/{SCHEME};\n"
    );
    let path = data_home.join("applications").join(desktop_file);
    write_atomic(&path, contents.as_bytes()).map_err(callback_receiver_error)
}

#[cfg(target_os = "linux")]
fn desktop_exec_quote(value: &str) -> String {
    let escaped = value
        .chars()
        .flat_map(|character| match character {
            '\\' | '"' | '`' | '$' => vec!['\\', character],
            _ => vec![character],
        })
        .collect::<String>();
    format!("\"{escaped}\"")
}

#[cfg(target_os = "linux")]
fn open_browser(login_url: &str) -> Result<(), AppError> {
    let status = std::process::Command::new("xdg-open")
        .arg(login_url)
        .status()
        .map_err(|error| browser_launch_error("xdg-open", error))?;
    if status.success() {
        Ok(())
    } else {
        Err(browser_launch_error(
            "xdg-open",
            std::io::Error::other(format!("xdg-open exited with status {status}")),
        ))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn prepare_platform_receiver(_paths: &StatePaths) -> Result<(), AppError> {
    Err(callback_receiver_error(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "this operating system has no callback receiver",
    )))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn open_browser(_login_url: &str) -> Result<(), AppError> {
    Err(interactive_login_unsupported())
}

#[cfg(any(not(any(target_os = "linux", target_os = "macos")), test))]
fn interactive_login_unsupported() -> AppError {
    retry_login_error(
        "auth.interactive_login_unsupported",
        "interactive browser login requires Linux or macOS; run `flea tori auth login` on a supported platform",
    )
}

#[cfg(target_os = "macos")]
fn open_browser(login_url: &str) -> Result<(), AppError> {
    let status = std::process::Command::new("/usr/bin/open")
        .arg(login_url)
        .status()
        .map_err(|error| browser_launch_error("/usr/bin/open", error))?;
    if status.success() {
        Ok(())
    } else {
        Err(browser_launch_error(
            "/usr/bin/open",
            std::io::Error::other(format!("open exited with status {status}")),
        ))
    }
}

#[cfg(target_os = "macos")]
fn prepare_platform_receiver(paths: &StatePaths) -> Result<(), AppError> {
    use std::process::Command;

    const BUNDLE_ID: &str = "fi.raine.flea.auth-callback";

    let app = paths.auth_callback_app();
    let register = |app: &Path| -> Result<(), AppError> {
        checked_command(Command::new(
            "/System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister",
        ).args(["-f", path_text(app)?]))?;
        set_default_url_handler(SCHEME, BUNDLE_ID)
    };
    if app.exists() {
        let metadata = fs::symlink_metadata(&app).map_err(callback_receiver_error)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(callback_receiver_error(std::io::Error::other(
                "callback receiver path is not an application directory",
            )));
        }
        return register(&app);
    }

    let temporary = paths
        .auth_dir()
        .join(format!(".Flea Auth.{}.app", uuid::Uuid::new_v4()));
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
            "Flea Auth",
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
            &format!(r#"[{{"CFBundleURLName":"Flea OAuth","CFBundleURLSchemes":["{SCHEME}"]}}]"#),
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

        register(&app)
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

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[test]
    fn callback_capture_recommends_public_login() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        let error = read(&paths).unwrap_err();

        assert_eq!(error.code, "auth.callback_not_captured");
        assert!(error.message.contains("`flea tori auth login`"));
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
    }

    #[test]
    fn unsupported_platform_error_identifies_public_login() {
        let error = interactive_login_unsupported();

        assert!(error.message.contains("`flea tori auth login`"));
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
    }

    #[test]
    fn captures_callback_in_a_private_bounded_file() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        let callback = format!("{SCHEME}://login?code=code&state=state");

        assert_eq!(
            serde_json::to_value(capture(&paths, &callback).unwrap()).unwrap(),
            json!({ "captured": true })
        );
        assert_eq!(read(&paths).unwrap(), callback);
    }

    #[test]
    fn rejects_callbacks_for_other_schemes() {
        let temporary = tempdir().unwrap();
        let paths = StatePaths::from_root(
            temporary.path().join("state"),
            crate::marketplace::MarketplaceContext::TORI_FI,
        );

        let error = capture(&paths, "https://example.com/callback").unwrap_err();

        assert_eq!(error.code, "auth.callback_not_captured");
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
        assert!(!paths.oauth_callback_file().exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_receiver_registers_the_custom_scheme_with_the_current_binary() {
        let temporary = tempdir().unwrap();
        let executable = Path::new("/opt/Flea $Tools/flea");

        let paths = StatePaths::from_root(
            "/home/example/.local/state/flea",
            crate::marketplace::MarketplaceContext::TORI_FI,
        );
        install_linux_receiver(
            &paths,
            executable,
            temporary.path(),
            "flea-auth-callback.desktop",
        )
        .unwrap();

        let desktop = fs::read_to_string(
            temporary
                .path()
                .join("applications/flea-auth-callback.desktop"),
        )
        .unwrap();
        assert!(desktop.contains("Type=Application"));
        assert!(desktop.contains(&format!("MimeType=x-scheme-handler/{SCHEME};")));
        assert!(desktop.contains(
            "Exec=\"/opt/Flea \\$Tools/flea\" tori auth callback --state-root \"/home/example/.local/state/flea\" %u"
        ));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_browser_failures_are_structured_and_actionable() {
        let error = browser_launch_error(
            "xdg-open",
            std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        );

        assert_eq!(error.code, "auth.browser_launch_failed");
        assert_eq!(error.details.unwrap()["launcher"], "xdg-open");
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
        assert!(error.message.contains("default browser"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn callback_script_shell_quotes_the_url_and_private_destination() {
        let script = callback_script(Path::new("/tmp/path with spaces/callback"));
        assert!(script.contains("quoted form of theURL"));
        assert!(script.contains("quoted form of \"/tmp/path with spaces/callback\""));
        assert!(script.contains("chmod 600"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_browser_failures_are_structured_and_actionable() {
        let error = browser_launch_error("/usr/bin/open", std::io::Error::other("failed"));

        assert_eq!(error.code, "auth.browser_launch_failed");
        assert_eq!(error.details.unwrap()["launcher"], "/usr/bin/open");
        assert_eq!(error.next_actions[0].command, "flea tori auth login");
    }
}
