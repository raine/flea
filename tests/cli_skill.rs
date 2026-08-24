use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const ROUTER_SKILL: &str = include_str!("../skills/flea/SKILL.md");
const TORI_SKILL: &str = include_str!("../skills/flea/tori.md");
const VINTED_SKILL: &str = include_str!("../skills/flea/vinted.md");

fn invoke(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_flea"))
        .env("HOME", home)
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("flea skill should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be UTF-8")
}

#[test]
fn skill_prints_the_compact_router() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(directory.path(), directory.path(), &["skill"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), ROUTER_SKILL);
    assert!(ROUTER_SKILL.contains("flea skill tori"));
    assert!(ROUTER_SKILL.contains("flea skill vinted"));
    assert!(ROUTER_SKILL.contains("Before searching, inspecting listings"));
    assert!(ROUTER_SKILL.split_whitespace().count() <= 200);
    assert!(!ROUTER_SKILL.contains("draft publish DRAFT_ID"));
    assert!(stderr(&output).is_empty());
}

#[test]
fn tori_skill_preserves_complete_operating_guidance() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(directory.path(), directory.path(), &["skill", "tori"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), TORI_SKILL);
    for guidance in [
        "flea tori search [QUERY] [filters]",
        "flea tori location search [NAME]",
        "flea tori category search QUERY",
        "favorite add|remove LISTING_ID",
        "saved-search list|show|create|update|delete",
        "draft validate DRAFT_ID",
        "draft publish DRAFT_ID --if-revision",
        ".data.revision",
        "listing list|show|update|dispose|delete",
        "Use `taxonomy_value` with `search --category`",
    ] {
        assert!(TORI_SKILL.contains(guidance), "missing {guidance}");
    }
    assert!(!TORI_SKILL.contains("flea vinted"));
}

#[test]
fn vinted_skill_preserves_complete_operating_guidance() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(directory.path(), directory.path(), &["skill", "vinted"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), VINTED_SKILL);
    for guidance in [
        "search requires account authentication",
        "Unqualified auth commands",
        "`--api` or `--browser` selects one layer",
        "flea vinted filter facets CODE",
        "flea vinted filter search CODE OPTION_TEXT",
        "flea vinted search [QUERY]",
        "flea vinted item show ITEM_ID",
        "multilingual",
        "portal-localized taxonomy labels",
        "--title LISTING_TITLE --description LISTING_DESCRIPTION",
        "marketplace_evidence.recommendations",
        "selection_required",
        "seller profile",
        "not a catalog filter or guaranteed item location",
        "flea vinted category compose CATEGORY_ID",
        "flea vinted draft publish DRAFT_ID",
        "flea vinted listing show ITEM_ID",
        "flea vinted listing list",
        "active and draft-associated items",
        "without relying on search indexing",
        "review-pending publication",
        "`uploaded_photo_ids`",
        "`assigned_photo_ids` for compatibility",
    ] {
        assert!(VINTED_SKILL.contains(guidance), "missing {guidance}");
    }
    assert!(!VINTED_SKILL.contains("flea tori"));
}

#[test]
fn skill_install_targets_an_explicit_agent() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let unrelated = home.join(".claude/skills/unrelated/SKILL.md");
    fs::create_dir_all(unrelated.parent().unwrap()).unwrap();
    fs::write(&unrelated, "unrelated skill\n").unwrap();

    let output = invoke(home, home, &["skill", "install", "--agent", "claude"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "installed flea skill for Claude Code at ~/.claude/skills/flea/SKILL.md\n"
    );
    assert_eq!(
        fs::read_to_string(home.join(".claude/skills/flea/SKILL.md")).unwrap(),
        ROUTER_SKILL
    );
    assert_eq!(fs::read_to_string(unrelated).unwrap(), "unrelated skill\n");
    assert!(!home.join(".codex/skills/flea/SKILL.md").exists());
}

#[test]
fn skill_install_defaults_to_detected_user_and_workspace_agents() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("home");
    let repo = directory.path().join("repo");
    fs::create_dir_all(home.join(".claude")).unwrap();
    fs::create_dir_all(home.join(".codex")).unwrap();
    fs::create_dir_all(repo.join(".opencode")).unwrap();

    let output = invoke(&home, &repo, &["skill", "install"]);
    let printed = stdout(&output);

    assert!(output.status.success(), "{}", stderr(&output));
    assert!(printed.contains("installed flea skill for Claude Code"));
    assert!(printed.contains("installed flea skill for OpenCode"));
    assert!(printed.contains("installed flea skill for Codex"));
    for path in [
        home.join(".claude/skills/flea/SKILL.md"),
        home.join(".config/opencode/skills/flea/SKILL.md"),
        home.join(".codex/skills/flea/SKILL.md"),
    ] {
        assert_eq!(fs::read_to_string(path).unwrap(), ROUTER_SKILL);
    }
}

#[test]
fn skill_install_reports_when_no_agent_is_detected() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(
        directory.path(),
        directory.path(),
        &["skill", "install", "--format", "json"],
    );
    let error = stdout(&output);

    assert_eq!(output.status.code(), Some(20));
    assert!(stderr(&output).is_empty());
    assert!(error.contains("no supported coding agents detected"));
    assert!(error.contains("use --agent to choose a target"));
}

#[test]
fn skill_install_rejects_an_unsupported_agent() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(
        directory.path(),
        directory.path(),
        &["skill", "install", "--agent", "unknown"],
    );

    assert_eq!(output.status.code(), Some(2));
    assert!(stdout(&output).is_empty());
    assert!(stderr(&output).contains("invalid value 'unknown'"));
}
