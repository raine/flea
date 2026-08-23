use crate::{
    cli::outcome::{CommandPresentation, PlainOutput},
    error::AppError,
    marketplace::MarketplaceId,
};
use clap::ValueEnum;
use serde::Serialize;

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

pub fn render_plain(
    presentation: &CommandPresentation,
    format: OutputFormat,
) -> Result<Option<String>, AppError> {
    match presentation {
        CommandPresentation::Structured => Ok(None),
        CommandPresentation::Plain(PlainOutput::AuthenticationLogin {
            marketplace,
            authenticated,
        }) if format == OutputFormat::Toon => {
            if !authenticated {
                return Err(AppError::output(
                    "authentication login output has an invalid status",
                ));
            }
            let name = match marketplace {
                MarketplaceId::Tori => "Tori",
                MarketplaceId::Vinted => "Vinted",
            };
            Ok(Some(format!("Signed in to {name}.\n")))
        }
        CommandPresentation::Plain(PlainOutput::AuthenticationLogin { .. }) => Ok(None),
        CommandPresentation::Plain(PlainOutput::Document(document)) => Ok(Some(document.clone())),
    }
}
