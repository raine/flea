use clap::Parser;
use tori::{
    cli::{Cli, Command, draft::DraftCommand, listing::ListingCommand},
    output::OutputFormat,
};

#[test]
fn every_command_leaf_parses() {
    let cases = [
        vec!["tori", "auth", "login"],
        vec!["tori", "auth", "start"],
        vec!["tori", "auth", "complete", "flow-1"],
        vec![
            "tori",
            "auth",
            "complete",
            "flow-1",
            "tori://oauth?code=x&state=y",
        ],
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
fn listing_tree_exposes_update_variant() {
    let cli = Cli::parse_from(["tori", "listing", "update", "listing-1", "--title", "Chair"]);
    let Command::Listing(listing) = cli.command else {
        panic!("expected listing command");
    };
    assert!(matches!(listing.command, ListingCommand::Update { .. }));
}
