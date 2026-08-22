# flea

`flea` lets coding agents search Tori.fi and create, publish, and manage
listings from the command line.

## Why flea?

Marketplace websites are awkward for agents to use reliably. Flea provides
focused commands and structured results for the complete listing workflow:

- Search and inspect public listings without signing in
- Discover valid categories, locations, and listing options
- Prepare and validate drafts before publishing
- Process photos locally and remove embedded metadata
- Recover safely when a network request fails partway through an operation
- Install a bundled skill that teaches agents how to use Flea

The bundled skill and `flea <command> --help` provide the current usage
guidance.

## Installation

Install the latest release:

```sh
curl -fsSL https://raw.githubusercontent.com/raine/flea/main/scripts/install | bash
```

Or install with Homebrew on macOS or Linux:

```sh
brew install raine/flea/flea
```

Other options:

```sh
cargo install --git https://github.com/raine/flea --locked
nix profile install github:raine/flea
```

Verify the installation:

```sh
flea --version
flea --help
```

## Install the agent skill

Install the bundled skill for every detected supported coding agent:

```sh
flea skill install
```

Select explicit targets when needed:

```sh
flea skill install --agent claude
flea skill install --agent opencode
flea skill install --agent codex
flea skill install --agent claude --agent codex
```

| Agent | Installed skill path |
| --- | --- |
| Claude Code | `~/.claude/skills/flea/SKILL.md` |
| OpenCode | `~/.config/opencode/skills/flea/SKILL.md` |
| Codex | `~/.codex/skills/flea/SKILL.md` |

Print the embedded skill for inspection or custom integration:

```sh
flea skill
```

The canonical skill lives at
[`skills/flea/SKILL.md`](skills/flea/SKILL.md). Installed files are generated
copies.

Once installed, ask the coding agent to perform the marketplace task in natural
language. The skill directs it to discover machine values, inspect remote state,
and request authorization before immediate or destructive mutations.

## Capabilities

Public operations require no account authentication:

- Search listings with structured filters, facets, locations, sorting, and
  bounded pagination
- Explain opaque search matches with bounded public item inspection
- Inspect public listing details
- Discover deterministic Tori location identifiers

Authenticated operations cover:

- Browser authentication and local credential refresh
- Category and composer-option discovery
- Offline draft preview and image preprocessing
- Draft creation, copying, inspection, updates, image management, validation,
  publication, and deletion
- Published listing inspection, updates, sold-state transitions, and deletion

Run command help for current syntax, constraints, and examples:

```sh
flea search --help
flea draft --help
flea draft create --help
flea listing update --help
```

## Structured output

Flea emits TOON by default to keep agent context compact:

```text
ok: true
data:
  query: tuoli
  results[1]:
    - listing_id: "42346404"
      title: Baden tuoli
      price:
        amount: 37
        currency: EUR
      location: "Helsinki, Uusimaa"
      url: "https://www.tori.fi/recommerce/forsale/item/42346404"
next_actions[1]{command}:
  "flea search 'tuoli' --page 2 --limit 20"
```

Use JSON when another tool requires it:

```sh
flea --format json search "tuoli"
```

Results use one envelope with these fields when applicable:

- `ok`
- `data`
- `error`
- `partial`
- `observation`
- `next_actions`
- `diagnostics`

Agents should consume semantic fields such as `trade_type`, `price.kind`,
`price.amount`, and `price.currency`. Localized fields such as `price.display`
are presentation text.

## Safety semantics

Returned IDs, revisions, field names, and option values are opaque machine
values. Agents discover them through Flea instead of guessing them.

Remote state is authoritative. Agents inspect a draft or listing after every
mutation and follow returned `next_actions`.

Errors answer two separate questions:

- `upstream_transient` reports whether the upstream failure appears temporary.
- `safe_to_retry` reports whether repeating the complete unchanged command is
  safe and capable of making progress.

A temporary failure after a mutation can set `upstream_transient: true` and
`safe_to_retry: false`. The agent must inspect authoritative state before
performing another mutation.

Draft workflows can complete partially. Recovery output classifies requested
work as persisted, absent, indeterminate, or unattempted. Only work proven
absent is eligible for direct retry. Indeterminate work requires read-only
inspection.

A failed draft creation can still return a persisted draft ID. Continue against
that draft instead of repeating creation and risking a duplicate.

Publication requires the exact revision returned by `draft show` or
`draft validate`. `draft publish`, `draft delete`, `listing dispose`, and
`listing delete` act immediately without interactive confirmation. The bundled
skill requires explicit user authorization for these operations.

## Image privacy

Flea accepts JPEG, PNG, HEIC, and HEIF images. It decodes and re-encodes pixels
locally before upload, removing EXIF, GPS, XMP, embedded thumbnails, and other
source metadata.

macOS uses ImageIO through `sips` for HEIC and HEIF conversion. Other platforms
can provide the optional `heif-convert` command. Original files are read only,
and private temporary conversion artifacts are removed after processing.

## Development

Run the repository validation suite with:

```sh
just check
```

Run the development binary with:

```sh
cargo run -- --help
```

## License

Flea is available under the [MIT License](LICENSE).

Flea is an independent, unofficial tool. It is not affiliated with or endorsed
by Tori.fi.
