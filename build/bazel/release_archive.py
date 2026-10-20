"""Package declared release inputs with stable archive headers and checksums."""

import gzip
import hashlib
import json
from pathlib import Path
import sys
import tarfile


def archive(output, entries):
    with Path(output).open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.PAX_FORMAT) as tar:
                for name, source, executable in sorted(entries):
                    source = Path(source)
                    info = tarfile.TarInfo(name)
                    info.size = source.stat().st_size
                    info.mode = 0o755 if executable else 0o644
                    info.mtime = info.uid = info.gid = 0
                    with source.open("rb") as data:
                        tar.addfile(info, data)


def main():
    manifest = json.loads(Path(sys.argv[1]).read_text())
    catalogs = Path(manifest["catalogs"])
    entries = [("catalogs/" + path.relative_to(catalogs).as_posix(), path, False)
               for path in catalogs.rglob("*") if path.is_file()]
    if not any(name.endswith(".cgs.json") for name, _, _ in entries):
        raise ValueError("Appliance archive requires packed catalogs")
    entries += [("plasm-server", manifest["server"], True),
                ("monty", manifest["monty"], True),
                ("monty-LICENSE.txt", manifest["license"], False)]
    appliance, client, cgs = manifest["archives"]
    archive(appliance, entries)
    archive(client, [("plasm", manifest["client"], True)])
    archive(cgs, [("plasm-cgs", manifest["cgs"], True)])
    sums = []
    for output in sorted(manifest["archives"]):
        with Path(output).open("rb") as data:
            digest = hashlib.file_digest(data, "sha256").hexdigest()
        sums.append(f"{digest}  {Path(output).name}\n")
    Path(manifest["checksums"]).write_text("".join(sums))


if __name__ == "__main__":
    main()
