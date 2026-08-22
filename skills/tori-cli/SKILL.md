---
name: tori-cli
description: Operate Tori.fi through the tori CLI. Use for marketplace search, location and category discovery, authentication, draft creation and publishing, image management, and published listing management.
---

# Tori CLI

Use `tori` as the interface to Tori.fi. Keep the default TOON output for compact agent-readable results. Use `--format json` only when another tool requires JSON.

## Operating rules

- Run `tori <command> --help` for exact arguments.
- Follow `next_actions` when present. Empty optional sections are omitted.
- Treat returned IDs and option values as machine values. Discover them with `category`, `location`, or the relevant `show` command instead of guessing.
- Inspect remote state after mutations. Draft and listing responses are authoritative.
- Do not repeat uncertain mutations. Use `partial`, error details, and returned IDs to recover.
- A field cannot appear in both flags and `--input` JSON.

## Commands

```sh
tori search [QUERY] [filters]           # Public search, no login required
tori location search NAME              # Resolve marketplace location IDs
tori category search QUERY              # Resolve category machine values

tori auth status
tori auth login                         # Opens browser authentication
tori auth logout

tori draft create [fields]
tori draft show DRAFT_ID
tori draft update DRAFT_ID [fields]
tori draft image add DRAFT_ID PATH...
tori draft image remove DRAFT_ID IMAGE_ID...
tori draft publish DRAFT_ID
tori draft delete DRAFT_ID

tori listing list
tori listing show LISTING_ID
tori listing update LISTING_ID [fields]
tori listing dispose LISTING_ID
tori listing delete LISTING_ID
```

Authenticate before account draft or published listing work. Build a draft incrementally, inspect required fields and allowed options with `draft show`, upload images, and inspect again. Publish, dispose, and delete act without confirmation, so run them only when requested.
