use std::process::{Command, Output};

use clap::{Command as ClapCommand, CommandFactory};
use flea::cli::Cli;

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
    assert_complete_help(&Cli::command(), "");
}

#[test]
fn bare_invocation_uses_clap_help_on_stderr() {
    let output = invoke(&[]);
    let out = stdout(&output);
    let err = stderr(&output);

    assert_eq!(output.status.code(), Some(2));
    assert!(out.is_empty());
    assert!(err.contains("Manage Tori.fi listing workflows with Flea"));
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
        &["draft", "--help"][..],
        &["draft", "-h"][..],
        &["help", "draft"][..],
        &["draft", "help"][..],
        &["draft", "image", "--help"][..],
    ] {
        let output = invoke(args);
        let out = stdout(&output);

        assert_eq!(output.status.code(), Some(0), "args: {args:?}");
        assert!(stderr(&output).is_empty(), "args: {args:?}");
        assert!(out.contains("Usage: flea draft"), "args: {args:?}");
        assert_no_envelope_fields(&out);
    }
}

#[test]
fn help_tables_include_agent_oriented_summaries() {
    let top = stdout(&invoke(&["--help"]));
    assert!(top.contains("auth      Manage browser authentication"));
    assert!(top.contains("category  Discover Tori category machine values"));
    assert!(top.contains("draft     Preview input and manage remote drafts"));
    assert!(top.contains("item      Inspect public marketplace listings"));
    assert!(top.contains("listing   Manage published listings"));
    assert!(top.contains("skill     Print or install the coding-agent skill"));

    let item = stdout(&invoke(&["item", "show", "--help"]));
    assert!(item.contains("Usage: flea item show [OPTIONS] <LISTING_ID>"));
    assert!(item.contains("Numeric marketplace listing ID returned by `flea search`"));
    assert!(item.contains("--raw"));

    let draft = stdout(&invoke(&["draft", "--help"]));
    assert!(draft.contains("create    Create a remote draft"));
    assert!(draft.contains("preview   Preview and validate draft input locally"));
    assert!(draft.contains("image     Manage draft images"));
    assert!(draft.contains("validate  Validate publication readiness"));
    assert!(draft.contains("publish   Publish a remote draft"));

    let create = stdout(&invoke(&["draft", "create", "--help"]));
    assert!(create.contains("--from-listing <FROM_LISTING>"));
    assert!(create.contains("Authenticated seller listing ID to copy into a fresh draft"));
    assert!(create.contains("Public listings owned by another seller are not copyable"));
    assert!(create.contains("--input <PATH>"));
    assert!(create.contains("Read listing fields from a JSON object"));

    let category_search = stdout(&invoke(&["category", "search", "--help"]));
    assert!(category_search.contains("canonical taxonomy_value"));
    assert!(category_search.contains("`flea search --category`"));
    assert!(category_search.contains("flea search --category 2.93.3215.8368"));

    let search = stdout(&invoke(&["search", "--help"]));
    assert!(search.contains("--area <PLACE,PLACE,...>"));
    assert!(search.contains("Canonical taxonomy_value from `flea category search`"));
    assert!(search.contains("--explain <LIMIT>"));
    assert!(search.contains("at most LIMIT public item detail requests"));
    assert!(search.contains("Helsinki-area example:"));
    assert!(search.contains("--area Helsinki,Espoo,Vantaa"));

    let auth = stdout(&invoke(&["auth", "--help"]));
    assert!(auth.contains("login   Sign in through the browser"));
    assert!(auth.contains("status  Show authentication status"));
    assert!(auth.contains("logout  Clear authentication state"));

    let skill = stdout(&invoke(&["skill", "--help"]));
    assert!(skill.contains("install  Install the flea skill for coding agents"));
    assert!(skill.contains("flea skill [OPTIONS] [COMMAND]"));
}

#[test]
fn version_flags_use_clap_stdout_and_propagate_to_subcommands() {
    for (args, command_name) in [
        (&["--version"][..], "flea"),
        (&["-V"][..], "flea"),
        (&["draft", "--version"][..], "flea-draft"),
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
fn invalid_parser_usage_uses_clap_stderr() {
    let output = invoke(&["unknown-command"]);
    let out = stdout(&output);
    let err = stderr(&output);

    assert_eq!(output.status.code(), Some(2));
    assert!(out.is_empty());
    assert!(err.contains("error: unrecognized subcommand 'unknown-command'"));
    assert!(err.contains("Usage: flea [OPTIONS] <COMMAND>"));
    assert_no_envelope_fields(&err);
}
