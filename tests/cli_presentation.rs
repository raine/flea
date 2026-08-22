use std::process::{Command, Output};

fn invoke(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tori"))
        .args(args)
        .output()
        .expect("tori should run")
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

#[test]
fn bare_invocation_uses_clap_help_on_stderr() {
    let output = invoke(&[]);
    let out = stdout(&output);
    let err = stderr(&output);

    assert_eq!(output.status.code(), Some(2));
    assert!(out.is_empty());
    assert!(err.contains("Agent CLI for Tori.fi listing workflows"));
    assert!(err.contains("Usage: tori [OPTIONS] <COMMAND>"));
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
        assert!(out.contains("Usage: tori [OPTIONS] <COMMAND>"));
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
        assert!(out.contains("Usage: tori draft"), "args: {args:?}");
        assert_no_envelope_fields(&out);
    }
}

#[test]
fn version_flags_use_clap_stdout_and_propagate_to_subcommands() {
    for (args, command_name) in [
        (&["--version"][..], "tori"),
        (&["-V"][..], "tori"),
        (&["draft", "--version"][..], "tori-draft"),
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
    assert!(err.contains("Usage: tori [OPTIONS] <COMMAND>"));
    assert_no_envelope_fields(&err);
}
