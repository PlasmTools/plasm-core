# Quarantined catalogs

Catalogs here are preserved for repair and are not discovered or packed from
`apis/`. Do not restore a catalog to `apis/` until its typed input contracts,
teaching coverage, and pack preflight pass without weakening validation.

## Discord

Moved here at the user's request on 2026-10-06 with all existing WIP preserved.
The remaining payload-contract blockers include explicit-null clears, native
unions/maps, unique-array constraints, multipart bodies, and operations missing
from the pinned OpenAPI specification. No replacement JSON-bag contract is used.
