---
name: flea
description: Operate Tori.fi through the flea CLI. Use for marketplace search, location and category discovery, authentication, draft creation and publishing, image management, and published listing management.
---

# Flea

Use `flea` as an independent interface to Tori.fi. Keep the default TOON output for compact agent-readable results. Use `--format json` only when another tool requires JSON.

## Operating rules

- Run `flea <command> --help` for exact arguments.
- Follow `next_actions` when present. Empty optional sections are omitted.
- Treat returned IDs and option values as machine values. Discover them with `category`, `location`, or the relevant `show` command instead of guessing.
- Read commerce data from `trade_type`, `price.kind`, `price.amount`, and `price.currency`. `price.amount` is a JSON number when `price.kind` is `fixed`. Never parse `price.display`.
- Inspect remote state after mutations. Draft and listing responses are authoritative.
- Use `draft preview` before creation when a complete local input needs checking. Preview is not publication readiness.
- Read `upstream_transient` and `safe_to_retry` independently. A temporary upstream failure can leave a mutation unsafe to repeat.
- Do not repeat uncertain mutations. Use `partial`, error details, returned IDs, and the authoritative command in `next_actions` to recover.
- Draft field failures report persisted, absent, indeterminate, and unattempted fields. Retry only fields proven absent. Inspect the draft before acting on an indeterminate field.
- Draft recovery summaries include bounded field and image lifecycle classifications, the failed stage, completed steps, observation status and time, and the latest ETag or revision. Treat indeterminate work as observation-only. Destructive cleanup commands require explicit intent.
- Image recovery reports upload, attachment, and processing independently. A completed upload can remain unattached, and an attached image can remain processing or fail.
- A field cannot appear in both flags and `--input` JSON.
- `flea auth status` applies the same 30-second bearer-validity policy as authenticated commands. It refreshes near-expiry or expired credentials through the locked atomic command path. Treat `authenticated: true` as usable under that policy. Follow the reported browser-login action for `temporarily_unavailable`, `refresh_rejected`, or `malformed` because an attempted token mutation can have an uncertain outcome.

## Marketplace discovery

1. Define geographic scope. `--location Helsinki` means the exact Tori city. A phrase such as "Helsinki area" has no fixed boundary, so ask which places it includes or state an explicit choice such as `--area Helsinki,Espoo,Vantaa`. Use `--latitude`, `--longitude`, and `--radius-km` when the request defines an actual distance. State the chosen scope in the findings.
2. Start with the requested product name and appropriate filters. If recall may be poor, run a small set of meaningful aliases, spelling or hyphenation variants, and word-order variants. Broaden category, price, or geography only when useful, and say what changed.
3. Merge searches by numeric `listing_id`, keeping one entry per listing. Search rank is evidence of relevance, not product identity, and relevance ranks from different queries are not directly comparable.
4. Separate exact matches from plausible matches. Require the title or public details to confirm the requested model or identifying attributes before calling a match exact. For a generic title or a result matched through hidden text, run `flea item show LISTING_ID` and use its description and attributes when available. Keep uncertain candidates plausible and discard clear noise.
5. Return concise linked entries, typically title, normalized price, location, and canonical URL. Compare or filter fixed prices with `price.amount`, and report free or negotiable results from `price.kind`. State the ordering, such as relevance within one query, distance, recency, or price. For merged queries, choose a useful deterministic ordering rather than implying one global relevance rank.

For JSON automation, select the normalized fields directly:

```sh
flea --format json listing list \
  | jq '.data.listings[] | {listing_id, trade_type, price: {kind: .price.kind, amount: .price.amount, currency: .price.currency}}'
```

## Category discovery

Category search returns at most 20 ranked matches by default, with `returned`, `total`, and `truncated` metadata. Use the exact `category_id` from a result as the machine value. Querying an exact ID or label puts that category first.

Refine broad terms with a returned parent ID or path instead of increasing the limit until the full taxonomy is printed. For example:

```sh
flea category search tarvikkeet
flea category search tarvikkeet --path 'Urheilu ja ulkoilu > Pyöräily'
PARENT="$(flea --format json category search tarvikkeet \
  --path 'Urheilu ja ulkoilu > Pyöräily' \
  | jq -er '.data.context.category_id')"
flea category search tarvikkeet --parent "$PARENT"
```

Both refined searches cover descendants and rank `Pyöräilyvarusteet` prominently. Follow `next_actions` when a page is truncated because those commands retain the query, hierarchy context, offset, and limit. Use an explicit `--limit` from 1 through 100 only when a larger page is useful.

## Commands

```sh
flea search [QUERY] [filters]           # Public search, no login required
flea item show LISTING_ID               # Inspect public details, no login required
flea location search NAME              # Resolve marketplace location IDs
flea category search QUERY              # Resolve category machine values

flea auth status
flea auth login                         # Opens browser authentication
flea auth logout

flea draft preview [fields]                 # Offline and zero-mutation by default
flea draft preview --input PATH --verify-category
flea draft create [fields]
flea draft show DRAFT_ID
flea draft update DRAFT_ID [fields]
flea draft image add DRAFT_ID PATH...
flea draft image remove DRAFT_ID IMAGE_ID...
flea draft validate DRAFT_ID
flea draft publish DRAFT_ID --if-revision REVISION
flea draft delete DRAFT_ID

flea listing list
flea listing show LISTING_ID
flea listing update LISTING_ID [fields]
flea listing dispose LISTING_ID
flea listing delete LISTING_ID
```

Local draft preview works without authentication. Category-enriched preview and account draft or published listing work require authentication. Preview reports local assumptions and unverifiable requirements, while `draft validate DRAFT_ID` authoritatively checks an existing remote draft without changing it. Build a draft incrementally, inspect required fields and allowed options with `draft show`, and upload images. Before publication, carry the exact validated revision into the mutation:

```sh
validation="$(flea --format json draft validate DRAFT_ID)"
printf '%s\n' "$validation" | jq -e '.ok and .data.ready'
revision="$(printf '%s\n' "$validation" | jq -er '.data.revision')"
flea draft publish DRAFT_ID --if-revision "$revision"
```

A revision conflict is unsafe to retry unchanged. Follow its read-only `draft show` or `draft validate` next action, review the changed state, and publish only with the revision from that review. Publish, dispose, and delete act without confirmation, so run them only when requested.
