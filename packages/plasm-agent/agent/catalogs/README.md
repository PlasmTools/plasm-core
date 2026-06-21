# Catalogs

Author CGS (`domain.yaml`) + CML (`mappings.yaml`) here — the **single source of capability**.

v1 layout: one subdirectory per API (`catalogs/<entry_id>/`).

For a working example, symlink or copy from the repo-wide catalog tree:

```bash
ln -s ../../../apis/pokeapi catalogs/pokeapi
```

Catalogs are **platform-independent data** (not native cdylib plugins). The framework loads YAML/JSON and pins `catalog_cgs_hash` at session open.

See `skills/plasm-authoring/` in the plasm-oss root for authoring doctrine.
