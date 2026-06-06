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

import shutil
import sys
from pathlib import Path

ALLOWLIST = [
    "plasm-language-definition.md",
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
        shutil.copy2(src, ref_dst / name)
        print(f"copied {name}")

    if apis_readme.is_file():
        shutil.copy2(apis_readme, ref_dst / "apis-readme.md")
        print("copied apis/README.md -> reference/apis-readme.md")

    auth_dst = doc_site / "docs" / "authoring"
    auth_dst.mkdir(parents=True, exist_ok=True)
    if authoring_skill.is_dir():
        for fname in ("SKILL.md", "reference.md"):
            p = authoring_skill / fname
            if p.is_file():
                dest_name = "index.md" if fname == "SKILL.md" else "reference.md"
                shutil.copy2(p, auth_dst / dest_name)
                print(f"copied plasm-authoring/{fname} -> authoring/{dest_name}")
    else:
        print(f"warn: authoring skill not found at {authoring_skill}", file=sys.stderr)

    print("\nNext: re-apply link sanitization edits if upstream paths changed (see doc-site README).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
