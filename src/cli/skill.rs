use std::{
    fs,
    path::{Path, PathBuf},
};

use clap::{Args, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::error::{AppError, ExitClass};

const SKILL_NAME: &str = "flea";
const SKILL_CONTENT: &str = include_str!("../../skills/flea/SKILL.md");

#[derive(Debug, Args)]
pub struct SkillArgs {
    #[command(subcommand)]
    pub command: Option<SkillCommand>,
}

#[derive(Debug, Subcommand)]
pub enum SkillCommand {
    #[command(
        about = "Install the flea skill for coding agents",
        long_about = "Install the bundled flea skill into one or more supported coding-agent skill directories."
    )]
    Install(SkillInstallArgs),
}

#[derive(Debug, Args)]
pub struct SkillInstallArgs {
    /// Target a coding agent. Repeat to select multiple agents. Defaults to all detected agents.
    #[arg(long = "agent", value_enum)]
    pub agent: Vec<CodingAgent>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, ValueEnum)]
pub enum CodingAgent {
    Claude,
    Opencode,
    Codex,
}

struct AgentTarget {
    arg: CodingAgent,
    name: &'static str,
    user_parent: PathBuf,
    skill_dir: PathBuf,
    workspace_markers: &'static [&'static str],
}

impl AgentTarget {
    fn new(
        arg: CodingAgent,
        name: &'static str,
        user_parent: PathBuf,
        workspace_markers: &'static [&'static str],
    ) -> Self {
        let skill_dir = user_parent.join("skills").join(SKILL_NAME);
        Self {
            arg,
            name,
            user_parent,
            skill_dir,
            workspace_markers,
        }
    }

    fn is_detected(&self, cwd: &Path) -> bool {
        self.user_parent.is_dir()
            || cwd.ancestors().any(|directory| {
                self.workspace_markers
                    .iter()
                    .any(|marker| directory.join(marker).exists())
            })
    }
}

pub fn dispatch(args: SkillArgs) -> Result<Value, AppError> {
    match args.command {
        None => Ok(json!({ "document": SKILL_CONTENT })),
        Some(SkillCommand::Install(args)) => install(args),
    }
}

fn install(args: SkillInstallArgs) -> Result<Value, AppError> {
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| skill_error("cannot determine home directory"))?;
    let cwd = std::env::current_dir().map_err(|error| {
        skill_error("cannot determine the current directory").with_source(error)
    })?;
    let agents = all_agents(&home);
    let targets: Vec<&AgentTarget> = if args.agent.is_empty() {
        agents
            .iter()
            .filter(|agent| agent.is_detected(&cwd))
            .collect()
    } else {
        agents
            .iter()
            .filter(|agent| args.agent.contains(&agent.arg))
            .collect()
    };

    if targets.is_empty() {
        return Err(AppError::new(
            "skill.no_agents_detected",
            "no supported coding agents detected (expected ~/.claude, ~/.config/opencode, ~/.codex, or workspace agent config); use --agent to choose a target",
            ExitClass::Validation,
        ));
    }

    let mut document = String::new();
    for target in targets {
        let path = target.skill_dir.join("SKILL.md");
        fs::create_dir_all(&target.skill_dir).map_err(|error| {
            skill_error(format!("cannot create skill directory for {}", target.name))
                .with_source(error)
        })?;
        fs::write(&path, SKILL_CONTENT).map_err(|error| {
            skill_error(format!("cannot install skill for {}", target.name)).with_source(error)
        })?;
        document.push_str(&format!(
            "installed {SKILL_NAME} skill for {} at {}\n",
            target.name,
            shrink_home(&path, &home)
        ));
    }

    Ok(json!({ "document": document }))
}

fn all_agents(home: &Path) -> Vec<AgentTarget> {
    vec![
        AgentTarget::new(
            CodingAgent::Claude,
            "Claude Code",
            home.join(".claude"),
            &[".claude"],
        ),
        AgentTarget::new(
            CodingAgent::Opencode,
            "OpenCode",
            home.join(".config").join("opencode"),
            &[".opencode"],
        ),
        AgentTarget::new(
            CodingAgent::Codex,
            "Codex",
            home.join(".codex"),
            &[".codex"],
        ),
    ]
}

fn shrink_home(path: &Path, home: &Path) -> String {
    path.strip_prefix(home)
        .map(|relative| format!("~/{}", relative.display()))
        .unwrap_or_else(|_| path.display().to_string())
}

fn skill_error(message: impl Into<String>) -> AppError {
    AppError::new("skill.install_failed", message, ExitClass::Validation)
}
