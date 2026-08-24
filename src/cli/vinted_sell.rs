use std::{fs, io::Read, path::PathBuf};

use clap::Args;

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            guided_sell::{GuidedSelections, GuidedSellFacts, GuidedSellRequest, GuidedVintedSell},
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
    /// JPEG, PNG, HEIC, or HEIF image path in final display order.
    #[arg(long, value_name = "PATH")]
    pub image: Vec<PathBuf>,
    /// Opaque runtime choice from an earlier guided result, as FIELD=ID.
    #[arg(long, value_name = "FIELD=ID")]
    pub select: Vec<String>,
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
        input_path: args.input,
        images: args.image,
        selections: GuidedSelections::parse(&args.select)?,
    };
    let (result, next_actions) = GuidedVintedSell::new(session, discovery_api, search_api)
        .prepare(portal, facts, &request)
        .await?;
    Ok(CommandOutcome::new(CommandData::VintedGuidedSell(result)).with_next_actions(next_actions))
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
