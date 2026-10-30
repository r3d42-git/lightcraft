#!/usr/bin/env bash
# Source after R3D_SOURCE, R3D_PROJECT and R3D_INSTANCE have been assigned.
export R3D_SCRATCH_ROOT="${R3D_SCRATCH_ROOT:-/Volumes/LightCraft Build}"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$R3D_SCRATCH_ROOT/lightcraft/target-$R3D_INSTANCE}"
export R3D_BACKUP_STAGING="${R3D_BACKUP_STAGING:-$R3D_SCRATCH_ROOT/lightcraft/backup-staging}"
# macOS supplies an inherited TMPDIR on the system volume. Use a dedicated
# override so that ordinary login environments need no manual cleanup first.
export R3D_TMPDIR="${R3D_TMPDIR:-$R3D_SCRATCH_ROOT/lightcraft/tmp-$R3D_INSTANCE}"
export TMPDIR="$R3D_TMPDIR"
export DIST="${DIST:-$R3D_SCRATCH_ROOT/lightcraft/dist-$R3D_INSTANCE}"

r3d_validate_scratch_paths() {
  python3 "$R3D_SOURCE/r3d/scratch-paths.py" "$R3D_SCRATCH_ROOT" "$R3D_PROJECT" "$@"
}

if ! r3d_validate_scratch_paths "$CARGO_TARGET_DIR" "$R3D_BACKUP_STAGING" "$TMPDIR" "$DIST"; then
  return 1
fi
mkdir -p "$TMPDIR" || return 1
