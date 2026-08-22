# Changelog

## Unreleased

- Favorites folders and saved status can be inspected, and marketplace listings
  can be saved to or removed from the default or an explicitly selected folder

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
