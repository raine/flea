use std::{fs, io::Read, path::PathBuf};

use clap::{Args, Subcommand};
use serde_json::Value;

use crate::{
    cli::outcome::{CommandData, CommandOutcome},
    error::AppError,
    marketplace::{
        PortalId,
        vinted::{
            composer::{
                PublicationCategoryCollection, VintedPublicationComposer, categories_for_search,
            },
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
        about = "Compose a complete Vinted publication form",
        long_about = "Combine the selected category with runtime attributes, brands, colors, price configuration, and package sizes. Optional partial or complete ListingInput JSON confirms seller facts and enables payload validation."
    )]
    Compose {
        /// Runtime leaf category ID.
        category_id: u64,
        /// Partial or complete ListingInput JSON, or `-` for stdin.
        #[arg(long, value_name = "PATH")]
        input: Option<PathBuf>,
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
            Self::Compose { .. } => "category compose",
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
    if let VintedCategoryCommand::Compose { category_id, input } = command {
        let supplied = input.as_ref().map(read_json).transpose()?;
        let composer = VintedPublicationComposer::new(session, api)
            .compose(portal, category_id, supplied)
            .await?;
        return Ok(CommandOutcome::new(CommandData::VintedComposer(composer)));
    }

    let normalize_categories = matches!(command, VintedCategoryCommand::Search { .. });
    let request = match command {
        VintedCategoryCommand::List => DiscoveryRequest::Catalogs,
        VintedCategoryCommand::Search { keyword } => DiscoveryRequest::SearchCatalog { keyword },
        VintedCategoryCommand::Compose { .. } => unreachable!("handled above"),
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
    if normalize_categories {
        let catalogs = api
            .execute(&credentials, &DiscoveryRequest::Catalogs)
            .await?;
        let categories = categories_for_search(&response, &catalogs);
        let count = categories.len();
        Ok(CommandOutcome::new(CommandData::VintedCategories(
            PublicationCategoryCollection { categories, count },
        )))
    } else {
        Ok(CommandOutcome::new(CommandData::Raw(response)))
    }
}

fn read_json(path: &PathBuf) -> Result<Value, AppError> {
    let bytes = if path.as_os_str() == "-" {
        let mut bytes = Vec::new();
        std::io::stdin()
            .lock()
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|error| AppError::usage(format!("Failed to read JSON input: {error}")))?;
        bytes
    } else {
        fs::read(path).map_err(|error| {
            AppError::usage(format!(
                "Failed to read JSON input `{}`: {error}",
                path.display()
            ))
        })?
    };
    if bytes.len() > 1024 * 1024 {
        return Err(AppError::usage("JSON input exceeds 1 MiB"));
    }
    serde_json::from_slice(&bytes)
        .map_err(|error| AppError::usage(format!("Input must be valid JSON: {error}")))
}
