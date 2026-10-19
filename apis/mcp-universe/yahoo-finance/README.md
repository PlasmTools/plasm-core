# Yahoo Finance MCP — financial observations

Implemented: Instrument lookup, historical PriceBar queries, annual/quarterly
IncomeStatement observations, InstitutionalHolding positions, CorporateAction
history, OptionExpiry dates and OptionContract quotes. Bar identity
includes ticker, interval and timestamp. Wire identity paths are explicit:
`symbol` for instruments and `Date` for bars. Missing numerical values remain
nullable; no zero substitution is performed.

Representation is part of each semantic value domain. `fraction_held` carries
provider `pctHeld`, `fraction_change` carries `pctChange`, and
`implied_volatility` carries `impliedVolatility`, all as raw fractions.
`percent_change` carries option `percentChange` in percent units. Each served
domain description includes its provider field name and unit. No mapping
rescales these values, and the catalog gives no blanket conversion instruction.

An institutional holder is the provider's reported label, not an identifier for
an inferred corporate group. Its observation key includes ticker, label and
report time. This surface supplies no alias or parent-organization relation.
Different labels remain distinct; the absence of an exact label from a returned
report does not establish that an institution does not exist, or that a similar
label is equivalent. Broader matching is an authored selection decision.

Queries require explicit scope: historical date bounds, statement frequency,
or option expiration and right. The catalogue does not advertise implicit
relations that hide these choices. Income statements expose named typed metrics;
other statement families and holder types are outside this financial-task slice.

Live integration verifies two symbols with the same three dates retain six
distinct identities through Python admission, MCP dispatch and resource reads.
Historical queries declare exhaustive response coverage: the pinned wrapper
serializes the entire frame returned by `Ticker.history` without pagination,
sampling or a row limit. This proves exhaustion of that query response, not
market-data accuracy or the presence of every calendar date. The upstream
library may itself report unavailable data as an empty frame; the wrapper does
not provide evidence to distinguish that from genuine absence. Raised errors
and non-JSON error text remain failures. Offline contract tests in the parent
repository's `scripts/mcp_universe/test_history_contract.py` lock this scope.
Catalogue fields use the upstream automatically adjusted
prices. An observation from a later call is not an atomic snapshot of an earlier
one. Institutional report dates remain integer Unix epoch milliseconds, matching
the pinned server's pandas serialization. Option expiry scalar strings use the
existing scalar identity decoder. Financial metrics and quote fields are nullable.

Compose instrument → expiries → calls/puts and instrument → historical bars.
Retain ticker, expiry, interval and observation scope where output omits them.
Do not treat separate live calls as a common atomic snapshot. Unknown statement
metrics are not silently guessed or normalized into existing fields.

Several tools return JSON record arrays as text; invalid requests may return
ordinary error strings. A versioned decoder must distinguish malformed content,
failure, successful empty results and missing values without coercing null to
zero. Do not infer success solely from the MCP exchange succeeding.

Source: [pinned Universe implementation](https://github.com/SalesforceAIResearch/MCP-Universe/blob/0df6c42717f2a618d34f5b271efd05be462b9e4d/mcpuniverse/mcp/servers/yahoo_finance/server.py).
Acceptance cases and evidence gates: [integration ladder](../INTEGRATION-TESTING.md).
