#![allow(clippy::result_large_err)]

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct ToriAuthArgs {
    #[command(subcommand)]
    pub command: ToriAuthCommand,
}

#[derive(Subcommand)]
pub enum ToriAuthCommand {
    #[command(
        about = "Sign in through the browser",
        long_about = "Open the selected marketplace sign-in flow in the default browser, wait for its callback receiver, and store account-scoped credentials."
    )]
    Login,
    #[command(hide = true)]
    Callback {
        #[arg(long, hide = true)]
        state_root: std::path::PathBuf,
        #[arg(hide = true)]
        callback_url: String,
    },
    #[command(
        about = "Show authentication status",
        long_about = "Validate whether authenticated commands are usable. The selected marketplace determines whether validation uses local expiry, an online account request, or token refresh."
    )]
    Status,
    #[command(
        about = "Clear authentication state",
        long_about = "Remove stored credentials and incomplete OAuth state for the selected marketplace and portal."
    )]
    Logout,
}

impl ToriAuthCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Login => "auth login",
            Self::Callback { .. } => "auth callback",
            Self::Status => "auth status",
            Self::Logout => "auth logout",
        }
    }
}

impl std::fmt::Debug for ToriAuthCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Login => formatter.write_str("Login"),
            Self::Callback { .. } => formatter.write_str("Callback"),
            Self::Status => formatter.write_str("Status"),
            Self::Logout => formatter.write_str("Logout"),
        }
    }
}

#[derive(Debug, Args)]
pub struct VintedAuthArgs {
    #[command(subcommand)]
    pub command: VintedAuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedAuthCommand {
    #[command(
        about = "Sign in through the browser",
        long_about = "Open the Vinted sign-in flow in the default browser, wait for its callback, and store account-scoped credentials."
    )]
    Login,
    #[command(
        about = "Show authentication status",
        long_about = "Validate the stored Vinted session with local expiry and an online account request."
    )]
    Status,
    #[command(
        about = "Clear authentication state",
        long_about = "Remove stored Vinted credentials for the selected portal."
    )]
    Logout,
}

impl VintedAuthCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Login => "auth login",
            Self::Status => "auth status",
            Self::Logout => "auth logout",
        }
    }
}
