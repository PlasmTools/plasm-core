"""Offline harness-integrity checks: no compilation, judgment or provider calls."""
import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import unittest

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


class HarnessIntegrityTests(unittest.TestCase):
    def test_progress_freeze_preserves_inputs_and_includes_central_matcher(self):
        path = HERE / 'progress-v3/run.py'
        spec = importlib.util.spec_from_file_location('progress_runner', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        current = module.verify()
        original = json.loads((path.parent / 'original-harness/LOCK.json').read_text())
        runner = 'fixtures/discovery/progress-v3/run.py'
        self.assertEqual({k: v for k, v in current.items() if k != runner},
                         {k: v for k, v in original.items() if k != runner})
        archived = path.parent / 'original-harness/run.py'
        self.assertEqual(hashlib.sha256(archived.read_bytes()).hexdigest(), original[runner])
        self.assertIn(ROOT / 'crates/plasm-agent-core/src/discovery_matcher.rs', module.source_paths())

    def test_retired_live_runners_refuse_before_build_or_dispatch(self):
        for version in ['live-v2', 'live-v2.1']:
            for command in ['build', 'oracle', 'live']:
                result = subprocess.run([sys.executable, str(HERE / version / 'run.py'), command],
                                        capture_output=True, text=True)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Retired runner:', result.stderr)

    def test_archived_probes_are_not_cargo_example_targets(self):
        for name in ['binding_scope_probe.rs', 'canonical_discovery_probe.rs']:
            self.assertFalse((ROOT / 'crates/plasm-agent-core/examples' / name).exists())
            self.assertTrue((HERE / 'archived-probes' / name).is_file())


if __name__ == '__main__':
    unittest.main()
