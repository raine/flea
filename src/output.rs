use clap::ValueEnum;
use serde::Serialize;
use serde_json::Value;

use crate::error::AppError;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum OutputFormat {
    #[default]
    Toon,
    Json,
}

pub fn render<T: Serialize>(value: &T, format: OutputFormat) -> Result<String, AppError> {
    match format {
        OutputFormat::Json => serde_json::to_string_pretty(value).map_err(|error| {
            AppError::output("failed to serialize JSON output").with_source(error)
        }),
        OutputFormat::Toon => toon_format::encode_default(value).map_err(|error| {
            AppError::output("failed to serialize TOON output").with_source(error)
        }),
    }
}

pub fn render_auth_start(data: &Value) -> Result<String, AppError> {
    let login_url = required_string(data, "login_url")?;
    let expires_at = data
        .get("expires_at_unix")
        .and_then(Value::as_u64)
        .ok_or_else(|| AppError::output("authentication start output has an invalid expiry"))?;
    let completion_command = required_string(data, "completion_command")?;

    Ok(format!(
        "Open this URL in a browser to sign in to Tori:\n\n{login_url}\n\nExpires at Unix time {expires_at}.\n\nAfter sign-in, run:\n{completion_command}\n"
    ))
}

fn required_string<'a>(data: &'a Value, field: &str) -> Result<&'a str, AppError> {
    data.get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            AppError::output(format!(
                "authentication start output has an invalid {field}"
            ))
        })
}
