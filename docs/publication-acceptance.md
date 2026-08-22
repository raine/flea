# Manual publication acceptance

Use a real, low-value test item that complies with Tori's rules. Automated tests
and the live smoke harness must not publish listings.

1. Authenticate with `tori auth start`, complete the browser flow, and run
   `tori auth status`.
2. Choose a selectable machine value with `tori category search QUERY` or
   `tori category list`.
3. Create a draft with an explicit category, title, description, price, trade
   type, postal code, and delivery configuration. Add one disposable test image.
4. Run `tori draft show DRAFT_ID`. Confirm every required field is set, the
   image state is `ready`, and the normalized values match the intended test
   listing.
5. Run `tori draft publish DRAFT_ID` once. Record the trace ID, listing ID,
   completed steps, warnings, and returned listing state. Do not repeat the
   command after an ambiguous failure. Inspect the draft and listing first.
6. Run `tori listing show LISTING_ID` until Tori reports the expected observed
   state. Verify the title, description, price, category, delivery, image order,
   and available actions in both CLI output and the Tori website.
7. Exercise one safe field change with `tori listing update`, then verify the
   preserved fields and replacement value with `tori listing show`.
8. Remove the test listing with `tori listing delete LISTING_ID` and verify that
   `tori listing show LISTING_ID` returns `listing.not_found`.
9. Inspect the matching JSONL trace. Confirm publication step boundaries,
   correlation fields, HTTP statuses, and the absence of tokens, cookies,
   callback URLs, signing headers, and image bytes.

Acceptance requires one publication request, correct normalized output, a
recoverable structured envelope for any failure, successful cleanup, and no
secret material in stdout or tracing logs.
