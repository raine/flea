# Coding-agent skill

The CLI bundles the agent-oriented guidance from
[`skills/tori-cli/SKILL.md`](../skills/tori-cli/SKILL.md). The repository file is
the source used at compile time.

Print the embedded skill Markdown for inspection or custom integrations:

```sh
tori skill
```

Install it for every detected supported coding agent:

```sh
tori skill install
```

Detection covers Claude Code, OpenCode, and Codex user directories, plus their
workspace configuration markers. Select explicit targets with the repeatable
`--agent` option:

```sh
tori skill install --agent claude
tori skill install --agent opencode
tori skill install --agent codex
tori skill install --agent claude --agent codex
```

Explicit targets are installed even when their agent directory does not exist.
Without explicit targets, the command reports an error when no supported agent
is detected. Run `tori skill install --help` for the complete command syntax.
