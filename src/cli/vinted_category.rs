use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};
use serde_json::Value;

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            publication_discovery::{
                DiscoveryRequest, VintedPublicationDiscoveryApi, validate_request,
            },
            search::VintedSearchSession,
        },
    },
};

#[derive(Debug, Args)]
pub struct VintedCategoryArgs {
    #[command(subcommand)]
    pub command: VintedCategoryCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedCategoryCommand {
    #[command(
        about = "List the Vinted publication catalog tree",
        long_about = "Fetch the authenticated minimized Vinted catalog tree used by the publication form."
    )]
    List,
    #[command(
        about = "Search Vinted publication categories",
        long_about = "Search authenticated Vinted publication categories by portal-localized keyword."
    )]
    Search {
        /// Portal-localized category search text.
        keyword: String,
    },
    #[command(
        about = "Discover layered Vinted category attributes",
        long_about = "Post a JSON array of selected category attributes and return the next layered attribute configuration. Include the category selection and repeat after each parent selection."
    )]
    Attributes {
        /// JSON selection array, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: PathBuf,
    },
    #[command(
        about = "Search brands valid for a Vinted category",
        long_about = "Fetch minimized brand choices scoped to a runtime Vinted category ID and optional search text."
    )]
    Brands {
        /// Runtime category ID.
        category_id: u64,
        /// Optional brand search text.
        #[arg(default_value = "")]
        keyword: String,
    },
    #[command(
        about = "List Vinted publication colors",
        long_about = "Fetch the authenticated color choices exposed to the Vinted publication form."
    )]
    Colors,
    #[command(
        about = "Show Vinted publication configuration",
        long_about = "Fetch upload session, price limits, image limits, measurements, and other runtime publication configuration."
    )]
    Configuration,
    #[command(
        about = "List package sizes for a Vinted category",
        long_about = "Fetch shipping package sizes and optional parcel measurement configuration for a runtime category ID."
    )]
    PackageSizes {
        /// Runtime category ID.
        category_id: u64,
    },
}

impl VintedCategoryCommand {
    pub const fn telemetry_name(&self) -> &'static str {
        match self {
            Self::List => "category list",
            Self::Search { .. } => "category search",
            Self::Attributes { .. } => "category attributes",
            Self::Brands { .. } => "category brands",
            Self::Colors => "category colors",
            Self::Configuration => "category configuration",
            Self::PackageSizes { .. } => "category package-sizes",
        }
    }
}

pub async fn execute(
    portal: PortalId,
    command: VintedCategoryCommand,
    session: &dyn VintedSearchSession,
    api: &dyn VintedPublicationDiscoveryApi,
) -> Result<CommandOutcome, AppError> {
    let request = match command {
        VintedCategoryCommand::List => DiscoveryRequest::Catalogs,
        VintedCategoryCommand::Search { keyword } => DiscoveryRequest::SearchCatalog { keyword },
        VintedCategoryCommand::Attributes { input } => DiscoveryRequest::Attributes {
            selections: read_json(&input)?,
        },
        VintedCategoryCommand::Brands {
            category_id,
            keyword,
        } => DiscoveryRequest::Brands {
            category_id,
            keyword,
        },
        VintedCategoryCommand::Colors => DiscoveryRequest::Colors,
        VintedCategoryCommand::Configuration => DiscoveryRequest::Configuration,
        VintedCategoryCommand::PackageSizes { category_id } => {
            DiscoveryRequest::PackageSizes { category_id }
        }
    };
    validate_request(&request)?;
    let credentials = session.credentials(portal)?;
    let response = api.execute(&credentials, &request).await?;
    Ok(CommandOutcome::new(CommandData::Raw(response)))
}

fn read_json(path: &PathBuf) -> Result<Value, AppError> {
    let bytes = if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| AppError::usage(format!("Failed to read attributes: {error}")))?;
        bytes
    } else {
        fs::read(path).map_err(|error| {
            AppError::usage(format!(
                "Failed to read attributes `{}`: {error}",
                path.display()
            ))
        })?
    };
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::usage("Attribute input exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::usage(format!("Attributes must be valid JSON: {error}")))
}
