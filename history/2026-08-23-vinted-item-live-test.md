# Vinted item-show live test report

Date: 2026-08-23
Portal: Vinted Finland
Result: pass

This report contains aggregate and structural observations only. The test run did
not retain raw responses, listing IDs, seller names, locations, descriptions,
URLs, image metadata, credentials, cookies, account or device identifiers, or
request identifiers.

## Live discovery and sample

Twenty-two authenticated live searches completed successfully. Searches covered
newest listings across several pages, generic browsing, low and high price
bounds, and Finnish queries spanning clothing, books, phones, footwear, bags,
jewelry, children's clothing, designer goods, and common brands.

The primary comparison sample contained 72 available listings selected from 174
unique search results. It covered:

- 60 distinct category IDs
- 25 prices below EUR 5
- 40 prices from EUR 5 through EUR 24.99
- 6 prices from EUR 25 through EUR 99.99
- 1 price of at least EUR 100
- 48 private sellers with an exposure-permitted city
- 3 private sellers with an exposure-permitted country fallback and no city
- 21 private sellers whose explicit location fields were hidden by the exposure
  flag

Each of the 72 listings succeeded in both normalized and `--raw` JSON modes.
All 72 live upstream payloads used the direct `item` plus `plugins` response
shape. The live endpoint did not return a `data`-wrapped item payload.

A separate business/pro probe inspected 240 available listings selected from 14
successful searches. Every inspected seller had private classification, and no
business-information plugin was present. The Finnish live catalog sample did
not supply a business/pro case.

## Normalization and location safety

For every primary-sample listing, normalized title, description, listing ID,
and numeric price were compared with the corresponding live raw payload in
memory. All comparisons matched.

Seller location expectations were derived only from these permitted raw fields:

1. `seller_info_business.data.seller_location`, when such a plugin exists
2. `item.user.city` when `item.user.expose_location` is true
3. `item.user.country_title_local` as fallback when exposure is true and city is
   absent

The normalized `seller.seller_disclosed_location` value and source matched those
rules in all 72 comparisons. The 21 hidden-location responses produced no
normalized location. No value was inferred from title, description, URL,
breadcrumb, catalog, or other presentation fields.

This field describes seller-disclosed profile location. It does not describe or
guarantee the item's physical location.

## Output and repeatability

- Explicit JSON normalized output succeeded for all 72 primary listings.
- Explicit JSON `--raw` output succeeded for all 72 primary listings.
- Default TOON normalized and `--raw` output each rendered successfully for an
  additional current search result, with empty stderr.
- Five current listings were shown twice more in normalized mode. All 10 repeat
  calls succeeded without shape or compatibility errors.
- No live raw document was written to disk or copied into this report.

Existing sanitized fixtures prove exact raw-document preservation inside the CLI
envelope and cover direct and wrapped response normalization. The implementation
and fixtures also cover malformed documents, removed-listing mapping, missing
location, business plugin location, and the rule against presentation-text
location inference.

## Validation and error behavior

- A current stored session authenticated successfully.
- An isolated empty credential store returned `vinted_auth.required` with the
  authentication exit class before item network access.
- Zero, negative, decimal, path-like, overlong, and unsigned-64-overflow IDs all
  returned `vinted_item.invalid_id` with validation exit code 20.
- A low positive nonexistent ID and a leading-zero variant returned
  `vinted_item.not_found` with validation exit code 20 and no retry guidance.
- The maximum unsigned-64 ID reached the upstream contract and returned
  `vinted_item.upstream_failed` with upstream exit code 40. The error was
  non-transient and not safe to retry.
- Omitting `ITEM_ID` produced a command-line usage failure that identified the
  required argument.

## Live limitations

The catalog sample supplied no business/pro seller, absent-location response,
wrapped response, malformed success response, or listing that became removed or
unavailable between search and item inspection. The test therefore makes no
live claim for those scenarios. Their behavior is covered by the existing
sanitized fixture suite where applicable.

An invalid or expired stored session was not manufactured from the configured
credentials. Missing-credential behavior and successful authenticated behavior
were exercised without reading, copying, or modifying credential contents.
