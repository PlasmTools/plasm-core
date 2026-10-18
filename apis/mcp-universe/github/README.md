# GitHub MCP — benchmark binding

Status: authored development catalog, **not ready for the full repository task
set**. The tested direct-HTTP catalog at `apis/github` is unchanged. This catalog
reuses its repository-scoped domain identities, with independent MCP mappings.

## Source contract

MCP-Universe revision `0df6c42717f2a618d34f5b271efd05be462b9e4d` selects
`ghcr.io/github/github-mcp-server:0.5.0`. The inspected tag resolves to Git commit
`6d6979749de45c0c0397a666c8d3d7b7e51ce2af`. The mapping follows the
[repository handlers](https://github.com/github/github-mcp-server/blob/6d6979749de45c0c0397a666c8d3d7b7e51ce2af/pkg/github/repositories.go),
[issue handlers](https://github.com/github/github-mcp-server/blob/6d6979749de45c0c0397a666c8d3d7b7e51ce2af/pkg/github/issues.go),
[search handlers](https://github.com/github/github-mcp-server/blob/6d6979749de45c0c0397a666c8d3d7b7e51ce2af/pkg/github/search.go), and
[pull-request handlers](https://github.com/github/github-mcp-server/blob/6d6979749de45c0c0397a666c8d3d7b7e51ce2af/pkg/github/pullrequests.go).

The pinned container's installed `tools/list` has been inspected (51 tools).
Disposable live repository proofs have now exercised creation, directory
metadata, replacement and copying. See [coverage and blockers](COVERAGE.md).

## Surface

Twelve entities: Repository, Issue, IssueLabel, IssueComment, Branch, BranchHead,
ForkReceipt, PullRequest, FileRevision, FileContent, DirectoryEntry and CodeMatch.
Repository → issues and Repository → pull_requests select concrete variants of
the same repository-scoped enumeration. Both variants retain conversation
labels and comments with explicit parent scope.

| Capability | MCP tool | State |
|---|---|---|
| repo_search | search_repositories | Paginated, provider completeness and total checked |
| repo_create | create_repository | Protocol-tested |
| issue_query | list_issues | Protocol-tested, paginated |
| issue_get | get_issue | Protocol-tested, numeric identity |
| issue_create | create_issue | Protocol-tested |
| issue_update | update_issue | Protocol-tested, numeric identity |
| comment_query | get_issue_comments | Protocol-tested, paginated |
| comment_create | add_issue_comment | Protocol-tested |
| branch_create | create_branch | Protocol-tested |
| pr_create | create_pull_request | Protocol-tested |
| pr_get | get_pull_request | Authored; new protocol gate pending |
| file_write | create_or_update_file | Protocol-tested |
| file_read | get_file_contents | Protocol-tested, embedded text/blob resource |
| directory_entries | get_file_contents | Root/nested metadata tested; provider-bounded and explicitly partial |
| code_search | search_code | Protocol-tested typed path/identity, provider completeness contract |
| fork_initiate | fork_repository | Exact text receipt; live initiation and provenance verified |
| branch_heads | list_branches | Paginated branch observations; live fork readiness verified |

Issue and comment lists use declared page-number traversal with page size 100.
Pagination arguments belong to the transport mapping, not selection or scope.
Repository search also paginates. Its declared collection contract requires
`incomplete_results: false`, a stable unsigned `total_count`, and exactly that
many delivered rows. It stops on the declared total, including at an exact full
page boundary. Missing evidence, short delivery, changing totals and totals over
1000 fail explicitly. The 1000-result bound follows the
[GitHub search contract](https://github.com/github/docs/blob/main/content/rest/search/search.md);
the caller must narrow a larger search. No automatic query rewriting partitions
the search. State and labels are actual selection filters.

`list_issues` returns a closed `Issue | PullRequest` result from the repository
query capability. `.of_type(...)` selects the required nominal variant. The two
repository relations apply explicit variant selection over that same complete
enumeration. `Issue.get` asserts the absence of the provider `pull_request` key
before identity validation or caching; a pull request is rejected, not coerced.
PullRequest uses the existing compound owner/repo/number identity and does not
expose the issue endpoint's opaque ID as a pull-request ID. Listing summaries
leave head/base unobserved; `PullRequest.get` obtains those fields from the
dedicated provider endpoint. These changes require the new protocol gate before
the earlier protocol results can be claimed for this mapping.
Comment identity includes the parent issue. File writes send plain text: this
server base64-encodes the bytes itself. Optional inputs use explicit omission.

## Promotion blockers

The three original transport blockers are repaired: the shared identity codec
restores declared scalar types, MCP uses the existing bounded pagination driver,
and explicit `embedded_resource` selection retains URI, MIME type and text/blob.
File reads require an explicit branch; directory JSON is rejected rather than
misrepresented as file contents. Remaining gates:

1. Forking is exposed as initiation, preserving plain `Fork is in progress`
   instead of claiming completion. A separate branch observation establishes
   destination readiness; protocol and live fork-to-write tests pass.
2. Full task support still needs the remaining capability/workflow coverage in
   COVERAGE.md. Bridge, completion without a template and disposable cleanup
   have been exercised; that does not establish all-task readiness.
3. The pinned native file evaluator sends `ref`, while the pinned server accepts
   `branch`. This is recorded separately from catalog and model failures.
4. Live reads after a successful write returned old bytes from the server even
   after six additional reads. Protocol evidence confirms fresh Plasm dispatches.

These are integration gates, not reasons to rewrite `apis/github` or loosen the
benchmark checks. The premature three-task model run was aborted and preserved;
no scoring was restarted during this repair work.

## Offline protocol verification

From the parent monorepo, with compatible Rust host and worker binaries:

```sh
PLASM_UNIVERSE_HOST_BINARY="$PWD/target/debug/examples/mcp_catalog_host" \
PLASM_MONTY_BINARY="/absolute/path/to/monty" \
PYTHONPATH=target/mcp-universe-slice/upstream:. \
target/mcp-universe-agent/venv/bin/python -m unittest scripts.mcp_universe.test_github_protocol
```

The stateful Python peer speaks real Streamable HTTP MCP through the auditing
gateway and Rust host. It models pinned wire envelopes, not a live GitHub API.
The main test exercises ten tools, colliding issue numbers in different
repositories, a 103-row aggregate across two pages, later-page failure, text and
binary file resources, directory rejection, empty versus failed reads, exact
write counts, plaintext file bytes and retained comments after a later failed
read. Separate Get and Update tests enforce numeric wire arguments; neither is
marked as an expected failure.

The search test covers 103 and exactly 1000 results, zero results, a 1001-result
cap violation, incomplete/missing evidence, changed totals and premature end.
The abstract TCP tests exercise provider totals through page-number and cursor
traversal, including host continuation resume. Collection evidence is generic
core machinery; the GitHub field names and limit live only in this catalog.

Focused verification includes identity-codec and binding laws, MCP CML
validation, response/collection evidence units, independent TCP protocol tests
and four Python/GitHub protocol tests.
Search extension verification: 3 MCP CML tests, 3 response/collection unit tests,
11 TCP protocol tests and all 4 Python/GitHub tests passed (21 total).
Those original checks used no live GitHub requests or model generations. The local Python SDK
logs empty-SSE-frame and session-delete warnings; these do not fail the asserted
request/result contracts and are not evidence of live-server compatibility.

See the [integration ladder](../INTEGRATION-TESTING.md) for abstract and live-server
gates. Historical evaluations and the direct-HTTP catalog are intact.

## Latest workflow evidence

`scripts/mcp_universe/test_github_protocol.py` now includes directory metadata,
code-search identity, explicit refusal of completeness-dependent partial reads,
SHA replacement and read-after-write freshness. File writing is an action with
explicit read-observation invalidation, rather than creation-only.

Live proof: `target/mcp-universe-agent/plasm-universe-proof-d7cc94b169394e5d/REPORT.json`.
Root metadata, replacement, nested discovery and copy assertions passed;
immediate and later README consistency assertions were **false**, recorded as
measurements rather than hidden by retrying the write. Native branch existence
passed. Repository deletion was independently verified with HTTP 404. Zero model
calls. The report's additional-read counter is zero-based (5 means 6); the proof
script now records the count directly.

## Mixed issue enumeration

The provider's issue listing returns `Issue | PullRequest`. The Issue query owns
that operation; its routing owner does not determine the returned row type.
Narrow mixed rows with `.of_type(Issue)` or `.of_type(PullRequest)` before using
variant fields or methods (use the served session symbols). Repository `issues`
and `pull_requests` relations declare that selection explicitly. Issue detail
reads reject pull-request responses; PullRequest detail reads use the dedicated
provider operation. Both preserve compound owner/repo/number identity. Listing
summaries leave unavailable branch fields unobserved. New protocol verification
is pending; historical evidence above applies to the earlier mapping.
