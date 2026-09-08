---
name: flea
description: Route Tori.fi and Vinted tasks to flea's complete marketplace guides.
---

# Flea

Use flea to operate Tori.fi and Vinted. Keep the default TOON output for agent
operations because it is token-efficient. Pass `--format json` only when a
downstream command requires strict JSON for machine parsing. Run
`flea capabilities` for current support.

## Load marketplace guidance

Before searching, inspecting listings, creating or editing a draft, publishing,
or managing account state, load the complete guide for the marketplace involved:

```sh
flea skill tori
flea skill vinted
```

Use `flea skill tori` for every Tori task and `flea skill vinted` for every
Vinted task. Follow the selected guide without loading the other marketplace's
guide.

## Browser selection

Use `--browser-url http://localhost:9222` for existing debugging-enabled Chrome.
Preserve it on follow-up commands. See `flea skill vinted` for approval and
manual-browser guidance.

## Shared safety rules

- Treat IDs, revisions, and options as opaque. Discover them with the commands
  documented in the marketplace guide.
- Follow `next_actions` and `safe_to_retry`; inspect uncertain mutations before
  retrying.
- Remote state wins. Do not duplicate a field in flags and `--input` JSON.
- Put category fields in `attributes` using the draft's composer model.
