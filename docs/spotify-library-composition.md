# Spotify Library composition

Status: implemented in the working tree. Spotify catalog version 25 declares
the Library projection. No agent evaluation or commit is implied.

## Domain contract

Keep Song, Album, and Playlist as distinct domain identities. Add a Library
read model with authenticated songs, albums, and playlists relations.

Library.songs means the distinct Song identities reachable from directly saved
songs, songs in saved albums, and songs in saved playlists. It excludes catalog
search, recommendations, and independently liked items unless saved through one
of those sources. Filtering and ranking apply after collection composition.
Library.albums and Library.playlists return the corresponding saved collections.
Existing Song library operations retain their narrower direct-save meaning.

No genre, ranking, task-specific answer, or scenario selection belongs in this
view. Song detail hydration supplies genre and play_count through existing reads.

## Original expressiveness blocker

`ViewNodeSpec` names one capability with scalar binds. `ViewParamBinding::NodeField`
reads the first row of a prior node. There is no declared per-row traversal node.
`ViewRelationBinding` selects rows from one node; it cannot union typed rowsets
from several nodes. Consequently the previous view schema could not express the
required variable-cardinality album and playlist traversal followed by a Song
identity union.

Relevant implementation: plasm-core/src/schema.rs, plasm-runtime/src/view_dag_run.rs,
and plasm-runtime/src/view_plan.rs. The existing Album.songs and Playlist.songs
relations both target Song; entity separation is not the blocker.

## Implemented composition

A view node can now declare `traverse: {node: collections, relation: items}`
instead of a capability invocation. This consumes every source row through the
ordinary relation materializer and observes GET-embedded parent relations when
necessary. A relation output can declare
`binding: {kind: node_union_rows, nodes: [direct, nested]}` to union compatible
rowsets by Ref identity in first occurrence order.

View collection reads consume all pages within existing runtime safety bounds.
Final view coverage combines all node proofs. Wrong-identity view GETs are rejected
before decoded parent/child entities enter the session cache. A complete field
cache entry without the required relation is insufficient for parent traversal.
Embedded child rows are cached as summaries: a parent GET does not establish
the child's detail projection. Relation traversal hydrates those children before
returning them. Parameterless GETs have no requested row identity to compare;
ID-addressed GETs retain strict identity validation.

Successful view rows are published into the session graph, so identity-bound
fanout can navigate their declared relations. A view row is a full snapshot:
publication replaces prior membership, including through read-branch commit.
Inherited sibling snapshots are not re-published as new observations.
Direct relation navigation accepts the same bounded-singleton proof as scalar
extraction (`take 1`); a zero-row bounded receiver still fails at runtime.

## General implementation boundary

The extension adds typed rowset traversal over an existing declared
relation, followed by an identity union of compatible rowsets. Reuse ordinary
runtime relation materialization and entity identity. Do not add Spotify-specific
execution, template-generated IDs, or a harness-side reconstruction path.

Validation must reject unknown/forward node references, absent relations, and
unions with incompatible target entities. Execution must preserve all source
coverage proofs: an incomplete contributing collection cannot yield a Complete
aggregate merely because other branches completed. Empty collections are lawful;
unavailable traversal must remain observable. Mutation freshness and invocation
scope follow existing execution laws.

## Verification before catalog publication

1. Extend an abstract view fixture with overlapping child identities across
   direct rows and two container collections. Include an empty container,
   multiple containers, and an incomplete branch.
2. Verify identity deduplication, all-container traversal, ordering invariance,
   coverage propagation, scope binding, and rejection of incompatible unions.
3. Author the Library entity/view/capability, explicit view_embed relations,
   discovery descriptions, CML view mapping, and catalog version increment.
4. Validate the Spotify catalog against its independent OpenAPI through Hermit;
   exercise the composed collection, not only individual inner mappings.
5. Inspect seeded teaching to prove Library.songs is executable without requiring
   the agent to reconstruct the three branches. No AppWorld rerun is implied.

## Validation boundary

The abstract `view_rowsets` fixture exercises a paginated collection, overlapping
child identities, an empty collection, source-order reversal, required credentials,
missing embeds, and divergent parent identities through live mock transport. A
property test generates union inputs and Complete/Partial/Unknown source proofs.
The language-view suite also checks ordinary inline and bound relation navigation.

Spotify schema validation passes. The independent OpenAPI/Hermit spec-response
run currently reports 87 passes, 4 failures, and 3 existing skips. All four Library
failures stop on the same list-to-GET identity mismatch: its independently generated
Album list and Album GET examples use different IDs. Runtime identity checking
is retained. This run does not verify the composed Spotify graph; the abstract
coherent-graph evidence is separate from the existing inner mapping checks.
No mock identity inference, gold shaping, or agent evaluation was added.
