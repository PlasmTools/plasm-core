"""Frozen breadth controls through the shared native measurement harness."""
import importlib.util
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('trajectory', HERE.parent / 'trajectory-v4/run.py')
runner = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runner)
original_verify = runner.verify


def verify():
    runner.HERE = HERE.parent / 'trajectory-v4'
    original_verify()
    runner.HERE = HERE
    return original_verify()


def files():
    return [HERE / name for name in ['cases.json', 'profiles.json', 'PROTOCOL.md', 'run.py']] + [
        HERE.parent / 'trajectory-v4/run.py',
        runner.ROOT / 'crates/plasm-agent-core/src/discovery_matcher/online/resource_growth.rs',
        *sorted((runner.ROOT / 'fixtures/schemas/discovery_trajectory_v4').glob('*.yaml')),
    ]


runner.HERE = HERE
runner.TEST_NAME = 'frozen_discovery_resource_breadth_v5'
runner.fixture_files = files
runner.verify = verify
runner.main()
