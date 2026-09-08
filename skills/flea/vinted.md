---
name: flea-vinted
description: Operate Vinted with flea.
---

# Flea Vinted guide

## Rules

- Never guess IDs or reuse options from another category or search context.
  Prefer semantic values and runtime discovery; remote state is authoritative.
- Follow `next_actions`, choice-local commands, and `safe_to_retry`.
  Never blindly retry an uncertain mutation; inspect the returned item or draft.
- Category selection and validation are not authorization to publish.

## Authentication and extension setup

Search, discovery, and account reads use API credentials. Publication and edits
also require Flea's Chrome extension in your normal signed-in Chrome session.
There is no fallback browser, separate profile, or alternate browser transport.

```sh
flea extension setup
flea vinted auth login
flea vinted auth status
```

Load the printed directory with **Load unpacked** in `chrome://extensions`
(Developer mode). On macOS, interactive setup copies the path: use
**Command+Shift+G** in the folder picker, paste, and press Return. Open or reload
exactly one `https://www.vinted.fi` tab, sign in, and complete human verification.
Rerun setup after moving the executable or refreshing extension files, then
reload the extension and tab.

Auth commands default to both layers; `--api` or `--browser` selects one layer.
Follow login actions for missing or expired API credentials. To clear only those
credentials, use `flea vinted auth logout --api`. Browser logout is manual:
sign out on the Vinted website in Chrome. Flea does not clear shared cookies.

## Search and inspect

```sh
flea vinted filter list [--query TEXT] [filters]
flea vinted filter facets CODE [--query TEXT] [filters]
flea vinted filter search CODE OPTION_TEXT [--query TEXT] [filters]
flea vinted search [QUERY] [--price-from EUR] [--price-to EUR]
flea vinted search [QUERY] --sort relevance|newest|price-asc|price-desc
flea vinted search [QUERY] --page PAGE --limit LIMIT
flea vinted item show ITEM_ID [--raw]
```

Rediscover filters when the query or category changes. Use `filter facets` for
lazy/truncated options and `filter search` for labels such as brands. Apply
returned IDs with `--catalog`, `--brand`, `--size`, `--status`, `--color`,
`--material`, or repeatable `--attribute CODE=ID[,ID...]`. Prices accept up to two
decimal places. Omit QUERY to browse; add `--include-facets` for contextual filters.

Shopping search is multilingual; publication taxonomy discovery is not.
`item show` keeps original seller text; `--raw` preserves upstream JSON.
Location filtering is unavailable. `seller.seller_disclosed_location` is seller
profile data, not a catalog filter or guaranteed item location. Never infer it
from presentation text.

## Prepare a listing from seller facts

```sh
flea vinted sell --input facts.json --image front.jpg --output listing.json
```

`sell` discovers and validates without remote mutation. Use a durable facts file:
consumed stdin cannot be replayed. Semantic facts accept title, description,
price, category phrase, size, condition, brand, colors, package_size, images,
and additional category fields in `attributes`. Keep listing text in the seller's
language. For seller-confirmed unisex items, set `"is_unisex": true`; description
text alone does not set it or replace selection of an actual category branch.

1. Inspect category paths and explicitly select a current leaf with the returned
   `--select category=ID` command, even if there is only one candidate.
2. Resolve missing or ambiguous values using returned `--select FIELD=ID`
   continuations. Inspect choice-local commands, not only top-level actions.
   An unrecognized phrase is not proof of ambiguity: inspect accepted labels
   and match seller facts, never pick the first option or invent an ID.
3. When ready, `--output listing.json` saves validated `listing_input` from
   `proposed_mutation`. It writes only when ready and never overwrites an
   existing path. Review the result, then deliberately run its publish command.

## Discover categories and composer fields

```sh
flea vinted category search SEARCH_TEXT \
  --title LISTING_TITLE --description LISTING_DESCRIPTION
flea vinted category list --roots
flea vinted category list --parent PARENT_ID
flea vinted category search SEARCH_TEXT --parent PARENT_ID
flea vinted category compose CATEGORY_ID --input listing.json
flea vinted category compose CATEGORY_ID --field attribute.size --input listing.json
flea vinted category compose CATEGORY_ID --field attribute.condition --input listing.json
```

Search uses current portal-localized full paths, not translation. If results are
empty or broad, browse roots/children and search observed labels. For example,
Finnish `lasten kengät` does not automatically match the `Lapset` branch; use
observed `Lapset kengät`, then narrow the actual branch and shoe type. Preserve age,
gender-branch, and item-type constraints. Ask for missing seller facts rather
than choosing by popularity or stereotypes; never invent a generic unisex branch.
Nonleaf results are browse targets, not publication choices. Honor
`selection_required`; inspect full paths, warnings, and pagination actions.
Avoid bare `category list`, which exports the entire raw tree.

Recommendations are hints, not selection authority. `--marketplace-evidence` on
`category search` or `sell` optionally adds relative listing-count support, not
classification confidence. Zero counts do not exclude a category. If a hint
service fails (including HTTP 404), browse the catalog instead of retrying it;
that failure is not evidence that the category is absent.

Composer reports readiness and correction actions. Prefer focused `--field`
output; paginate with `--option-limit` and `--option-offset`, keeping the same
input and selections. Apply an option and rerun; never submit group headings.
Follow `issue_actions` or `next_actions` through dependent attribute layers.
`form.fields` identifies required attributes; `form.options` supplies runtime IDs.
Continuation commands carry selected parent values, and `selection_payload`
preserves them. Do not hand-build a different selection context.
Use `--full` only when the complete form is needed.

- Prefer semantic `size`, `condition`, `colors`, and `package_size` in listing
  input; verify matched labels/scopes in `semantic_resolutions`. Explicit
  `item_attributes`, `color_ids`, and `package_size_id` require current runtime IDs.
  An EU size number is not its option ID.
- Brands and packages are category scoped, colors portal scoped, configuration
  account scoped, and attributes selection scoped. Follow brand correction
  actions and check `brand_validation`: ambiguous brands
  need a discovered ID and canonical name; custom brands require explicit support.
- Choose packages from upstream descriptions, not invented weight/dimension
  limits. If unclear, use `flea vinted category package-sizes CATEGORY_ID`.

## Publish, verify, and edit

```sh
flea vinted publish --input listing.json --image front.jpg --image back.jpg
flea vinted draft list
flea vinted draft show DRAFT_ID
flea vinted draft validate DRAFT_ID
flea vinted draft create --input listing.json --image front.heic
flea vinted draft publish DRAFT_ID --input listing.json
flea vinted draft delete DRAFT_ID
flea vinted listing list
flea vinted listing show ITEM_ID
flea vinted listing update ITEM_ID --input changes.json
```

Publication and draft completion require complete listing input. Draft completion
reuses remote photos unless `--image` replaces the entire set. Draft updates
also replace the complete input and photo set, not just supplied fields.
Pass images in display order; the first is the main photo. Confirm remote order
in `assigned_photos` (`display_order: 0` is main). Correlate remote photos using
these assigned IDs, not temporary `uploaded_photo_ids`.

`listing list` finds active and draft-associated account items without search
indexing, including review-pending publications. Inspect their IDs with
`listing show`. Condition/size upstream IDs are not composer IDs: use a verified
composer identity, never substitute an upstream ID when matching is unavailable.
For condition, use `identity.composer_id` only when `identity.status` is
`composer_matched`; `upstream_only` and `unavailable` are explicit limits.
Inspection reports observed size, `is_unisex`, and `category_path` when available.
Unknown values stay null; do not infer them from the title or submitted input.

Confirmed publications have `safe_to_retry: false`, including review-pending
results. `verification.status` may be `public`, `moderated`, or `timed_out`.
For uncertain or timed-out verification, follow the returned `listing show`
action, not another publish or update. Flea verifies accepted mutations with
bounded reads, never a repeated write.

`listing update` only accepts a nonempty partial JSON object of `title`,
`description`, and/or `price` strings for an owned, editable public listing.
Omitted values and existing photo order are preserved. All other changes,
including category, attributes, shipping, and photos, are unsupported; do not
work around this with guessed IDs. There is no remote revision precondition,
so a concurrent edit can be overwritten. Inspect the authoritative result before
another update; if verification fails, use the returned read-only action.
