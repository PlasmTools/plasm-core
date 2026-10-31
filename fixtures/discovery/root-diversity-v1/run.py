"""Use the native trajectory runner for separately frozen ordinary controls."""
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


runner.HERE = HERE
runner.TEST_NAME = 'frozen_discovery_root_counter_controls'
runner.verify = verify
runner.main()
