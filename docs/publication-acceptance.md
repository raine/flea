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
3. Run `flea draft preview` with the explicit category, title, description,
   price, trade type, postal code, delivery configuration, and one disposable
   test image. Confirm that local validation passes and review every assumption
   and unverifiable requirement. Preview creates no remote state and is not an
   authoritative publication-readiness check.
4. Create the draft from the previewed input. Run `flea draft show DRAFT_ID`.
   Confirm every required field is set, the
   image state is `ready`, and the normalized values match the intended test
   listing.
5. Run the authoritative read-only `flea draft validate DRAFT_ID` check on the
   persisted draft. Confirm it reports `ready: true`. The command makes only
   read requests. Resolve every reported missing, invalid, pending, or
   unverifiable requirement before publication.
6. Run `flea draft publish DRAFT_ID` once. Record the trace ID, listing ID,
   publication status, mutation flag, completed steps, warnings, and returned
   listing state. Publication performs an authoritative active-listing check
   before its first mutation. An active ID returns `already_published` with
   `mutations_performed: false` and its public URL.
7. Run `flea listing show LISTING_ID`. Detail observation falls back to the
   published-listing collection by exact ID, so every active item from
   `listing list` remains observable. Verify the title, description, price,
   category, delivery, location, image order, public URL, and available actions
   in both CLI output and the Tori website. A persisted publication whose
   bounded observation remains unavailable returns
   `publication.observation_uncertain`, timing and attempt details, and this
   read-only command as its next action.
8. Exercise one safe field change with `flea listing update`, then verify the
   preserved fields and replacement value with `flea listing show`.
9. Remove the test listing with `flea listing delete LISTING_ID` and verify that
   `flea listing show LISTING_ID` returns `listing.not_found`.
10. Inspect the matching JSONL trace. Confirm publication step boundaries,
   correlation fields, HTTP statuses, and the absence of tokens, cookies,
   callback URLs, signing headers, and image bytes.

Acceptance requires one publication request, correct normalized output, a
recoverable structured envelope for any failure, successful cleanup, and no
secret material in stdout or tracing logs.
