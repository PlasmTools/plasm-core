#!/usr/bin/env python3
"""Flag teaching-table-facing description antipatterns in apis/*/domain.yaml.

Doctrine: plasm-oss/skills/plasm-authoring/reference.md
(Teaching-table-facing descriptions, Gloss: do not restate typed structure)

Rule tiers:
  error — A, B, C on entity / capability / view descriptions
  warn  — D, E, F, G; B on values: rows; A on values: rows
"""
from __future__ import annotations

import argparse
import re
import sys
from dataclasses import dataclass
from pathlib import Path
from typing import Literal

Severity = Literal["error", "warn"]

RULES: dict[str, tuple[Severity, str, re.Pattern[str]]] = {
    "A_identity_restatement": (
        "error",
        "Identity restatement (by id/key/identifier/scoped by)",
        re.compile(
            r"\b(by identifier|scoped by|by id\b|by key\b|by issue key|by team key)",
            re.I,
        ),
    ),
    "B_eval_example_leakage": (
        "error",
        "Eval/example leakage (e.g. project-specific keys)",
        re.compile(
            r"e\.g\.\s*(ENG-|PROJ-|octocat/)",
            re.I,
        ),
    ),
    "C_field_param_inventory": (
        "error",
        "Field/param inventory in parentheses",
        re.compile(
            r"description:\s*.*\([^)]*,[^)]*\b(state|assignee|labels?|priority|title)\b",
            re.I,
        ),
    ),
    "D_generic_get_boilerplate": (
        "warn",
        "Generic get/query boilerplate (Fetch/Load/Get one …)",
        re.compile(
            r"description:\s.*\b(Fetch one|Load one|Get a single|Get one)\b",
            re.I,
        ),
    ),
    "E_composed_projection_dup": (
        "warn",
        "Composed projection duplication (plus/+ listing nodes)",
        re.compile(
            r"description:\s.*(\brow plus\b|\bissue \+|\bcomments \+|plus issues|plus status|plus recent|\(cycle \+)",
            re.I,
        ),
    ),
    "F_scoping_parenthetical": (
        "warn",
        "Scoping parenthetical (by team key) on capability",
        re.compile(r"\(by team key\)|\(by issue", re.I),
    ),
    "G_tabular_jargon": (
        "warn",
        "Tabular jargon (row/column)",
        re.compile(
            r"\b(\w+\s+){0,2}row\b|\bper-row\b|\bcolumn\b",
            re.I,
        ),
    ),
}

G_TABULAR_JARGON_ALLOW = re.compile(
    r"workflow column|column name|Workflow column",
    re.I,
)

TOP_SECTIONS = frozenset({"entities", "capabilities", "views", "values", "auth"})


@dataclass
class Finding:
    catalog: str
    path: Path
    line_no: int
    rule_id: str
    severity: Severity
    message: str
    snippet: str
    context: str


def repo_apis_root(script_path: Path) -> Path:
    return script_path.resolve().parent.parent / "apis"


def classify_description_context(
    section: str | None,
    in_fields: bool,
    in_parameters: bool,
    in_output: bool,
) -> str:
    if section == "capabilities":
        return "capability"
    if section == "views":
        return "view"
    if section == "values":
        return "value"
    if section == "entities":
        if in_fields:
            return "field"
        return "entity"
    if in_output:
        return "output"
    return "other"


def effective_severity(rule_id: str, base: Severity, context: str) -> Severity:
    if rule_id == "A_identity_restatement" and context == "value":
        return "warn"
    if rule_id == "B_eval_example_leakage" and context == "value":
        return "warn"
    if rule_id == "C_field_param_inventory" and context == "field":
        return "warn"
    if rule_id == "F_scoping_parenthetical" and context not in {
        "capability",
        "view",
        "entity",
    }:
        return "warn"
    if context in {"capability", "entity", "view"}:
        return base
    if context == "value":
        return "warn"
    return "warn"


def scan_domain_yaml(path: Path, catalog: str) -> list[Finding]:
    findings: list[Finding] = []
    text = path.read_text(encoding="utf-8")
    lines = text.splitlines()

    section: str | None = None
    in_fields = False
    in_parameters = False
    in_output = False
    section_indent = 0

    for line_no, line in enumerate(lines, start=1):
        stripped = line.lstrip()
        if not stripped or stripped.startswith("#"):
            continue
        indent = len(line) - len(stripped)

        if indent <= section_indent and stripped.endswith(":"):
            top = stripped[:-1].strip()
            if top in TOP_SECTIONS:
                section = top
                section_indent = indent
                in_fields = False
                in_parameters = False
                in_output = False
                continue
            if section and indent > 0 and top not in TOP_SECTIONS:
                if section in {"entities", "capabilities", "views", "values"}:
                    in_fields = False
                    in_parameters = False
                    in_output = False

        if stripped == "fields:":
            in_fields = True
            in_parameters = False
            continue
        if stripped == "parameters:":
            in_parameters = True
            in_fields = False
            continue
        if stripped == "output:":
            in_output = True
            continue

        if indent <= section_indent + 2 and stripped.endswith(":"):
            if stripped.startswith("fields:") or stripped.startswith("parameters:"):
                pass
            elif section in {"entities", "capabilities", "views", "values"}:
                in_fields = False
                in_parameters = False
                in_output = False

        if not re.match(r"description:\s*", stripped):
            continue

        if stripped.startswith("auth.token_url") or "token_url:" in stripped:
            continue

        context = classify_description_context(
            section, in_fields, in_parameters, in_output
        )
        snippet = stripped[:120]

        for rule_id, (base_sev, message, pattern) in RULES.items():
            if not pattern.search(line):
                continue
            if rule_id == "G_tabular_jargon" and G_TABULAR_JARGON_ALLOW.search(line):
                continue
            sev = effective_severity(rule_id, base_sev, context)
            findings.append(
                Finding(
                    catalog=catalog,
                    path=path,
                    line_no=line_no,
                    rule_id=rule_id,
                    severity=sev,
                    message=message,
                    snippet=snippet,
                    context=context,
                )
            )

    return findings


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--catalog",
        action="append",
        dest="catalogs",
        metavar="NAME",
        help="Only scan apis/NAME (default: all catalogs)",
    )
    parser.add_argument(
        "--fail-on",
        choices=("error", "warn", "none"),
        default="error",
        help="Exit non-zero when findings at or above this severity exist",
    )
    parser.add_argument(
        "--apis-root",
        type=Path,
        default=None,
        help="Override apis directory (default: plasm-oss/apis)",
    )
    args = parser.parse_args()

    script = Path(__file__)
    apis_root = args.apis_root or repo_apis_root(script)
    if not apis_root.is_dir():
        print(f"check_catalog_description_hygiene: missing {apis_root}", file=sys.stderr)
        return 2

    catalogs = sorted(
        p.name
        for p in apis_root.iterdir()
        if p.is_dir() and (p / "domain.yaml").is_file()
    )
    if args.catalogs:
        wanted = set(args.catalogs)
        catalogs = [c for c in catalogs if c in wanted]
        missing = wanted - set(catalogs)
        for m in sorted(missing):
            print(f"check_catalog_description_hygiene: unknown catalog {m}", file=sys.stderr)

    all_findings: list[Finding] = []
    for catalog in catalogs:
        domain = apis_root / catalog / "domain.yaml"
        all_findings.extend(scan_domain_yaml(domain, catalog))

    severity_rank = {"error": 2, "warn": 1}
    fail_rank = severity_rank.get(args.fail_on, 0)

    for f in all_findings:
        if severity_rank[f.severity] < fail_rank:
            continue
        rel = f.path.relative_to(apis_root.parent)
        print(
            f"{rel}:{f.line_no}: [{f.severity}] {f.rule_id} ({f.context}): "
            f"{f.message} — {f.snippet}"
        )

    errors = sum(1 for f in all_findings if f.severity == "error")
    warns = sum(1 for f in all_findings if f.severity == "warn")
    print(
        f"check_catalog_description_hygiene: {len(catalogs)} catalogs, "
        f"{errors} error(s), {warns} warn(s)"
    )

    if args.fail_on == "none":
        return 0
    for f in all_findings:
        if severity_rank[f.severity] >= fail_rank:
            return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
