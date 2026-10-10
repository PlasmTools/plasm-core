# Embedded collection membership audit

Executable many-relations require a complete membership contract at CGS
compilation. `from_parent_get` and `prefer_from_parent_get` with omitted or unknown `collection_coverage`
fails as `SchemaError::RelationMembershipUnproven`. A declaration does not prove
an absent response path: runtime checks still distinguish missing/null arrays
from observed empty arrays and preserve partial parent coverage.

This audit replaces 48 missing contracts across 15 catalogs. Exhaustive arrays
below describe membership of the declared relation under its parent, not a claim
that an unrelated global catalog query is complete.

| Catalog | Relations | Contract/remedy |
| --- | --- | --- |
| hackernews | Item.kids, Item.poll_options, User.submitted | Complete parent item/user arrays in the [official in-memory API](https://github.com/HackerNews/API). |
| architect-exchange | User.accounts | `/whoami` accounts array; vendored `apis/architect-exchange/openapi-api-gateway.json`, `WhoAmIResponse.accounts`, no continuation parameters. |
| linear | Issue.labels, Issue.children | Cursor-paginated scoped queries, with independent connection cursors and archived children included. [Linear pagination](https://linear.app/developers/pagination). Partial nested arrays were removed from Issue Get. |
| dnd5e | AbilityScore.skills, Rule.subsections, ClassLevel.features, SubclassLevel.features, Spell.classes, Spell.subclasses | Exhaustive SRD resource arrays in the parent response, rather than paginated root collections. [API resource reference](https://www.dnd5eapi.co/), vendored catalog OpenAPI and mappings. |
| jira | Issue.attachments | Parent issue `fields.attachment` array; attachment membership is not a paginated comments/changelog connection. [Issue API](https://developer.atlassian.com/cloud/jira/platform/rest/v3/api-group-issues/). |
| pokeapi | Pokemon.forms, Pokemon.abilities, Pokemon.types, Pokemon.moves, Type.moves, Ability.pokemon, Type.pokemon | Complete typed parent-resource arrays. Root resource lists paginate separately. [PokéAPI resource schemas](https://pokeapi.co/docs/v2). |
| github | Commit.files, Issue.labels | Removed exhaustive files relation: [Get a commit](https://docs.github.com/en/rest/commits/commits#get-a-commit) paginates files and has a 3,000-file ceiling. Repository file reads through CommitFile remain available; no false completeness assertion. The single Label query uses issue_number scope for paginated issue labels and repository scope for repository labels. |
| google-sheets | Spreadsheet.sheets | Complete spreadsheet sheets array; catalog Get does not restrict the parent to a page of sheets. [Spreadsheet resource](https://developers.google.com/workspace/sheets/api/reference/rest/v4/spreadsheets). |
| vultr | KubernetesNodePool.worker_nodes | Node pool response's `nodes` array; GET node pool has no nested continuation. [Kubernetes API reference](https://docs.vultr.com/public/doc-assets/pdfs/collection_item/products-kubernetes.pdf). |
| rickandmorty | Location.residents, Episode.characters | Complete URL arrays on each parent resource; root resource pagination is separate. [Resource schemas](https://rickandmortyapi.com/documentation). |
| gmail | Thread.messages | Thread Get returns the thread's messages; listing threads is paginated separately. [Thread retrieval](https://developers.google.com/workspace/gmail/api/guides/threads). |
| appworld/amazon | RatingDistribution.breakdown, Product.variations, ProductFeatureChoices.seller_options, Cart.cart_items, Order.items | Closed nested arrays from the pinned local `openapi.json` response contracts and parent mappings; no independent nested continuation. |
| appworld/splitwise | GroupBalanceParticipant.incoming, GroupBalanceParticipant.outgoing, BalanceSummary.people, Group.members, Expense.shares, Activity.owed_by, GroupBalance.participants | Closed membership/balance/share arrays from the pinned local `openapi.json` contracts. Parent list acquisition retains its own coverage. |
| appworld/todoist | Project.pending_invites, Project.collaborators, Task.labels | Closed parent-resource arrays from the pinned local `openapi.json` contracts. |
| appworld/gmail | EmailThread.emails, EmailThread.drafts, Email.recipients, Email.attachments, Draft.recipients, Draft.attachments | Closed parent-resource arrays from the pinned local `openapi.json` contracts. |

Abstract fixtures declare complete membership only for exhaustive mock arrays.
The former unproven catalog fixture case is now a compilation regression; missing
wire observations and unfinished pagination remain runtime regressions. Packing
all catalogs is a release gate. Live authenticated Linear traversal must also be
checked after deployment; a static declaration or unauthenticated response alone
does not establish an end-to-end traversal witness.
