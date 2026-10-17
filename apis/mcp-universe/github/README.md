# GitHub MCP — authoring brief

Status: selected, not authored. Start with Repository, Issue and IssueComment;
defer broader coverage until composition and mutation accounting pass.

Use repository scope with issue number and comment identity. Define repository
→ issues and issue → comments relations from tools actually exposed by the
resolved MCP server. Capabilities: list/get issues, list comments, create/update
an issue and add a comment. Do not copy the full direct-HTTP catalogue surface.

The surveyed Universe registry selects GitHub server image v0.5.0; resolve its
digest and capture `tools/list` before authoring. Its JSON-text output needs an
explicit decoder. Page arguments without response pagination headers do not
prove exhaustive results. Preserve issue versus pull-request distinctions.

Source: [Universe registry](https://github.com/SalesforceAIResearch/MCP-Universe/blob/0df6c42717f2a618d34f5b271efd05be462b9e4d/mcpuniverse/mcp/configs/server_list.json),
[v0.5.0 issue implementation](https://github.com/github/github-mcp-server/blob/v0.5.0/pkg/github/issues.go).
Acceptance cases and evidence gates: [integration ladder](../INTEGRATION-TESTING.md).
