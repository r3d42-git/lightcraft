#!/usr/bin/env bash
# Source this file; no global shell configuration is changed.
R3D_SOURCE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
R3D_PROJECT="$(dirname "$R3D_SOURCE")"
# Each update checkout gets its own build and runtime; explicit caller overrides survive.
R3D_INSTANCE="${R3D_INSTANCE:-$(basename "$R3D_SOURCE")}"
export CARGO_HOME="${CARGO_HOME:-$R3D_PROJECT/.tools/cargo}"
export RUSTUP_HOME="${RUSTUP_HOME:-$R3D_PROJECT/.tools/rustup}"
source "$R3D_SOURCE/r3d/scratch-env.sh" || return 1
export PATH="$CARGO_HOME/bin:$PATH"
export LIGHTCRAFT_R3D_CONFIG_DIR="${LIGHTCRAFT_R3D_CONFIG_DIR-$R3D_PROJECT/test-runtime/$R3D_INSTANCE/config}"
export LIGHTCRAFT_R3D_LIBRARY="${LIGHTCRAFT_R3D_LIBRARY-$R3D_PROJECT/test-runtime/$R3D_INSTANCE/library}"
export R3D_INPUT_DIR="${R3D_INPUT_DIR:-$R3D_PROJECT/test-runtime/$R3D_INSTANCE/input}"
export R3D_CONTROL_PORT="${R3D_CONTROL_PORT:-18473}"
# Do not inherit the official application's optical database/helper.
export LIGHTCRAFT_LENSFUN_DIR="${LIGHTCRAFT_LENSFUN_DIR-$LIGHTCRAFT_R3D_CONFIG_DIR/optics/lensfun}"
export LIGHTCRAFT_R3D_EXIFTOOL="${LIGHTCRAFT_R3D_EXIFTOOL-$LIGHTCRAFT_R3D_CONFIG_DIR/optics/exiftool/exiftool}"
