//! Chrome native messaging installation and private local transport.

#[cfg(unix)]
mod install;
#[cfg(unix)]
mod protocol;
#[cfg(unix)]
mod socket;

use serde_json::Value;

use crate::AppError;

pub const HOST_NAME: &str = "app.flea.bridge";

pub fn setup() -> Result<Value, AppError> {
    #[cfg(unix)]
    return install::setup();
    #[cfg(not(unix))]
    Err(unsupported())
}

pub fn configured() -> bool {
    #[cfg(unix)]
    return install::configured();
    #[cfg(not(unix))]
    false
}

/// Execute once, without retrying even when the remote outcome is unknown.
pub fn request(command: Value) -> Result<Value, AppError> {
    #[cfg(unix)]
    return socket::request(command);
    #[cfg(not(unix))]
    {
        let _ = command;
        Err(unsupported())
    }
}

/// Serve native framing exclusively on standard output.
pub fn serve(origin: &str) -> Result<(), AppError> {
    #[cfg(unix)]
    return socket::serve(origin);
    #[cfg(not(unix))]
    {
        let _ = origin;
        Err(unsupported())
    }
}

#[cfg(not(unix))]
fn unsupported() -> AppError {
    AppError::usage("The browser extension supports Chrome on macOS and Linux.")
}

#[cfg(unix)]
fn failure(message: &'static str) -> AppError {
    AppError::upstream("extension.transport_failed", message)
}
