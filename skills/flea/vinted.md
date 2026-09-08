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

## Choose the browser

By default, Flea launches its dedicated Chrome profile and chooses an available
local debugging port. To use an existing debugging-enabled Chrome instead:

```sh
flea --browser-url http://localhost:9222 vinted auth status --browser
flea --browser-url http://localhost:9222 vinted auth login --browser
```

The global `--browser-url` option selects the browser for browser authentication,
publication, and listing edits. Flea does not access its managed profile or
launch another Chrome when this option is supplied. Connection failures do not
fall back to the managed browser. API-only operations are unchanged; browser
sign-in does not replace the account credentials needed for API operations.

Chrome can enable debugging through command-line flags or
`chrome://inspect/#remote-debugging`. For the latter, ask the user to approve
Flea's connection in Chrome within 60 seconds. Do not bypass approval or human
verification. Keep the debugging endpoint private; do not expose it publicly.

On macOS and Linux, Flea automatically starts a background helper that retains
this approved connection across CLI commands. No separate terminal or server
command is needed. Browser operations using the same endpoint and local state
directory queue behind the active browser session, with a five-minute wait
limit. API-only work is not serialized by the helper. Windows uses per-command
connections instead.

Release the retained connection when finished:

```sh
flea browser disconnect --browser-url http://localhost:9222
```

Disconnect waits for active browser work to finish, then releases access without
closing Chrome, logging out, or clearing browser data. It does not start a helper
if none exists. Chrome disconnects are detected by periodic liveness checks;
failed commands are never automatically replayed. A later command can start a
fresh session and require approval again. The helper's local socket is private
to the current OS user; cookies and request bodies are not persisted by it.

Pass the same `--browser-url` on every invocation that should use this browser.
Add it to returned `next_actions` and other continuation commands even when
those commands omit it. Browser logout clears Vinted cookies and local storage
in the selected browser, not necessarily Flea's dedicated profile.

To open the dedicated Flea profile manually without enabling debugging:

```sh
flea browser
```

Close any debugging-enabled Flea Chrome instance first. Close this manual browser
before using managed browser automation again, or enable debugging through
Chrome's UI and explicitly connect with `--browser-url`. Do not combine
the bare `flea browser` command with `--browser-url`; the `disconnect` subcommand
requires it.

## Publish Vinted listings

Publication commands use ordinary Chrome cookies and human verification through
Flea's built-in browser connection. No separate browser automation client is
required. Use the default managed profile or select an existing browser with
`--browser-url` as described above.
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
For seller-confirmed unisex items, set `"is_unisex": true` in the facts JSON.
Mentioning unisex only in description text does not set the structured flag.
This flag does not replace selecting an actual category branch in Vinted's tree.
It performs scoped runtime discovery without mutating remote state. Select a
current category leaf explicitly with `--select category=ID`, even when search
returns only one candidate. After category selection, complete unambiguous facts
produce a validated `proposed_mutation`. Pass `--output listing.json` to save the
validated `listing_input` without manual copying, then run the returned publish
command. Output is written only when ready and never overwrites an existing
path; publication remains a separate deliberate action. Missing or ambiguous values contain runtime
choices and resumable `--select FIELD=ID` commands. Prefer a durable facts file
for this multi-step workflow: consumed stdin cannot be replayed. Follow the
returned actions instead of fuzzy matching, selecting the first category, or
inventing IDs. An unrecognized semantic phrase is not evidence of several
matching choices: inspect the returned accepted labels and select the one that
matches seller facts. Choice-local commands are executable continuations; do
not expect every choice command to be repeated in top-level `next_actions`.

`listing list` enumerates active and draft-associated items for the authenticated
account without relying on search indexing. Use it to verify that a
review-pending publication exists while Vinted hides it from public search,
then inspect the returned item ID with `listing show`. Listing conditions expose
separate `identity.upstream_id` and `identity.composer_id` values. Use the
composer ID for publication correlation when `identity.status` is
`composer_matched`. Treat `upstream_only` and `unavailable` as explicit limits,
not as permission to submit a listing-side ID to the composer. Listing inspection
also exposes observed `size` and `is_unisex`, and a `category_path` when current
catalog lookup succeeds. Size keeps upstream and composer IDs separate; unknown
values remain null rather than being inferred from the title or submitted facts.

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

### Find a publication category

The current publication catalog is authoritative. Category search matches
portal-localized taxonomy labels across complete category paths. Publication
search adds hints, not selection authority. Marketplace-count recommendations
are opt-in with `--marketplace-evidence` on `category search` or `sell`; normal
discovery does not call that optional service. If explicitly requested evidence
fails, its warning remains visible and catalog discovery still works.
When local path matches exist, hints only annotate those candidates; unrelated
hint-only results are excluded. Hint-only fallback is used when local matching
finds nothing. A successful hint stage does not mean every hint was included.
Supply known title and description as recommendation context. Inspect full
paths, leaf flags, provenance, warnings, and truncation. Honor `selection_required`.
When available, `marketplace_evidence.recommendations` contains relative listing
support, not classification confidence, and zero matching listings do not
exclude a valid category.

If results are empty, broad, or a hint service fails, browse the catalog:

```sh
flea vinted category list --roots
flea vinted category list --parent PARENT_ID
flea vinted category search 'OBSERVED ANCESTOR AND SHOE TYPE' --parent PARENT_ID
```

Compact browsing needs no keyword or marketplace search. Follow pagination
actions for truncated results. `category list` without browsing flags remains
the full raw catalog export; do not load that entire tree into agent context as
the normal fallback. An HTTP 404 from optional ranking is not proof that no
category exists and is not a reason to keep retrying that service.

Interpret seller language using observed taxonomy labels. For example, Finnish
`lasten kengät` or English `children's shoes` can lead an agent to the observed
`Lapset` branch, but the CLI does not translate or equate `lasten` with `Lapset`.
Search `Lapset kengät`, then narrow the actual girls/boys branch and shoe type.
Preserve child/baby/adult and type constraints when rephrasing. Ask for missing
material facts rather than choosing a branch by popularity, color stereotypes,
or guessed IDs. Do not invent a generic unisex branch when the tree has none.
Keep the seller's listing text in their chosen language.

Nonleaf results are browse targets, never publication choices. Select a current
leaf only when its complete path fits established seller facts, then use the
returned `--select category=ID` continuation or `category compose ID`. A single
candidate still requires intentional selection. Category selection alone does
not mean the listing is ready or authorized for publication.

Composer returns readiness, issues, selected values, brand validation, and
correction actions. Prefer focused field options instead of dumping `--full`:

```sh
flea vinted category compose CATEGORY_ID --field attribute.size --input listing.json
flea vinted category compose CATEGORY_ID --field attribute.condition --input listing.json
```

Focused output includes only that field's issues and selectable options, with
`--option-limit` and `--option-offset` pagination. Preserve the same input and
selection context across pages. Apply the chosen option to the input and rerun
the composer; never submit group headings. Use `--full` only when the complete
form is genuinely needed. Discover size, condition, and other attributes for
the chosen category; never assume an EU shoe size is its opaque option ID or
reuse IDs from another branch.

Use upstream package descriptions shown with choices to decide package size.
If descriptions do not establish applicable limits, inspect package discovery
rather than inventing weights or dimensions.

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
