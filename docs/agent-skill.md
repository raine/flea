# Coding-agent skill

The canonical agent guidance lives at
[`skills/flea/SKILL.md`](../skills/flea/SKILL.md). The CLI embeds that
repository file at compile time, and both printing and installation publish its
content unchanged.

Print the embedded skill Markdown for inspection or custom integrations:

```sh
flea skill
```

Install it for every detected supported coding agent:

```sh
flea skill install
```

Detection covers Claude Code, OpenCode, and Codex user directories, plus their
workspace configuration markers. Select explicit targets with the repeatable
`--agent` option:

```sh
flea skill install --agent claude
flea skill install --agent opencode
flea skill install --agent codex
flea skill install --agent claude --agent codex
```

Published copies use these paths:

| Agent | Installed skill path |
| --- | --- |
| Claude Code | `~/.claude/skills/flea/SKILL.md` |
| OpenCode | `~/.config/opencode/skills/flea/SKILL.md` |
| Codex | `~/.codex/skills/flea/SKILL.md` |

These files are generated copies. Edit the canonical repository file instead.
Explicit targets are installed even when their agent directory does not exist.
Without explicit targets, the command reports an error when no supported agent
is detected. Run `flea skill install --help` for the complete command syntax.
