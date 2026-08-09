# GitLab HTTP REST API (v4)

Curated Plasm CGS slice for **GitLab REST v4**: projects, issues, and merge requests (read and write), plus **issue/MR notes**, **subscribe/unsubscribe**, **time stats**, and **award emoji**. The full product API is large; this tree grows by iterative passes over the upstream OpenAPI description.

## What is covered vs missing

**Implemented in this tree**

- **Projects:** `query`, `get`, `create`, `update`, `delete`
- **Issues:** global / per-project `query`, `get`, `create`, `update`, `delete`, `subscribe` / `unsubscribe`, `time_stats` (action), award-emoji list/get/create/delete
- **Merge requests:** global / per-project `query`, `get`, `create`, `update`, `delete`, `merge` (PUT), subscribe/unsubscribe, `time_stats`, award emoji
- **IssueNote / MergeRequestNote:** scoped list (`for-issue-query` / `for-mr-query`), `get`, `create`, `update`, `delete` (paths follow GitLab [Notes](https://docs.gitlab.com/ee/api/notes.html) on issues / merge requests)

**Still out of scope** (add in later phases)

- **Resource areas:** groups, namespaces, users, snippets, wiki pages, branches, tags, commits and repository files, pipelines, jobs and artifacts, environments, releases, container registry, packages, deploy keys, runners, members, invitations, milestones as first-class entities, labels (standalone API), epics, todos, search API, audit events, and the rest of the tagged API groups in the OpenAPI file.
- **Query / body completeness:** list and write capabilities expose a **subset** of OpenAPI parameters and JSON body fields—extend `parameters:` / CML `body` when you need more.
- **Auth:** only **`PRIVATE-TOKEN`** via `GITLAB_TOKEN` is declared. Other GitLab auth styles (OAuth2 flows, `private_token` query param, job tokens, etc.) are not wired in this CGS.
- **Eval harness:** there is no `apis/gitlab/eval/cases.yaml` (optional; add when you want `plasm-eval coverage` / NL cases).

If you need a capability, treat it as a **new authoring pass**: find the operation in `openapi.yaml`, then extend the domain and mappings following the plasm-authoring loop.

## OpenAPI source

The machine-readable spec used for authoring lives in this directory:

- `openapi.yaml` — downloaded from the GitLab repository (`doc/api/openapi/openapi_v2.yaml`).

To refresh:

```bash
curl -fsSL -o apis/gitlab/openapi.yaml \
  'https://gitlab.com/gitlab-org/gitlab/-/raw/master/doc/api/openapi/openapi_v2.yaml'
```

## Authentication

Personal access tokens and project access tokens use the **`PRIVATE-TOKEN`** header. Set:

```bash
export GITLAB_TOKEN='glpat-...'
```

Self-managed instances use the same header against your own origin.

## Backend URL

- **GitLab.com:** `https://gitlab.com`
- **Self-managed:** `https://gitlab.example.com` (no path suffix; `/api/v4` is encoded in `mappings.yaml`).

## Example

```bash
cargo run -p plasm-repl -- \
  --schema apis/gitlab \
  --backend https://gitlab.com \
  project query --search 'plasm' --limit 5

cargo run -p plasm-repl -- \
  --schema apis/gitlab \
  --backend https://gitlab.com \
  issue query --state opened --limit 10
```

Compound-key entities (**Issue**, **MergeRequest**): positional `id` is the **IID**; the project is `--project-id` (numeric id or URL-encoded path).

```bash
cargo run -p plasm-repl -- \
  --schema apis/gitlab \
  --backend https://gitlab.com \
  issue 42 --project-id 'namespace%2Fproject-name'
```

**Writes (examples)**

```bash
# Create a project (name required; optional path, visibility, …)
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  project create --name 'demo' --path 'demo' --visibility private

# Create an issue in a project
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  issue create --project-id 12345 --title 'Bug' --description 'Details…'

# Merge request: create, then merge (optional merge flags)
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  mergerequest merge-request-create --project-id 12345 --title 'Feature' \
  --source-branch my-feature --target-branch main
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  mergerequest --project-id 12345 7 merge-request-merge

# Issue note: list and add (scoped subcommands use kebab-case capability ids)
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  issuenote --project-id 12345 --iid 1 issue-note-for-issue-query --limit 20
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  issuenote --project-id 12345 --iid 1 issue-note-create --body 'LGTM'

# Subscribe / time stats / award emoji (compound key: --project-id then IID, then action)
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  issue --project-id 12345 1 subscribe
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  issue --project-id 12345 1 time-stats
cargo run -p plasm-repl -- --schema apis/gitlab --backend https://gitlab.com \
  issueawardemoji --project-id 12345 --iid 1 issue-award-emoji-create --name thumbsup
```

CLI entity names are lowercased (`mergerequest`, `issuenote`, `issueawardemoji`, …). Subcommands for non-primary capabilities are derived from the capability id (e.g. `issue-note-create`, not `create`).

## Further authoring

Follow the loop in [skills/plasm-authoring/SKILL.md](../../skills/plasm-authoring/SKILL.md): extend `domain.yaml` / `mappings.yaml`, run `plasm --schema apis/gitlab --help`, then exercise calls in live or replay mode.
