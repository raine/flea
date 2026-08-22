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
    data.get("expires_at_unix")
        .and_then(Value::as_u64)
        .ok_or_else(|| AppError::output("authentication start output has an invalid expiry"))?;
    let completion_command = required_string(data, "completion_command")?;

    Ok(format!(
        "Sign in to Tori\n\n1. Open this URL:\n\n{login_url}\n\n2. Finish signing in.\n3. When the browser asks, choose Open ToriAuthHelper.app. The Vend tab may keep showing ‘Kirjaudutaan’; you can close it after the helper opens.\n4. Return here and run:\n\n{completion_command}\n\nComplete these steps within 10 minutes.\n"
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
