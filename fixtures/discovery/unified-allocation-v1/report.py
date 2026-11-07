"""Audit the single preregistered candidate; no model or evaluation calls."""
import hashlib
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[3]
HERE = Path(__file__).resolve().parent
PAIRS = [
    ("trajectory", "trajectory-v4-harness-parity-20261030"),
    ("breadth", "resource-breadth-v5-baseline-r2"),
    ("ordinary", "root-diversity-v1-baseline"),
]


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def audited(directory):
    for name, expected in json.loads((directory / "HASHES.json").read_text()).items():
        if sha(directory / name) != expected:
            raise ValueError(f"Artifact drift: {directory / name}")
    return json.loads((directory / "SUMMARY.json").read_text())


def key(row):
    return (row["id"], row["distractors"], row["rotating"])


def timing(row, prefix):
    values = row["timing"]["selection_us"]
    end = row["first_complete"] or len(values)
    return sum(values[:end] if prefix else values[end:]) / 1000


def main():
    rows = []
    totals = {}
    for cohort, baseline in PAIRS:
        old = audited(ROOT / "eval-reports" / baseline)
        new = audited(ROOT / "eval-reports" / f"unified-allocation-v1-{cohort}")
        old_rows = {key(r): r for r in old["cases"]}
        assert set(old_rows) == {key(r) for r in new["cases"]}
        assert old["provider_calls"] == new["provider_calls"] == 0
        pairs = [(old_rows[key(r)], r) for r in new["cases"]]
        totals[cohort] = {
            "baseline_gate_failures": len(old["quality_failures"]),
            "candidate_gate_failures": len(new["quality_failures"]),
            "baseline_prefix_bytes": sum(a["bytes_to_completion_or_horizon"] for a, _ in pairs),
            "candidate_prefix_bytes": sum(b["bytes_to_completion_or_horizon"] for _, b in pairs),
            "baseline_prefix_selection_ms": sum(timing(a, True) for a, _ in pairs),
            "candidate_prefix_selection_ms": sum(timing(b, True) for _, b in pairs),
            "baseline_post_completion_selection_ms": sum(timing(a, False) for a, _ in pairs),
            "candidate_post_completion_selection_ms": sum(timing(b, False) for _, b in pairs),
        }
        for a, b in pairs:
            rows.append({
                "cohort": cohort, "case": key(b),
                "baseline_complete": a["first_complete"], "candidate_complete": b["first_complete"],
                "recovered": not a["gate_pass"] and b["gate_pass"],
                "gate_regression": a["gate_pass"] and not b["gate_pass"],
                "completion_regression": a["first_complete"] is not None and (
                    b["first_complete"] is None or b["first_complete"] > a["first_complete"]),
                "request_regression": a["first_complete"] is not None and
                    b["requests_to_completion_or_horizon"] > a["requests_to_completion_or_horizon"],
                "prefix_bytes_delta": b["bytes_to_completion_or_horizon"] - a["bytes_to_completion_or_horizon"],
                "max_packet_bytes": b["max_packet_bytes"],
            })
    service = ROOT / "eval-reports/unified-allocation-v1-service"
    for name, expected in json.loads((service / "HASHES.json").read_text()).items():
        assert sha(service / name) == expected
    proofs = "7 passed; 0 failed;" in (service / "TEST.log").read_text()
    matcher_log = ROOT / "eval-reports/unified-allocation-v1-matcher.log"
    matcher = re.search(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored", matcher_log.read_text())
    assert matcher, "Missing matcher result"
    restored_log = ROOT / "eval-reports/unified-allocation-v1-restored-matcher.log"
    restored = re.search(r"test result: (\w+)\. (\d+) passed; (\d+) failed; (\d+) ignored", restored_log.read_text())
    assert restored and restored.group(3) == "0", "Restored matcher did not pass"
    gates = {
        "recovers_v4_failure": any(r["cohort"] == "trajectory" and r["recovered"] for r in rows),
        "no_gate_regression": not any(r["gate_regression"] for r in rows),
        "no_completion_or_request_regression": not any(r["completion_regression"] or r["request_regression"] for r in rows),
        "v4_prefix_bytes_decrease": totals["trajectory"]["candidate_prefix_bytes"] < totals["trajectory"]["baseline_prefix_bytes"],
        "packet_budget": all(r["max_packet_bytes"] <= 16000 for r in rows),
        "seven_service_proofs": proofs,
        "matcher_invariants": matcher.group(3) == "0",
    }
    out = ROOT / "eval-reports/unified-allocation-v1"
    out.mkdir(exist_ok=True)
    audit = {"mechanical_adoption_gates": gates, "all_mechanical_gates_pass": all(gates.values()),
             "totals": totals, "rows": rows, "provider_calls": 0,
             "matcher": {"passed": int(matcher.group(2)), "failed": int(matcher.group(3)), "ignored": int(matcher.group(4)), "log_sha256": sha(matcher_log)},
             "restored_matcher": {"passed": int(restored.group(2)), "failed": int(restored.group(3)), "ignored": int(restored.group(4)), "log_sha256": sha(restored_log)},
             "timing_limit": "Single diagnostic runs; build/test queue and host conditions differ. No speedup claim.",
             "scope": "Consumed development fixtures; no holdout or full-agent readiness claim."}
    (out / "AUDIT.json").write_text(json.dumps(audit, indent=2) + "\n")
    lines = ["# Unified allocation A1", "", audit["scope"], "", "## Frozen adoption gates", ""]
    lines += [f"- {name}: {'PASS' if passed else 'FAIL'}" for name, passed in gates.items()]
    lines += ["", "Candidate rejected. Baseline allocation and v13 native/TypeScript policy restored.",
              f"Restored matcher verification: {restored.group(2)} passed, {restored.group(3)} failed, {restored.group(4)} ignored.",
              "", "The broader matcher check passed 133 tests and failed two: supplier nomination after required acquisition, and acquisition-cycle deferral classification. The candidate filters SuppliesInput nominations but does not reconstruct those originating inside required ancestry; it also omits the empty-alternative cycle receipt. These are implementation/invariant failures, so this run does not establish that every correct unified allocator would fail. No second arm or post-result fix was run."]
    lines += ["", "## Every variant", "", "| Cohort / case | Growth | Rotated | Completion before → after | Prefix byte delta |", "|---|---:|---|---|---:|"]
    for r in rows:
        name, growth, rotated = r["case"]
        lines.append(f"| {r['cohort']} / {name} | {growth} | {rotated} | {r['baseline_complete']} → {r['candidate_complete']} | {r['prefix_bytes_delta']:+} |")
    lines += ["", "## Construction cost", "", audit["timing_limit"], "", "```json", json.dumps(totals, indent=2), "```", "", "Zero provider calls. The unchanged native harness checks packet integrity and insertion parity. Service proofs check ownership and frontier continuation; they do not measure live judgment accuracy."]
    (out / "REPORT.md").write_text("\n".join(lines) + "\n")
    (out / "HASHES.json").write_text(json.dumps({p.name: sha(p) for p in out.iterdir() if p.is_file() and p.name != "HASHES.json"}, indent=2) + "\n")
    print(json.dumps({"output": str(out), "gates": gates}))


if __name__ == "__main__":
    main()
