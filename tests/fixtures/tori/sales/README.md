# Tori sold-ad summary fixture

`page.json` is synthetic, not an account response. Its shape follows the Tori
Android `AdSummaryService`, `AdSummariesResult`, `AdSummaryQuery`,
`AdSummaryResult`, `AdSummaryState`, and `AdSummaryData` classes.

The service declares `GET search` on `AD-SUMMARIES` with `facet`, `offset`, and
`limit` (defaults 0 and 50). Read-only discovery confirmed `DISPOSED` is the
`Myydyt` facet and successive offsets return different sold ads. The fixture
includes unknown state and private extra fields to test tolerant, allowlisted
output. Nullable display fields are preserved rather than interpreted as prices.

Summary dates describe listings, not sale completion. This fixture makes no
claim about ToriDiili transactions, sorting direction, or history retention.
