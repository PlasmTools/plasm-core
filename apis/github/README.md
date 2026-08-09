# GitHub REST API — Plasm CGS Schema

A [Plasm](../../README.md) domain model for the [GitHub REST API](https://docs.github.com/en/rest). The surface is **authored for agent workflows** (repos, issues, PRs, CI runs, collaborators, tags, etc.)—not a mechanical export of every OpenAPI operation. The loaded CGS (see the tables below, or `plasm schema validate` / `dump_prompt`) currently has **19** entities and **67** capabilities.

```bash
# Run against the live API (requires GITHUB_TOKEN in env)
export GITHUB_TOKEN=ghp_...
cargo run -p plasm-repl -- \
  --schema apis/github \
  --backend https://api.github.com \
  --repl
```

**Eval:** NL→Plasm cases live in [`eval/cases.yaml`](eval/cases.yaml). Check coverage (forms + entity domains vs CGS) with:

```bash
cargo run -p plasm-eval -- coverage --schema apis/github --cases apis/github/eval/cases.yaml
```

**External benchmarks:** For [MCPMark](https://mcpmark.ai/docs/introduction) GitHub tasks, harness coupling, verifier fit, leakage boundaries, and a pilot design, see [`docs/mcpmark-plasm-github-assessment.md`](../../docs/mcpmark-plasm-github-assessment.md). Executable pilot steps (patch, `github_plasm`, env) live in [`docs/mcpmark-pilot-runbook.md`](../../docs/mcpmark-pilot-runbook.md); apply [`scripts/mcpmark_github_plasm.patch`](../../scripts/mcpmark_github_plasm.patch) to your MCPMark checkout.

---


## What the CGS design is

A CGS (Capability Graph Schema) is a semantic domain model for an API. It is explicitly **not** a mirror of the OpenAPI spec. Where OpenAPI describes RPC endpoints ("here is a GET at this path that accepts these parameters"), a CGS describes business objects ("here is an entity called Issue, here is what it contains, here is how it relates to other entities, and here are the operations available on it").

This distinction matters for agent tooling. An OpenAPI-derived tool list gives an agent 2000+ functions for the GitHub API. This CGS keeps a **typed, enumerable** slice: multiple endpoints can still map to the same entity, and new capabilities are added when they clarify a recurring task—not when a path appears in the spec.

The two files:

**`domain.yaml`** — the semantic model. Declares entities, fields, relations, and capability signatures. No HTTP details. This is what the runtime type-checks queries against and what the CLI generator reads to produce typed flags.

**`mappings.yaml`** — the HTTP wiring. Declares how each capability compiles to an HTTP request using CML (Capability Mapping Language). Path segments, query params, pagination config, response envelope shape. The domain model doesn't know these exist.

The runtime composes them at load time and the two artifacts never need to be touched together.

### Compound keys (`key_vars`)

GitHub's issue and pull request identifiers are path-compound: `owner + repo + number`. A GitHub issue has a global numeric `id` that is not addressable via the API — the only way to fetch an issue is `GET /repos/{owner}/{repo}/issues/{number}`.

The CGS models this with `key_vars`:

```yaml
Issue:
  key_vars: [owner, repo, number]
Repository:
  key_vars: [owner, repo]   # `repo` is the slug; JSON wire field is still GitHub's `name`
Commit:
  key_vars: [owner, repo, sha]   # immutable commit identity; GET path segment accepts SHA or ref name
Branch:
  key_vars: [owner, repo, name]   # `name` is the branch name
Label:
  key_vars: [owner, repo, name]   # label *name* in the URL (numeric `id` kept as a field)
Milestone:
  key_vars: [owner, repo, number] # milestone number within the repo
Release:
  key_vars: [owner, repo, id]     # numeric release id in the API
```

This declares stable identity for path-addressed resources. The runtime binds every `key_vars` part into the CML environment by name. **REPL** expressions use the same names, e.g. `Issue(owner=octocat,repo=Hello-World,number=42)` and `Repository(owner=rust-lang,repo=rust)`.

**Global issue / PR search** (`issue_search`, `pr_search`) returns rows that expose `repository_url` rather than top-level `owner`/`repo`. The domain uses generic `segments_after_prefix` field derivation so those search hits still materialize full compound `Issue` / `PullRequest` identity for cache and hydration.

**CLI** (see `plasm` generated help): for HTTP GETs whose path has multiple `{var}` segments, earlier segments are required `--owner`, `--repo`, … flags (kebab-case) and the **last** segment is the positional `id` argument — except when a `key_vars` part is not present on the URL path, in which case it becomes its own required `--flag`.

This is a generalisation of the common `id_field` (a single-field key). For simple entities `id_field: x` is shorthand for `key_vars: [x]`. Existing schemas are unchanged.

### Scope parameters and `entity_ref` (FK modelling)

GitHub REST uses a **path pair** `owner` + repo slug for almost every repository-scoped URL. In this CGS:

- **`Repository`** uses compound key `owner` + `repo`. The JSON field for the slug is still GitHub’s `name` on the wire; `domain.yaml` maps it with `path: name` onto the `repo` slot so **`Repository` refs, CML path `repo`, and decoded rows agree**.
- **`owner` is not** typed as `entity_ref → User` because the namespace is shared with **organizations**: a repo under an org uses the **org login** as `owner`, not a user account.
- **`entity_ref` is used when the API names a single, typed foreign key** — e.g. `assignee` → `User`, `username` (user repos list) → `User`, `org` → `Organization`, `actor` (workflow run filter) → `User`, and `repository`-style scopes elsewhere when the parameter is explicitly one login or one org.
- **Booleans and flags** — e.g. **`anon`** on `contributor_query` — are **not** references; they map to GitHub’s boolean query parameters.

Scoped capabilities that take a `repository` `entity_ref` splat into the same `owner` / `repo` path variables as plain string scope.

### Issue comments and `via_param` (authoring guide)

The [Plasm authoring reference](../../skills/plasm-authoring/reference.md) allows **at most one** `via_param` on a relation (single scope parameter wired from the parent). GitHub’s per-issue comments endpoint needs **three** scope values (`owner`, `repo`, `issue_number`), so this schema does **not** declare an `Issue → IssueComment` relation for auto-traversal.

List comments with the scoped capability on `IssueComment` instead:

```bash
issuecomment issue-comment-query --owner octocat --repo Hello-World --issue_number 42
```

Use `issuecomment repo-comment-query` for all issue comments in a repository (repo scope only).

### The singleton kind

The authenticated user's profile has no stable URL segment—the subject is always “whoever the credentials represent.” The `kind: singleton` capability models that pattern:

```yaml
user_get_me:
  kind: singleton
  entity: User
  description: "Profile of the currently authenticated user"
```

CLI: `user get-me` (no positional ID required). Internally dispatched as a parameterless `QueryExpr` with `is_collection: false` decoding.

---

## What is implemented

### Entities

| Entity | Key | Fields | Relations |
|--------|-----|--------|-----------|
| `Repository` | `id` (numeric) + compound `owner`+`repo` (slug; wire `name`) | owner, repo, full_name, description, private, fork, language, stargazers_count, forks_count, open_issues_count, default_branch, archived, visibility, html_url, created/updated/pushed_at | → `User` (`repo_owner`), → workflows / workflow runs |
| `Issue` | compound `owner`+`repo`+`number` | owner, repo, number, id, repository_url, title, body, state, state_reason, issue type fields, sub-issue summary, locked, comments, html_url, created/updated/closed_at | → `User` (user, assignee), → `Milestone`, → `IssueType`, → labels / sub-issues |
| `PullRequest` | compound `owner`+`repo`+`number` | owner, repo, number, id, repository_url, title, body, state, locked, draft, head/base refs, mergeability, html_url, created/updated/closed/merged_at | → `User` (user, assignee), → `Milestone`, → changed files / reviews / review comments |
| `Commit` | compound `owner`+`repo`+`sha` | owner, repo, sha, message, html_url | — |
| `Branch` | compound `owner`+`repo`+`name` (branch) | owner, repo, name, commit_sha, protected, html_url | — |
| `PullRequestReview` | compound `owner`+`repo`+`pull_number`+`id` (review id) | id, owner, repo, pull_number, state, body, commit_id, submitted_at, html_url | → `PullRequest`, → `User` |
| `PullRequestFile` | `filename` (string) | filename, sha, status, additions, deletions, patch | — |
| `User` | `login` (string) | login, id, name, company, blog, location, email, bio, public_repos, public_gists, hireable, site_admin, html_url, avatar_url, created_at | — |
| `Label` | compound `owner`+`repo`+`name` (`name` in URL; `id` on row) | owner, repo, name, id, description, color, default | — |
| `Milestone` | compound `owner`+`repo`+`number` | owner, repo, number, id, title, description, state, open_issues, closed_issues, due_on, html_url, created/updated/closed_at | — |
| `Release` | compound `owner`+`repo`+`id` | owner, repo, id, tag_name, name, body, draft, prerelease, html_url, created_at, published_at | → `User` (author) |
| `IssueComment` | `id` (integer) | body, html_url, created_at, updated_at | → `User` |
| `Organization` | `login` (string) | id, name, description, blog, location, email, avatar_url, url, public_repos, public_gists, followers, following, type | — |
| `IssueType` | compound `org`+`id` | org, id, node_id, name, description, color, is_enabled, created/updated_at | — |
| `Gist` | `id` (string) | description, html_url, public, comments, created_at, updated_at | → `User` (owner) |
| `Notification` | `id` (string, thread id) | unread, reason, updated_at, last_read_at, url, subscription_url | — |
| `PullRequestReviewComment` | `id` (integer) | body, path, diff_hunk, commit_id, pull_number, review_id, line/side coordinates, subject_type, html_url, created/updated_at | → `User` |
| `RepositoryTag` | `name` (string, per repo) | owner, repo, name, commit_sha, zipball/tarball URLs | — |
| `Contributor` | `login` (string, per repo list row) | owner, repo, login, contributions | — |
| `Workflow` | compound `owner`+`repo`+`id` | owner, repo, id, name, path, state, html_url, badge_url, created/updated_at | → workflow runs |
| `WorkflowRun` | compound `owner`+`repo`+`id` | owner, repo, id, name, display_title, typed status/conclusion, event, workflow_id, run number/attempt, head branch/SHA, actors, html_url, run_started/created/updated_at | → workflow, jobs, artifacts, usage, actors |
| `WorkflowJob` | compound `owner`+`repo`+`run_id`+`id` | owner, repo, run_id, id, run_attempt, name, typed status/conclusion, runner labels, started/completed_at, html_url | — |
| `WorkflowArtifact` | compound `owner`+`repo`+`id` | owner, repo, id, run_id, name, size_in_bytes, expired, archive_download_url, created/updated/expires_at | — |
| `WorkflowRunUsage` | compound `owner`+`repo`+`run_id` | owner, repo, run_id, run_duration_ms, Ubuntu/macOS/Windows billed job counts and milliseconds | — |

### Capabilities

| Capability | Kind | CLI | Endpoint |
|------------|------|-----|----------|
| `repo_search` | search | `repository search --q "..."` | `GET /search/repositories` |
| `auth_user_repos_query` | query (primary) | `repository query` / `repository auth-user-repos-query` | `GET /user/repos` |
| `user_repos_query` | query (scoped) | `repository user-repos-query --username octocat` | `GET /users/{username}/repos` |
| `repo_get` | get | `repository --owner O <name>` (repo slug is the positional `id`) | `GET /repos/{owner}/{repo}` |
| `repo_collaborators_query` | query (scoped) | `user repo-collaborators-query --owner O --repo R` | `GET /repos/{owner}/{repo}/collaborators` |
| `repo_forks_query` | query (scoped) | `repository repo-forks-query --owner O --repo R` | `GET /repos/{owner}/{repo}/forks` |
| `issue_get` | get | `issue --owner O --repo R N` | `GET /repos/{owner}/{repo}/issues/{number}` |
| `issue_query` | query (scoped) | `issue query --owner octocat --repo Hello-World` | `GET /repos/{owner}/{repo}/issues` |
| `issue_search` | search | `issue search --q "is:issue is:open"` | `GET /search/issues` |
| `issue_sub_issue_query` | query (scoped) | `issue issue-sub-issue-query --owner O --repo R --issue_number N` | `GET /repos/{owner}/{repo}/issues/{issue_number}/sub_issues` |
| `pr_get` | get | `pullrequest --owner O --repo R N` | `GET /repos/{owner}/{repo}/pulls/{number}` |
| `pr_query` | query (scoped) | `pullrequest query --owner octocat --repo Hello-World` | `GET /repos/{owner}/{repo}/pulls` |
| `pr_search` | search | `pullrequest search --q "is:pr is:merged"` | `GET /search/issues` |
| `commit_query` | query (scoped) | `commit query --owner O --repo R` | `GET /repos/{owner}/{repo}/commits` |
| `commit_get` | get | `commit --owner O --repo R <ref>` (SHA, branch, or tag) | `GET /repos/{owner}/{repo}/commits/{ref}` |
| `branch_query` | query (scoped) | `branch query --owner O --repo R` | `GET /repos/{owner}/{repo}/branches` |
| `branch_get` | get | `branch --owner O --repo R <branch>` | `GET /repos/{owner}/{repo}/branches/{branch}` |
| `pr_review_query` | query (scoped) | `pullrequestreview pr-review-query --owner O --repo R --pull_number N` | `GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews` |
| `pr_file_query` | query (scoped) | `pullrequestfile pr-file-query --owner O --repo R --pull_number N` | `GET /repos/{owner}/{repo}/pulls/{pull_number}/files` |
| `pr_review_comment_query` | query (scoped) | `pullrequestreviewcomment pr-review-comment-query --owner O --repo R --pull_number N` | `GET /repos/{owner}/{repo}/pulls/{pull_number}/comments` |
| `repo_tags_query` | query (scoped) | `repositorytag repo-tags-query --owner O --repo R` | `GET /repos/{owner}/{repo}/tags` |
| `contributor_query` | query (scoped) | `contributor contributor-query --owner O --repo R` | `GET /repos/{owner}/{repo}/contributors` |
| `workflow_run_query` | query (scoped) | `workflowrun workflow-run-query --owner O --repo R` | `GET /repos/{owner}/{repo}/actions/runs` |
| `workflow_run_get` | get | `workflowrun --owner O --repo R <id>` | `GET /repos/{owner}/{repo}/actions/runs/{run_id}` |
| `workflow_query` | query (scoped) | `workflow query --owner O --repo R` | `GET /repos/{owner}/{repo}/actions/workflows` |
| `workflow_get` | get | `workflow --owner O --repo R <id>` | `GET /repos/{owner}/{repo}/actions/workflows/{id}` |
| `workflow_dispatch` | action | `workflow --owner O --repo R <id> dispatch --ref main` | `POST /repos/{owner}/{repo}/actions/workflows/{id}/dispatches` |
| `workflow_run_by_workflow_query` | query (scoped) | `workflowrun workflow-run-by-workflow-query --owner O --repo R --workflow_id ID` | `GET /repos/{owner}/{repo}/actions/workflows/{workflow_id}/runs` |
| `workflow_run_rerun` | action | `workflowrun --owner O --repo R <id> workflow-run-rerun` | `POST /repos/{owner}/{repo}/actions/runs/{id}/rerun` |
| `workflow_run_rerun_failed_jobs` | action | `workflowrun --owner O --repo R <id> workflow-run-rerun-failed-jobs` | `POST /repos/{owner}/{repo}/actions/runs/{id}/rerun-failed-jobs` |
| `workflow_run_cancel` | action | `workflowrun --owner O --repo R <id> workflow-run-cancel` | `POST /repos/{owner}/{repo}/actions/runs/{id}/cancel` |
| `workflow_run_logs_delete` | action | `workflowrun --owner O --repo R <id> workflow-run-logs-delete` | `DELETE /repos/{owner}/{repo}/actions/runs/{id}/logs` |
| `workflow_job_query` | query (scoped) | `workflowjob workflow-job-query --owner O --repo R --run_id ID` | `GET /repos/{owner}/{repo}/actions/runs/{run_id}/jobs` |
| `workflow_artifact_query` | query (scoped) | `workflowartifact workflow-artifact-query --owner O --repo R --run_id ID` | `GET /repos/{owner}/{repo}/actions/runs/{run_id}/artifacts` |
| `workflow_run_usage_get` | get | `workflowrunusage --owner O --repo R <run_id>` | `GET /repos/{owner}/{repo}/actions/runs/{run_id}/timing` |
| `user_get` | get | `user octocat` | `GET /users/{login}` |
| `user_get_me` | singleton | `user get-me` | `GET /user` |
| `user_search` | search | `user search --q "location:london"` | `GET /search/users` |
| `label_query` | query (scoped) | `label label-query --owner octocat --repo Hello-World` | `GET /repos/{owner}/{repo}/labels` |
| `label_get` | get | `label --owner O --repo R <name>` | `GET /repos/{owner}/{repo}/labels/{name}` |
| `milestone_query` | query (scoped) | `milestone milestone-query --owner octocat --repo Hello-World` | `GET /repos/{owner}/{repo}/milestones` |
| `milestone_get` | get | `milestone --owner O --repo R <number>` | `GET /repos/{owner}/{repo}/milestones/{milestone_number}` |
| `release_query` | query (scoped) | `release release-query --owner octocat --repo Hello-World` | `GET /repos/{owner}/{repo}/releases` |
| `release_get` | get | `release --owner O --repo R <id>` | `GET /repos/{owner}/{repo}/releases/{release_id}` |
| `issue_comment_query` | query (scoped) | `issuecomment issue-comment-query --owner octocat --repo Hello-World --issue_number 42` | `GET /repos/{owner}/{repo}/issues/{number}/comments` |
| `repo_comment_query` | query (scoped) | `issuecomment repo-comment-query --owner octocat --repo Hello-World` | `GET /repos/{owner}/{repo}/issues/comments` |
| `org_get` | get | `organization rust-lang` | `GET /orgs/{org}` |
| `issue_type_query` | query (scoped) | `issuetype issue-type-query --org rust-lang` | `GET /orgs/{org}/issue-types` |
| `org_repos_query` | query (scoped) | `repository org-repos-query --org rust-lang` | `GET /orgs/{org}/repos` |
| `org_members_query` | query (scoped) | `user org-members-query --org rust-lang` | `GET /orgs/{org}/members` |
| `gist_query` | query | `gist query` | `GET /gists` |
| `gist_get` | get | `gist <gist_id>` | `GET /gists/{gist_id}` |
| `notification_query` | query | `notification query` | `GET /notifications` |
| `notification_get` | get | `notification <thread_id>` | `GET /notifications/threads/{thread_id}` |
| `notification_mark_read` | action | `notification <thread_id> mark-read` | `PATCH /notifications/threads/{thread_id}` |
| `issue_update` | update | `issue --owner O --repo R N update …` | `PATCH /repos/{owner}/{repo}/issues/{number}` |
| `issue_assignees_add` | action | `issue --owner O --repo R N issue-assignees-add …` | `POST /repos/{owner}/{repo}/issues/{number}/assignees` |
| `issue_assignees_remove` | action | `issue --owner O --repo R N issue-assignees-remove …` | `DELETE /repos/{owner}/{repo}/issues/{number}/assignees` |
| `issue_assign_copilot` | action | `issue --owner O --repo R N issue-assign-copilot …` | `POST /repos/{owner}/{repo}/issues/{number}/assignees` |
| `issue_sub_issue_add` | action | `issue --owner O --repo R N issue-sub-issue-add …` | `POST /repos/{owner}/{repo}/issues/{number}/sub_issues` |
| `issue_sub_issue_remove` | action | `issue --owner O --repo R N issue-sub-issue-remove …` | `DELETE /repos/{owner}/{repo}/issues/{number}/sub_issue` |
| `issue_sub_issue_reprioritize` | action | `issue --owner O --repo R N issue-sub-issue-reprioritize …` | `PATCH /repos/{owner}/{repo}/issues/{number}/sub_issues/priority` |
| `issue_comment_create` | create | `issuecomment issue-comment-create …` | `POST /repos/{owner}/{repo}/issues/{issue_number}/comments` |
| `issue_comment_update` | update | `issuecomment <id> issue-comment-update --owner O --repo R …` | `PATCH /repos/{owner}/{repo}/issues/comments/{id}` |
| `pr_update` | update | `pullrequest --owner O --repo R N pr-update …` | `PATCH /repos/{owner}/{repo}/pulls/{number}` |
| `pr_merge` | action | `pullrequest --owner O --repo R N pr-merge …` | `PUT /repos/{owner}/{repo}/pulls/{number}/merge` |
| `pr_update_branch` | action | `pullrequest --owner O --repo R N pr-update-branch …` | `PUT /repos/{owner}/{repo}/pulls/{number}/update-branch` |
| `pr_review_create` | create | `pullrequestreview pr-review-create --owner O --repo R --pull_number N …` | `POST /repos/{owner}/{repo}/pulls/{pull_number}/reviews` |
| `pr_review_submit` | action | `pullrequestreview --owner O --repo R --pull_number N <review_id> pr-review-submit …` | `POST /repos/{owner}/{repo}/pulls/{pull_number}/reviews/{id}/events` |
| `pr_review_delete` | delete | `pullrequestreview --owner O --repo R --pull_number N <review_id> pr-review-delete` | `DELETE /repos/{owner}/{repo}/pulls/{pull_number}/reviews/{id}` |
| `pr_review_comment_create` | create | `pullrequestreviewcomment pr-review-comment-create --owner O --repo R --pull_number N …` | `POST /repos/{owner}/{repo}/pulls/{pull_number}/comments` |
| `label_create` | create | `label --owner O --repo R create …` | `POST /repos/{owner}/{repo}/labels` |
| `commit_file_get` | get | `commitfile --owner O --repo R <path>` (optional `--ref`) | `GET /repos/{owner}/{repo}/contents/{path}` — read/existence (404 if missing); use before update |
| `repo_content_create` | action | `repository --owner O --repo R repo-content-create …` | `PUT /repos/{owner}/{repo}/contents/{path}` (new file; no sha) |
| `repo_content_update` | action | `repository --owner O --repo R repo-content-update …` | `PUT /repos/{owner}/{repo}/contents/{path}` (existing file; sha required) |
| `pr_review_get` | get | `pullrequestreview --owner O --repo R --pull_number N <review_id>` | `GET /repos/{owner}/{repo}/pulls/{pull_number}/reviews/{id}` |
| `pr_review_comment_get` | get | `pullrequestreviewcomment --owner O --repo R <id>` | `GET /repos/{owner}/{repo}/pulls/comments/{id}` |

### CLI examples

```bash
# Search repositories
plasm --schema apis/github --backend https://api.github.com \
  repository search --q "language:rust stars:>1000"

# List open issues in a repo
plasm --schema apis/github --backend https://api.github.com \
  issue query --owner rust-lang --repo rust --state open --limit 20

# Get a specific user
plasm --schema apis/github --backend https://api.github.com \
  user torvalds

# Get authenticated user (singleton)
plasm --schema apis/github --backend https://api.github.com \
  user get-me

# List PRs filtered to open drafts
plasm --schema apis/github --backend https://api.github.com \
  pullrequest query --owner owner --repo repo --state open

# Get one repository (compound key: owner flag + name slug positional)
plasm --schema apis/github --backend https://api.github.com \
  repository --owner rust-lang rust

# Recent commits and a single commit by ref (SHA or branch name)
plasm --schema apis/github --backend https://api.github.com \
  commit query --owner rust-lang --repo rust --limit 5
plasm --schema apis/github --backend https://api.github.com \
  commit --owner rust-lang --repo rust main

# Branches, PR reviews, PR files
plasm --schema apis/github --backend https://api.github.com \
  branch query --owner rust-lang --repo rust --limit 20
plasm --schema apis/github --backend https://api.github.com \
  branch --owner rust-lang --repo rust master
plasm --schema apis/github --backend https://api.github.com \
  pullrequestreview pr-review-query --owner rust-lang --repo rust --pull_number 1
plasm --schema apis/github --backend https://api.github.com \
  pullrequestfile pr-file-query --owner rust-lang --repo rust --pull_number 1 --limit 50

# Search issues globally
plasm --schema apis/github --backend https://api.github.com \
  issue search --q "is:issue is:open label:good-first-issue language:rust" --sort reactions

# Comments on an issue (scoped query — three scope params)
plasm --schema apis/github --backend https://api.github.com \
  issuecomment issue-comment-query --owner octocat --repo Hello-World --issue_number 42

# Organization profile and repos (token not required for public orgs)
plasm --schema apis/github --backend https://api.github.com \
  organization rust-lang
plasm --schema apis/github --backend https://api.github.com \
  repository org-repos-query --org rust-lang --limit 10

# Gists and notifications (require GITHUB_TOKEN)
plasm --schema apis/github --backend https://api.github.com \
  gist query --limit 5
plasm --schema apis/github --backend https://api.github.com \
  notification query --limit 20
```

---

## Testing status

### CLI validation

Schema loads and CLI generates correctly. Validated with:

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/github
cargo run -p plasm-repl -- --schema apis/github --help
```

All entities, subcommands, typed flags, pagination controls (`--limit`, `--all`, `--page`), and compound-key help text verified. For multi-segment URL paths, earlier segments use `--kebab` flags and the **last** segment is the positional `id`; the generated `Ref` uses structured `key_vars` (same shape as the REPL’s `Entity(k=v,…)` form).

### Against mock server

Not yet tested. The GitHub OpenAPI spec is at [github/rest-api-description](https://github.com/github/rest-api-description). To run against a hermit mock:

```bash
# Download the spec
curl -o /tmp/github.json \
  https://raw.githubusercontent.com/github/rest-api-description/main/descriptions/api.github.com/api.github.com.json

# Start hermit mock (in a separate terminal)
hermit --specs /tmp/github.json --port 9090 --use-examples

# Run against mock
plasm --schema apis/github --backend http://localhost:9090 \
  user torvalds
```

Note: hermit mock testing for GitHub is likely to surface decode issues because GitHub's spec example values may not match the field shapes declared in this domain model.

### Against the real GitHub API

Not yet tested end-to-end. To test:

```bash
export GITHUB_TOKEN=ghp_your_token_here

# Test user get
plasm --schema apis/github --backend https://api.github.com \
  --mode live user torvalds

# Test repo search
plasm --schema apis/github --backend https://api.github.com \
  --mode live repository search --q "plasm stars:>10"

# Test issue query (rate limited — use --limit to cap requests)
plasm --schema apis/github --backend https://api.github.com \
  --mode live issue query --owner rust-lang --repo rust --state open --limit 10
```

Required: `GITHUB_TOKEN` environment variable containing a GitHub Personal Access Token with `repo` scope for private repos, or a fine-grained token with Issues and Contents read access.

The Plasm HTTP client sends a **`User-Agent`** header on every request (GitHub rejects requests without one, which previously surfaced as opaque JSON parse errors on HTML error pages).

---

## What remains to be implemented

### High priority — read operations

*(Previously listed here: `repo_get`, commit list/get, PR reviews/files, branches — **implemented** in this schema revision.)*

Commit `message` is read from nested `commit.message`; branch and tag `commit_sha` from `commit.sha`. Other nested GitHub JSON (e.g. `commit.author`) may still need explicit `path:` if surfaced as first-class fields.

### Medium priority — write operations

**Canonical gap analysis:** see [`WRITE_GAPS.md`](WRITE_GAPS.md) for every mapped non-GET capability (path, completeness vs GitHub’s JSON body), representative missing REST writes by resource family, P0/P1/P2 priorities, and OAuth scope hints.

**Summary:** write bindings include issue/PR/comment/label/milestone/release CRUD, contents put/delete, and notification thread read-state—see [`WRITE_GAPS.md`](WRITE_GAPS.md) §1 for the full list. Remaining high-value gaps include **reactions**, **PR review requests / inline review comments**, **Actions dispatch/rerun**, **gist** writes, and **repo/git admin** endpoints (not OpenAPI parity).

### Lower priority — broader surface

**GitHub Actions** — workflow runs, jobs, artifacts. Large surface area with complex nested entities.

**GitHub Projects (v2)** — uses GraphQL primarily; REST surface is limited.

**Rate limit info** (`GET /rate_limit`) — useful as a `kind: singleton` with no entity affinity.

---

## Known limitations

**MCP `plasm` tool vs full issue/comment bodies** — In the CGS, `Issue.body`, `PullRequest.body`, `IssueComment.body`, etc. use `string_semantics: markdown`. For those fields, Plasm’s default **agent presentation** is **reference-only**: table/compact Markdown in the `plasm` tool shows **`(in artifact)`** instead of the full string (to keep tool responses small). That is **not** data loss: the run snapshot JSON still contains the **full** values (`CachedEntity::payload_to_json` in the artifact). Agents **must** follow **`resource_link`** / `_meta.plasm.steps` and call MCP **`resources/read`** on each `plasm://execute/…/run/…` URI to audit or quote long markdown. The `plasm` tool description and server `initialize.instructions` state this explicitly. **HTTP** `POST /execute` with `Accept: application/json` returns **full** field values in the JSON `results` array (no `(in artifact)` substitution there)—only the Markdown path used by MCP applies CGS summarization.

**`owner`/`repo` not in response bodies** — GitHub's issue and PR responses do not always include `owner` or `repo` as top-level fields. For compound-key GETs, path-scope values from `Ref.key_vars` are merged into the CML environment (and the runtime may pre-inject them into the decoded payload) so compound identity still materialises. The same pattern applies to `repo_get`, `commit_get`, and `branch_get`.

**`labels` query param is a CSV string** — The `labels` filter on `issue_query` takes a comma-separated string (`"bug,enhancement"`). The domain model declares it as `type: string`. A `type: array` with a CML `join` expression would be more idiomatic but is functionally equivalent.

**`pr_search` shares the `/search/issues` endpoint with `issue_search`** — GitHub's search API returns both issues and PRs from a single endpoint, discriminated by a `pull_request` key in the response. Both capabilities point at the same URL; the `is:pr` / `is:issue` qualifier in the `q` parameter is the only differentiator. This is an API design artifact, not a CGS limitation.

**`issue_query` lists issues and pull requests** — The repo-scoped `GET /repos/{owner}/{repo}/issues` endpoint returns **both** issues and PRs. For issue-only lists, prefer `issue_search` with `is:issue` in `q`, or filter client-side using row shape / `pull_request` presence.

**No pagination for `user_get_me` / `user_get`** — These are single-entity endpoints, correctly modelled without pagination. `auth_user_repos_query` (`GET /user/repos`) and `user_repos_query` (`GET /users/{username}/repos`) paginate.

**Authenticated “my repos”** — Use `auth_user_repos_query` (`GET /user/repos`) for repositories the token can access. It supports `visibility`, `affiliation` (comma-separated `owner`, `collaborator`, `organization_member`), and sort options. Prefer this over `user_repos_query` when the goal is “repos for the current user” without synthesizing a username: `user_repos_query` targets `GET /users/{username}/repos` and requires a concrete **username** scope.

**Profile `public_repos` vs `/user/repos`** — `user_get_me` includes `public_repos` (counts **owned** public repos on the profile). `auth_user_repos_query` lists repos the token may access (including collaborator and org repos). Those numbers need not match; treat repo listing as ground truth for “what this binding can see.”

**Notifications and org OAuth restrictions** — Thread endpoints (e.g. `notification_get`) can return **403** if the token is allowed for the user but the **`plasmhq` organization** (or another org) has **OAuth App access restrictions**. That is a GitHub org policy issue, not a Plasm decode bug—approve the OAuth app for the org or use a token not limited by those rules.

**Repository list rows** decode to compound `Repository` refs (`owner` + `repo`); `owner` comes from `owner.login` and `repo` from the wire `name` field via `path:` in `domain.yaml`, so query→`repo_get` hydration resolves `/repos/{owner}/{repo}`.

**No `Issue → IssueComment` relation** — Multi-parameter sub-resource scope is not expressible as a single `via_param` today; use `issue_comment_query` / `repo_comment_query` explicitly (see [`reference.md`](../../skills/plasm-authoring/reference.md), section *via_param — sub-resource traversal*).

**Organization / gist / notification modelling vs OpenAPI** — Cross-checked against [github/rest-api-description](https://github.com/github/rest-api-description) (`descriptions/api.github.com/api.github.com.json`). `Notification` omits nested `repository` and `subject` objects from the thread schema (scalar fields only). `Gist` declares an `owner` relation; list/get responses include `owner` as a nested user object. `gist_query` maps to `GET /gists` (authenticated user’s gists); `GET /gists/public` and starred variants are not mapped yet.

**`notification_mark_read`** — GitHub returns `205 Reset Content` with an empty body; the capability is `kind: action` with side-effect output. If a decoder expects JSON, live testing may require runtime tolerance for empty 2xx bodies (same class as other action mappings).
