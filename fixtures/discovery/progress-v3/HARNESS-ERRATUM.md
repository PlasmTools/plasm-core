# Source provenance correction

The original runner omitted `discovery_matcher.rs` from its source manifest.
The corrected runner includes that central module as well as its child modules.
The original runner and lock are preserved under `original-harness/` for audit;
they are historical source, not supported execution entry points.

Only the runner digest in `LOCK.json` changes. Protocol, fixture inputs, generator
and labels are unchanged. This correction does not retroactively strengthen prior
results, and no evaluation was rerun when making it.
