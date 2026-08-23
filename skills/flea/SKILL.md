---
name: flea
description: Operate Tori.fi workflows and authenticated Vinted search, item inspection, drafts, and publication with flea.
---

# Flea

TOON is default output. Add `--format json` for JSON. Use command help for
syntax and `flea capabilities` for support.

## Rules

- Treat IDs, revisions, and option values as opaque. Discover them with
  `category`, `location`, or `show`.
- Follow `next_actions` and honor `safe_to_retry`. Inspect uncertain mutations
  before retrying them.
- Inspect returned drafts and listings after mutations. Remote state wins.
- A field cannot appear in both flags and `--input` JSON.
- Optional category fields belong in `attributes`; keys and values must match
  the selected draft's composer model.

## Find Vinted listings

Vinted search requires authentication. Tokens are short-lived and Flea cannot
refresh them, so follow the login action for missing or expired sessions.

```sh
flea vinted auth status
flea vinted auth login
flea vinted search [QUERY] [--price-from EUR] [--price-to EUR]
flea vinted search [QUERY] --sort relevance|newest|price-asc|price-desc
flea vinted search [QUERY] --page PAGE --limit LIMIT
flea vinted item show ITEM_ID
flea vinted item show ITEM_ID --raw
```

Inspect search IDs. `seller.seller_disclosed_location` is exposure-permitted
seller profile data, not a catalog filter or guaranteed item location. Never
infer it from presentation text. `--raw` preserves upstream JSON.

## Publish Vinted listings

Use runtime values for category, attributes, currency, price, and package.
Never guess facts. Pass complete JSON and ordered images when creating or
directly publishing:

```sh
flea vinted category list
flea vinted category attributes --input selections.json
flea vinted draft create --input listing.json --image front.heic
flea vinted draft publish DRAFT_ID --input listing.json
flea vinted draft delete DRAFT_ID
flea vinted publish --input listing.json --image front.jpg
```

Completion reuses verified ordered remote photos. `--image` replaces and
verifies all photos. Inspect partial state before retrying uncertainty.

## Find Tori listings

```sh
flea tori search [QUERY] [filters]
flea tori item show LISTING_ID
flea tori location search [NAME]
flea tori category search QUERY
flea tori category list [--parent ID]
```

Public search and item inspection need no login. Define geography explicitly:
`--location Helsinki` selects the city, `--area` accepts places, and coordinates
with `--radius-km` define a boundary. Clarify ambiguous areas.

Merge searches by `listing_id`; ranks across queries are not comparable. Use
`--explain N` or `item show` for opaque matches. Return linked title, price,
location, and URL with scope and ordering. Manage favorites with
`flea tori favorite add|remove LISTING_ID`. Use structured price fields, never
parse `price.display`.

Use `taxonomy_value` with `search --category` and `category_id` for drafts.
Follow pagination actions instead of dumping the taxonomy.

Manage authenticated alerts with
`flea tori saved-search list|show|create|update|delete`. Choose notification
channels explicitly. Omitted update channels retain remote state.

## Create and publish listings

Authenticated work begins with `flea tori auth status`; follow its reported login
action when needed.

```sh
flea tori draft preview --input listing.json
flea tori draft create --input listing.json --image photo.jpg
flea tori draft create --from-listing LISTING_ID
flea tori draft show DRAFT_ID
flea tori draft update DRAFT_ID [fields]
flea tori draft image add DRAFT_ID PATH...
flea tori draft validate DRAFT_ID
```

Preview checks local input; `--verify-category` checks its category.
`draft validate` checks publication readiness.

Discover fields with `draft show DRAFT_ID --include-fields` and values with
`--include-options FIELD`. Set condition with `--condition VALUE`, other optional
fields in `attributes`, and clear one with JSON `null`.

Publish with the exact validated revision:

```sh
validation="$(flea --format json tori draft validate DRAFT_ID)"
printf '%s\n' "$validation" | jq -e '.ok and .data.ready'
revision="$(printf '%s\n' "$validation" | jq -er '.data.revision')"
flea tori draft publish DRAFT_ID --if-revision "$revision"
```

On revision conflict, follow `next_actions`, inspect state, and use the returned
revision.

Manage account listings with `flea tori listing list|show|update|dispose|delete`.
`dispose` marks a listing sold.
