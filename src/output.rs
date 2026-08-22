use std::ffi::OsString;

use clap::ValueEnum;
use serde::Serialize;

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
        OutputFormat::Json => {
            serde_json::to_string_pretty(value).map_err(|error| AppError::output(error.to_string()))
        }
        OutputFormat::Toon => {
            toon_format::encode_default(value).map_err(|error| AppError::output(error.to_string()))
        }
    }
}

pub fn format_from_args<I, T>(args: I) -> Option<OutputFormat>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let mut args = args.into_iter().map(Into::into);
    while let Some(argument) = args.next() {
        if argument == "--format" {
            return args.next().and_then(|value| match value.to_str() {
                Some("json") => Some(OutputFormat::Json),
                Some("toon") => Some(OutputFormat::Toon),
                _ => None,
            });
        }
        if let Some(value) = argument
            .to_str()
            .and_then(|value| value.strip_prefix("--format="))
        {
            return match value {
                "json" => Some(OutputFormat::Json),
                "toon" => Some(OutputFormat::Toon),
                _ => None,
            };
        }
    }
    None
}
