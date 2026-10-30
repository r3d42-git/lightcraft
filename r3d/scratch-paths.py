#!/usr/bin/env python3
"""Fail closed before local build scripts create temporary data."""
from pathlib import Path
import plistlib
import subprocess
import sys


def disk_info(path):
    # diskutil accepts volume mountpoints/devices, not arbitrary directories.
    # df also resolves macOS firmlinks (e.g. /Users on the Data volume).
    filesystem = subprocess.check_output(
        ["df", "-P", str(path)], stderr=subprocess.PIPE, text=True)
    lines = filesystem.splitlines()
    if len(lines) < 2 or not lines[-1].split():
        raise ValueError(f"cannot determine the volume containing {path}")
    device = lines[-1].split()[0]
    if not device.startswith("/dev/"):
        raise ValueError(f"not a local disk volume: {path}")
    return plistlib.loads(subprocess.check_output(
        ["diskutil", "info", "-plist", device], stderr=subprocess.PIPE))


def validate(root, project, paths):
    root = Path(root).resolve()
    if not root.is_dir():
        raise ValueError(f"scratch volume is unavailable: {root}")
    info = disk_info(root)
    if not info.get("MountPoint") or Path(info["MountPoint"]).resolve() != root:
        raise ValueError(f"scratch root must be an actual volume mount point: {root}")
    if info.get("FilesystemType", info.get("TypeBundle")) != "apfs":
        raise ValueError("scratch volume must use APFS")
    device = info.get("DeviceIdentifier")
    if not device or device in {
        disk_info(Path("/"))["DeviceIdentifier"],
        disk_info(Path.home())["DeviceIdentifier"],
        disk_info(Path(project).resolve())["DeviceIdentifier"],
    }:
        raise ValueError("scratch must be a separate volume from macOS, user data and the project")
    exclusion = subprocess.check_output(
        ["tmutil", "isexcluded", str(root)], stderr=subprocess.PIPE, text=True)
    if not exclusion.strip().startswith("[Excluded]"):
        raise ValueError(f"scratch volume is not excluded from Time Machine: {root}")
    for raw in paths:
        path = Path(raw)
        if not path.is_absolute():
            raise ValueError(f"temporary path must be absolute: {raw}")
        # resolve() follows existing symlinks, including ancestors of new paths.
        path = path.resolve()
        if path == root or root not in path.parents:
            raise ValueError(f"temporary path must be inside {root}: {raw}")
        ancestor = path
        while not ancestor.exists():
            ancestor = ancestor.parent
        if disk_info(ancestor).get("DeviceIdentifier") != device:
            raise ValueError(f"temporary path crosses onto another volume: {raw}")


if __name__ == "__main__":
    try:
        validate(sys.argv[1], sys.argv[2], sys.argv[3:])
    except (ValueError, KeyError, OSError, subprocess.CalledProcessError,
            plistlib.InvalidFileException) as exc:
        print(f"LightCraft scratch check failed: {exc}\n"
              "Mount the separate APFS build volume, exclude the volume from "
              "Time Machine, and source r3d/dev-env.sh again. See r3d/SCRATCH.md.",
              file=sys.stderr)
        sys.exit(1)
