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

pub fn render_auth_login(data: &Value) -> Result<String, AppError> {
    if data.get("authenticated").and_then(Value::as_bool) != Some(true) {
        return Err(AppError::output(
            "authentication login output has an invalid status",
        ));
    }
    Ok("Signed in to Tori.\n".to_owned())
}

pub fn render_skill(data: &Value) -> Result<String, AppError> {
    data.get("document")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| AppError::output("skill output has an invalid document"))
}
