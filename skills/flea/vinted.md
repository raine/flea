---
name: flea-vinted
description: Operate Vinted with flea.
---

# Flea Vinted guide

## Rules

- Treat IDs, revisions, and options as opaque. Use semantic listing values when
  supported, and discover explicit IDs with `category`, `filter`, or `show` only
  for deterministic advanced workflows.
- Follow `next_actions` and `safe_to_retry`; inspect uncertain mutations.
- Remote state wins. Do not duplicate a field in flags and `--input` JSON.
- Put category fields in `attributes` using the draft's composer model.

## Find Vinted listings

Vinted search requires account authentication. Follow login actions for missing
or expired short-lived tokens. Filter codes and option IDs are contextual.
Discover them instead of guessing or reusing examples. Unqualified auth commands
manage both account credentials and the persistent publication browser;
`--api` or `--browser` selects one layer.

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

Publication commands use ordinary Chrome cookies and human verification through
Flea's built-in browser connection. No separate browser automation client is
required. Flea chooses an available local debugging port automatically.
Publication also uses account credentials for discovery and account reads, so
set up both layers:

```sh
flea vinted auth login
flea vinted auth status
flea vinted sell --input facts.json --image front.jpg
flea --format json vinted category search SEARCH_TEXT \
  --title LISTING_TITLE --description LISTING_DESCRIPTION
flea vinted category compose CATEGORY_ID --input listing.json
flea vinted category compose CATEGORY_ID --full
flea vinted draft show DRAFT_ID
flea vinted draft validate DRAFT_ID
flea vinted draft create --input listing.json --image front.heic
flea vinted draft publish DRAFT_ID --input listing.json
flea vinted draft delete DRAFT_ID
flea vinted publish --input listing.json --image front.jpg
flea vinted listing show ITEM_ID
flea vinted listing update ITEM_ID --input changes.json
flea vinted listing list
```

Prefer `vinted sell` when starting from seller facts rather than opaque IDs. Its
semantic JSON accepts title, description, price, category phrase, size,
condition, brand, colors, package size, additional `attributes`, and `images`.
It performs scoped runtime discovery without mutating remote state. Exact label
matches produce a validated `proposed_mutation`; save its `listing_input` and
run the returned existing publish command. Ambiguities contain runtime choices
and resumable `--select FIELD=ID` commands. Follow those actions instead of
fuzzy matching, selecting the first category, or inventing IDs. Marketplace
facet category evidence always requires explicit selection.

`listing list` enumerates active and draft-associated items for the authenticated
account without relying on search indexing. Use it to verify that a
review-pending publication exists while Vinted hides it from public search,
then inspect the returned item ID with `listing show`. Listing conditions expose
separate `identity.upstream_id` and `identity.composer_id` values. Use the
composer ID for publication correlation when `identity.status` is
`composer_matched`. Treat `upstream_only` and `unavailable` as explicit limits,
not as permission to submit a listing-side ID to the composer.

`listing update` accepts only a partial JSON object with one or more string
fields named `title`, `description`, and `price`. It updates an owned, editable
public listing in place while preserving every omitted value and the complete
existing photo order. Category, condition, attribute, brand, color, package,
shipping, parcel, and photo changes are unsupported and must use no guessed ID
or workaround. Vinted provides no remote revision precondition, so this
replacement-style update cannot guarantee protection from a concurrent edit
between Flea's read and write. Inspect the returned authoritative listing before
another update. After an accepted update, Flea briefly polls account reads while
Vinted processes the item; it never repeats the write. If verification still
fails, follow the returned `listing show` action rather than retrying the update.

Publication category search uses portal-localized taxonomy labels. Supply the
known listing title and description as recommendation context. Output reports
the portal and request locale, resolves opaque IDs through the localized
catalog, and preserves upstream aliases or suggestions. When direct results
need ranking, flea discovers publishable leaves from live Vinted category
facets. `marketplace_evidence.recommendations` contains a relative score and
compact live-listing evidence for each category ID. Honor `selection_required`
and compare the ranked candidates when evidence is weak or nearby categories
remain plausible. Follow a leaf result's `next_actions` into `category compose`.
The default response provides readiness, issues, selected values, brand
validation, and correction actions in a bounded structure. Use `--full` when
the next action requires complete fields and runtime option catalogs for brands,
colors, package sizes, currencies, or category attributes.

Brands and package sizes are category scoped. Colors are portal scoped,
configuration is account scoped, and attributes are selection scoped. Put
semantic `size`, `condition`, `colors`, and `package_size` values directly in
listing input. Localized labels and canonical machine values resolve against
those live scoped catalogs. Use `semantic_resolutions` to confirm matched labels
and scopes. Ambiguous or unavailable values produce field-level correction
actions. Explicit `item_attributes`, `color_ids`, and `package_size_id` remain
available when an advanced workflow already has exact runtime IDs.

Composer `form.fields` includes the first layer of required category attributes,
while `form.options` supplies their runtime IDs. Follow the matching
`issue_actions` or `next_actions` command to select an option and discover
dependent layers. These commands carry the category and selected parent values
without requiring a hand-built selection array. Attribute output preserves
`selection_payload`; continue following `next_actions` through additional
layers.

A supplied brand name without an ID triggers category-scoped matching. One exact
or normalized match fills its opaque ID and canonical name. Check
`brand_validation.status` for
`resolved`, `custom`, or `ambiguous`. For ambiguity, choose one bounded option
and rerun the composer with its ID and canonical name. A brand remains custom
only when the scoped response permits custom brands. Use focused discovery when
other composer issue actions request it.

Write listing text naturally in the seller's language. Vinted's buyer-facing
experience translates supported member-authored content and offers the original.
Structured taxonomy localization and seller-text translation are separate.

Pass photos in the intended display order. The first `--image` becomes the main
photo; subsequent images follow in the order supplied:

```sh
flea vinted publish --input listing.json --image front.jpg --image back.jpg
```

Here, `front.jpg` is the main photo. Confirm the authoritative order in the
result's `assigned_photos`: `display_order: 0` identifies the main photo.

Completion reuses photos; `--image` replaces them. Publication results keep
`assigned_photo_ids` for compatibility and add `assigned_photos` with
`display_order`. Both reflect authoritative post-mutation draft or listing
state. `uploaded_photo_ids` are temporary upload-session mutation inputs, even
when their values equal assigned IDs. Use assigned IDs to correlate remote
photos. A confirmed publication that enters review triggers bounded account
inspection without another publication mutation. Read `verification.status` as
`public`, `moderated`, or `timed_out`; inspection failures return
`vinted.publication_verification_uncertain`. All of these confirmed mutation
outcomes set `safe_to_retry: false`. Follow the exact `listing show` action for
a timed-out or uncertain verification. `auth logout` clears both authentication
layers; `auth logout --browser` clears browser cookies and Vinted local storage,
then closes the selected tab without deleting the profile.
