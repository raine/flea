# Changelog

## v0.1.4 (2026-09-08)

- Breaking: Vinted publishing and editing now require the Chrome extension in
  your normal signed-in browser instead of a separate browser profile; run
  `flea extension setup` to get started
- Update installer and Cargo installations to the latest release with
  `flea update`; Homebrew and Nix installations continue using their package manager
- Filter each Vinted search page for disclosed Finnish sellers and maximum
  reported shipping cost with `--seller-country FI` and `--max-shipping`
- See buyer-protection fees and fee-inclusive item prices in Vinted search,
  with optional seller details and shipping quotes; checkout prices may differ
- See why Vinted search results were excluded by each filter, including missing
  information and lookup limits
- Update the title, description, and price of your Vinted listings without
  replacing their photos or other listing details
- Improve listing-update verification while Vinted processes changes, and show
  condition, size, and category details more reliably
- Find Vinted publication categories more reliably, with category-tree browsing
  when search does not find the right match
- Make guided Vinted listing choices clearer, keeping category matches focused
  and excluding non-selectable size and condition headings
- Inspect individual listing fields with `flea vinted category compose --field`
  and save ready-to-publish proposals with `flea vinted sell --output`
- Fix Nix builds failing when downloading dependencies

## v0.1.3 (2026-09-08)

- Breaking: Tori commands now use `flea tori ...`. Existing unscoped credentials
  are removed; sign in again with `flea tori auth login`
- Discover supported marketplaces, portals, and operations offline with
  `flea marketplaces` and `flea capabilities`
- Add Vinted Finland login, status, and logout for both catalog access and a
  persistent publication browser, with automatic catalog session refresh
- Search and browse Vinted with category and attribute filters, decimal price
  ranges, sorting, pagination, and raw output; discover available filter options
- Inspect Vinted items, including seller-disclosed locations when available,
  and list your active listings and drafts without relying on search indexing
- Create, inspect, update, validate, delete, and publish Vinted drafts, or
  publish directly through a signed-in Chrome browser
- Prepare Vinted listings with `flea vinted sell`, resolving category, brand,
  size, condition, color, and package choices into a validated proposal without
  publishing; ambiguous choices can be selected and resumed
- Discover Vinted publication categories and required attributes, with
  listing-based category rankings and concise validation guidance; use `--full`
  for complete composer options
- Reuse existing Vinted draft photos when publishing, optionally replace the
  full photo set, and inspect the resulting photo order
- Automatically check confirmed Vinted publications for public or moderated
  status, with safe follow-up guidance when verification times out or fails
- Fix Tori favorite and draft changes, postal-code updates, photo removal, and
  deletion of newly created drafts
- Recover Tori price and trade details from listing subtitles when missing from
  the listing's structured details
- Speed up large HEIC/HEIF photo preparation on macOS and improve timeout errors
- Add dedicated agent guides through `flea skill tori` and `flea skill vinted`

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
