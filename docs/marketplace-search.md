# Public marketplace search

Flea is an independent, unofficial tool and is not affiliated with or endorsed by Tori.fi.
Public marketplace search is available without Tori.fi account authentication. Output uses the
standard single envelope, with TOON by default and JSON through `--format json`.

## Basic search

```sh
flea search "ruokapöytä"
flea search "iphone" --price-from 100 --price-to 500 --limit 50
flea search --trade-type give-away --condition 3 --seller private
flea search "takki" --shipping --sort newest --page 2 --limit 20
```

The positional query is optional when filters identify the desired listings.

## Explain or inspect a search result

Search summaries can omit the description that caused a generic-title result to match. Search
protocol documents do not provide per-result match fields or snippets. Use `--explain LIMIT` to
hydrate at most `LIMIT` opaque results from the public item service, in search result order:

```sh
flea search "micro mini potkulauta" --explain 5 --format json
```

`LIMIT` must be from 1 through 20. The default search makes no item detail requests. Results whose
titles already contain every normalized query token need no explanation and consume no requests.
Each explained result has a `match_explanation` with `source_field: description`, a sanitized
excerpt of at most 160 characters, and the matching query terms. `evidence_origin: public_item`
identifies the source document, while `match_method: cli_derived_token_match` makes clear that the
CLI compared normalized tokens rather than receiving match evidence from the search service.

The top-level `explain` summary reports the request limit, attempted requests, successful
hydrations, explanations, and whether additional opaque results were left unhydrated by the bound.
A failed detail request appears in `failures` with its listing ID, structured error code, and retry
classification. Other search results and successful explanations remain in the response.

For the full public detail, pass the numeric `listing_id` from a search result to the item command:

```sh
flea item show 45917182
flea item show 45917182 --format json
```

`flea item show` uses the public listing-detail service and does not read account credentials or
attach an authorization header. Normalized output includes the title, full description, structured
price and textual location, condition, seller and shipping metadata, images, publication time, and
canonical URL when Tori supplies each value. Seller and shipping objects remain present with null
fields when Tori withholds those details from anonymous clients. Precise coordinates and upstream
owner identifiers are omitted.

Use `--raw` to inspect the bounded upstream protocol document:

```sh
flea item show 42346404 --raw --format json
```

The command rejects malformed IDs locally. Removed or missing listings return `item.not_found`, and
an upstream expiration response returns `item.expired`. These structured errors include the
requested ID and an action that returns to public search.

## Categories and dynamic facets

`--category` accepts a Tori taxonomy value and selects the upstream parameter from its depth:

- `0.93` uses `category`
- `1.93.3215` uses `sub_category`
- `2.93.3215.46` uses `product_category`

Dynamic options returned by Tori can be supplied repeatedly as `--facet NAME=VALUE`:

```sh
flea search "tuoli" --category 1.93.3215 --facet brand=42 --facet brand=84
flea search "tuoli" --include-facets --format json
```

`--include-facets` requests available filter metadata. Each normalized facet includes its machine
name, display label, type, range metadata when present, and bounded recursive options with machine
values. `option_count` and `truncated` disclose the 500-option per-facet output bound. Use dedicated
flags for category, location, price, trade type, condition, seller, shipping, coordinates, sorting,
and pagination.

Trade types are `sell`, `give-away`, and `wanted`. Seller values are `private` and `business`.
Condition uses Tori's returned machine value and may be repeated. `--shipping` sends the
source-observed `shipping_exists=true` filter.

Sorting accepts `relevance`, `newest`, `price-asc`, and `price-desc`. These map to the observed Tori
values `RELEVANCE`, `PUBLISHED_DESC`, `PRICE_ASC`, and `PRICE_DESC`.

## Locations

Discover Tori location identifiers with a bounded, deterministic name search. The result reports
`returned`, `total`, and `truncated` counts for the 100-location output bound:

```sh
flea location search Helsinki
flea location search Uusimaa --format json
```

`--location` selects one exact location. It accepts either the exact identifier or an unambiguous,
case-insensitive exact Tori place name. Unknown names return `search.location_not_found` with a
location-discovery command. Names matching multiple Tori locations return
`search.location_ambiguous` with the matching IDs, so the caller can choose instead of relying on
an undocumented interpretation.

A runnable exact Helsinki search is:

```sh
flea search "tuoli" --location Helsinki --limit 20
```

`--area` explicitly searches a set of 2 through 20 Tori locations. Supply a comma-separated list
of exact IDs or unambiguous names. Tori's search API represents the set as repeated `location`
filters. The CLI does not infer what a phrase such as "Helsinki area" includes. This Helsinki-area
example explicitly includes three neighboring capital-region municipalities:

```sh
flea search "tuoli" --area Helsinki,Espoo,Vantaa --limit 20
```

Normalized output exposes every selected location under `resolved_area.locations`, including its
Tori ID, name, parent, and taxonomy depth. Exact searches continue to expose
`resolved_location`. Pagination actions preserve an area with its resolved IDs.

Coordinate radius searches require all three arguments. Radius is expressed in kilometers and is
encoded as Tori's integer meter value:

```sh
flea search "tuoli" \
  --latitude 60.1699 \
  --longitude 24.9384 \
  --radius-km 20 \
  --limit 20
```

`--distance-km` is an alias for `--radius-km`. Latitude must be from -90 through 90, longitude from
-180 through 180, and radius must be positive and at most 1000 km. `--area`, `--location`, and
coordinate radius arguments are mutually exclusive.

Default listing output is a compact discovery projection. Every result provides its listing ID,
title, structured price, concise textual location, canonical listing URL, ISO 8601 publication
time, and image count when Tori supplies the corresponding data. Distance, condition, shipping,
and seller information appear when available. Precise coordinates, image URL arrays, internal
listing types, duplicate display prices, millisecond timestamps, empty collections, labels, flags,
and other protocol fields are omitted.

The top-level `query` and optional resolved `location` describe search context once. Resolved
location context contains its machine ID, name, and parent when available. `applied_filters`
contains remaining active filters only when present, without repeating the query or resolved
location. `facets` appears only when `--include-facets` returns facet data. `--raw` returns the
bounded upstream document when protocol inspection requires all upstream fields.

## Pagination

Pages are one-indexed. Tori accepts pages 1 through 50 and page sizes 1 through 300. The CLI
default is 20 results and never fetches all matches implicitly.

Normalized pagination contains `page`, `limit`, `returned`, `total`, `has_next`, and `next_page`
when another accessible page exists. Upstream boundaries and calculated implementation fields stay
out of default output.

The envelope provides a next-page action while another upstream page is accessible. At the page
boundary, its executable refinement action returns to page 1 and adds `--include-facets`, so the
query, category, price, facets, or location can be narrowed to reach additional matches.

## JSON input

`--input PATH` accepts a JSON object. Use `-` to read at most 1 MiB from standard input. Supported
keys use flag names with underscores, including `query`, `category`, `location`, `area`,
`latitude`, `longitude`, `radius_km`, `price_from`, `price_to`, `trade_type`, `condition`,
`seller`, `shipping`, `facets`, `sort`, `page`, `limit`, `explain`, `include_facets`, and `raw`.

```sh
cat >search.json <<'JSON'
{
  "query": "tuoli",
  "location": "Helsinki",
  "price_to": 100,
  "facets": {"brand": ["42", "84"]},
  "page": 1,
  "limit": 20
}
JSON

flea search --input search.json --format json
```

A field present in both JSON and a command argument is rejected. Repeated scalar flags are rejected
by the parser. Repeated `--facet` and `--condition` values are intentional multi-value inputs.

## Flag reference

- `QUERY`: optional free-text query, at most 500 characters
- `--category TAXONOMY`: validated category, subcategory, or product category value, at most 64
  characters
- `--location ID_OR_NAME`: one exact Tori identifier or unambiguous place name, at most 256
  characters
- `--area PLACE,PLACE,...`: 2 through 20 explicit Tori identifiers or unambiguous place names
- `--latitude NUMBER`: decimal latitude, requires longitude and radius
- `--longitude NUMBER`: decimal longitude, requires latitude and radius
- `--radius-km NUMBER`, `--distance-km NUMBER`: positive radius up to 1000 km
- `--price-from INTEGER`: minimum price in euros
- `--price-to INTEGER`: maximum price in euros
- `--trade-type VALUE`: `sell`, `give-away`, or `wanted`
- `--condition VALUE`: repeatable Tori condition machine value, 1 through 256 characters
- `--seller VALUE`: `private` or `business`
- `--shipping`: require listings with Tori shipping
- `--facet NAME=VALUE`: repeatable dynamic Tori facet
- `--sort VALUE`: `relevance`, `newest`, `price-asc`, or `price-desc`
- `--page INTEGER`: page 1 through 50
- `--limit INTEGER`: results 1 through 300, default 20
- `--explain LIMIT`: hydrate and explain at most 1 through 20 opaque results
- `--include-facets`: include normalized available facets and options
- `--input PATH`: JSON input object or `-` for standard input
- `--raw`: return bounded upstream JSON inside the standard envelope
- `--format toon|json`: global output format, default TOON

Search uses bounded transient retries for GET requests, the shared signed transport, bounded
responses, structured errors, and sanitized diagnostics. It does not read account credentials,
refresh tokens, or attach an authorization header.
