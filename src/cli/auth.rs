use clap::{Args, Subcommand};
use serde::Serialize;
use serde_json::Value;

use crate::error::AppError;

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Serialize, Subcommand)]
#[serde(tag = "operation", rename_all = "snake_case")]
pub enum AuthCommand {
    Start,
    Complete {
        flow_id: String,
        callback_url: String,
    },
    Status,
    Logout,
}

pub fn dispatch(command: AuthArgs) -> Result<Value, AppError> {
    unavailable("auth", command.command)
}

fn unavailable<T: Serialize>(name: &str, command: T) -> Result<Value, AppError> {
    let details =
        serde_json::to_value(command).map_err(|error| AppError::output(error.to_string()))?;
    Err(AppError::protocol_unavailable(name, details))
}
