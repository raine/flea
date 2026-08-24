# Changelog

## Unreleased

- Resolve semantic Vinted size, condition, color, and package values against
  live scoped publication catalogs while preserving explicit opaque IDs
- Add a non-interactive Vinted sell workflow that resolves semantic seller facts
  through scoped runtime discovery and returns either a validated publication
  proposal or structured resumable ambiguity choices
- Resolve unique exact or normalized Vinted brand names to category-scoped IDs,
  while preserving bounded ambiguity and runtime custom-brand policy
- Rank ambiguous Vinted publication categories with supplied listing text and
  compact scores derived from live marketplace category evidence
- Normalize Vinted listing conditions against selection-scoped runtime composer
  options while preserving distinct upstream and composer identity namespaces
- Replace the duplicate Vinted `auth` and `auth web` trees with one
  `auth login/status/logout` command set that manages both catalog credentials
  and the publication browser by default; `--api` and `--browser` select one
  layer, and existing state remains in place
- Show required Vinted category attributes and executable option-selection
  actions in the primary publication composer
- Verify confirmed Vinted publications with bounded account polling, reporting
  public, moderated, timed-out, and uncertain outcomes without risking a
  duplicate publication
- Link supplied Vinted brands outside initial composer suggestions directly to
  focused category-scoped discovery
- Keep default Vinted composer output concise with readiness, selected values,
  issues, and next actions; complete runtime option catalogs are available with
  `--full`
- Separate temporary Vinted upload photo IDs from authoritative assigned photo
  IDs and report assigned display order after every publication mutation
- Report Vinted publication category locale, expose upstream suggestions, and
  guide zero-result searches through the localized catalog
- Add bounded Vinted draft listing, complete remote draft inspection, and
  publication-readiness validation with reusable photo ordering
- Reuse verified remote photos when publishing Vinted drafts, with explicit
  replace-all image uploads and inspectable partial replacement state
- Add authenticated Vinted item inspection with exact raw output and
  exposure-aware seller-disclosed location fields
- Add direct Vinted account listing inspection and bounded active and draft
  enumeration without relying on search indexing
- Vinted catalog search supports categories, common and dynamic attributes,
  decimal price ranges, every catalog sort order, pagination, and raw output
- Vinted filter commands discover active filters, retrieve lazy facets, and
  search contextual option lists such as brands
- Nested Vinted options preserve hierarchy, counts, selected state, metadata,
  totals, and truncation in the shared facet output

## v0.1.2 (2026-08-23)

- New `flea favorite` commands list favorites folders, check whether a listing
  is saved, and save or remove a listing in the default or a chosen folder
- New `flea saved-search` commands list, inspect, rename, and delete search
  alerts
- Search alerts can be created with the same query and filter arguments as
  `flea search`
- Email, push, and notification-center alerts can be switched on or off per
  saved search, leaving untouched channels as they are
- A failed saved-search change reports a read-only recovery command and only
  calls the change safe to retry once a recovery read confirms it did not take
  effect

## v0.1.1 (2026-08-22)

- Browser login works on Linux, with clearer errors and retry instructions when
  a browser cannot be opened
- Authentication error messages point to `flea auth login` for recovery
- Category commands state up front that they require authentication
- Search results include category identifier and a readable category path
- Category results expose a `taxonomy_value` you can pass straight to search
- Search facets prioritize selected and nonzero-hit options, report how many
  options were truncated, and suggest a follow-up command for a broader view
- New `--facet-option-limit` flag for retrieving more facet options

## v0.1.0 (2026-08-22)

- Initial release
