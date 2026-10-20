#!/usr/bin/env bash
# Source this file; no global shell configuration is changed.
R3D_SOURCE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
R3D_PROJECT="$(dirname "$R3D_SOURCE")"
export CARGO_HOME="$R3D_PROJECT/.tools/cargo"
export RUSTUP_HOME="$R3D_PROJECT/.tools/rustup"
export CARGO_TARGET_DIR="$R3D_PROJECT/.tools/target"
export PATH="$CARGO_HOME/bin:$PATH"
export LIGHTCRAFT_R3D_CONFIG_DIR="$R3D_PROJECT/test-runtime/config"
export LIGHTCRAFT_R3D_LIBRARY="$R3D_PROJECT/test-runtime/library"
# Do not inherit a different process's optical database/helper.
export LIGHTCRAFT_LENSFUN_DIR="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/lensfun"
export LIGHTCRAFT_R3D_EXIFTOOL="$LIGHTCRAFT_R3D_CONFIG_DIR/optics/exiftool/exiftool"
