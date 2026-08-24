---
name: flea-vinted
description: Operate Vinted with flea.
---

# Flea Vinted guide

TOON is default. Add `--format json` for JSON. Run `flea capabilities` for
support.

## Rules

- Treat IDs, revisions, and options as opaque. Discover them with `category`,
  `filter`, or `show`.
- Follow `next_actions` and `safe_to_retry`; inspect uncertain mutations.
- Remote state wins. Do not duplicate a field in flags and `--input` JSON.
- Put category fields in `attributes` using the draft's composer model.

## Find Vinted listings

Vinted search requires authentication. Follow login actions for missing or
expired short-lived tokens. Filter codes and option IDs are contextual.
Discover them instead of guessing or reusing examples.

```sh
flea vinted auth status
flea vinted auth login
flea vinted filter list [--query TEXT] [filters]
flea vinted filter facets CODE [--query TEXT] [filters]
flea vinted filter search CODE OPTION_TEXT [--query TEXT] [filters]
flea vinted search [QUERY] [--price-from EUR] [--price-to EUR]
flea vinted search [QUERY] --sort relevance|newest|price-asc|price-desc
flea vinted search [QUERY] --page PAGE --limit LIMIT
flea vinted item show ITEM_ID [--raw]
```

Discover filters after changing query or category. Retrieve lazy options with
`filter facets`; search brands with `filter search`. Apply IDs using the common
filter flags or repeatable `--attribute CODE=ID[,ID...]`. Prices accept two
decimal places. Omit the query to browse and add `--include-facets` for filters.

Shopping queries are multilingual and can match listings from connected markets.
`item show` preserves the seller's original text; Vinted's buyer-facing web UI
translates listing text and offers the original.

Vinted is shipping-first, so location filtering is unavailable. Inspect search
IDs. `seller.seller_disclosed_location` is exposure-permitted seller profile
data, not a catalog filter or guaranteed item location. Never infer it from
presentation text. `--raw` preserves upstream JSON.

## Publish Vinted listings

Web commands use Chrome cookies and human verification through `agent-browser`:

```sh
flea vinted auth web login
flea vinted auth web status
flea vinted category compose CATEGORY_ID --input listing.json
flea vinted draft show DRAFT_ID
flea vinted draft validate DRAFT_ID
flea vinted draft create --input listing.json --image front.heic
flea vinted draft publish DRAFT_ID --input listing.json
flea vinted draft delete DRAFT_ID
flea vinted publish --input listing.json --image front.jpg
flea vinted listing show ITEM_ID
```

Publication category search uses portal-localized taxonomy labels, unlike
multilingual shopping search. Use Finnish category terms on the `fi` portal and
treat the returned category IDs as opaque. Write listing text naturally in the
seller's language; rely on Vinted's buyer-facing translation instead of creating
translated duplicate listings.

Completion reuses photos; `--image` replaces them. Complete verification
manually, inspect before retrying, and clear cookies with `auth web logout`.
