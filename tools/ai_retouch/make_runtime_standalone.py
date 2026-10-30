"""Developer packaging step: detach the private runtime from a uv-installed Python.

Copies the already installed standalone CPython and its standard library; keeps
the runtime's pinned site-packages and model files. No downloads or global edits.
"""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", required=True, type=Path)
    parser.add_argument("--python-root", required=True, type=Path)
    args = parser.parse_args()
    root = args.runtime.resolve(strict=True)
    source = args.python_root.resolve(strict=True)
    version = "python3.11"
    if root == source or source in root.parents or root in source.parents:
        raise ValueError("Source and runtime must be separate directories")
    if not (root / "worker.py").is_file() or not (root / "lib" / version / "site-packages").is_dir():
        raise ValueError("Expected an existing private AI runtime")
    if not (source / "lib" / version / "os.py").is_file():
        raise ValueError("Expected standalone CPython 3.11 with its standard library")
    binary = source / "bin" / version
    if not binary.is_file():
        raise ValueError("Missing standalone Python binary")
    # The existing site-packages contain our tested pins, not the base Python's
    # bootstrap pip/setuptools. Preserve them exactly.
    shutil.copytree(source / "lib", root / "lib", dirs_exist_ok=True,
                    symlinks=True, ignore=shutil.ignore_patterns("site-packages", "__pycache__"))
    destination = root / "bin" / version
    temporary = destination.with_suffix(".standalone")
    shutil.copy2(binary, temporary)
    temporary.replace(destination)  # replaces the link; never writes through it
    for name in ("python", "python3"):
        link = root / "bin" / name
        if link.is_symlink():
            link.unlink()
        elif link.exists():
            raise ValueError(f"Unexpected non-symlink launcher: {link}")
        link.symlink_to(version)
    venv = root / "pyvenv.cfg"
    previous = venv.read_text() if venv.exists() else None
    if venv.exists():
        venv.unlink()
    # No pyvenv.cfg: CPython discovers the adjacent standard library and uses
    # this runtime as sys.prefix, including its unchanged site-packages.
    check = subprocess.check_output([str(destination), "-I", "-c",
        "import sys,json,ssl,sqlite3,numpy,torch; "
        "print(json.dumps({'prefix':sys.prefix,'base_prefix':sys.base_prefix,"
        "'numpy':numpy.__version__,'torch':torch.__version__}))"], text=True)
    info = json.loads(check)
    if Path(info["prefix"]).resolve() != root or Path(info["base_prefix"]).resolve() != root:
        raise RuntimeError("The runtime still depends on an external Python prefix")
    manifest = {"python_source": str(source), "python_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
                "previous_venv_configuration": previous, "validation": info}
    (root / "standalone-runtime.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(check.strip())


if __name__ == "__main__":
    main()
