#!/usr/bin/env python3
"""
Project OSS-canonical authoring skills (and optionally apis/README.md) into
doc-site/docs/ for MkDocs publish.

Does **not** import from a private monorepo `docs/` tree. Public reference and
operator pages are edited directly under `doc-site/docs/`.

Usage (from doc-site/ or anywhere):
  python scripts/sync_allowlisted_docs.py [/path/to/plasm-oss]
  python scripts/sync_allowlisted_docs.py --check [/path/to/plasm-oss]

Default plasm-oss root: parent of `doc-site/` (this script lives at
`plasm-oss/doc-site/scripts/sync_allowlisted_docs.py`).

Projections:
  skills/plasm-authoring/SKILL.md     → doc-site/docs/authoring/index.md
  skills/plasm-authoring/reference.md → doc-site/docs/authoring/reference.md
  apis/README.md                      → doc-site/docs/reference/apis-readme.md
                                        (optional; skipped if missing)

`--check` compares projected output to committed files (via a temp dir) and
exits non-zero if they differ — intended for CI.
"""

from __future__ import annotations

import argparse
import filecmp
import re
import sys
import tempfile
from pathlib import Path

GITHUB_REPO = "https://github.com/PlasmTools/plasm-core"
GITHUB_BLOB = f"{GITHUB_REPO}/blob/main"
GITHUB_TREE = f"{GITHUB_REPO}/tree/main"

# Private / product-only paths — publish as inline code, not doc links.
PRIVATE_DOC_LINKS = (
    "saas-architecture.md",
    "oss-saas-boundary.md",
    "private-control-plane-api.md",
    "plasm-mcp-tenant-configuration.md",
    "oss-core-ui-surface.md",
    "env-profiles.md",
)


def _crate_blob(path: str) -> str:
    path = path.removeprefix("plasm-oss/").lstrip("/")
    return f"{GITHUB_BLOB}/{path}"


def sanitize_for_publish(text: str, *, filename: str) -> str:
    """Rewrite relative / private links for public doc-site publish."""

    # OSS crate sources at various relative depths.
    for _ in range(3):
        text = re.sub(
            r"\[([^\]]*)\]\((?:\.\./)+crates/([^)]+)\)",
            lambda m: f"[{m.group(1)}]({_crate_blob(f'crates/{m.group(2)}')})",
            text,
        )
        text = re.sub(
            r"\[([^\]]*)\]\((?:\.\./)+plasm-oss/crates/([^)]+)\)",
            lambda m: f"[{m.group(1)}]({_crate_blob(f'crates/{m.group(2)}')})",
            text,
        )
    text = re.sub(
        r"\[([^\]]*)\]\(plasm-oss/crates/([^)]+)\)",
        lambda m: f"[{m.group(1)}]({_crate_blob(f'crates/{m.group(2)}')})",
        text,
    )

    # OSS apis/ and fixtures/ trees.
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+plasm-oss/apis/([^)]+)\)",
        lambda m: f"[{m.group(1)}]({GITHUB_TREE}/apis/{m.group(2)})",
        text,
    )
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+apis/([^)]+)\)",
        lambda m: f"[{m.group(1)}]({GITHUB_TREE}/apis/{m.group(2)})",
        text,
    )
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+fixtures/schemas/([^)]+)\)",
        lambda m: f"[{m.group(1)}]({GITHUB_TREE}/fixtures/schemas/{m.group(2)})",
        text,
    )

    # Authoring skill cross-links.
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+plasm-oss/skills/plasm-authoring/reference\.md([^)]*)\)",
        lambda m: f"[{m.group(1)}](../authoring/reference.md{m.group(2)})",
        text,
    )
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+skills/plasm-authoring/reference\.md([^)]*)\)",
        lambda m: f"[{m.group(1)}](../authoring/reference.md{m.group(2)})",
        text,
    )
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+plasm-oss/skills/plasm-forge/SKILL\.md\)",
        rf"[\1]({GITHUB_TREE}/skills/plasm-forge/SKILL.md)",
        text,
    )
    text = re.sub(
        r"\[plasm-catalog-([a-z0-9-]+)\]\(\.\./plasm-catalog-\1/SKILL\.md\)",
        lambda m: f"[plasm-catalog-{m.group(1)}]({GITHUB_TREE}/skills/plasm-catalog-{m.group(1)}/SKILL.md)",
        text,
    )
    text = re.sub(
        r"\[`?\.cursor/agents/plasm-forge\.md`?\]\((?:\.\./)+\.cursor/agents/plasm-forge\.md\)",
        f"[`.cursor/agents/plasm-forge.md`]({GITHUB_TREE}/.cursor/agents/plasm-forge.md)",
        text,
    )

    # Published reference pages (when skills still link via monorepo docs/ paths).
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+docs/schema-overlay\.md\)",
        r"[\1](../reference/schema-overlay.md)",
        text,
    )
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+docs/incremental-teaching-prompts\.md([^)]*)\)",
        r"[\1](../reference/incremental-teaching-prompts.md\2)",
        text,
    )
    # Remaining private-monorepo docs/ links → inline code (not published here).
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+docs/[^)]+\)",
        r"`\1`",
        text,
    )
    text = re.sub(
        r"\[([^\]]*)\]\((?:guardians-alignment|plan-flow-typing|intent-discovery|research-discovery-annotation-rubric)\.md\)",
        r"`\1`",
        text,
    )

    # Private SaaS / product-only docs and deploy paths.
    for doc in PRIVATE_DOC_LINKS:
        text = re.sub(rf"\[([^\]]*)\]\({re.escape(doc)}\)", r"`\1`", text)
    text = re.sub(r"\[([^\]]*)\]\((?:\.\./)+deploy/[^)]+\)", r"`\1`", text)
    text = re.sub(
        r"\[([^\]]*)\]\((?:\.\./)+crates/plasm-saas/[^)]+\)",
        r"`\1`",
        text,
    )
    text = re.sub(r"\[[^\]]*\]\(web/lib/[^)]+\)", "`ProjectMcp (Phoenix)`", text)
    text = re.sub(
        r"\[([^\]]*)\]\(phoenix-client-route-map\.md\)",
        r"[Plasm Cloud](https://platform.plasm.tools)",
        text,
    )

    # Authoring self-link after SKILL.md → index.md rename.
    if filename == "index.md":
        text = re.sub(r"\[([^\]]*)\]\(SKILL\.md\)", r"[\1](index.md)", text)

    if filename == "apis-readme.md":
        text = text.replace(
            "../skills/plasm-authoring/reference.md",
            "../authoring/reference.md",
        )
        text = re.sub(
            r"\[([^\]]+)\]\((?:\.\./)+docs/incremental-teaching-prompts\.md([^)]*)\)",
            r"[Incremental teaching](incremental-teaching-prompts.md\2)",
            text,
        )
        text = re.sub(
            r"\[PokéAPI mini\]\((?:\.\./)+fixtures/schemas/pokeapi_mini/\)",
            f"[PokéAPI mini]({GITHUB_TREE}/fixtures/schemas/pokeapi_mini/)",
            text,
        )
        text = re.sub(
            r"`?\[eval/README\.md\]\((?:\.\./)+eval/README\.md\)`?",
            f"[eval/README.md]({GITHUB_TREE}/eval/README.md)",
            text,
        )
        text = re.sub(
            r"\[([a-z0-9-]+)\]\(\1/\)",
            lambda m: f"[{m.group(1)}]({GITHUB_TREE}/apis/{m.group(1)}/)",
            text,
        )

    return text


def project_text(src: Path, dest_name: str) -> str:
    content = src.read_text(encoding="utf-8")
    return sanitize_for_publish(content, filename=dest_name)


def write_projection(src: Path, dst: Path) -> None:
    dst.parent.mkdir(parents=True, exist_ok=True)
    dst.write_text(project_text(src, dst.name), encoding="utf-8")


def projection_pairs(oss_root: Path) -> list[tuple[Path, Path]]:
    """(source, relative dest under doc-site/docs/) pairs that exist."""
    authoring = oss_root / "skills" / "plasm-authoring"
    pairs: list[tuple[Path, Path]] = []
    skill = authoring / "SKILL.md"
    if skill.is_file():
        pairs.append((skill, Path("authoring/index.md")))
    ref = authoring / "reference.md"
    if ref.is_file():
        pairs.append((ref, Path("authoring/reference.md")))
    apis_readme = oss_root / "apis" / "README.md"
    if apis_readme.is_file():
        pairs.append((apis_readme, Path("reference/apis-readme.md")))
    return pairs


def run_sync(oss_root: Path, docs_root: Path) -> list[Path]:
    pairs = projection_pairs(oss_root)
    if not pairs:
        raise FileNotFoundError(
            f"no projectable sources under {oss_root} "
            "(expected skills/plasm-authoring/SKILL.md and/or reference.md)"
        )
    written: list[Path] = []
    for src, rel in pairs:
        dst = docs_root / rel
        write_projection(src, dst)
        written.append(dst)
        print(f"projected {src.relative_to(oss_root)} -> docs/{rel.as_posix()}")
    return written


def run_check(oss_root: Path, docs_root: Path) -> int:
    pairs = projection_pairs(oss_root)
    if not pairs:
        print(
            f"error: no projectable sources under {oss_root}",
            file=sys.stderr,
        )
        return 1

    mismatches: list[str] = []
    missing_committed: list[str] = []

    with tempfile.TemporaryDirectory(prefix="plasm-doc-sync-") as tmp:
        tmp_root = Path(tmp)
        for src, rel in pairs:
            projected = tmp_root / rel
            write_projection(src, projected)
            committed = docs_root / rel
            if not committed.is_file():
                missing_committed.append(rel.as_posix())
                continue
            if not filecmp.cmp(projected, committed, shallow=False):
                mismatches.append(rel.as_posix())

    if missing_committed:
        for path in missing_committed:
            print(f"error: missing committed projection: docs/{path}", file=sys.stderr)
    if mismatches:
        for path in mismatches:
            print(
                f"error: docs/{path} differs from projection "
                f"(re-run sync_allowlisted_docs.py and commit)",
                file=sys.stderr,
            )

    if missing_committed or mismatches:
        return 1

    print("check ok: projected authoring/apis outputs match committed files.")
    return 0


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Project plasm-authoring skills (and optionally apis/README.md) "
            "into doc-site/docs/."
        )
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="exit non-zero if projected outputs differ from committed files",
    )
    parser.add_argument(
        "oss_root",
        nargs="?",
        default=None,
        help="plasm-oss repository root (default: parent of doc-site/)",
    )
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv if argv is not None else sys.argv[1:])
    script_dir = Path(__file__).resolve().parent
    doc_site = script_dir.parent
    default_oss = doc_site.parent
    oss_root = Path(args.oss_root).resolve() if args.oss_root else default_oss
    docs_root = doc_site / "docs"

    authoring = oss_root / "skills" / "plasm-authoring"
    if not authoring.is_dir():
        print(f"error: authoring skill not found: {authoring}", file=sys.stderr)
        return 1

    if args.check:
        return run_check(oss_root, docs_root)

    run_sync(oss_root, docs_root)
    print("\nProjected and sanitized for public doc-site publish.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
