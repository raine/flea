use clap::Parser;
use flea::{
    cli::{
        Cli, Command, category::CategoryCommand, draft::DraftCommand, listing::ListingCommand,
        search::SearchSort,
    },
    output::OutputFormat,
};

#[test]
fn every_command_leaf_parses() {
    let cases = [
        vec!["flea", "auth", "login"],
        vec!["flea", "auth", "status"],
        vec!["flea", "auth", "logout"],
        vec!["flea", "category", "search", "chairs"],
        vec!["flea", "category", "list"],
        vec!["flea", "draft", "create"],
        vec!["flea", "draft", "create", "--from-listing", "listing-1"],
        vec!["flea", "draft", "preview", "--title", "Chair"],
        vec!["flea", "draft", "show", "draft-1"],
        vec!["flea", "draft", "update", "draft-1", "--title", "Chair"],
        vec!["flea", "draft", "image", "add", "draft-1", "one.jpg"],
        vec!["flea", "draft", "image", "remove", "draft-1", "image-1"],
        vec!["flea", "draft", "validate", "draft-1"],
        vec![
            "flea",
            "draft",
            "publish",
            "draft-1",
            "--if-revision",
            "one",
        ],
        vec!["flea", "draft", "delete", "draft-1"],
        vec!["flea", "listing", "list"],
        vec!["flea", "listing", "show", "listing-1"],
        vec!["flea", "listing", "update", "listing-1", "--price", "45"],
        vec!["flea", "listing", "dispose", "listing-1"],
        vec!["flea", "listing", "delete", "listing-1"],
        vec!["flea", "item", "show", "42346404"],
        vec!["flea", "item", "show", "42346404", "--raw"],
        vec!["flea", "search", "chair"],
        vec!["flea", "search", "chair", "--area", "Helsinki,Espoo,Vantaa"],
        vec!["flea", "location", "search", "Helsinki"],
        vec!["flea", "skill"],
        vec!["flea", "skill", "install", "--agent", "claude"],
    ];

    for arguments in cases {
        Cli::try_parse_from(&arguments)
            .unwrap_or_else(|error| panic!("failed to parse {arguments:?}: {error}"));
    }
}

#[test]
fn publish_requires_an_expected_revision() {
    assert!(Cli::try_parse_from(["flea", "draft", "publish", "draft-1"]).is_err());
}

#[test]
fn parses_draft_show_expansions() {
    let cli = Cli::parse_from([
        "flea",
        "draft",
        "show",
        "draft-1",
        "--include-fields",
        "--include-options",
        "category",
    ]);
    let Command::Draft(draft) = cli.command else {
        panic!("expected draft command");
    };
    let DraftCommand::Show {
        draft_id,
        include_fields,
        include_options,
    } = draft.command
    else {
        panic!("expected draft show command");
    };
    assert_eq!(draft_id, "draft-1");
    assert!(include_fields);
    assert_eq!(include_options.as_deref(), Some("category"));

    let cli = Cli::parse_from(["flea", "draft", "show", "draft-1", "--include-options"]);
    let Command::Draft(draft) = cli.command else {
        panic!("expected draft command");
    };
    assert!(matches!(
        draft.command,
        DraftCommand::Show {
            include_options: Some(ref value),
            ..
        } if value == "*"
    ));
}

#[test]
fn parses_category_search_hierarchy_and_pagination_options() {
    let cli = Cli::parse_from([
        "flea",
        "category",
        "search",
        "tarvikkeet",
        "--path",
        "Urheilu ja ulkoilu > Pyöräily",
        "--offset",
        "20",
        "--limit",
        "10",
    ]);
    let Command::Category(category) = cli.command else {
        panic!("expected category command");
    };
    let CategoryCommand::Search {
        query,
        parent,
        path,
        offset,
        limit,
    } = category.command
    else {
        panic!("expected category search command");
    };
    assert_eq!(query, "tarvikkeet");
    assert!(parent.is_none());
    assert_eq!(path.as_deref(), Some("Urheilu ja ulkoilu > Pyöräily"));
    assert_eq!(offset, 20);
    assert_eq!(limit, 10);
}

#[test]
fn category_search_rejects_malformed_limits_and_conflicting_context() {
    for arguments in [
        vec!["flea", "category", "search", "chair", "--limit", "0"],
        vec!["flea", "category", "search", "chair", "--limit", "101"],
        vec!["flea", "category", "search", "chair", "--limit", "many"],
        vec![
            "flea",
            "category",
            "search",
            "chair",
            "--parent",
            "100",
            "--path",
            "Furniture",
        ],
    ] {
        assert!(Cli::try_parse_from(arguments).is_err());
    }
}

#[test]
fn parses_global_format_and_common_draft_input() {
    let cli = Cli::parse_from([
        "flea",
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
        "flea",
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
        "flea",
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
        "--include-facets",
        "--facet-option-limit",
        "250",
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
    assert!(search.include_facets);
    assert_eq!(search.facet_option_limit, Some(250));
}

#[test]
fn parses_concise_explicit_helsinki_area() {
    let cli = Cli::parse_from(["flea", "search", "chair", "--area", "Helsinki,Espoo,Vantaa"]);
    let Command::Search(search) = cli.command else {
        panic!("expected search command");
    };

    assert_eq!(search.area, ["Helsinki", "Espoo", "Vantaa"]);
}

#[test]
fn clap_rejects_conflicting_area_exact_location_and_coordinates() {
    for arguments in [
        vec![
            "flea",
            "search",
            "chair",
            "--area",
            "Helsinki,Espoo",
            "--location",
            "Helsinki",
        ],
        vec![
            "flea",
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
    let result = Cli::try_parse_from(["flea", "search", "chair", "--page", "1", "--page", "2"]);
    assert!(result.is_err());
}

#[test]
fn listing_tree_exposes_update_variant() {
    let cli = Cli::parse_from(["flea", "listing", "update", "listing-1", "--title", "Chair"]);
    let Command::Listing(listing) = cli.command else {
        panic!("expected listing command");
    };
    assert!(matches!(listing.command, ListingCommand::Update { .. }));
}
