# Manual publication acceptance

Use a real, low-value test item that complies with Tori's rules. Automated tests
and the live smoke harness must not publish listings.

1. Authenticate with `flea auth start`, complete the browser flow, and run
   `flea auth status`.
2. Choose a selectable machine value with `flea category search QUERY` or
   `flea category list`. Discover category IDs from Tori's listing-composer
   taxonomy for every workflow. Do not guess or retain an ID based on its label.

   Broad searches return 20 ranked results by default and report `returned`,
   `total`, and `truncated`. Refine a broad term with a returned hierarchy path
   instead of printing the full taxonomy. For example, discover bicycle
   accessories and pass the exact returned machine value into draft creation:

   ```sh
   flea category search tarvikkeet
   flea category search tarvikkeet --path 'Urheilu ja ulkoilu > Pyöräily'
   CATEGORY="$(flea --format json category search tarvikkeet \
     --path 'Urheilu ja ulkoilu > Pyöräily' \
     | jq -er '.data.categories[] | select(.label == "Pyöräilyvarusteet" and .selectable) | .category_id' \
     | head -n 1)"
   flea draft create --category "$CATEGORY"
   ```

   `--parent` accepts a returned category ID for the same descendant search,
   while `--path` accepts an exact label or returned path. `--limit` accepts 1
   through 100 results. Follow `next_actions` to continue a truncated search
   without losing its query or hierarchy context. Browse direct children with
   `flea category list` and `flea category list --parent RETURNED_PARENT_ID`.
   A query with no matches succeeds with an empty `categories` collection.
3. Create a draft with an explicit category, title, description, price, trade
   type, postal code, and delivery configuration. Add one disposable test image.
4. Run `flea draft show DRAFT_ID`. Confirm every required field is set, the
   image state is `ready`, and the normalized values match the intended test
   listing.
5. Run `flea draft publish DRAFT_ID` once. Record the trace ID, listing ID,
   completed steps, warnings, and returned listing state. Do not repeat the
   command after an ambiguous failure. Inspect the draft and listing first.
6. Run `flea listing show LISTING_ID` until Tori reports the expected observed
   state. Verify the title, description, price, category, delivery, image order,
   and available actions in both CLI output and the Tori website.
7. Exercise one safe field change with `flea listing update`, then verify the
   preserved fields and replacement value with `flea listing show`.
8. Remove the test listing with `flea listing delete LISTING_ID` and verify that
   `flea listing show LISTING_ID` returns `listing.not_found`.
9. Inspect the matching JSONL trace. Confirm publication step boundaries,
   correlation fields, HTTP statuses, and the absence of tokens, cookies,
   callback URLs, signing headers, and image bytes.

Acceptance requires one publication request, correct normalized output, a
recoverable structured envelope for any failure, successful cleanup, and no
secret material in stdout or tracing logs.
