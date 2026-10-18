# GitHub task capability readiness

Inventory: all 28 repository-management tasks at MCP-Universe
`0df6c42717f2a618d34f5b271efd05be462b9e4d`. This document is engineering
evidence, not model input. No solutions, guessed paths or evaluator outputs
are added to agent teaching.

| Tasks | Required workflow | Current gate |
|---|---|---|
| 0001–0005, 0021 | Discover source files; read bytes; create repository/branches; replace initialized files using SHA; copy files; open PR | Directory and code-search mappings added. Protocol metadata/update/reread passes. Native evaluator branch argument mismatch and provider read consistency remain blockers. |
| 0006–0010, 0023–0024 | Search repositories; enumerate/filter issues and labels; aggregate; write report | Basic paginated search/issues exist. Label observations and full composed report workflow need testing. |
| 0011–0012, 0014–0015, 0025–0026 | Compare repositories/content; initiate fork; establish readiness; modify fork | ForkReceipt, BranchHead and repository creation-time/issue-count observations implemented. Protocol and disposable live fork-to-write tests passed. Shared evaluator/consistency blockers remain. |
| 0016–0019, 0022, 0027–0030 | Create branches/files; author automation; create labeled issues; inspect resulting labels/comments; some also copy files/open PR | Typed IssueLabel relation, assignment/replacement and comments implemented. Protocol selection-to-update and live issue-to-comment tests passed. File authoring uses the existing tested write capability; no persistent workflow deployment is implied by these tests. Shared evaluator/consistency blockers remain. |

## Newly exercised boundaries

- Directory listing sends an explicit trailing slash; root is `/`.
- DirectoryEntry retains owner, repository, branch, path, kind and SHA.
- CodeMatch retains repository identity and path; search uses provider total and
  incomplete-result evidence. Search operates on indexed default branches.
- FileContent remains the embedded-resource text/blob representation; it does
  not invent SHA metadata absent from that response.
- File replacement is an action, not creation-only. It invalidates FileContent,
  DirectoryEntry and CodeMatch observations. A protocol test first reproduced
  stale rereads and then verified fresh bytes after this correction.
- Directory enumeration is bounded by the provider. It remains partial; a
  completeness-dependent composed selection fails explicitly. Observed paths
  and SHAs may be supplied to subsequent exact reads/updates. This limitation
  is not hidden by declaring the result exhaustive.

## Scoring gate

`scripts/mcp_universe/github_readiness.py` blocks the old scoring launcher
before credentials, repository creation or model calls. Each family requires
workflow evidence before promotion. Passing individual tool calls is insufficient.

The aborted three-task run is preserved in
`target/mcp-universe-agent/github-slice-luna-20261018`. Its selected tasks were
not ready. The direct-HTTP `apis/github` catalog is unchanged.

## Fork and automation completion evidence

- `ForkReceipt` preserves the selected MCP text block verbatim, whether JSON
  text or plain text. Generic `text_block` response selection does not parse or
  classify provider status. Source identity remains separate from destination
  readiness. BranchHead lists available destination branches with typed SHAs.
- Ownership admission records pending forks before dispatch. Duplicate initiation
  and preexisting destinations are rejected. Subsequent writes require a live
  destination ID, owner and parent repository ID matching the authorized source.
- Repository search exposes `created_at`, `open_issues_count`, `html_url` and
  `fork`. The provider count may include pull requests; it is not relabeled as
  an independently counted issue total.
- Issue → labels is a typed, complete embedded relation, not an untyped JSON
  field. Protocol tests cover filtered issue enumeration, title-based bounded
  updates, comments using observed issue identity, and successful empty labels.
- Pinned v0.5.0 `update_issue` ignores `labels=[]` (`issues.go:585`). Tests match
  this behavior and confirm existing labels remain. Nonempty replacement works.
  There is no label-clear tool in this pinned surface; the catalog does not claim
  that operation. This is distinct from reading an empty label set.
- Live artifact:
  `target/mcp-universe-agent/plasm-workflow-proof-976fff9d3c12/REPORT.json`.
  Zero model calls. Both created repositories were deleted with exact ID checks
  and independent absence verification; OWNERSHIP.json records both deletions.
  The saved receipt's empty source-repo presentation was corrected afterward by
  using the actual receipt text as its id field; protocol assertions now check
  both source owner and repository in both response forms.
- Rust response selection tests: 4 passed. Python protocol/ownership/readiness
  suite: 11 passed. The follow-up bounded automation-rule test is also exercised.

These are capability/integration proofs, not benchmark scores or evidence that
an installed background automation service will execute future events. Agents
can author scripts through file writes and execute composed issue workflows;
deployment permissions and triggers must be verified if a task requires them.
# Nominal result migration gate

Repository enumeration now returns Issue | PullRequest, with explicit concrete
selection in the issues and pull_requests relations. Exact Issue reads reject
pull-request responses before caching; dedicated PullRequest reads expose branch
details absent from listing summaries. Compound owner/repo/number identities
remain unchanged. The new mapping and exact-read assertion require protocol
validation; historical results below describe the earlier mapping.
