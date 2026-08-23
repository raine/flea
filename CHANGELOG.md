# Changelog

## Unreleased

- Reuse verified remote photos when publishing Vinted drafts, with explicit
  replace-all image uploads and inspectable partial replacement state
- Add authenticated Vinted item inspection with exact raw output and
  exposure-aware seller-disclosed location fields

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
