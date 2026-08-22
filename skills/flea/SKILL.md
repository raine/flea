---
name: flea
description: Operate Tori.fi through the flea CLI. Use for marketplace search, favorites, location and category discovery, authentication, draft creation and publishing, image management, and published listing management.
---

# Flea

Use default TOON output for compact results. Add `--format json` for JSON.
Run `flea <command> --help` for syntax and examples.

## Rules

- Treat IDs, revisions, and option values as opaque. Discover them with
  `category`, `location`, or the relevant `show` command.
- Follow `next_actions` and honor `safe_to_retry`. After an uncertain mutation,
  use the reported read-only inspection command. Retry only work explicitly
  classified as absent, and inspect indeterminate work before touching it.
- Inspect the returned draft or listing after a mutation. Remote state is
  authoritative.
- A field cannot appear in both flags and `--input` JSON.
- Optional category fields use the bounded `attributes` object in input JSON.
  Every key and value must match the selected draft's composer model.

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

Start with requested filters. Merge searches by `listing_id`; ranks across
queries are not comparable. Use `--explain N` or `item show` for opaque matches.

Return concise linked results with title, price, location, and URL. State the
scope and ordering. Save with `flea favorite add LISTING_ID`, optionally using
a folder from `flea favorite folders`. Remove with `flea favorite remove LISTING_ID`.
Use `price.amount`, `price.currency`, `trade_type`, and `price.kind`. Never parse
`price.display`.

Use `taxonomy_value` with `search --category` and `category_id` for drafts.
Follow pagination actions instead of dumping the taxonomy.

Manage authenticated alerts with `flea saved-search list|show|create|update|delete`.
Create accepts public-search query and filter arguments. Choose email, push,
notification-center, or no notifications explicitly. Omitted update channels
retain remote state. After uncertain mutations, follow the returned read-only
`next_actions`; retry only when `safe_to_retry` is true.

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
publication readiness.

Discover optional fields with `draft show DRAFT_ID --include-fields` and select
machine values with `--include-options FIELD`. Set condition with
`draft update DRAFT_ID --condition VALUE`. Set other optional fields under
`attributes` in input JSON, and use JSON `null` to clear one. Flea rejects
fields absent from the selected category and revalidates them after a category
change.

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
