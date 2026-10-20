#!/usr/bin/env python3
"""Install pinned external optical data/helper in an explicitly chosen test root.

No system installation, user profile lookup, photo access or source-code vendoring.
Network downloads occur only with --download; archives are always SHA-256 checked.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import shutil
import subprocess
import tarfile
import tempfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--cache", required=True, type=Path)
    parser.add_argument("--download", action="store_true")
    args = parser.parse_args()
    spec_path = Path(__file__).with_name("optics-sources.json")
    spec = json.loads(spec_path.read_text())
    target = args.root.resolve()
    manifest = target / "sources.json"
    if target.exists():
        if manifest.is_file() and json.loads(manifest.read_text()) == spec:
            print(f"Already prepared: {target}")
            return
        raise SystemExit(f"Refusing to replace an existing optical installation: {target}")
    args.cache.mkdir(parents=True, exist_ok=True)
    target.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix=".optics-", dir=target.parent) as staging:
        stage = Path(staging) / "optics"
        stage.mkdir()
        for name in ("lensfun", "exiftool"):
            source = spec[name]
            archive = args.cache / source["archive"]
            if not archive.exists():
                if not args.download:
                    raise SystemExit(f"Missing {archive}; rerun with --download to fetch pinned sources")
                part = archive.with_suffix(archive.suffix + ".part")
                subprocess.run(["curl", "--fail", "--location", "--proto", "=https", "--tlsv1.2",
                                "--output", str(part), source["url"]], check=True)
                part.rename(archive)
            digest = hashlib.sha256(archive.read_bytes()).hexdigest()
            if digest != source["sha256"]:
                raise SystemExit(f"Checksum mismatch for {archive}; no data installed")
            destination = stage / name
            destination.mkdir()
            total = 0
            with tarfile.open(archive, "r:gz") as bundle:
                for entry in bundle:
                    if not entry.name.startswith(source["prefix"]) or not entry.isfile():
                        continue
                    relative = PurePosixPath(entry.name[len(source["prefix"]):])
                    if relative.is_absolute() or ".." in relative.parts or not relative.parts:
                        raise SystemExit("Unsafe archive path")
                    # Only database files are extracted from Lensfun, never its code.
                    if name == "lensfun" and relative.suffix != ".xml":
                        continue
                    total += entry.size
                    if total > 200 * 1024 * 1024 or entry.size > 32 * 1024 * 1024:
                        raise SystemExit("Optics archive exceeds extraction limits")
                    output = destination.joinpath(*relative.parts)
                    output.parent.mkdir(parents=True, exist_ok=True)
                    stream = bundle.extractfile(entry)
                    if stream is None:
                        raise SystemExit(f"Cannot extract {entry.name}")
                    with stream, output.open("wb") as out:
                        shutil.copyfileobj(stream, out)
            if not any(destination.iterdir()):
                raise SystemExit(f"No expected files in {archive}")
        (stage / "exiftool/exiftool").chmod(0o755)
        (stage / "sources.json").write_text(json.dumps(spec, indent=2) + "\n")
        stage.rename(target)
    print(f"Prepared isolated optics data: {target}")


if __name__ == "__main__":
    main()
