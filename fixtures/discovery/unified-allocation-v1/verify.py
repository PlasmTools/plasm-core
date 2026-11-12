"""Freeze once, then verify allocation experiment evidence without running models."""

import argparse
import hashlib
import json
from pathlib import Path
import subprocess


HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
COHORTS = {
    "trajectory-v4": ("trajectory-v4-harness-parity-20261030", 18, 6),
    "resource-breadth-v5": ("resource-breadth-v5-baseline-r2", 32, 0),
    "root-diversity-v1": ("root-diversity-v1-baseline", 8, 0),
}


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def evidence():
    files = {str(p.relative_to(ROOT)): digest(p) for p in (HERE / "PROTOCOL.md", Path(__file__).resolve())}
    for cohort, (baseline, count, failures) in COHORTS.items():
        lock = ROOT / "fixtures/discovery" / cohort / "LOCK.json"
        files[str(lock.relative_to(ROOT))] = digest(lock)
        for name, expected in json.loads(lock.read_text()).items():
            if digest(ROOT / name) != expected:
                raise ValueError(f"Inherited fixture changed: {name}")
            files[name] = expected
        directory = ROOT / "eval-reports" / baseline
        summary = json.loads((directory / "SUMMARY.json").read_text())
        if len(summary["cases"]) != count or len(summary["quality_failures"]) != failures:
            raise ValueError(f"Baseline cohort changed: {baseline}")
        if summary["provider_calls"] != 0:
            raise ValueError(f"Unexpected provider usage: {baseline}")
        for name in ("SUMMARY.json", "MANIFEST.json", "HASHES.json", "RESULTS.json"):
            path = directory / name
            files[str(path.relative_to(ROOT))] = digest(path)
    return dict(sorted(files.items()))


def source_identity():
    # Baseline identity is recorded separately: a candidate is expected to change it.
    files = {}
    for crate in ("plasm-agent-core", "plasm-core"):
        for path in sorted((ROOT / "crates" / crate / "src").rglob("*.rs")):
            files[str(path.relative_to(ROOT))] = digest(path)
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--freeze", action="store_true", help="Create LOCK.json exclusively; never replace it")
    parser.add_argument("--baseline-source", action="store_true", help="Also require unchanged baseline Rust sources")
    args = parser.parse_args()
    current = evidence()
    lock_path = HERE / "LOCK.json"
    if args.freeze:
        lock = {
            "status": "preregistered; candidate not implemented or measured",
            "baseline_head": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
            "baseline_has_wip": True,
            "cohorts": COHORTS,
            "evidence_sha256": current,
            "baseline_sources_sha256": source_identity(),
        }
        with lock_path.open("x") as stream:
            json.dump(lock, stream, indent=2, sort_keys=True)
            stream.write("\n")
    lock = json.loads(lock_path.read_text())
    if current != lock["evidence_sha256"]:
        raise ValueError("Preregistered evidence changed")
    if args.baseline_source and source_identity() != lock["baseline_sources_sha256"]:
        raise ValueError("Baseline source changed")
    print("Verified protocol, inherited fixtures and 58 baseline variants (6 gate failures); no evaluations run.")


if __name__ == "__main__":
    main()
