use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    domain::envelope::NextAction,
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            draft::{DEFAULT_PAGE_SIZE, DraftListRequest, VintedDraftApi, VintedDrafts},
            publication::{
                ListingInput, PublicationOperation, VintedPublication, VintedPublicationApi,
            },
            readiness::VintedReadinessApi,
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
        about = "List the authenticated account's Vinted drafts",
        long_about = "Fetch one bounded page of remote Vinted drafts in newest-first order with stable IDs and concise summaries."
    )]
    List {
        /// One-based page number.
        #[arg(long, default_value_t = 1)]
        page: u32,
        /// Drafts per page, from 1 through 100.
        #[arg(long, default_value_t = DEFAULT_PAGE_SIZE)]
        limit: u16,
    },
    #[command(
        about = "Show complete remote Vinted draft state",
        long_about = "Fetch the authoritative editable draft state, including assigned photo IDs and display order, category, attributes, brand, colors, price, package, and revision metadata when available."
    )]
    Show {
        /// Numeric Vinted draft identifier.
        draft_id: String,
    },
    #[command(
        about = "Validate Vinted draft publication readiness",
        long_about = "Fetch authoritative remote draft state and report field-level local schema blockers, deterministic upstream validation errors, and account prerequisites without changing the draft."
    )]
    Validate {
        /// Numeric Vinted draft identifier.
        draft_id: String,
    },
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
        long_about = "Reuse the draft's verified remote photos by default, or replace the complete photo set when --image is passed, then complete the draft using a complete runtime-discovered JSON payload."
    )]
    Publish {
        /// Numeric Vinted draft identifier.
        draft_id: String,
        #[command(flatten)]
        values: DraftCompletionInputArgs,
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
            Self::List { .. } => "draft list",
            Self::Show { .. } => "draft show",
            Self::Validate { .. } => "draft validate",
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

#[derive(Debug, Args)]
pub struct DraftCompletionInputArgs {
    /// Complete Vinted listing JSON, or `-` for stdin.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
    /// Replace all remote photos with these images in final display order.
    #[arg(long, value_name = "PATH")]
    pub image: Vec<PathBuf>,
}

pub async fn execute_readiness(
    portal: PortalId,
    session: &dyn VintedSearchSession,
    readiness_api: &dyn VintedReadinessApi,
) -> Result<CommandOutcome, AppError> {
    let credentials = session.credentials(portal)?;
    let result = readiness_api.readiness(&credentials).await?;
    Ok(CommandOutcome::new(
        CommandData::VintedPublicationReadiness(result),
    ))
}

pub async fn execute_direct(
    portal: PortalId,
    args: PublicationInputArgs,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationApi,
    readiness_api: &dyn VintedReadinessApi,
) -> Result<CommandOutcome, AppError> {
    execute_operation(
        portal,
        PublicationOperation::Publish,
        Some(args),
        session,
        api,
        readiness_api,
    )
    .await
}

pub async fn execute_draft(
    portal: PortalId,
    command: VintedDraftCommand,
    session: &dyn VintedSearchSession,
    publication_api: &dyn VintedPublicationApi,
    draft_api: &dyn VintedDraftApi,
    readiness_api: &dyn VintedReadinessApi,
) -> Result<CommandOutcome, AppError> {
    match command {
        VintedDraftCommand::List { page, limit } => {
            let result = VintedDrafts::new(session, draft_api)
                .list(
                    portal,
                    DraftListRequest {
                        page,
                        per_page: limit,
                    },
                )
                .await?;
            return Ok(CommandOutcome::new(CommandData::VintedDraftCollection(
                result,
            )));
        }
        VintedDraftCommand::Show { draft_id } => {
            let result = VintedDrafts::new(session, draft_api)
                .show(portal, &draft_id)
                .await?;
            return Ok(CommandOutcome::new(CommandData::VintedDraft(result)));
        }
        VintedDraftCommand::Validate { draft_id } => {
            let result = VintedDrafts::new(session, draft_api)
                .validate(portal, &draft_id)
                .await?;
            return Ok(CommandOutcome::new(CommandData::VintedDraftValidation(
                result,
            )));
        }
        _ => {}
    }
    let (operation, values) = match command {
        VintedDraftCommand::List { .. }
        | VintedDraftCommand::Show { .. }
        | VintedDraftCommand::Validate { .. } => unreachable!("read commands returned above"),
        VintedDraftCommand::Create(values) => (PublicationOperation::CreateDraft, Some(values)),
        VintedDraftCommand::Update { draft_id, values } => {
            (PublicationOperation::UpdateDraft { draft_id }, Some(values))
        }
        VintedDraftCommand::Publish { draft_id, values } => (
            PublicationOperation::CompleteDraft { draft_id },
            Some(PublicationInputArgs {
                input: values.input,
                image: values.image,
            }),
        ),
        VintedDraftCommand::Delete { draft_id } => {
            (PublicationOperation::DeleteDraft { draft_id }, None)
        }
    };
    execute_operation(
        portal,
        operation,
        values,
        session,
        publication_api,
        readiness_api,
    )
    .await
}

async fn execute_operation(
    portal: PortalId,
    operation: PublicationOperation,
    values: Option<PublicationInputArgs>,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationApi,
    readiness_api: &dyn VintedReadinessApi,
) -> Result<CommandOutcome, AppError> {
    let (input, images) = match values {
        Some(values) => (Some(read_input(&values.input)?), values.image),
        None => (None, Vec::new()),
    };
    let result = VintedPublication::new(session, api, readiness_api)
        .execute(portal, operation, input, images)
        .await?;
    let next_actions = publication_next_actions(portal, result.item_id.as_deref());
    Ok(CommandOutcome::new(CommandData::VintedPublication(result)).with_next_actions(next_actions))
}

fn publication_next_actions(portal: PortalId, item_id: Option<&str>) -> Vec<NextAction> {
    item_id
        .map(|item_id| {
            vec![NextAction {
                command: format!("flea vinted --portal {portal} listing show {item_id}"),
            }]
        })
        .unwrap_or_default()
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
    fn publication_item_id_points_to_authoritative_inspection() {
        assert_eq!(
            publication_next_actions(PortalId::Fi, Some("9001"))[0].command,
            "flea vinted --portal fi listing show 9001"
        );
        assert!(publication_next_actions(PortalId::Fi, None).is_empty());
    }

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
