use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

fn invoke(home: &Path, cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_tori"))
        .env("HOME", home)
        .current_dir(cwd)
        .args(args)
        .output()
        .expect("tori skill should run")
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout should be UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr should be UTF-8")
}

#[test]
fn skill_prints_the_project_skill_source() {
    let directory = tempfile::tempdir().unwrap();
    let output = invoke(directory.path(), directory.path(), &["skill"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(stdout(&output), include_str!("../skills/tori-cli/SKILL.md"));
    assert!(stderr(&output).is_empty());
}

#[test]
fn skill_install_targets_an_explicit_agent() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path();
    let output = invoke(home, home, &["skill", "install", "--agent", "claude"]);

    assert!(output.status.success(), "{}", stderr(&output));
    assert_eq!(
        stdout(&output),
        "installed tori-cli skill for Claude Code at ~/.claude/skills/tori-cli/SKILL.md\n"
    );
    assert_eq!(
        fs::read_to_string(home.join(".claude/skills/tori-cli/SKILL.md")).unwrap(),
        include_str!("../skills/tori-cli/SKILL.md")
    );
    assert!(!home.join(".codex/skills/tori-cli/SKILL.md").exists());
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
    assert!(printed.contains("installed tori-cli skill for Claude Code"));
    assert!(printed.contains("installed tori-cli skill for OpenCode"));
    assert!(printed.contains("installed tori-cli skill for Codex"));
    assert!(home.join(".claude/skills/tori-cli/SKILL.md").exists());
    assert!(
        home.join(".config/opencode/skills/tori-cli/SKILL.md")
            .exists()
    );
    assert!(home.join(".codex/skills/tori-cli/SKILL.md").exists());
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
