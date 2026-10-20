"""Describe Bazel-built binaries to nextest without invoking Cargo compilation."""

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def matches_target(target, kind, name):
    # Cargo describes library outputs by crate type (e.g. NAPI's cdylib),
    # while nextest classifies their unit-test harnesses uniformly as lib.
    kinds = {"lib", "rlib", "dylib", "cdylib", "staticlib", "proc-macro"} if kind == "lib" else {kind}
    return target["name"] == name and bool(kinds.intersection(target["kind"]))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--binary", action="append", required=True)
    parser.add_argument("--runtime-tool", action="append", default=[])
    parser.add_argument("--runtime-file", action="append", default=[])
    parser.add_argument("--nextest", type=Path, required=True)
    parser.add_argument("--cargo", type=Path, required=True)
    parser.add_argument("--rustc", type=Path, required=True)
    parser.add_argument("--config", type=Path, required=True)
    args, extra = parser.parse_known_args()
    with tempfile.TemporaryDirectory(prefix="plasm-nextest-") as directory:
        return run(args, extra, Path(directory))


def run(args, extra, directory):
    # Bazel releases its output-base lock before `bazel run` executes. Preserve
    # this run's executable bytes before a subsequent build replaces outputs.
    def snapshot(path, name):
        destination = directory / name
        shutil.copy2(Path(path).resolve(), destination)
        return str(destination)

    test_paths = {}
    for index, item in enumerate(args.binary):
        identity, path = item.split("=", 1)
        test_paths[identity] = snapshot(path, f"test-{index}")
    manifest = args.manifest.resolve()
    environment = {
        key: value for key, value in os.environ.items()
        if not (key.startswith("CARGO_TARGET_") and key.endswith("_RUNNER"))
        and key not in {"RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER"}
    }
    environment["RUSTC"] = str(args.rustc.resolve())
    for index, item in enumerate(args.runtime_tool):
        variable, path = item.split("=", 1)
        environment[variable] = snapshot(path, f"tool-{index}")
    for item in args.runtime_file:
        variable, path = item.split("=", 1)
        environment[variable] = str(Path(path).resolve(strict=True))
    metadata = json.loads(subprocess.check_output([
        str(args.cargo.resolve()), "metadata", "--locked", "--offline", "--no-deps",
        "--format-version", "1", "--manifest-path", str(manifest),
    ], env=environment))
    environment["CARGO"] = str(args.cargo.resolve())
    environment["INSTA_WORKSPACE_ROOT"] = metadata["workspace_root"]
    compiler_version = subprocess.check_output(
        [str(args.rustc.resolve()), "-vV"], text=True, env=environment,
    )
    host = next(line.removeprefix("host: ") for line in compiler_version.splitlines()
                if line.startswith("host: "))
    libdir = subprocess.check_output(
        [str(args.rustc.resolve()), "--print", "target-libdir"], text=True, env=environment,
    ).strip()
    packages = {p["name"]: p for p in metadata["packages"]}
    binaries = {}
    for item in args.binary:
        identity, path = item.split("=", 1)
        package, kind, name = identity.split("::")
        package_id = packages[package]["id"]
        if not any(matches_target(t, kind, name) for t in packages[package]["targets"]):
            raise ValueError(f"Bazel test {identity} has no matching Cargo target")
        binary_id = package if kind == "lib" else f"{package}::{kind}/{name}"
        binaries[binary_id] = {
            "binary-id": binary_id,
            "binary-name": name,
            "package-id": package_id,
            "kind": kind,
            "binary-path": test_paths[identity],
            "build-platform": "target",
        }
    binary_metadata = {
        "rust-build-meta": {
            "target-directory": metadata["target_directory"],
            "base-output-directories": [],
            "non-test-binaries": {},
            "build-script-out-dirs": {},
            "linked-paths": [],
            "target-platform": None,
            "platforms": {
                "host": {
                    "platform": {"triple": host, "target-features": []},
                    "libdir": {"status": "available", "path": libdir},
                },
                "targets": [],
            },
        },
        "rust-binaries": binaries,
    }
    cargo_file = directory / "cargo.json"
    binary_file = directory / "binaries.json"
    cargo_file.write_text(json.dumps(metadata))
    binary_file.write_text(json.dumps(binary_metadata))
    config = args.config.resolve()
    if output_directory := environment.get("TEST_UNDECLARED_OUTPUTS_DIR"):
        # Keep CI reports with this Bazel test, including when separate output
        # bases execute suites concurrently against the same source checkout.
        config = directory / "nextest.toml"
        store = str(Path(output_directory).resolve() / "nextest")
        config.write_text(args.config.read_text() + "\n[store]\ndir = " + json.dumps(store) + "\n")
    result = subprocess.run([
        str(args.nextest.resolve()), "nextest", "run",
        "--cargo-metadata", str(cargo_file),
        "--binaries-metadata", str(binary_file),
        "--config-file", str(config),
        *extra,
    ], cwd=metadata["workspace_root"], env=environment)
    return result.returncode


if __name__ == "__main__":
    raise SystemExit(main())
