# GitHub REST vs Plasm CGS — write surface gap analysis

This document compares **non-GET** GitHub REST operations to what `[domain.yaml](domain.yaml)` declares and `[mappings.yaml](mappings.yaml)` binds. The CGS is **not** a full OpenAPI export of github.com—new capabilities are added for recurring agent workflows. For read gaps and MCP presentation notes, see `[README.md](README.md)`.

**References**

- [GitHub REST API](https://docs.github.com/en/rest)
- [github/rest-api-description](https://github.com/github/rest-api-description) OpenAPI — filter `paths` by HTTP verb

---

## 1. Plasm-mapped writes (inventory)

Each row has a matching key in `mappings.yaml` with the same method and path template as GitHub’s REST routes (unless noted).


| Capability               | `kind` (domain) | HTTP     | GitHub route (template)                                | Entity       |
| ------------------------ | --------------- | -------- | ------------------------------------------------------ | ------------ |
| `notification_mark_read` | action          | `PATCH`  | `/notifications/threads/{thread_id}`                   | Notification |
| `issue_create`           | create          | `POST`   | `/repos/{owner}/{repo}/issues`                         | Issue        |
| `issue_update`           | update          | `PATCH`  | `/repos/{owner}/{repo}/issues/{issue_number}`          | Issue        |
| `issue_delete`           | delete          | `DELETE` | `/repos/{owner}/{repo}/issues/{issue_number}`          | Issue        |
| `issue_comment_create`   | create          | `POST`   | `/repos/{owner}/{repo}/issues/{issue_number}/comments` | IssueComment |
| `issue_comment_update`   | update          | `PATCH`  | `/repos/{owner}/{repo}/issues/comments/{comment_id}`   | IssueComment |
| `issue_comment_delete`   | delete          | `DELETE` | `/repos/{owner}/{repo}/issues/comments/{comment_id}`   | IssueComment |
| `pr_create`              | create          | `POST`   | `/repos/{owner}/{repo}/pulls`                          | PullRequest  |
| `pr_update`               | update          | `PATCH`  | `/repos/{owner}/{repo}/pulls/{pull_number}`            | PullRequest  |
| `pr_merge`               | action          | `PUT`    | `/repos/{owner}/{repo}/pulls/{pull_number}/merge`      | PullRequest  |
| `label_create`           | create          | `POST`   | `/repos/{owner}/{repo}/labels`                         | Label        |
| `label_update`           | update          | `PATCH`  | `/repos/{owner}/{repo}/labels/{name}`                  | Label        |
| `label_delete`           | delete          | `DELETE` | `/repos/{owner}/{repo}/labels/{name}`                  | Label        |
| `milestone_create`       | create          | `POST`   | `/repos/{owner}/{repo}/milestones`                     | Milestone    |
| `milestone_update`       | update          | `PATCH`  | `/repos/{owner}/{repo}/milestones/{milestone_number}`  | Milestone    |
| `milestone_delete`       | delete          | `DELETE` | `/repos/{owner}/{repo}/milestones/{milestone_number}`  | Milestone    |
| `release_create`         | create          | `POST`   | `/repos/{owner}/{repo}/releases`                       | Release      |
| `release_update`         | update          | `PATCH`  | `/repos/{owner}/{repo}/releases/{release_id}`          | Release      |
| `release_delete`         | delete          | `DELETE` | `/repos/{owner}/{repo}/releases/{release_id}`          | Release      |
| `repo_content_put`       | action          | `PUT`    | `/repos/{owner}/{repo}/contents/{path}`                | Repository   |
| `repo_branch_create`     | action          | `POST`   | `/repos/{owner}/{repo}/git/refs`                       | Repository   |
| `repo_content_delete`    | action          | `DELETE` | `/repos/{owner}/{repo}/contents/{path}`                | Repository   |


Run `plasm schema validate apis/github` for the current capability count (`requirements.oauth.capabilities` must include every capability key).

---

## 2. Mapped writes — complete vs partial (body / parameters)


| Capability                                       | Coverage                   | Gaps vs GitHub REST                                                                                                                 |
| ------------------------------------------------ | -------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| `issue_create`                                   | **Partial**                | Core fields mapped. GitHub accepts additional metadata (e.g. assignees as logins) — align with `assignees` encoding at runtime.     |
| `issue_update`                                   | **Partial**                | `title`, `body`, `state`, `state_reason`, `labels`, `assignees`, `milestone`, `locked` in domain + mapping.                         |
| `issue_delete`                                   | **Complete** (minimal)     | `DELETE` with no JSON body.                                                                                                         |
| `issue_comment_delete`                           | **Complete** (minimal)     | `DELETE` timeline comment.                                                                                                          |
| `pr_create`                                      | **Partial**                | `title`, `head`, `base`, `body`, `draft`, `maintainer_can_modify`, `issue`. Further GitHub fields may exist.                        |
| `pr_update`                                       | **Partial**                | `title`, `body`, `state`, `base`. Optional: `draft`, `maintainer_can_modify`, etc.                                                  |
| `pr_merge`                                       | **Partial**                | `commit_title`, `commit_message`, `merge_method`.                                                                                   |
| `label_create` / `label_update` / `label_delete` | **Complete** (minimal)     | Standard label CRUD.                                                                                                                |
| `milestone_`*                                    | **Partial**                | Create/update send `title`, `description`, `state`, `due_on`.                                                                       |
| `release_`*                                      | **Partial**                | Create includes `generate_release_notes`; omit unsupported preview-only fields unless needed.                                       |
| `repo_content_put` / `repo_content_delete`       | **Partial**                | No `author`/`committer` overrides on Contents API. `repo_content_delete` uses JSON body on `DELETE` (supported by the HTTP client). |
| `repo_branch_create`                             | **Complete** (minimal)     | Creates `refs/heads/{name}` from an existing commit SHA. Low-level blob/tree/commit object creation is still not modeled.           |
| `notification_mark_read`                         | **Complete** (side-effect) | Often **205** empty body — see README.                                                                                              |


---

## 3. Representative GitHub write families — remaining gaps

Status: **mapped** or **missing** (still not in CGS).

### Issues


| REST                                    | Status                                  |
| --------------------------------------- | --------------------------------------- |
| `POST/PATCH/DELETE` issues and comments | **Mapped** (see table §1)               |
| `POST .../reactions` (issue / comment)  | **Missing**                             |
| Dedicated lock endpoint                 | **Missing** (use `issue_update.locked`) |


### Pull requests


| REST                                                                    | Status      |
| ----------------------------------------------------------------------- | ----------- |
| Create / patch / merge PR                                               | **Mapped**  |
| `requested_reviewers`, review dismiss                              | **Missing** |
| Review submit, `POST .../pulls/comments` inline comments           | **Mapped**  |


### Repository & git


| REST                                       | Status      |
| ------------------------------------------ | ----------- |
| Contents put/delete                        | **Mapped**  |
| `POST .../git/refs` branch creation        | **Mapped**  |
| Low-level git blobs/trees/commits          | **Missing** |
| Repo create/delete, forks POST             | **Missing** |


### Actions, collaborators, gists, bulk notifications


| REST                                 | Status      |
| ------------------------------------ | ----------- |
| Workflow dispatch, rerun             | **Mapped**  |
| Collaborators, hooks                 | **Missing** |
| Gist create/update                   | **Missing** |
| `PUT /notifications` (mark all read) | **Missing** |


---

## 4. Priority bands (remaining work)


| Tier   | Theme                           | Examples                                                        |
| ------ | ------------------------------- | --------------------------------------------------------------- |
| **P0** | Reactions, review requests      | `POST .../reactions`, `POST .../pulls/{id}/requested_reviewers` |
| **P1** | Remaining review + Actions      | review requests/dismiss, workflow run approval, workflow cancel gaps |
| **P2** | Repo admin, git plumbing        | Branch protection, low-level git objects, repo create/delete     |


---

## 5. OAuth scope hints


| Write area                                               | Typical classic scopes  |
| -------------------------------------------------------- | ----------------------- |
| Repo issues, PRs, labels, milestones, releases, contents | `repo` or `public_repo` |
| Notifications                                            | `notifications`         |
| Gists                                                    | `gist`                  |


---

## 6. Hermit / plasm-mock testing

`[crates/plasm-mock](../../crates/plasm-mock)` does **not** replay GitHub path-for-path. Validate writes against the live API or a contract mock.

---

## 7. Out of scope (by design)

- **GitHub Projects v2** — GraphQL-first.
- **Every** preview-gated beta route unless explicitly needed.

