use std::process::{Command, Output};

use clap::Command as ClapCommand;

fn invoke(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_flea"))
        .args(args)
        .output()
        .expect("flea should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be UTF-8")
}

fn assert_no_envelope_fields(text: &str) {
    for field in [
        "ok:",
        "\"ok\"",
        "diagnostics:",
        "\"diagnostics\"",
        "warnings:",
        "next_actions:",
        "cli.invalid_usage",
    ] {
        assert!(
            !text.contains(field),
            "found envelope field {field:?} in {text:?}"
        );
    }
}

fn assert_complete_help(command: &ClapCommand, path: &str) {
    let path = format!("{path} {}", command.get_name());
    if !command.is_hide_set() {
        assert!(
            command.get_about().is_some(),
            "visible command {path} lacks a summary"
        );
        if command.get_name() != "help" {
            assert!(
                command.get_long_about().is_some(),
                "visible command {path} lacks long help"
            );
        }
        for argument in command.get_arguments().filter(|arg| !arg.is_hide_set()) {
            assert!(
                argument.get_help().is_some(),
                "visible argument {} on {path} lacks help",
                argument.get_id()
            );
        }
    }
    for child in command.get_subcommands() {
        assert_complete_help(child, &path);
    }
}

#[test]
fn every_visible_command_and_argument_has_help_metadata() {
    assert_complete_help(&flea::command(), "");
}

#[test]
fn bare_invocation_uses_clap_help_on_stderr() {
    let output = invoke(&[]);
    let out = stdout(&output);
    let err = stderr(&output);

    assert_eq!(output.status.code(), Some(2));
    assert!(out.is_empty());
    assert!(err.contains("Manage marketplace workflows with Flea"));
    assert!(err.contains("Usage: flea [OPTIONS] <COMMAND>"));
    assert!(err.contains("Commands:"));
    assert_no_envelope_fields(&err);
}

#[test]
fn top_level_help_flags_and_command_use_stdout() {
    for args in [&["--help"][..], &["-h"][..], &["help"][..]] {
        let output = invoke(args);
        let out = stdout(&output);

        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert!(stderr(&output).is_empty(), "args: {args:?}");
        assert!(out.contains("Usage: flea [OPTIONS] <COMMAND>"));
        assert!(out.contains("Commands:"));
        assert_no_envelope_fields(&out);
    }
}

#[test]
fn nested_help_flags_and_commands_use_stdout() {
    for args in [
        &["tori", "draft", "--help"][..],
        &["tori", "draft", "-h"][..],
        &["help", "tori", "draft"][..],
        &["tori", "draft", "help"][..],
        &["tori", "draft", "image", "--help"][..],
    ] {
        let output = invoke(args);
        let out = stdout(&output);

        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert!(stderr(&output).is_empty(), "args: {args:?}");
        assert!(out.contains("Usage: flea tori draft"), "args: {args:?}");
        assert_no_envelope_fields(&out);
    }
}

#[test]
fn help_tables_include_agent_oriented_summaries() {
    let top = stdout(&invoke(&["--help"]));
    assert!(top.contains("capabilities  Show the marketplace capability matrix"));
    assert!(top.contains("marketplaces  List configured marketplaces and portals"));
    assert!(top.contains("tori          Manage Tori.fi"));
    assert!(top.contains("vinted        Manage Vinted"));
    assert!(top.contains("skill         Print or install the coding-agent skill"));

    let tori = stdout(&invoke(&["tori", "--help"]));
    assert!(tori.contains("auth          Manage browser authentication"));
    assert!(tori.contains("category      Discover Tori categories (authentication required)"));
    assert!(tori.contains("draft         Preview input and manage remote drafts"));
    assert!(tori.contains("item          Inspect public Tori listings"));
    assert!(tori.contains("listing       Manage published Tori listings"));
    assert!(tori.contains("saved-search  Manage Tori saved searches and alerts"));

    let item = stdout(&invoke(&["tori", "item", "show", "--help"]));
    assert!(item.contains("Usage: flea tori item show [OPTIONS] <LISTING_ID>"));
    assert!(item.contains("Numeric marketplace listing ID returned by `flea tori search`"));
    assert!(item.contains("--raw"));

    let vinted_item = stdout(&invoke(&["vinted", "item", "show", "--help"]));
    assert!(vinted_item.contains("Usage: flea vinted item show [OPTIONS] <ITEM_ID>"));
    assert!(vinted_item.contains("seller-disclosed profile information"));
    assert!(vinted_item.contains("not a catalog filter value"));
    assert!(vinted_item.contains("exact upstream JSON body"));

    let draft = stdout(&invoke(&["tori", "draft", "--help"]));
    assert!(draft.contains("create    Create a remote draft"));
    assert!(draft.contains("preview   Preview and validate draft input locally"));
    assert!(draft.contains("image     Manage draft images"));
    assert!(draft.contains("validate  Validate publication readiness"));
    assert!(draft.contains("publish   Publish a remote draft"));

    let create = stdout(&invoke(&["tori", "draft", "create", "--help"]));
    assert!(create.contains("--from-listing <FROM_LISTING>"));
    assert!(create.contains("Authenticated seller listing ID to copy into a fresh draft"));
    assert!(create.contains("Public listings owned by another seller are not copyable"));
    assert!(create.contains("--input <PATH>"));
    assert!(create.contains("Read listing fields from a JSON object"));

    let category_search = stdout(&invoke(&["tori", "category", "search", "--help"]));
    assert!(category_search.contains("canonical taxonomy_value"));
    assert!(category_search.contains("`flea tori search --category`"));
    assert!(category_search.contains("flea tori search --category 2.93.3215.8368"));

    let search = stdout(&invoke(&["tori", "search", "--help"]));
    assert!(search.contains("--area <PLACE,PLACE,...>"));
    assert!(search.contains("Canonical taxonomy_value from `flea tori category search`"));
    assert!(search.contains("--explain <LIMIT>"));
    assert!(search.contains("at most LIMIT public item detail requests"));
    assert!(search.contains("Helsinki-area example:"));
    assert!(search.contains("--area Helsinki,Espoo,Vantaa"));

    let vinted = stdout(&invoke(&["vinted", "--help"]));
    assert!(vinted.contains("search        Search Vinted listings"));
    assert!(vinted.contains("filter        Discover and search contextual Vinted filters"));
    assert!(vinted.contains("listing       Manage Vinted account listings"));

    let vinted_listing = stdout(&invoke(&["vinted", "listing", "--help"]));
    assert!(vinted_listing.contains("update  Update an owned public listing"));
    let vinted_listing_update = stdout(&invoke(&["vinted", "listing", "update", "--help"]));
    assert!(
        vinted_listing_update
            .contains("Usage: flea vinted listing update [OPTIONS] --input <PATH> <ITEM_ID>")
    );
    assert!(vinted_listing_update.contains("Omitted fields"));
    assert!(vinted_listing_update.contains("photo order are preserved"));
    assert!(vinted_listing_update.contains(
        "Category, condition, attribute, brand, color, package, and photo changes are not supported"
    ));
    assert!(vinted_listing_update.contains("concurrent edit can be overwritten"));

    let vinted_search = stdout(&invoke(&["vinted", "search", "--help"]));
    for flag in [
        "--catalog",
        "--brand",
        "--size",
        "--status",
        "--color",
        "--material",
        "--attribute",
        "--include-facets",
        "--raw",
    ] {
        assert!(vinted_search.contains(flag), "missing {flag}");
    }
    assert!(vinted_search.contains("--price-from 10.50"));

    let vinted_category = stdout(&invoke(&["vinted", "category", "search", "--help"]));
    assert!(vinted_category.contains("localized full-path tokens"));
    assert!(vinted_category.contains("Selection is always required"));
    assert!(vinted_category.contains("Browse roots or children"));

    let vinted_filter = stdout(&invoke(&["vinted", "filter", "--help"]));
    assert!(vinted_filter.contains("list    Discover contextual Vinted catalog filters"));
    assert!(vinted_filter.contains("facets  Retrieve options for a lazy or truncated filter"));
    assert!(vinted_filter.contains("search  Search options within a contextual Vinted filter"));

    let discovery_help = [
        ("attributes", "Selection-scoped", "--input selections.json"),
        ("brands", "Category-scoped", "$CATEGORY_ID"),
        ("package-sizes", "Category-scoped", "$CATEGORY_ID"),
        ("colors", "Portal-scoped", "flea vinted category colors"),
        (
            "configuration",
            "Account-scoped",
            "flea vinted category configuration",
        ),
    ];
    for (command, scope, example) in discovery_help {
        let help = stdout(&invoke(&["vinted", "category", command, "--help"]));
        assert!(help.contains(scope), "missing scope in {command} help");
        assert!(
            help.contains("Example:"),
            "missing example in {command} help"
        );
        assert!(
            help.contains(example),
            "missing runnable example in {command} help"
        );
    }
    let compose = stdout(&invoke(&["vinted", "category", "compose", "--help"]));
    assert!(compose.contains("Primary guided entry point"));
    assert!(compose.contains("category search SEARCH_TEXT"));

    let auth = stdout(&invoke(&["tori", "auth", "--help"]));
    assert!(auth.contains("login   Sign in through the browser"));
    assert!(auth.contains("status  Show authentication status"));
    assert!(auth.contains("logout  Clear authentication state"));

    let skill = stdout(&invoke(&["skill", "--help"]));
    assert!(skill.contains("tori     Print the complete Tori operating guide"));
    assert!(skill.contains("vinted   Print the complete Vinted operating guide"));
    assert!(skill.contains("install  Install the flea router skill for coding agents"));
    assert!(skill.contains("flea skill [OPTIONS] [COMMAND]"));
}

#[test]
fn category_help_explains_authentication_requirement() {
    for args in [
        &["tori", "category", "--help"][..],
        &["tori", "category", "search", "--help"][..],
        &["tori", "category", "list", "--help"][..],
    ] {
        let output = invoke(args);
        let out = stdout(&output);

        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert!(stderr(&output).is_empty(), "args: {args:?}");
        assert!(
            out.contains("Authentication is required."),
            "args: {args:?}"
        );
        assert!(out.contains("`flea tori auth login`"), "args: {args:?}");
    }
}

#[test]
fn version_flags_use_clap_stdout_and_propagate_to_subcommands() {
    for (args, command_name) in [
        (&["--version"][..], "flea"),
        (&["-V"][..], "flea"),
        (&["tori", "draft", "--version"][..], "flea-tori-draft"),
    ] {
        let output = invoke(args);
        let out = stdout(&output);

        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert!(stderr(&output).is_empty(), "args: {args:?}");
        assert_eq!(
            out,
            format!("{command_name} {}\n", env!("CARGO_PKG_VERSION"))
        );
        assert_no_envelope_fields(&out);
    }
}

#[test]
fn unknown_root_command_uses_a_structured_usage_error() {
    let output = invoke(&["unknown-command"]);
    let out = stdout(&output);
    let err = stderr(&output);

    assert_eq!(output.status.code(), Some(2));
    assert!(err.is_empty());
    assert!(out.contains("code: cli.invalid_usage"));
    assert!(out.contains("command: \"unknown-command\""));
}
