# Coding-agent skill

The canonical agent guidance lives at
[`skills/tori-cli/SKILL.md`](../skills/tori-cli/SKILL.md). The CLI embeds that
repository file at compile time, and both printing and installation publish its
content unchanged.

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

Published copies use these paths:

| Agent | Installed skill path |
| --- | --- |
| Claude Code | `~/.claude/skills/tori-cli/SKILL.md` |
| OpenCode | `~/.config/opencode/skills/tori-cli/SKILL.md` |
| Codex | `~/.codex/skills/tori-cli/SKILL.md` |

These files are generated copies. Edit the canonical repository file instead.
Explicit targets are installed even when their agent directory does not exist.
Without explicit targets, the command reports an error when no supported agent
is detected. Run `tori skill install --help` for the complete command syntax.
