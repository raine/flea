use clap::Parser;
use tori::{
    cli::{Cli, Command, draft::DraftCommand, listing::ListingCommand, search::SearchSort},
    output::OutputFormat,
};

#[test]
fn every_command_leaf_parses() {
    let cases = [
        vec!["tori", "auth", "login"],
        vec!["tori", "auth", "status"],
        vec!["tori", "auth", "logout"],
        vec!["tori", "category", "search", "chairs"],
        vec!["tori", "category", "list"],
        vec!["tori", "draft", "create"],
        vec!["tori", "draft", "create", "--from-listing", "listing-1"],
        vec!["tori", "draft", "show", "draft-1"],
        vec!["tori", "draft", "update", "draft-1", "--title", "Chair"],
        vec!["tori", "draft", "image", "add", "draft-1", "one.jpg"],
        vec!["tori", "draft", "image", "remove", "draft-1", "image-1"],
        vec!["tori", "draft", "publish", "draft-1"],
        vec!["tori", "draft", "delete", "draft-1"],
        vec!["tori", "listing", "list"],
        vec!["tori", "listing", "show", "listing-1"],
        vec!["tori", "listing", "update", "listing-1", "--price", "45"],
        vec!["tori", "listing", "dispose", "listing-1"],
        vec!["tori", "listing", "delete", "listing-1"],
        vec!["tori", "item", "show", "42346404"],
        vec!["tori", "item", "show", "42346404", "--raw"],
        vec!["tori", "search", "chair"],
        vec!["tori", "search", "chair", "--area", "Helsinki,Espoo,Vantaa"],
        vec!["tori", "location", "search", "Helsinki"],
        vec!["tori", "skill"],
        vec!["tori", "skill", "install", "--agent", "claude"],
    ];

    for arguments in cases {
        Cli::try_parse_from(&arguments)
            .unwrap_or_else(|error| panic!("failed to parse {arguments:?}: {error}"));
    }
}

#[test]
fn parses_global_format_and_common_draft_input() {
    let cli = Cli::parse_from([
        "tori",
        "draft",
        "update",
        "36443414",
        "--category",
        "chair",
        "--trade-type",
        "give_away",
        "--delivery",
        "pickup",
        "--input",
        "attributes.json",
        "--format",
        "json",
    ]);

    assert_eq!(cli.format, OutputFormat::Json);
    let Command::Draft(draft) = cli.command else {
        panic!("expected draft command");
    };
    let DraftCommand::Update { draft_id, values } = draft.command else {
        panic!("expected draft update command");
    };
    assert_eq!(draft_id, "36443414");
    assert_eq!(values.category.as_deref(), Some("chair"));
    assert_eq!(values.delivery, ["pickup"]);
}

#[test]
fn listing_update_rejects_conflicting_description_inputs() {
    let result = Cli::try_parse_from([
        "tori",
        "listing",
        "update",
        "listing-1",
        "--description",
        "text",
        "--description-file",
        "description.txt",
    ]);

    assert!(result.is_err());
}

#[test]
fn parses_public_search_coordinates_facets_and_pagination() {
    let cli = Cli::parse_from([
        "tori",
        "search",
        "chair",
        "--latitude",
        "60.1699",
        "--longitude",
        "24.9384",
        "--radius-km",
        "20",
        "--facet",
        "brand=42",
        "--facet",
        "brand=84",
        "--sort",
        "price-asc",
        "--page",
        "2",
        "--limit",
        "75",
    ]);
    let Command::Search(search) = cli.command else {
        panic!("expected search command");
    };
    assert_eq!(search.query.as_deref(), Some("chair"));
    assert_eq!(search.latitude, Some(60.1699));
    assert_eq!(search.radius_km, Some(20.0));
    assert_eq!(search.facets, ["brand=42", "brand=84"]);
    assert!(matches!(search.sort, Some(SearchSort::PriceAsc)));
    assert_eq!(search.page, Some(2));
    assert_eq!(search.limit, Some(75));
}

#[test]
fn parses_concise_explicit_helsinki_area() {
    let cli = Cli::parse_from(["tori", "search", "chair", "--area", "Helsinki,Espoo,Vantaa"]);
    let Command::Search(search) = cli.command else {
        panic!("expected search command");
    };

    assert_eq!(search.area, ["Helsinki", "Espoo", "Vantaa"]);
}

#[test]
fn clap_rejects_conflicting_area_exact_location_and_coordinates() {
    for arguments in [
        vec![
            "tori",
            "search",
            "chair",
            "--area",
            "Helsinki,Espoo",
            "--location",
            "Helsinki",
        ],
        vec![
            "tori",
            "search",
            "chair",
            "--area",
            "Helsinki,Espoo",
            "--latitude",
            "60",
        ],
    ] {
        assert!(Cli::try_parse_from(arguments).is_err());
    }
}

#[test]
fn clap_rejects_duplicate_scalar_search_flags() {
    let result = Cli::try_parse_from(["tori", "search", "chair", "--page", "1", "--page", "2"]);
    assert!(result.is_err());
}

#[test]
fn listing_tree_exposes_update_variant() {
    let cli = Cli::parse_from(["tori", "listing", "update", "listing-1", "--title", "Chair"]);
    let Command::Listing(listing) = cli.command else {
        panic!("expected listing command");
    };
    assert!(matches!(listing.command, ListingCommand::Update { .. }));
}
