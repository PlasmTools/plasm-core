"""Reject drift between Bazel's worker metadata and the vendored Cargo package."""

from pathlib import Path
import argparse
import tomllib


def load(path):
    return tomllib.loads(path.read_text())


def normalized_dependencies(dependencies, inherited, directory):
    result = {}
    for name, value in dependencies.items():
        value = {"version": value} if isinstance(value, str) else dict(value)
        if value.pop("workspace", False):
            base = inherited[name]
            base = {"version": base} if isinstance(base, str) else dict(base)
            if "features" in value:
                value["features"] = base.get("features", []) + value["features"]
            base.update(value)
            value = base
        if "path" in value:
            value["path"] = str((directory / value["path"]).resolve())
        if "features" in value:
            value["features"] = sorted(set(value["features"]))
        result[name] = value
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", type=Path, default=Path(__file__).parent / "Cargo.toml")
    metadata_dir = parser.parse_args().manifest.absolute().parent
    vendor = metadata_dir / "vendor/monty"
    workspace = load(vendor / "Cargo.toml")
    worker = load(vendor / "crates/monty-runtime/Cargo.toml")
    metadata = load(metadata_dir / "Cargo.toml")
    expected = normalized_dependencies(
        worker["dependencies"], workspace["workspace"]["dependencies"], vendor,
    )
    actual = normalized_dependencies(metadata["dependencies"], {}, metadata_dir)
    assert actual == expected, "worker dependency metadata differs from vendored Monty"
    assert metadata["features"] == worker["features"], "worker features differ"
    for field in ("version", "edition"):
        assert metadata["package"][field] == workspace["workspace"]["package"][field], field
    expected_patches = normalized_dependencies(workspace["patch"]["crates-io"], {}, vendor)
    actual_patches = normalized_dependencies(metadata["patch"]["crates-io"], {}, metadata_dir)
    assert actual_patches == expected_patches, "worker patches differ"
    original = {(p["name"], p["version"]) for p in load(vendor / "Cargo.lock")["package"]}
    resolved = {(p["name"], p["version"]) for p in load(metadata_dir / "Cargo.lock")["package"]}
    assert resolved <= original, f"worker versions changed: {resolved - original}"
    print(f"Monty worker metadata matches; {len(resolved)} locked package versions preserved")


if __name__ == "__main__":
    main()
