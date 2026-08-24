---
name: flea-tori
description: Operate Tori.fi with flea.
---

# Flea Tori guide

TOON is default. Add `--format json` for JSON. Run `flea capabilities` for
support.

## Rules

- Treat IDs, revisions, and options as opaque. Discover them with `category`,
  `location`, or `show`.
- Follow `next_actions` and `safe_to_retry`; inspect uncertain mutations.
- Remote state wins. Do not duplicate a field in flags and `--input` JSON.
- Put category fields in `attributes` using the draft's composer model.

## Find Tori listings

```sh
flea tori search [QUERY] [filters]
flea tori item show LISTING_ID
flea tori location search [NAME]
flea tori category search QUERY
flea tori category list [--parent ID]
```

Public search and item inspection need no login. Set geography with `--location`,
`--area`, or coordinates plus `--radius-km`; clarify ambiguous areas.

Merge searches by `listing_id`; ranks across queries differ. Use `--explain N`
or `item show` for opaque matches. Use structured prices, never `price.display`.
Manage favorites with `flea tori favorite add|remove LISTING_ID`.

Use `taxonomy_value` with `search --category` and `category_id` for drafts.
Follow pagination actions. Manage alerts with
`flea tori saved-search list|show|create|update|delete`; choose channels.

## Create and publish Tori listings

Start with `flea tori auth status` and follow its login action.

```sh
flea tori draft preview --input listing.json
flea tori draft create --input listing.json --image photo.jpg
flea tori draft create --from-listing LISTING_ID
flea tori draft show DRAFT_ID
flea tori draft update DRAFT_ID [fields]
flea tori draft image add DRAFT_ID PATH...
flea tori draft validate DRAFT_ID
```

Preview validates local input; `--verify-category` checks its category. Discover
fields with `draft show DRAFT_ID --include-fields` and values with
`--include-options FIELD`. Use `--condition`, `attributes`, and JSON `null` to
set or clear optional fields.

Publish with the exact validated revision:

```sh
validation="$(flea --format json tori draft validate DRAFT_ID)"
printf '%s\n' "$validation" | jq -e '.ok and .data.ready'
revision="$(printf '%s\n' "$validation" | jq -er '.data.revision')"
flea tori draft publish DRAFT_ID --if-revision "$revision"
```

On revision conflict, inspect state and use the returned revision. Manage account
listings with `flea tori listing list|show|update|dispose|delete`; `dispose` marks
one sold.
