# Google Maps MCP — authoring brief

Status: selected, not authored. Start with Place, GeographicPoint,
RouteObservation and MatrixElement observations.

Compose search → place details → distance matrix. Carry the requested place ID
when details omit it. A matrix element is addressed by both origin and
destination indices within its observation; preserve per-element status.
Route steps belong to a particular route observation, not permanent place edges.

The registry's npm declaration is unversioned. Resolve the installed package and
inspect its schema before mapping. The surveyed archived implementation drops
search continuation tokens and exposes only the first route leg: model these as
bounded results, not exhaustive collections or full multi-leg routes.

Source: [candidate upstream implementation](https://github.com/modelcontextprotocol/servers-archived/blob/9be4674d1ddf8c469e6461a27a337eeb65f76c2e/src/google-maps/index.ts).
This source is not proof of the installed package version.
Acceptance cases and evidence gates: [integration ladder](../INTEGRATION-TESTING.md).
