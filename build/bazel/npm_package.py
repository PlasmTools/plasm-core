"""Package declared npm sources and native artifacts without mutating sources."""

import gzip
import fnmatch
import hashlib
import io
import json
from pathlib import Path
import re
import sys
import tarfile


def main():
    spec = json.loads(Path(sys.argv[1]).read_text())
    package = json.loads(Path(spec["manifest"]).read_text())
    original_version = package["version"]
    version = spec["version"] or package["version"]
    if not re.fullmatch(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?(?:\+[0-9A-Za-z.-]+)?", version):
        raise ValueError(f"Invalid release version: {version}")
    package["version"] = version
    entries = dict(spec["files"])
    platform = spec["platform"]
    if platform:
        os_name, cpu, *libc = platform.split("-")
        binary = f"plasm-engine.{platform}.node"
        package = {
            "name": f"@plasm_lang/engine-{platform}",
            "version": version,
            "license": package["license"],
            "repository": package["repository"],
            "main": binary,
            "os": [os_name],
            "cpu": [cpu],
            "files": [binary, "monty", "monty-LICENSE.txt", "LICENSE-MIT", "LICENSE-APACHE"],
        }
        if libc:
            package["libc"] = ["glibc"]
        entries[binary] = spec["binding"]
        entries["monty"] = spec["monty"]
        entries["monty-LICENSE.txt"] = spec["monty_license"]
    elif package["name"] == "@plasm_lang/engine":
        package["optionalDependencies"] = {
            f"@plasm_lang/engine-{target}": version
            for target in ["darwin-arm64", "linux-x64-gnu"]
        }
        # Published packages contain completed artifacts, not build hooks.
        package.pop("scripts", None)
    if package["name"] == "@plasm_lang/vercel-agent":
        engine_version = spec["version"] or json.loads(Path(spec["engine_manifest"]).read_text())["version"]
        package.setdefault("dependencies", {})["@plasm_lang/engine"] = engine_version
    if "ignore_file" in spec:
        for pattern in Path(spec["ignore_file"]).read_text().splitlines():
            pattern = pattern.strip()
            if not pattern or pattern.startswith("#"):
                continue
            if pattern.startswith("!"):
                raise ValueError("npm packaging requires explicit support for ignore negations")
            pattern = pattern.strip("/")
            entries = {
                name: path for name, path in entries.items()
                if not (fnmatch.fnmatchcase(name, pattern) or name.startswith(pattern + "/")
                        or ("/" not in pattern and any(fnmatch.fnmatchcase(part, pattern) for part in Path(name).parts)))
            }
    payloads = {name: Path(path).read_bytes() for name, path in entries.items()}
    if not platform and package["name"] == "@plasm_lang/engine":
        loader = payloads["index.js"].decode()
        if f"bindingPackageVersion !== '{original_version}'" not in loader:
            raise ValueError("Native loader version checks do not match the source manifest")
        payloads["index.js"] = loader.replace(original_version, version).encode()
    payloads["package.json"] = (json.dumps(package, indent=2) + "\n").encode()
    with open(spec["output"], "wb") as raw:
        with gzip.GzipFile(fileobj=raw, filename="", mode="wb", mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as archive:
                for name, data in sorted(payloads.items()):
                    path = Path(name)
                    if path.is_absolute() or ".." in path.parts:
                        raise ValueError(f"Package path escapes archive: {name}")
                    info = tarfile.TarInfo("package/" + name)
                    info.mode = 0o755 if name == "monty" or name.startswith("bin/") else 0o644
                    info.size = len(data)
                    archive.addfile(info, io.BytesIO(data))
    digest = hashlib.sha256(Path(spec["output"]).read_bytes()).hexdigest()
    Path(spec["checksum"]).write_text(f"{digest}  {Path(spec['output']).name}\n")


if __name__ == "__main__":
    main()
