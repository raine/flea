use clap::{Args, Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

use crate::marketplace::vinted::search::{
    AttributeSelection, DecimalAmount, FilterContextInput, FilterRequest, SearchRequest, SearchSort,
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "kebab-case")]
pub enum VintedSearchSort {
    Relevance,
    Newest,
    PriceAsc,
    PriceDesc,
}

impl From<VintedSearchSort> for SearchSort {
    fn from(value: VintedSearchSort) -> Self {
        match value {
            VintedSearchSort::Relevance => Self::Relevance,
            VintedSearchSort::Newest => Self::Newest,
            VintedSearchSort::PriceAsc => Self::PriceAsc,
            VintedSearchSort::PriceDesc => Self::PriceDesc,
        }
    }
}

#[derive(Debug, Args)]
pub struct VintedSearchContextArgs {
    /// Minimum listing price in euros, with up to two decimal places.
    #[arg(long)]
    pub price_from: Option<DecimalAmount>,

    /// Maximum listing price in euros, with up to two decimal places.
    #[arg(long)]
    pub price_to: Option<DecimalAmount>,

    /// Result ordering.
    #[arg(long, value_enum)]
    pub sort: Option<VintedSearchSort>,

    /// One-indexed result page.
    #[arg(long)]
    pub page: Option<usize>,

    /// Results per page, from 1 through 96.
    #[arg(long)]
    pub limit: Option<usize>,

    /// Catalog or category option IDs. Repeat the option or separate IDs with commas.
    #[arg(long, value_delimiter = ',')]
    pub catalog: Vec<String>,

    /// Brand option IDs. Repeat the option or separate IDs with commas.
    #[arg(long, value_delimiter = ',')]
    pub brand: Vec<String>,

    /// Size option IDs. Repeat the option or separate IDs with commas.
    #[arg(long, value_delimiter = ',')]
    pub size: Vec<String>,

    /// Condition option IDs, encoded with Vinted's status filter code.
    #[arg(long, visible_alias = "condition", value_delimiter = ',')]
    pub status: Vec<String>,

    /// Color option IDs. Repeat the option or separate IDs with commas.
    #[arg(long, value_delimiter = ',')]
    pub color: Vec<String>,

    /// Material option IDs. Repeat the option or separate IDs with commas.
    #[arg(long, value_delimiter = ',')]
    pub material: Vec<String>,

    /// Dynamic filter selection as CODE=ID[,ID...]. Repeat for additional codes.
    #[arg(long = "attribute", value_name = "CODE=ID,ID")]
    pub attributes: Vec<AttributeSelection>,
}

impl VintedSearchContextArgs {
    fn into_input(self, query: Option<String>) -> FilterContextInput {
        FilterContextInput {
            query,
            price_from: self.price_from,
            price_to: self.price_to,
            sort: self.sort.map(Into::into),
            page: self.page,
            limit: self.limit,
            catalog: self.catalog,
            brand: self.brand,
            size: self.size,
            status: self.status,
            color: self.color,
            material: self.material,
            attributes: self.attributes,
        }
    }
}

#[derive(Debug, Args)]
pub struct VintedSearchArgs {
    /// Filter this upstream page by disclosed seller country: FI, Finland, or Suomi.
    #[arg(long, conflicts_with = "raw")]
    pub seller_country: Option<String>,

    /// Maximum reported starting shipping quote in EUR. Unknown and pickup-only excluded.
    #[arg(long, conflicts_with = "raw")]
    pub shipping_to: Option<DecimalAmount>,

    /// Fetch disclosed seller country (not a guaranteed shipping origin).
    #[arg(long, conflicts_with = "raw")]
    pub include_seller: bool,

    /// Fetch contextual shipping quotes, not guaranteed checkout prices.
    #[arg(long, conflicts_with = "raw")]
    pub include_shipping: bool,

    /// Free-text marketplace query. May be omitted to browse listings.
    pub query: Option<String>,

    #[command(flatten)]
    pub context: VintedSearchContextArgs,

    /// Fetch contextual filter metadata with the same search context.
    #[arg(long, conflicts_with = "raw")]
    pub include_facets: bool,

    /// Include hidden contextual filters in facet output.
    #[arg(long, requires = "include_facets")]
    pub include_hidden: bool,

    /// Maximum normalized options returned for each filter.
    #[arg(long, requires = "include_facets")]
    pub option_limit: Option<usize>,

    /// Return the exact upstream JSON body inside the standard output envelope.
    #[arg(long)]
    pub raw: bool,
}

impl From<VintedSearchArgs> for SearchRequest {
    fn from(args: VintedSearchArgs) -> Self {
        let input = args.context.into_input(args.query);
        Self {
            enrichment: crate::marketplace::vinted::search::enrichment::Options {
                seller_country: args.seller_country,
                shipping_to: args.shipping_to,
                include_seller: args.include_seller,
                include_shipping: args.include_shipping,
            },
            query: input.query,
            price_from: input.price_from,
            price_to: input.price_to,
            sort: input.sort,
            page: input.page,
            limit: input.limit,
            catalog: input.catalog,
            brand: input.brand,
            size: input.size,
            status: input.status,
            color: input.color,
            material: input.material,
            attributes: input.attributes,
            include_facets: args.include_facets,
            include_hidden: args.include_hidden,
            option_limit: args.option_limit,
            raw: args.raw,
        }
    }
}

#[derive(Debug, Args)]
pub struct VintedFilterArgs {
    #[command(subcommand)]
    pub command: VintedFilterCommand,
}

#[derive(Debug, Subcommand)]
pub enum VintedFilterCommand {
    #[command(
        about = "Discover contextual Vinted catalog filters",
        long_about = "Discover filter codes, metadata, selected state, and available options for a catalog search context.",
        after_long_help = "Examples:\n  flea vinted filter list\n  flea vinted filter list --query takki --catalog 123"
    )]
    List {
        /// Catalog text used to determine contextual filters.
        #[arg(long)]
        query: Option<String>,

        #[command(flatten)]
        context: VintedSearchContextArgs,

        /// Include filters marked hidden by Vinted.
        #[arg(long)]
        include_hidden: bool,

        /// Maximum normalized options returned for each filter.
        #[arg(long)]
        option_limit: Option<usize>,

        /// Return the exact upstream JSON body.
        #[arg(long)]
        raw: bool,
    },
    #[command(
        about = "Retrieve options for a lazy or truncated filter",
        long_about = "Retrieve a filter's facet options from the lazy facets endpoint using the same catalog context as item search.",
        after_long_help = "Example:\n  flea vinted filter facets brand --query takki --catalog 123"
    )]
    Facets {
        /// Filter code returned by `vinted filter list`.
        code: String,

        /// Catalog text used to determine contextual options.
        #[arg(long)]
        query: Option<String>,

        #[command(flatten)]
        context: VintedSearchContextArgs,

        /// Maximum normalized options returned.
        #[arg(long)]
        option_limit: Option<usize>,

        /// Return the exact upstream JSON body.
        #[arg(long)]
        raw: bool,
    },
    #[command(
        about = "Search options within a contextual Vinted filter",
        long_about = "Search option labels for a filter code, such as brand, while preserving the surrounding catalog search context.",
        after_long_help = "Example:\n  flea vinted filter search brand Marimekko --query mekko"
    )]
    Search {
        /// Filter code returned by `vinted filter list`.
        code: String,

        /// Text used to search options within the filter.
        text: String,

        /// Catalog text used to determine contextual options.
        #[arg(long)]
        query: Option<String>,

        #[command(flatten)]
        context: VintedSearchContextArgs,

        /// Maximum normalized options returned.
        #[arg(long)]
        option_limit: Option<usize>,

        /// Return the exact upstream JSON body.
        #[arg(long)]
        raw: bool,
    },
}

impl VintedFilterCommand {
    pub fn telemetry_name(&self) -> &'static str {
        match self {
            Self::List { .. } => "filter list",
            Self::Facets { .. } => "filter facets",
            Self::Search { .. } => "filter search",
        }
    }
}

impl From<VintedFilterCommand> for FilterRequest {
    fn from(command: VintedFilterCommand) -> Self {
        match command {
            VintedFilterCommand::List {
                query,
                context,
                include_hidden,
                option_limit,
                raw,
            } => Self::List {
                context: context.into_input(query),
                include_hidden,
                option_limit,
                raw,
            },
            VintedFilterCommand::Facets {
                code,
                query,
                context,
                option_limit,
                raw,
            } => Self::Facets {
                code,
                context: context.into_input(query),
                option_limit,
                raw,
            },
            VintedFilterCommand::Search {
                code,
                text,
                query,
                context,
                option_limit,
                raw,
            } => Self::Search {
                code,
                text,
                context: context.into_input(query),
                option_limit,
                raw,
            },
        }
    }
}
