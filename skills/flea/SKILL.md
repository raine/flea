---
name: flea
description: Operate Tori.fi through the flea CLI. Use for marketplace search, location and category discovery, authentication, draft creation and publishing, image management, and published listing management.
---

# Flea

Use `flea` as a structured CLI for Tori.fi. Keep the default TOON output for
compact agent-readable results. Add `--format json` when another tool requires
JSON. Run `flea <command> --help` for current syntax, filters, and examples.

## Rules

- Treat returned IDs, revisions, and option values as opaque machine values.
  Discover them with `category`, `location`, or the relevant `show` command.
- Follow `next_actions` and honor `safe_to_retry`. After an uncertain mutation,
  use the reported read-only inspection command. Retry only work explicitly
  classified as absent, and inspect indeterminate work before touching it.
- Inspect the returned draft or listing after a mutation. Remote state is
  authoritative.
- A field cannot appear in both flags and `--input` JSON.
- Publish, dispose, and delete act immediately without confirmation. Run them
  only when the user explicitly requests them.

## Find listings

```sh
flea search [QUERY] [filters]
flea item show LISTING_ID
flea location search [NAME]
flea category search QUERY
flea category list [--parent ID]
```

Public search and item inspection need no login. Define geographic scope
explicitly: `--location Helsinki` selects the exact Tori city, `--area` accepts
named places, and coordinates with `--radius-km` define a distance boundary.
Ask what an ambiguous phrase such as "Helsinki area" includes, or state the
places chosen.

Start with the requested product and filters. Use a few meaningful aliases when
recall is poor. Merge searches by numeric `listing_id`; ranks from different
queries are not comparable. Use `--explain N` for opaque matches and `item show`
for full details. Call a match exact only when its title or public details
confirm the requested identity.

Return concise linked results with title, price, location, and URL. State the
scope and ordering. Search summaries expose `price.amount` and
`price.currency`. Item, draft, and account-listing output also provide
normalized `trade_type` and `price.kind`. Never parse `price.display`.

Use returned `category_id` values. Refine broad category searches with
`--parent` or `--path`, and follow pagination actions instead of dumping the
full taxonomy.

## Create and publish listings

Authenticated work begins with `flea auth status`; follow its reported login
action when needed.

```sh
flea draft preview --input listing.json
flea draft create --input listing.json --image photo.jpg
flea draft create --from-listing LISTING_ID
flea draft show DRAFT_ID
flea draft update DRAFT_ID [fields]
flea draft image add DRAFT_ID PATH...
flea draft validate DRAFT_ID
```

Preview checks local input without creating a draft. Add `--verify-category` to
check the category. `draft validate` authoritatively checks an existing draft's
publication readiness. Request `--include-fields` or `--include-options FIELD`
from `draft show` only when needed.

Publish with the exact validated revision:

```sh
validation="$(flea --format json draft validate DRAFT_ID)"
printf '%s\n' "$validation" | jq -e '.ok and .data.ready'
revision="$(printf '%s\n' "$validation" | jq -er '.data.revision')"
flea draft publish DRAFT_ID --if-revision "$revision"
```

A revision conflict is unsafe to retry unchanged. Follow its read-only
`next_actions`, review the latest state, and publish only with the revision from
that review.

Manage existing account listings with `flea listing list`, `show`, `update`,
`dispose`, and `delete`. `dispose` marks a listing as sold. Use command help for
exact arguments.
