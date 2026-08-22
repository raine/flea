# Public marketplace search

Public marketplace search is available without Tori account authentication. Output uses the
standard single envelope, with TOON by default and JSON through `--format json`.

## Basic search

```sh
tori search "ruokapöytä"
tori search "iphone" --price-from 100 --price-to 500 --limit 50
tori search --trade-type give-away --condition 3 --seller private
tori search "takki" --shipping --sort newest --page 2 --limit 20
```

The positional query is optional when filters identify the desired listings.

## Categories and dynamic facets

`--category` accepts a Tori taxonomy value and selects the upstream parameter from its depth:

- `0.93` uses `category`
- `1.93.3215` uses `sub_category`
- `2.93.3215.46` uses `product_category`

Dynamic options returned by Tori can be supplied repeatedly as `--facet NAME=VALUE`:

```sh
tori search "tuoli" --category 1.93.3215 --facet brand=42 --facet brand=84
tori search "tuoli" --include-facets --format json
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
tori location search Helsinki
tori location search Uusimaa --format json
```

`--location` accepts either the exact identifier or an exact case-insensitive Tori place name.
Resolution prefers the shallowest exact match and then the lexicographically smallest identifier,
which makes duplicate names deterministic.

A runnable Helsinki name search is:

```sh
tori search "tuoli" --location Helsinki --limit 20
```

Coordinate radius searches require all three arguments. Radius is expressed in kilometers and is
encoded as Tori's integer meter value:

```sh
tori search "tuoli" \
  --latitude 60.1699 \
  --longitude 24.9384 \
  --radius-km 20 \
  --limit 20
```

`--distance-km` is an alias for `--radius-km`. Latitude must be from -90 through 90, longitude from
-180 through 180, and radius must be positive and at most 1000 km. A named location cannot be
combined with coordinate radius arguments.

Normalized listing output omits precise upstream coordinates. It includes textual location and a
finite positive `distance` when Tori supplies one, while omitting Tori's zero placeholder. `--raw`
explicitly returns the bounded upstream document when protocol inspection requires all upstream
fields.

## Pagination

Pages are one-indexed. Tori accepts pages 1 through 50 and page sizes 1 through 300. The CLI
default is 20 results and never fetches all matches implicitly.

Normalized pagination includes:

- The explicit `page` and `limit`
- Returned and total counts
- Calculated total pages
- Accessible pages under Tori's page 50 boundary
- Previous and next page numbers when accessible
- `capped: true` when matches exist beyond the accessible range

The envelope provides a next-page action while another upstream page is accessible. At the page
boundary, its executable refinement action returns to page 1 and adds `--include-facets`, so the
query, category, price, facets, or location can be narrowed to reach additional matches.

## JSON input

`--input PATH` accepts a JSON object. Use `-` to read at most 1 MiB from standard input. Supported
keys use flag names with underscores, including `query`, `category`, `location`, `latitude`,
`longitude`, `radius_km`, `price_from`, `price_to`, `trade_type`, `condition`, `seller`, `shipping`,
`facets`, `sort`, `page`, `limit`, `include_facets`, and `raw`.

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

tori search --input search.json --format json
```

A field present in both JSON and a command argument is rejected. Repeated scalar flags are rejected
by the parser. Repeated `--facet` and `--condition` values are intentional multi-value inputs.

## Flag reference

- `QUERY`: optional free-text query, at most 500 characters
- `--category TAXONOMY`: validated category, subcategory, or product category value, at most 64
  characters
- `--location ID_OR_NAME`: exact Tori identifier or place name, at most 256 characters
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
- `--include-facets`: include normalized available facets and options
- `--input PATH`: JSON input object or `-` for standard input
- `--raw`: return bounded upstream JSON inside the standard envelope
- `--format toon|json`: global output format, default TOON

Search uses bounded transient retries for GET requests, the shared signed transport, bounded
responses, structured errors, and sanitized diagnostics. It does not read account credentials,
refresh tokens, or attach an authorization header.
