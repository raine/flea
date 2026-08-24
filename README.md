# flea

`flea` gives coding agents explicit command trees for Tori.fi and Vinted.
Tori supports listing workflows. Vinted supports persisted browser
authentication, contextual catalog search, item inspection, source-derived
draft operations, and publication.

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

Print the compact router or a complete marketplace guide for inspection and
custom integration:

```sh
flea skill
flea skill tori
flea skill vinted
```

The router lives at [`skills/flea/SKILL.md`](skills/flea/SKILL.md), with the
standalone Tori and Vinted guides beside it. Installation registers only the
router. It directs coding agents to load the matching guide from the installed
flea binary before operating a marketplace, keeping guidance aligned with the
binary version.

Pass `--format toon` or `--format json` explicitly to receive the selected guide
and its identifier in the standard structured envelope. Without an explicit
format, each read command prints only its Markdown document.

Once installed, ask the coding agent to perform the marketplace task in natural
language. The router directs it to load the complete marketplace guide, discover
machine values, and inspect remote state.

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

Vinted catalog operations require authentication. Filter codes and option IDs
are contextual data, so discover them before applying catalog or dynamic
selections:

```sh
flea vinted auth login
flea vinted filter list --query takki
flea vinted filter facets catalog --query takki
flea vinted filter search brand Marimekko --query takki
flea vinted search takki --catalog 123 --brand 53 --status 1
flea vinted search mekko --price-from 10.50 --price-to 80 --sort newest
flea vinted search --attribute contextual_code=10,20 --include-facets
```

`--catalog`, `--brand`, `--size`, `--status` (condition), `--color`, and
`--material` are shortcuts for Vinted attributes. Use repeatable
`--attribute CODE=ID[,ID...]` for active filter codes returned by discovery.
Search supports browsing without text, every Vinted sort order, decimal euro
prices, pagination, bounded results, contextual facets, and exact upstream
`--raw` output. Lazy and truncated filters use `filter facets`; searchable
option lists use `filter search`.

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

Vinted publication accepts a complete JSON listing payload. Creating, updating,
and directly publishing a listing requires images. Draft completion fetches and
verifies the remote photo IDs in display order, then reuses them without
uploading bytes. Passing `--image` while completing a draft performs a verified
replace-all operation. Flea strips image metadata and converts supported
HEIC/HEIF input before upload. Category IDs, dynamic attributes, currency, price
bounds, and package IDs are runtime portal values. The publication composer
combines those sources into normalized fields, options, requirements, validation
issues, and a direct `ListingInput` value when all seller facts are confirmed.

Publication executes Vinted API requests inside a visible, persistent Google
Chrome session so Vinted receives the browser cookies, CSRF state, and
human-verification state associated with that session. Flea launches ordinary Chrome with a dedicated profile and a
localhost debugging port. `agent-browser` connects to that Chrome instance as a
client and does not launch its automation-oriented Chromium build. Publication
relies on the browser login and requires no native Vinted token. Install the
client:

```sh
brew install agent-browser
agent-browser install
```

Set up authentication and sign in once:

```sh
flea vinted auth login
flea vinted auth status
```

Vinted authentication has two layers. Account credentials support catalog,
discovery, and account reads. A persistent Chrome profile supplies the cookies,
CSRF state, and human verification required for publication. The unqualified
commands manage both layers and report `ready.catalog` and `ready.publication`.
Use `--api` or `--browser` when only one layer should be changed or inspected.

The login command leaves the visible publication browser open when sign-in or a
human check needs user interaction. Complete that interaction in the browser,
then run `flea vinted auth status --browser`. Flea stores account credentials
and the dedicated Chrome profile in their established private state locations. `flea vinted auth logout` clears both layers; `--api` or `--browser`
limits the clear operation to one layer.

```sh
flea --format json vinted category search SEARCH_TEXT \
  --title LISTING_TITLE --description LISTING_DESCRIPTION
flea vinted category compose CATEGORY_ID
flea vinted category compose CATEGORY_ID --input listing.json
flea vinted category compose CATEGORY_ID --full
flea vinted category attributes --input selections.json
flea vinted category brands CATEGORY_ID BRAND_TEXT
flea vinted category package-sizes CATEGORY_ID
flea vinted category colors
flea vinted category configuration
flea vinted readiness
flea vinted draft list --page 1 --limit 20
flea vinted draft show DRAFT_ID
flea vinted draft validate DRAFT_ID
flea vinted draft create --input listing.json --image front.heic
flea vinted draft update DRAFT_ID --input listing.json --image front.jpg
flea vinted draft publish DRAFT_ID --input listing.json
flea vinted draft delete DRAFT_ID
flea vinted publish --input listing.json --image front.jpg
flea vinted listing show ITEM_ID
flea vinted listing list
```

Publication category search sends the keyword to Vinted's authenticated
portal-localized category service. Supply the listing title and description to
rank ambiguous results using current marketplace category facets. The output
contains publishable leaves plus relative scores and compact live-listing
evidence keyed by category ID. `selection_required` stays true because weak or
nearby matches require agent judgment. Category IDs and ranking behavior come
from scoped runtime discovery data. Flea does not translate category queries or
maintain marketplace category mappings.

Sellers can write accurate titles and natural descriptions in their own
language. Vinted's buyer-facing web experience provides translation for
supported member-authored content and lets buyers view the original. The item
API preserves the seller's original text, while structured category IDs render
with Vinted's taxonomy labels for the viewer's locale. Buyer-facing text
translation and seller-side category discovery are separate services.

Pass one or more `--image` values to `draft publish` only when replacing the
complete remote photo set. Publication output separates `uploaded_photo_ids`
from `assigned_photos` and `assigned_photo_ids`. Upload IDs come from the upload
response in command input order. They are temporary mutation inputs scoped to
the upload session, not listing photo identities. Flea fetches the resulting
item after every successful create, update, completion, replacement, or direct
publication. `assigned_photos` contains those authoritative draft or listing
photo IDs with zero-based `display_order`; `assigned_photo_ids` preserves the
existing flat field for structured-output compatibility and contains the same
authoritative IDs in display order. ID equality between the two sets carries no
meaning.

A confirmed publication can remain hidden while Vinted reviews it. When direct
item inspection returns HTTP 404, Flea checks the authenticated account listing
and returns `status: "pending"` with the listing ID, canonical URL, and
`authoritative_state` when that listing is hidden or moderated. The result sets
`safe_to_retry` to `false`: inspect the existing listing instead of publishing
the item again.

`category compose` is the guided publication entry point. Category search emits
one compose action per leaf result. Its default response contains readiness,
selected values, validation issues, brand validation, and correction actions.
This bounded response keeps the information for the next agent action ahead of
large runtime catalogs. Add `--full` to include the complete form. The full form
contains required fields such as size and condition in `form.fields` and their
runtime IDs in `form.options`. Each missing attribute option also has an exact
command in `issue_actions` and envelope `next_actions`. Following one of these
commands carries the category and chosen option into the next attribute layer
without constructing a selection payload.

When partial input supplies a brand name outside the initial suggestions, the
composer emits a focused `category brands` action. Use that action's opaque ID
and canonical name together in the next composer input.

Composer issues for other fields link to focused discovery or a correction
command. Discovery output declares its scope: brands and package sizes are
category scoped, colors are portal scoped, configuration is account scoped, and
dynamic attributes are selection scoped. Attribute output preserves the
submitted `selection_payload` and emits exact commands that append each opaque
option for the next layer. `category attributes --input` remains available for
workflows that already have a selection array.

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
custom shipment prices, and parcel dimensions. Draft inspection reads remote
editable state and reports authoritative assigned IDs in display order. These
IDs identify the current remote photo assignment and can change when that set is
replaced. Completion reuses the inspected draft assignment, then reports the
authoritative listing IDs fetched after completion. Validation separates field
schema blockers, upstream validation errors, and account prerequisites. Draft
updates replace the complete image assignment. Draft completion reuses remote
photos unless `--image` supplies a complete replacement set.

Run `flea vinted readiness` before publication. It validates the authenticated
session and classifies every selling prerequisite Vinted exposes as
`confirmed_ready`, `confirmed_blocked`, or `unknown`. Detectable blockers stop
publication before image upload. Phone, email, identity, and two-factor checks
remain manual actions in Vinted. When a prerequisite appears only in a mutation
response, Flea returns the draft ID and uploaded photo metadata as continuation
context together with the safe user action or Vinted URL.

Publication output points to `vinted listing show ITEM_ID`, which reads the
account wardrobe and editable item directly without waiting for search indexing.
Condition output separates the editable listing's `upstream_id` from the
selection-scoped `composer_id`. Flea resolves the composer identity by matching
the localized listing condition against live attribute discovery for the
listing's category. `identity.status` is `composer_matched`, `upstream_only`, or
`unavailable`; null IDs remain in their named namespace and never imply that an
upstream ID can be submitted to the composer.

During Vinted review, inspection falls back to the bounded account collections
and returns `state: moderated` with available summary fields and canonical URL.
Follow its next action after waiting for review instead of changing listing
input. Editable fields, photo order, and shipping details appear when Vinted
makes them available. Account listing enumeration combines active and
draft-associated wardrobe items within a fixed output bound.

Run command help for current syntax, constraints, and examples:

```sh
flea vinted search --help
flea vinted filter --help
flea tori search --help
flea tori favorite --help
flea tori saved-search --help
flea tori saved-search create --help
flea tori draft --help
flea tori draft create --help
flea tori listing update --help
flea vinted item show --help
flea vinted listing --help
flea vinted draft --help
flea vinted publish --help
flea vinted auth --help
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

The validation suite uses [cargo-nextest](https://nexte.st/) to run tests in
parallel. It is included in the Nix development shell. Install it for other
development environments with:

```sh
cargo install cargo-nextest --locked
```

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
