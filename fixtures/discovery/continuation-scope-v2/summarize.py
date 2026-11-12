"""Score native diagnostic stdout without disguising quality failures as test passes."""
import argparse
import json
import itertools
from pathlib import Path


def validate_matrix(rows):
    keys = ("oversized", "changed", "rotating", "shared_exploration")
    expected = set(itertools.product((False, True), repeat=4))
    actual = [tuple(row[key] for key in keys) for row in rows]
    if len(actual) != 16 or set(actual) != expected:
        raise ValueError("Require all 16 unique factorial cells")
    for row in rows:
        if any(type(row[key]) is not bool for key in keys):
            raise ValueError("Require boolean factors")
        if [turn["turn"] for turn in row["turns"]] != list(range(1, 7)):
            raise ValueError("Require six ordered turns per cell")


def summarize(rows):
    results = []
    for row in rows:
        turns = row["turns"]
        windows = [turns[:3], turns[3:]] if row["changed"] else [turns]
        first = [next((t["turn"] for t in window if t["target_relevant"]), None)
                 for window in windows]
        if row["oversized"]:
            passed = first[0] is None and (not row["changed"] or first[1] is not None)
        elif row["changed"]:
            passed = first[0] is not None and first[1] is not None and first[1] <= 4
        else:
            passed = first[0] is not None and first[0] <= 4
        results.append({**{k: v for k, v in row.items() if k != "turns"},
                        "first_target_judgment": first, "gate_pass": passed,
                        "unique": turns[-1]["unique"],
                        "repeated": sum(t["repeated"] for t in turns),
                        "max_bytes": max(t["bytes"] for t in turns)})
    return {"development_only": True, "provider_calls": 0, "cells": results,
            "quality_failures": sum(not row["gate_pass"] for row in results)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("log", type=Path)
    parser.add_argument("--quality", action="store_true", help="Exit nonzero on quality failures")
    args = parser.parse_args()
    marker = "CONTINUATION_DIAGNOSTIC "
    lines = [line.split(marker, 1)[1] for line in args.log.read_text().splitlines() if marker in line]
    if len(lines) != 1:
        raise ValueError("Require exactly one complete native diagnostic result")
    rows = json.loads(lines[0])
    validate_matrix(rows)
    result = summarize(rows)
    print(json.dumps(result, indent=2))
    if args.quality:
        raise SystemExit(bool(result["quality_failures"]))
