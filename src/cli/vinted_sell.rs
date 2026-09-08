use std::{
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use clap::Args;

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    domain::envelope::{NextAction, Warning},
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            guided_sell::{
                GuidedSelections, GuidedSellFacts, GuidedSellOutput, GuidedSellRequest,
                GuidedSellStatus, GuidedVintedSell,
            },
            publication_discovery::VintedPublicationDiscoveryApi,
            search::{VintedSearchApi, VintedSearchSession},
        },
    },
};

#[derive(Debug, Args)]
pub struct VintedSellArgs {
    /// Semantic seller facts JSON, or `-` for stdin.
    #[arg(long, value_name = "PATH")]
    pub input: PathBuf,
    /// Save the validated ready listing input without overwriting an existing path.
    #[arg(long, value_name = "PATH")]
    pub output: Option<PathBuf>,
    /// JPEG, PNG, HEIC, or HEIF image path in final display order.
    #[arg(long, value_name = "PATH")]
    pub image: Vec<PathBuf>,
    /// Opaque runtime choice from an earlier guided result, as FIELD=ID.
    #[arg(long, value_name = "FIELD=ID")]
    pub select: Vec<String>,
    /// Request optional marketplace-count evidence during category discovery.
    #[arg(long)]
    pub marketplace_evidence: bool,
}

pub async fn execute(
    portal: PortalId,
    args: VintedSellArgs,
    session: &dyn VintedSearchSession,
    discovery_api: &dyn VintedPublicationDiscoveryApi,
    search_api: &dyn VintedSearchApi,
) -> Result<CommandOutcome, AppError> {
    let facts = read_facts(&args.input)?;
    let request = GuidedSellRequest {
        marketplace_evidence: args.marketplace_evidence,
        output_path: args.output,
        input_path: args.input,
        images: args.image,
        selections: GuidedSelections::parse(&args.select)?,
    };
    let (mut result, mut next_actions, mut warnings) =
        GuidedVintedSell::new(session, discovery_api, search_api)
            .prepare(portal, facts, &request)
            .await?;
    if let Some(path) = &request.output_path {
        export_ready(portal, path, &mut result, &mut next_actions)?;
    }
    if request.input_path.as_os_str() == "-" {
        warnings.push(Warning {
            code: "vinted.guided_sell.stdin_not_replayable".into(),
            message: "Stdin has been consumed. Before resuming, save the original facts to a durable JSON file and replace <saved-facts.json> in the suggested commands with its path.".into(),
        });
    }
    Ok(CommandOutcome::new(CommandData::VintedGuidedSell(result))
        .with_next_actions(next_actions)
        .with_warnings(warnings))
}

fn read_facts(path: &PathBuf) -> Result<GuidedSellFacts, AppError> {
    let bytes = if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| AppError::usage(format!("Failed to read sell facts: {error}")))?;
        bytes
    } else {
        fs::read(path).map_err(|error| {
            AppError::usage(format!(
                "Failed to read sell facts `{}`: {error}",
                path.display()
            ))
        })?
    };
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::usage("Sell facts exceed 1 MiB"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::usage(format!("Sell facts must be valid JSON: {error}")))
}

fn export_ready(
    portal: PortalId,
    path: &Path,
    result: &mut GuidedSellOutput,
    next_actions: &mut [NextAction],
) -> Result<(), AppError> {
    if result.status != GuidedSellStatus::Ready {
        return Ok(());
    }
    let mutation = result.proposed_mutation.as_mut().ok_or_else(|| {
        AppError::unexpected("A ready Vinted sell result omitted its proposed mutation")
    })?;
    let mut command = format!(
        "flea vinted --portal {portal} publish --input={}",
        quoted_path(path)?
    );
    for image in &mutation.image_paths {
        command.push_str(" --image=");
        command.push_str(&quoted_path(image)?);
    }
    let contents = serde_json::to_vec_pretty(&mutation.listing_input).map_err(|error| {
        AppError::unexpected(format!("Failed to serialize listing input: {error}"))
    })?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Stage in the destination directory, then expose the complete file exclusively.
    let save = || -> std::io::Result<()> {
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(&contents)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
        Ok(())
    };
    save().map_err(|error| {
        AppError::output(format!(
            "Failed to save listing input to `{}` without overwriting: {error}",
            path.display()
        ))
    })?;
    crate::storage::atomic_file::sync_directory(parent).map_err(|error| AppError::output(format!(
        "Listing input was saved to `{}`, but syncing its directory failed: {error}. The file may already exist; inspect it before retrying.", path.display()
    )))?;
    for action in next_actions {
        if action.command == mutation.command {
            action.command.clone_from(&command);
        }
    }
    mutation.command = command;
    Ok(())
}

fn quoted_path(path: &Path) -> Result<String, AppError> {
    let value = path.to_str().ok_or_else(|| {
        AppError::usage("Export and image paths must be valid UTF-8 for an exact publish command")
    })?;
    Ok(format!("'{}'", value.replace('\'', "'\\''")))
}
