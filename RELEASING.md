# Releasing

**Canonical release documentation and CI** for OSS binaries, install plane, and tags live in the **Plasm monorepo**, not in this `plasm-core` repository:

**[RELEASING.md](https://github.com/PlasmTools/plasm/blob/main/RELEASING.md)** on [PlasmTools/plasm](https://github.com/PlasmTools/plasm)

- Annotated tags (`v*.*.*`) and CircleCI workflow **`release`** run on **PlasmTools/plasm** only.
- GitHub Release tarballs are uploaded to **PlasmTools/plasm-core** from monorepo CI scripts.

Update **`CHANGELOG.md`** in this repo for OSS workspace release notes; follow the monorepo checklist to ship.
Keep the live changelog short (recent `0.4.x` only); older entries go in [`CHANGELOG-ARCHIVE.md`](CHANGELOG-ARCHIVE.md).
Do not cut a SemVer re-ship with a byte-identical changelog blurb — amend the prior note or write one honest line (“pipeline re-ship of vX.Y.Z”).
