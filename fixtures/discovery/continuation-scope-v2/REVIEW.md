# Continuation scope diagnostic provenance

This diagnostic originally ran from the already occupied `continuation-v1`
directory on 2026-11-01. Review moved these new files into this separate directory
and restored the historical continuation-v1 protocol unchanged. The original lock
is preserved as `eval-reports/continuation-v1-20261101/ORIGINAL-LOCK.json`.
The lock in this directory identifies the reviewed files, not the original run.

Sixteen cells completed with four quality-gate failures. Native test success
establishes diagnostic integrity, not a passed discovery-quality gate. The CLI
now rejects incomplete matrices before scoring. No native or provider rerun was
needed for this post-run reporting change.
