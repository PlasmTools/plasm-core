#!/usr/bin/env python3
"""Reject Rust Debug formatting in shared agent error definitions."""
from pathlib import Path
import re
import sys

ROOT = Path(__file__).resolve().parents[2]
CRATES = ("plasm-core", "plasm-cml", "plasm-compile", "plasm-runtime", "plasm-agent-core")
violations = []
for crate in CRATES:
    for path in sorted((ROOT / "crates" / crate / "src").rglob("*.rs")):
        if "tests" in path.parts or "tests" in path.stem or path.stem == "ref_model":
            continue
        source = path.read_text()
        # Error attributes can span lines; braces and format flags are inside
        # the first Rust string literal, not the remainder of the enum variant.
        for match in re.finditer(r'#\[error\(\s*"((?:\\.|[^"\\])*)"', source, re.S):
            if re.search(r'\{[^{}]*:[^{}]*\?\}', match[1]):
                line = source.count("\n", 0, match.start()) + 1
                violations.append(f"{path.relative_to(ROOT)}:{line}")
if violations:
    print("Agent errors require semantic corrections, not Debug formatting:")
    print("\n".join(violations))
    sys.exit(1)
print("agent-error-presentation: error definitions contain no Debug formatting")
