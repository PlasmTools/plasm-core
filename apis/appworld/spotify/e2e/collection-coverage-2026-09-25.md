# Playlist collection coverage — Spotify v34

The unchanged authored Python program now executes collection compute over all
owned-playlist songs against a fresh AppWorld `a1d3dfd_1` world. It returns 37
distinct titles with Complete coverage. A subsequent aggregate returns 37 with
Complete coverage. This proves this read/compute boundary, not the CSV/email task.

## Backend contract

Pinned image: `4c618232fff3`; AppWorld runtime `0.2.0.dev0`, dataset `0.2.0`.

In `appworld/apps/spotify/apis.py`, `show_playlist` retrieves and authorizes the
playlist, then returns `playlist.to_dict(humanize=True)`. In `models.py`,
`Playlist._to_humanized_dict(name="default")` serializes every member of
`self.songs` without a limit or continuation. Each child retains its shortened
song fields and artists; its `song_id` is renamed to `id` on the wire.

Accordingly, `Playlist.songs` declares `collection_coverage: complete` and uses
`songs[*]`, preserving child objects rather than reducing them to IDs. Runtime
identity decoding supplies the typed Song identity. This says nothing about
the completeness of the upstream playlist query or other relations.

## Validation

- Split catalog schema validation: passed, 27 entities and 92 capabilities.
- Pack and installed NAPI load: passed. Spotify IL catalog hash:
  `05afb7c16a943219e7077d4f3b3a980508abb1d67cf3d2a480b70b96492b699f`.
- Hermit: 122 passed, 6 failed, 12 skipped, identical totals and failing
  capabilities on the unchanged v33 catalog. Two failures concern User search
  HTTP 404s; four concern Library view identity divergence in generated mock
  responses. Embedded playlist traversal is skipped by this validator.
  Catalog-wide Hermit conformance remains open.
- Live sandbox: `a1d3dfd_1-compute-coverage.py` passed through the rebuilt installed
  NAPI engine and packaged Monty worker. No stack-size override or external result
  transformation. Projection, distinct and collection text compute executed in
  the reviewed DAG. All 94 transport requests across compute and count returned
  HTTP 200; only reads and authentication POSTs occurred.
- Negative control: the same program and rebuilt engine using the previous v33
  pack still reject unknown collection coverage. The change does not globally
  bypass the collection-compute gate.
- Public Spotify: not applicable; this catalog targets the AppWorld simulation.
- No task completion or CSV/email effects submitted. Full task acceptance remains
  blocked by the separately recorded composition gaps.

Host evidence in the parent workspace:
`target/appworld-python-development/coverage-20260925/` contains backend source
excerpts, both Hermit logs, binary hashes, catalog packs, teaching, plans, live
results, transport events, the negative control and native evaluation output.
The source program remains
`scripts/appworld/python-development/programs/a1d3dfd_1-compute-coverage.py`.
Stored results are operator evidence, not agent processing inputs.


## Follow-up: Song artists and full composition — v35

`Song._to_humanized_dict` serializes every member of `self.artists` across its
humanized variants, without paging or truncation. The v35 `Song.artists`
`from_parent_get` relation therefore asserts complete membership at `artists[*]`.
Other unannotated relations retain their conservative coverage behavior.

Schema validation passes (27 entities, 92 capabilities). Packing passes with IL
hash `bd250ded124fefcd1fa11e85e925de4538eb4871f0e4d3cfaeca187f79427e9c`.
Hermit again reports 122 passed, 6 failed and 12 skipped, with the same User
search and Library mock identity failures described above.

A fresh-world authored program now renders artists for the 37 distinct playlist
songs, computes a CSV, creates the file and sends the email through the reviewed
DAG. Native acceptance is 8/11: three content comparisons retain the serializer's
CSV quotes rather than decoding them. A fresh minimal-quoting probe fails before
IO because conditional compute expressions are unsupported. Neither result is
represented as full task acceptance or catalog-wide conformance.

Evidence: parent `target/appworld-python-development/map-composition-20260925/`
and `map-composition-minimal-csv-20260925/`. No artifact processing or model
evaluation was used; both owned containers were removed.
