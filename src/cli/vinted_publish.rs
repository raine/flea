use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            publication::{
                ListingInput, PublicationOperation, VintedPublication, VintedPublicationApi,
            },
            search::VintedSearchSession,
        },
    },
};

#[derive(Debug, Args)]
pub struct VintedDraftArgs {
    #[command(subcommand)]
    pub command: VintedDraftCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedDraftCommand {
    #[command(
        about = "Create a Vinted draft from a complete listing input",
        long_about = "Sanitize and upload images in argument order, then create a Vinted draft from a complete JSON payload whose category, attributes, price, and package values were discovered at runtime."
    )]
    Create(PublicationInputArgs),
    #[command(
        about = "Replace a Vinted draft from a complete listing input",
        long_about = "Sanitize and upload the complete image set, then replace a Vinted draft using a complete JSON payload. Unspecified remote values are not preserved."
    )]
    Update {
        /// Numeric Vinted draft identifier.
        draft_id: String,
        #[command(flatten)]
        values: PublicationInputArgs,
    },
    #[command(
        about = "Publish a Vinted draft from a complete listing input",
        long_about = "Sanitize and upload the complete image set, then complete a Vinted draft using a complete runtime-discovered JSON payload."
    )]
    Publish {
        /// Numeric Vinted draft identifier.
        draft_id: String,
        #[command(flatten)]
        values: PublicationInputArgs,
    },
    #[command(
        about = "Delete a Vinted draft",
        long_about = "Permanently delete the selected Vinted draft without uploading images or changing a public listing."
    )]
    Delete {
        /// Numeric Vinted draft identifier.
        draft_id: String,
    },
}

impl VintedDraftCommand {
    pub const fn telemetry_name(&self) -> &'static str {
        match self {
            Self::Create(_) => "draft create",
            Self::Update { .. } => "draft update",
            Self::Publish { .. } => "draft publish",
            Self::Delete { .. } => "draft delete",
        }
    }
}

#[derive(Debug, Args)]
pub struct PublicationInputArgs {
    /// Complete Vinted listing JSON, or `-` for stdin.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
    /// JPEG, PNG, HEIC, or HEIF image in final display order.
    #[arg(long, value_name = "PATH", required = true)]
    pub image: Vec<PathBuf>,
}

pub async fn execute_direct(
    portal: PortalId,
    args: PublicationInputArgs,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationApi,
) -> Result<CommandOutcome, AppError> {
    execute_operation(
        portal,
        PublicationOperation::Publish,
        Some(args),
        session,
        api,
    )
    .await
}

pub async fn execute_draft(
    portal: PortalId,
    command: VintedDraftCommand,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationApi,
) -> Result<CommandOutcome, AppError> {
    let (operation, values) = match command {
        VintedDraftCommand::Create(values) => (PublicationOperation::CreateDraft, Some(values)),
        VintedDraftCommand::Update { draft_id, values } => {
            (PublicationOperation::UpdateDraft { draft_id }, Some(values))
        }
        VintedDraftCommand::Publish { draft_id, values } => (
            PublicationOperation::CompleteDraft { draft_id },
            Some(values),
        ),
        VintedDraftCommand::Delete { draft_id } => {
            (PublicationOperation::DeleteDraft { draft_id }, None)
        }
    };
    execute_operation(portal, operation, values, session, api).await
}

async fn execute_operation(
    portal: PortalId,
    operation: PublicationOperation,
    values: Option<PublicationInputArgs>,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationApi,
) -> Result<CommandOutcome, AppError> {
    let (input, images) = match values {
        Some(values) => (Some(read_input(&values.input)?), values.image),
        None => (None, Vec::new()),
    };
    let result = VintedPublication::new(session, api)
        .execute(portal, operation, input, images)
        .await?;
    Ok(CommandOutcome::new(CommandData::VintedPublication(result)))
}

fn read_input(path: &PathBuf) -> Result<ListingInput, AppError> {
    let bytes = if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| {
                AppError::usage(format!("Failed to read publication input: {error}"))
            })?;
        bytes
    } else {
        fs::read(path).map_err(|error| {
            AppError::usage(format!(
                "Failed to read publication input `{}`: {error}",
                path.display()
            ))
        })?
    };
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::usage("Publication input exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        AppError::usage(format!(
            "Publication input must be valid Vinted listing JSON: {error}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn reads_complete_listing_input() {
        let path = std::env::temp_dir().join(format!(
            "flea-vinted-input-{}.json",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            r#"{"title":"Item","description":"Description","catalog_id":1,"price":"5.00","currency":"EUR","package_size_id":2}"#,
        )
        .unwrap();
        let input = read_input(&path).unwrap();
        fs::remove_file(path).unwrap();
        assert_eq!(input.catalog_id, 1);
        assert_eq!(input.currency, "EUR");
    }
}
