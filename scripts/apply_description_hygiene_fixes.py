#!/usr/bin/env python3
"""Apply teaching-table description hygiene fixes to apis/*/domain.yaml.

Companion to check_catalog_description_hygiene.py — run check after apply.
"""
from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

# Full description value replacements (after stripping leading whitespace from value).
VALUE_REPLACEMENTS: dict[str, str | None] = {
    "Load one issue by identifier (e.g. ENG-42).": None,
    "Fetch one team by key (e.g. ENG).": None,
    "Fetch one project by id.": None,
    "Fetch one cycle by id.": None,
    "Fetch one document by id.": None,
    "Fetch one initiative by id.": None,
    "Get a single issue by key or ID (e.g. PROJ-123)": None,
    "Get a single issue by key or ID (e.g. PROJ-123).": None,
    "Get a project by key or ID": None,
    "Get a project by key or ID.": None,
    "A Linear team (workspace grouping); agents address teams by short key (e.g. ENG).": (
        "A Linear team (workspace grouping)."
    ),
    "A Linear issue (task, bug, or feature); use human identifier (e.g. ENG-42).": (
        "A Linear work item (task, bug, or feature)."
    ),
    "An issue label (addressed by name in filters and writes).": "An issue label.",
    "Find issues by title text and name-typed filters (team key, state, label, assignee).": (
        "Find issues by title and filter criteria."
    ),
    "Update issue fields (state, assignee, labels, priority, …) by identifier.": (
        "Change issue workflow, ownership, labels, or priority."
    ),
    "Add a comment on an issue (scoped by issue identifier).": "Add a comment on an issue.",
    "List comments on an issue (scoped by issue identifier).": "List comments on an issue.",
    "Create an issue on a team (by team key).": "Create an issue.",
    "Create a project on a team (by team key).": "Create a project on a team.",
    "List cycles for a team (by team key).": "List cycles for a team.",
    "List workflow columns for a team (by team key).": "List workflow columns for a team.",
    "Issue context when scoped by identifier.": "Triage bundle for one issue.",
    "Load issue context (issue + comments) for one identifier.": "Triage bundle for one issue.",
    "One issue plus its comments and relation hooks for triage.": (
        "Bundle for triaging one issue."
    ),
    "Project row plus recent status updates.": "Project with recent status updates.",
    "Project plus recent status updates.": "Project with recent status updates.",
    "Project context by project id.": "Project context snapshot.",
    "Issue row plus comments for one identifier.": "Triage snapshot for one issue.",
    "Project plus status updates.": "Project status snapshot.",
    "Cycle board (cycle + issues in iteration).": "Cycle board snapshot.",
    "Cycle plus issues in that iteration.": "Cycle board snapshot.",
    "Linear web URL from issue identifier.": "Shareable web URL for an issue.",
    "Assemble the Linear web URL for an issue identifier.": "Shareable web URL for an issue.",
    "Human-visible issue key (e.g. ENG-42).": "Human-visible issue key.",
    "Issue anchor (human identifier, e.g. ENG-42).": "Issue anchor.",
    "Team short key (e.g. ENG).": "Team short key.",
    "Owning team (by team key).": "Owning team.",
    "Transition readiness snapshot scoped by issue key.": "Transition readiness snapshot.",
    "Sprint board snapshot scoped by sprint id.": "Sprint board snapshot.",
    "Target repo (owner/name), e.g. octocat/Hello-World.": "Target repository (owner/name).",
}

# Regex substitutions applied to description values (order matters).
VALUE_REGEX: list[tuple[re.Pattern[str], str]] = [
    (re.compile(r"\s*\(scoped by [^)]+\)"), ""),
    (re.compile(r"\s*by identifier\.?", re.I), ""),
    (re.compile(r"\s*\(by team key\)"), ""),
    (re.compile(r"\s*by team key", re.I), ""),
    (re.compile(r"\s*by project id\.?", re.I), ""),
    (re.compile(r"\s*by sprint id\.?", re.I), ""),
    (re.compile(r"\s*by issue key\.?", re.I), ""),
    (re.compile(r"\s*by id\.?", re.I), ""),
    (re.compile(r"\s*by key\.?", re.I), ""),
    (re.compile(r"\s*\(e\.g\.\s*[A-Z]{2,}-\d+\)"), ""),
    (re.compile(r",\s*e\.g\.\s*[A-Z]{2,}-\d+"), ""),
    (re.compile(r"\s*e\.g\.\s*PROJ-\d+"), ""),
    (re.compile(r",\s*e\.g\.\s*octocat/Hello-World"), ""),
    (re.compile(r"\s*e\.g\.\s*octocat/Hello-World\.?"), ""),
    (re.compile(r"\s*for one identifier\.?", re.I), ""),
]

GENERIC_GET = re.compile(
    r"^(Fetch one|Load one|Get one|Get a single)\s+.+\.?$",
    re.I,
)


def repo_apis_root(script_path: Path) -> Path:
    return script_path.resolve().parent.parent / "apis"


def clean_value(raw: str) -> str | None:
    val = raw.strip()
    if val in VALUE_REPLACEMENTS:
        repl = VALUE_REPLACEMENTS[val]
        return repl.strip() if repl else None

    for pat, repl in VALUE_REGEX:
        val = pat.sub(repl, val)
    val = re.sub(r"\s+", " ", val).strip()
    val = val.rstrip(".,; ")
    if not val:
        return None
    if GENERIC_GET.match(val):
        return None
    # Field inventory parentheticals with comma lists
    val = re.sub(
        r"\s*\([^)]*,\s*(state|assignee|labels?|priority|title)[^)]*\)",
        "",
        val,
        flags=re.I,
    ).strip()
    if not val:
        return None
    return val


def process_file(path: Path, dry_run: bool) -> int:
    lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    out: list[str] = []
    changed = 0
    i = 0
    while i < len(lines):
        line = lines[i]
        m = re.match(r"^(\s*)description:\s*(.*)$", line.rstrip("\n"))
        if not m:
            out.append(line)
            i += 1
            continue

        indent, rest = m.group(1), m.group(2).strip()
        # Multiline description — skip
        if rest in {"", "|", ">"}:
            out.append(line)
            i += 1
            continue

        quote = None
        if rest.startswith('"') or rest.startswith("'"):
            quote = rest[0]
            if rest.count(quote) >= 2 and rest.endswith(quote):
                raw = rest[1:-1]
            else:
                out.append(line)
                i += 1
                continue
        else:
            raw = rest

        new_val = clean_value(raw)
        if new_val == raw.strip().rstrip(".,; "):
            # try with trailing period normalization
            normalized_old = raw.strip()
            if new_val and not normalized_old.endswith(".") and new_val == normalized_old + ".":
                out.append(line)
                i += 1
                continue
            if new_val is None and normalized_old in VALUE_REPLACEMENTS:
                changed += 1
                i += 1
                continue
            if new_val == normalized_old:
                out.append(line)
                i += 1
                continue

        changed += 1
        if new_val is None:
            i += 1
            continue

        if quote:
            escaped = new_val.replace(quote, f"\\{quote}")
            out.append(f"{indent}description: {quote}{escaped}{quote}\n")
        else:
            out.append(f"{indent}description: {new_val}\n")
        i += 1

    if changed and not dry_run:
        path.write_text("".join(out), encoding="utf-8")
    return changed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--catalog", action="append", dest="catalogs")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--apis-root", type=Path, default=None)
    args = parser.parse_args()

    apis_root = args.apis_root or repo_apis_root(Path(__file__))
    catalogs = sorted(
        p.name
        for p in apis_root.iterdir()
        if p.is_dir() and (p / "domain.yaml").is_file()
    )
    if args.catalogs:
        catalogs = [c for c in catalogs if c in set(args.catalogs)]

    total = 0
    for catalog in catalogs:
        n = process_file(apis_root / catalog / "domain.yaml", args.dry_run)
        if n:
            print(f"{catalog}: {n} description(s) updated")
            total += n
    print(f"apply_description_hygiene_fixes: {total} change(s)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
