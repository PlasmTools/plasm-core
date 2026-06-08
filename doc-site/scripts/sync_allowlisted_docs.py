#!/usr/bin/env python3
"""
Copy allowlisted markdown from a maintainer monorepo into doc-site/docs/reference/
and refresh authoring snapshots.

Usage (from doc-site/):
  python scripts/sync_allowlisted_docs.py /path/to/plasm/monorepo/root

Default monorepo root: ../../../../ relative to this script when nested:
  plasm/plasm-oss/doc-site/scripts/sync_allowlisted_docs.py -> plasm/
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

GITHUB_REPO = "https://github.com/PlasmTools/plasm-core"
GITHUB_BLOB = f"{GITHUB_REPO}/blob/main"
GITHUB_TREE = f"{GITHUB_REPO}/tree/main"

ALLOWLIST = [
    "plasm-language-definition.md",
    "plasm-row-compute.md",
    "plasm-long-operations.md",
    "incremental-teaching-prompts.md",
    "tool-model-http.md",
    "oss-core-trace-artifacts.md",
    "mcp-session-reuse.md",
    "mcp-trace-correlation.md",
    "mcp-logical-sessions.md",
    "plasm-mcp-incoming-auth.md",
    "oss-appliance-mcp-persistence.md",
    "oss-outgoing-oauth-promotion.md",
    "genco-plugin-pipeline.md",
    "cgs-extensions-roadmap.md",
    "correction-catalogue.md",
    "schema-overlay.md",
    "plasm-cgs-remote-terminal.md",
    "appliance-surface-inventory.md",
]


# Private monorepo docs — publish as inline code, not doc links.
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
    """Rewrite monorepo-relative links for public doc-site publish."""

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

    # Monorepo docs/ → published reference/.
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

    if filename == "plasm-row-compute.md":
        text = text.replace(
            "plasm-language-definition.md#binding-rhs-shapes-label--",
            "plasm-language-definition.md#binding-rhs-shapes-label",
        )
        text = text.replace(
            "plasm-language-definition.md#relation-binding-proofs-query_scoped_bindings",
            "plasm-language-definition.md#typed-semantic-core-lean-oriented-sketch",
        )

    return text


def copy_and_sanitize(src: Path, dst: Path) -> None:
    content = src.read_text(encoding="utf-8")
    sanitized = sanitize_for_publish(content, filename=dst.name)
    dst.write_text(sanitized, encoding="utf-8")


def main() -> int:
    script_dir = Path(__file__).resolve().parent
    doc_site = script_dir.parent
    ref_dst = doc_site / "docs" / "reference"

    monorepo = Path(sys.argv[1]).resolve() if len(sys.argv) > 1 else script_dir.parent.parent.parent
    docs_src = monorepo / "docs"
    authoring_skill = monorepo / "plasm-oss" / "skills" / "plasm-authoring"
    if not authoring_skill.is_dir():
        authoring_skill = monorepo / "skills" / "plasm-authoring"
    apis_readme = monorepo / "apis" / "README.md"

    if not docs_src.is_dir():
        print(f"error: docs directory not found: {docs_src}", file=sys.stderr)
        return 1

    ref_dst.mkdir(parents=True, exist_ok=True)

    for name in ALLOWLIST:
        src = docs_src / name
        if not src.is_file():
            print(f"warn: skip missing {src}", file=sys.stderr)
            continue
        copy_and_sanitize(src, ref_dst / name)
        print(f"copied {name}")

    if apis_readme.is_file():
        copy_and_sanitize(apis_readme, ref_dst / "apis-readme.md")
        print("copied apis/README.md -> reference/apis-readme.md")

    auth_dst = doc_site / "docs" / "authoring"
    auth_dst.mkdir(parents=True, exist_ok=True)
    if authoring_skill.is_dir():
        for fname in ("SKILL.md", "reference.md"):
            p = authoring_skill / fname
            if p.is_file():
                dest_name = "index.md" if fname == "SKILL.md" else "reference.md"
                copy_and_sanitize(p, auth_dst / dest_name)
                print(f"copied plasm-authoring/{fname} -> authoring/{dest_name}")
    else:
        print(f"warn: authoring skill not found at {authoring_skill}", file=sys.stderr)

    print("\nSynced and sanitized for public doc-site publish.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
