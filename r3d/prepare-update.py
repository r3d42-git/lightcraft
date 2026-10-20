#!/usr/bin/env python3
"""Prepare an already fetched upstream revision plus our committed changes.

Creates a fresh worktree; never installs a build or opens a photo library.
On a conflict, the worktree remains for review. No automatic conflict resolution.
"""
import argparse
import json
from pathlib import Path
import subprocess


ROOT = Path(__file__).resolve().parents[1]


def git(*args):
    return subprocess.check_output(["git", "-C", str(ROOT), *args], text=True).strip()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("revision", help="Already fetched upstream tag or commit")
    parser.add_argument("--destination", required=True, type=Path)
    parser.add_argument("--prepare", action="store_true", help="Default is a read-only preview")
    args = parser.parse_args()
    base = json.loads((ROOT / "r3d/upstream-base.json").read_text())["commit"]
    target = git("rev-parse", "--verify", args.revision + "^{commit}")
    head = git("rev-parse", "HEAD")
    if git("status", "--porcelain"):
        raise SystemExit("Commit and verify the current changes first; working tree is not clean.")
    subprocess.run(["git", "-C", str(ROOT), "merge-base", "--is-ancestor", base, head], check=True)
    if git("rev-list", "--merges", f"{base}..{head}"):
        raise SystemExit("Merge commits in the patch series require manual integration; no worktree created.")
    commits = git("rev-list", "--reverse", "--no-merges", f"{base}..{head}").splitlines()
    report = {"old_base": base, "new_base": target, "source_head": head,
              "commits": commits, "destination": str(args.destination.resolve())}
    print(json.dumps(report, indent=2))
    if not args.prepare:
        return
    if args.destination.exists():
        raise SystemExit("Destination already exists; choose a new directory.")
    subprocess.run(["git", "-C", str(ROOT), "worktree", "add", "--detach", str(args.destination.resolve()), target], check=True)
    for commit in commits:
        subprocess.run(["git", "-C", str(args.destination), "cherry-pick", commit], check=True)
    # Updating this manifest is intentionally left uncommitted for review.
    manifest_path = args.destination / "r3d/upstream-base.json"
    manifest = json.loads(manifest_path.read_text())
    manifest.update({"release": args.revision, "commit": target})
    manifest_path.write_text(json.dumps(manifest, indent=2) + "\n")
    print("Prepared only. Review INTEGRATION.md, run cargo xtask ci and image regressions; then build and verify the app.")


if __name__ == "__main__":
    main()
