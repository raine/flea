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
    pub fn capability_id(&self) -> crate::marketplace::CapabilityId {
        use crate::marketplace::CapabilityId;

        match self {
            Self::Login | Self::Callback { .. } => CapabilityId::AuthLogin,
            Self::Status => CapabilityId::AuthStatus,
            Self::Logout => CapabilityId::AuthLogout,
        }
    }

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
        about = "Sign in for Vinted catalog and publication commands",
        long_about = "Set up both Vinted authentication layers: account credentials for catalog operations and the persistent interactive browser required for publication. Use --api or --browser to set up only one layer."
    )]
    Login(VintedAuthScopeArgs),
    #[command(
        about = "Show Vinted catalog and publication authentication status",
        long_about = "Validate both Vinted authentication layers and report whether catalog and publication commands are ready. Use --api or --browser to inspect only one layer."
    )]
    Status(VintedAuthScopeArgs),
    #[command(
        about = "Clear Vinted authentication state",
        long_about = "Clear both account credentials and the persistent publication browser profile. Use --api or --browser to clear only one layer."
    )]
    Logout(VintedAuthScopeArgs),
}

#[derive(Clone, Copy, Debug, Default, Args)]
pub struct VintedAuthScopeArgs {
    /// Operate only on account credentials used by catalog API requests.
    #[arg(long, conflicts_with = "browser")]
    pub api: bool,
    /// Operate only on the persistent browser used by publication requests.
    #[arg(long, conflicts_with = "api")]
    pub browser: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VintedAuthScope {
    All,
    Api,
    Browser,
}

impl VintedAuthScopeArgs {
    pub const fn scope(self) -> VintedAuthScope {
        if self.api {
            VintedAuthScope::Api
        } else if self.browser {
            VintedAuthScope::Browser
        } else {
            VintedAuthScope::All
        }
    }
}

impl VintedAuthCommand {
    pub fn capability_id(&self) -> crate::marketplace::CapabilityId {
        use crate::marketplace::CapabilityId;

        match self {
            Self::Login(_) => CapabilityId::AuthLogin,
            Self::Status(_) => CapabilityId::AuthStatus,
            Self::Logout(_) => CapabilityId::AuthLogout,
        }
    }

    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Login(_) => "auth login",
            Self::Status(_) => "auth status",
            Self::Logout(_) => "auth logout",
        }
    }
}
