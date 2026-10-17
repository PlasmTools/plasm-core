# Yahoo Finance MCP — authoring brief

Status: selected, not authored. Start with Instrument, PriceBarObservation,
OptionExpiry and OptionContract observations. Add financial metrics after this
relationship chain is verified.

Compose instrument → expiries → calls/puts and instrument → historical bars.
Retain ticker, expiry, interval and observation scope where output omits them.
Do not treat separate live calls as a common atomic snapshot. Variable statement
keys require explicit metric/value normalization or a typed dynamic value.

Several tools return JSON record arrays as text; invalid requests may return
ordinary error strings. A versioned decoder must distinguish malformed content,
failure, successful empty results and missing values without coercing null to
zero. Do not infer success solely from the MCP exchange succeeding.

Source: [pinned Universe implementation](https://github.com/SalesforceAIResearch/MCP-Universe/blob/0df6c42717f2a618d34f5b271efd05be462b9e4d/mcpuniverse/mcp/servers/yahoo_finance/server.py).
Acceptance cases and evidence gates: [integration ladder](../INTEGRATION-TESTING.md).
