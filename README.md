# flea

`flea` gives coding agents explicit command trees for Tori.fi and Vinted.
Tori supports listing workflows. Vinted supports persisted browser
authentication, search, item inspection, source-derived draft operations, and
publication.

## Why flea?

Marketplace websites are awkward for agents to use reliably. Flea provides
focused commands and structured results for the complete listing workflow:

- Search and inspect public listings without signing in
- Save and remove favorites in Tori folders
- Create and manage saved searches and their email, push, or in-app alerts
- Discover valid categories, locations, and listing options
- Prepare and validate drafts before publishing
- Process photos locally and remove embedded metadata
- Recover safely when a network request fails partway through an operation
- Install a bundled skill that teaches agents how to use Flea

The bundled skill and `flea <marketplace> <command> --help` provide the
current usage guidance. Run `flea capabilities` for the offline capability
matrix and `flea marketplaces` for available portal bindings.

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
language. The skill directs it to discover machine values and inspect remote
state.

## Capabilities

Every marketplace command names its marketplace explicitly:

```sh
flea tori auth status
flea vinted --portal fi auth status
flea tori capabilities
flea vinted --portal fi capabilities
```

Tori public operations require no account authentication:

- Search listings with structured filters, facets, locations, sorting, and
  bounded pagination
- Explain opaque search matches with bounded public item inspection
- Inspect public listing details
- Discover deterministic Tori location identifiers

Authenticated operations cover:

- Browser authentication and local credential refresh
- Favorites folder discovery and saved-listing management
- Saved-search listing, inspection, creation, notification/name updates, and deletion
- Category and composer-option discovery
- Offline draft preview and image preprocessing
- Draft creation, copying, inspection, updates, image management, validation,
  publication, and deletion
- Published listing inspection, updates, sold-state transitions, and deletion

Authenticated Vinted search results can be inspected by their numeric item ID:

```sh
flea vinted search "wool coat"
flea vinted item show ITEM_ID
flea vinted item show ITEM_ID --raw
```

Normalized details expose `seller.seller_disclosed_location` only when Vinted
returns an explicit seller city or country and permits exposure, or when its
business-seller information plugin supplies a location. This is seller profile
information. It is not a catalog location filter and does not guarantee the
item's physical location. `--raw` returns the exact upstream JSON value inside
the standard envelope.

Vinted publication accepts a complete JSON listing payload and one or more
images. Flea strips image metadata, converts supported HEIC/HEIF input, uploads
images in argument order, and uses Vinted's draft or direct-publication
endpoint. Category IDs, dynamic attributes, currency, price bounds, and package
IDs are runtime portal values.

```sh
flea vinted category list
flea vinted category attributes --input selections.json
flea vinted category package-sizes CATEGORY_ID
flea vinted draft create --input listing.json --image front.heic
flea vinted draft update DRAFT_ID --input listing.json --image front.jpg
flea vinted draft publish DRAFT_ID --input listing.json --image front.jpg
flea vinted draft delete DRAFT_ID
flea vinted publish --input listing.json --image front.jpg
```

A minimal complete input has this shape:

```json
{
  "title": "Truthful title",
  "description": "Truthful description",
  "catalog_id": 123,
  "price": "5.00",
  "currency": "EUR",
  "package_size_id": 1,
  "item_attributes": [{ "code": "condition", "ids": [1] }]
}
```

Optional fields include brand, ISBN, color, measurements, manufacturer fields,
custom shipment prices, and parcel dimensions. Draft update and publish replace
the complete image assignment, so pass every intended image in display order.

Run command help for current syntax and constraints:

```sh
flea tori search --help
flea tori favorite --help
flea tori saved-search --help
flea tori saved-search create --help
flea tori draft --help
flea tori draft create --help
flea tori listing update --help
flea vinted item show --help
flea vinted draft --help
flea vinted publish --help
```

## Structured output

Flea emits TOON by default to keep agent context compact:

```text
ok: true
context:
  marketplace: tori
  portal: fi
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
  "flea tori search 'tuoli' --page 2 --limit 20"
```

Use JSON when another tool requires it:

```sh
flea --format json tori search "tuoli"
```

Results use one envelope with these fields when applicable:

- `ok`
- `context`
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

Saved-search mutation failures return `saved-search list` or `saved-search show`
as read-only recovery actions. Flea only marks the same mutation safe to retry
when an authenticated recovery read proves the intended result absent. A
recovery read can also prove that the mutation succeeded despite its failed
response.

Publication requires the exact revision returned by `draft show` or
`draft validate`.

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
by Tori.fi or Vinted.
