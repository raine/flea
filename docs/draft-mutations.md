# Draft field mutations and recovery

## Copying an owned listing

`flea draft create --from-listing LISTING_ID` accepts listings in the
authenticated seller's listing collection, including inactive listings retained
there. A public listing owned by another seller and a deleted listing are outside
this source scope. Flea checks collection membership before requesting copy data
or allocating a draft. An unsupported source returns `listing.not_copyable` with
a `listing_copy_eligibility` observation. This eligibility fact does not assert
that the public listing is absent. Use `flea item show LISTING_ID` for an
independent public-presence observation.

For an eligible source, the copy endpoint supplies draft-safe values. Flea copies
title, description, trade type, price, category machine values, category
attributes, location, postal code, and delivery when each field is available.
The result's `listing_copy.copied_fields` lists the values supplied to the fresh
draft. Seller identity, contact details, listing identity, publication state,
revision, and embedded image fields are omitted. Any such fields received from
the source appear in `listing_copy.omitted_fields`.

Source images use a separate byte payload. Flea preprocesses every image and
uploads it as a fresh draft attachment. Published image URLs and remote image
identifiers are never attached directly. `listing_copy.image_handling` reports
`fresh_upload_from_source_bytes`, and `source_image_count` states how many source
images entered that process. Location and delivery are ordinary copied draft
values, so the normal composer validation and mutation rules below apply.

Flea applies requested draft fields through deterministic atomic mutation
groups. Composer groups contain one top-level field and use one upstream
request. Price uses the dedicated item update endpoint. Delivery uses the
dedicated delivery composer endpoint. The order is:

1. `category`
2. `title`
3. `description`
4. `trade_type`
5. `price`
6. `postal_code`
7. Optional source-backed composer fields from `attributes`, in lexical order
8. `delivery`

Image upload and attachment follow the requested field groups during creation.
Category is first because its composer response supplies the field schema for
the selected category. Trade type precedes price because the dedicated sale
price mutation requires sale intent. Each `attributes` entry becomes a separate
composer field mutation. Flea accepts it only when the refreshed composer marks
the field optional and exposes a supported type. Select values must match the
source-backed options. Fields absent from the composer and composer fields that
Flea cannot safely encode have distinct validation codes.

A successful group is committed independently. A create or update command with
multiple fields is therefore ordered but is not an all-or-nothing transaction.
Flea stops at the first failed group. It does not repeat, bisect, or replay an
uncertain mutation to diagnose the field.

## Draft creation contract

Creation has a pre-allocation boundary. Flea prepares images, validates stable
field shapes, and checks cross-field invariants before requesting a remote draft.
Validation that depends on category, select, or composer evidence joins this
boundary whenever that evidence is available. A preflight error reports
`allocation: unattempted`, has no draft ID, and is safe from duplicate drafts.

A returned draft ID proves that allocation succeeded. Any later failure uses
`draft.create_incomplete` and a `partial.create` contract with
`allocation: persisted`, `retry_create: false`, and
`duplicate_draft_risk: true`. The original failure code remains in
`error.details.cause_code`. Repeating the create command can allocate a duplicate
and is never a continuation action.

The partial result classifies every requested field and image. Continue against
the existing draft ID with the reported `draft update` and `draft image add`
actions for absent or unattempted work. Inspect indeterminate work before any
further mutation. Draft deletion remains an explicitly reported destructive
action and is never automatic.

## Failure output

A field failure identifies its boundary in `error.details` and `partial`:

- `active_step` and `fields` identify the attempted group, such as
  `apply_price` and `[price]`.
- `persisted_fields` match the requested values in authoritative draft state.
- `absent_fields` do not match the requested values in authoritative draft
  state.
- `indeterminate_fields` cannot be classified because authoritative inspection
  failed.
- `unattempted_fields` belong to groups after the failure boundary.

An ambiguous response, including an empty success body, a success body without
`ad`, or an HTML 5xx response, triggers a bounded read-only draft observation
inside the workflow. The requested field values are compared with this
authoritative state without repeating the mutation. If every requested value is
present, the command succeeds and continues with later groups. Unrecognized 2xx
response shapes report `mutation.response_model_drift`; other ambiguous
responses report `mutation.observed_success`. An ETag change proves that a
mutation occurred, but field comparison proves what persisted.

Observed disagreement produces a bounded mixed outcome. Requested values that
match appear in `persisted_fields`, mismatches appear in `absent_fields`, and
values unavailable to authoritative observation appear in
`indeterminate_fields`. Image attachment applies the same rule to every
requested image, including attachment and processing state. If observation
fails, active work remains indeterminate and requires manual inspection.

Mutation recovery actions include only fields listed in `absent_fields`.
Persisted fields must not be included in a recovery update. An indeterminate
field has only a read-only inspection action.

## Recover one invalid field

For an update containing title, price, and postal code, a rejected price can
produce this state:

```text
persisted_fields: [title]
absent_fields: [price]
indeterminate_fields: []
unattempted_fields: [postal_code]
```

Correct the price and submit only that proven-absent field:

```sh
cat >price-recovery.json <<'JSON'
{
  "price": 25
}
JSON

flea draft update DRAFT_ID --input price-recovery.json
flea draft show DRAFT_ID
```

After inspection, submit postal code separately if authoritative state proves it
absent:

```sh
flea draft update DRAFT_ID --postal-code 00100
```

If `price` appears in `indeterminate_fields`, run the reported `flea draft show`
action and compare the authoritative value with the requested value. Do not
retry price while its state is indeterminate.
