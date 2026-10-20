#!/usr/bin/env bash
# Build, sign and (optionally) notarize the macOS release artifacts:
#
#   $DIST/lightcraft-<version>-macos-<arch>.dmg          LightCraft R3D.app on a drag-to-Applications DMG
#   $DIST/lightcraft-cli-<version>-macos-<arch>.zip      the headless CLI
#
# Usage: packaging/macos/package.sh [--arch universal|aarch64|x86_64] [--skip-build] [--dev-app]
# Fonts: use CRAFT_FONTS_DIR or ../craft-fonts; download the pinned checkout if absent.
#
# Signing (env):
#   MACOS_SIGN_IDENTITY   codesign identity (name or SHA-1). Default "-" = ad-hoc (local testing;
#                         Gatekeeper will reject the result on other Macs). In CI, import-cert.sh sets it.
#   MACOS_KEYCHAIN        keychain holding the identity (optional)
# Notarization: MACOS_NOTARY_PROFILE defaults to ToolBoxS; requires a real identity.
# Legacy credentials (when explicitly disabling the profile):
#   APPLE_ID, APPLE_PASSWORD (app-specific password), APPLE_TEAM_ID
set -euo pipefail
# shellcheck source=../env.sh
. "$(dirname "${BASH_SOURCE[0]}")/../env.sh"
HERE="$ROOT/packaging/macos"

ARCH=universal
SKIP_BUILD=0
DEV_APP=0
while [ $# -gt 0 ]; do
  case "$1" in
    --arch) ARCH="$2"; shift 2 ;;
    --skip-build) SKIP_BUILD=1; shift ;;
    --dev-app) DEV_APP=1; shift ;;
    -h | --help) sed -n '2,16p' "$0"; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$ARCH" in
  universal) TARGETS=(aarch64-apple-darwin x86_64-apple-darwin) ;;
  aarch64) TARGETS=(aarch64-apple-darwin) ;;
  x86_64) TARGETS=(x86_64-apple-darwin) ;;
  *) echo "unknown --arch $ARCH" >&2; exit 2 ;;
esac

# Keep in sync with LSMinimumSystemVersion in Info.plist.in.
export MACOSX_DEPLOYMENT_TARGET=11.0
IDENTITY="${MACOS_SIGN_IDENTITY:--}"
SHORT_VERSION="${VERSION%%-*}"
WORK="$CARGO_TARGET_DIR/macos-package"
APP="$WORK/LightCraft R3D.app"
DMG="$DIST/lightcraft-r3d-$VERSION-macos-$ARCH.dmg"
CLI_ZIP="$DIST/lightcraft-r3d-cli-$VERSION-macos-$ARCH.zip"

NOTARIZE=0
MACOS_NOTARY_PROFILE="${MACOS_NOTARY_PROFILE-ToolBoxS}"
NOTARY_AUTH=(--keychain-profile "$MACOS_NOTARY_PROFILE")
if [ "$DEV_APP" = 1 ]; then
  # Local, explicitly isolated development artifact. Never use release credentials.
  ARCH=local
  IDENTITY="-"
  : "${LIGHTCRAFT_R3D_CONFIG_DIR:?source r3d/dev-env.sh before --dev-app}"
  : "${LIGHTCRAFT_R3D_LIBRARY:?source r3d/dev-env.sh before --dev-app}"
fi
if [ "$IDENTITY" = "-" ]; then
  warn "macOS: ad-hoc signing (no MACOS_SIGN_IDENTITY); artifacts are not notarized"
elif [ -n "$MACOS_NOTARY_PROFILE" ]; then
  NOTARIZE=1
elif [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  NOTARY_AUTH=(--apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID")
  NOTARIZE=1
else
  warn "macOS: APPLE_ID / APPLE_PASSWORD / APPLE_TEAM_ID incomplete; signed but not notarized"
fi

echo "==> LightCraft $VERSION for macOS ($ARCH), identity: $IDENTITY, notarize: $NOTARIZE"

# ---- build -------------------------------------------------------------------------------------
if [ "$SKIP_BUILD" = 0 ] && [ "$DEV_APP" = 1 ]; then
  (cd "$ROOT" && cargo build --locked -p lightcraft -p lightcraft-cli)
elif [ "$SKIP_BUILD" = 0 ]; then
  # shellcheck source=../fonts.sh
  . "$ROOT/packaging/fonts.sh"
  prepare_fonts
  args=()
  for t in "${TARGETS[@]}"; do args+=(--target "$t"); done
  (cd "$ROOT" && cargo build --release --locked -p lightcraft -p lightcraft-cli --features lightcraft/heif,lightcraft-cli/heif "${args[@]}")
fi

rm -rf "$WORK"
mkdir -p "$WORK/bin"
for bin in lightcraft lightcraft-cli; do
  inputs=()
  if [ "$DEV_APP" = 1 ]; then
    inputs=("$CARGO_TARGET_DIR/debug/$bin")
  else
    for t in "${TARGETS[@]}"; do inputs+=("$CARGO_TARGET_DIR/$t/release/$bin"); done
  fi
  lipo -create -output "$WORK/bin/$bin" "${inputs[@]}"
  lipo -info "$WORK/bin/$bin"
done

# ---- signing helpers ---------------------------------------------------------------------------
sign() {
  # Hardened runtime + secure timestamp for a real identity; ad-hoc can't be timestamped.
  local ts=(--timestamp)
  [ "$IDENTITY" = "-" ] && ts=(--timestamp=none)
  local kc=()
  [ -n "${MACOS_KEYCHAIN:-}" ] && kc=(--keychain "$MACOS_KEYCHAIN")
  codesign --force --sign "$IDENTITY" ${kc[@]+"${kc[@]}"} "${ts[@]}" "$@"
}

notarize() {
  local file="$1" out id status
  echo "==> notarizing $(basename "$file") (this can take a few minutes)"
  out="$(xcrun notarytool submit "$file" "${NOTARY_AUTH[@]}" --wait --timeout 1h --output-format json)" || true
  echo "$out"
  id="$(printf '%s' "$out" | plutil -extract id raw -o - - 2>/dev/null || true)"
  status="$(printf '%s' "$out" | plutil -extract status raw -o - - 2>/dev/null || true)"
  if [ "$status" != "Accepted" ]; then
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" "${NOTARY_AUTH[@]}" || true
    fi
    echo "error: notarization of $(basename "$file") failed (status: ${status:-unknown})" >&2
    exit 1
  fi
}

# ---- LightCraft R3D.app ----------------------------------------------------------------------------
echo "==> assembling $APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
# Executable and icon carry the display name (CFBundleExecutable / CFBundleIconFile).
cp "$WORK/bin/lightcraft" "$APP/Contents/MacOS/LightCraft"
cp "$ROOT/assets/app-icon/lightcraft.icns" "$APP/Contents/Resources/LightCraft.icns"
sed -e "s/@VERSION@/$VERSION/g" -e "s/@SHORT_VERSION@/$SHORT_VERSION/g" \
  -e "s/@BUILD_SHA@/${LIGHTCRAFT_BUILD_SHA:-unknown}/g" \
  "$HERE/Info.plist.in" >"$APP/Contents/Info.plist"
if [ "$DEV_APP" = 1 ]; then
  # Launch Services supplies the same explicit isolation as run-dev.sh, even when
  # the development bundle is opened from Finder. Only this generated app changes.
  python3 - "$APP/Contents/Info.plist" <<'PY'
import os, plistlib, sys
path = sys.argv[1]
with open(path, "rb") as stream:
    data = plistlib.load(stream)
data["CFBundleDisplayName"] = "LightCraft R3D Dev"
for key in ("CFBundleDocumentTypes", "UTExportedTypeDeclarations", "UTImportedTypeDeclarations"):
    data.pop(key, None)
data["LightCraftVersion"] += "-r3d-dev"
data["LSEnvironment"] = {
    key: os.environ[key] for key in (
        "LIGHTCRAFT_R3D_CONFIG_DIR", "LIGHTCRAFT_R3D_LIBRARY",
        "LIGHTCRAFT_LENSFUN_DIR", "LIGHTCRAFT_R3D_EXIFTOOL"
    ) if key in os.environ
}
with open(path, "wb") as stream:
    plistlib.dump(data, stream)
PY
fi
plutil -lint "$APP/Contents/Info.plist"
printf 'APPL????' >"$APP/Contents/PkgInfo"
# Licences (and the craft-fonts font licences when built with CRAFT_FONTS_DIR) inside the bundle.
mkdir -p "$APP/Contents/Resources/Licenses"
copy_docs "$APP/Contents/Resources/Licenses"

# Remove inherited build-time extended attributes BEFORE code signing.
# Never repair, re-sign or strip metadata from the published release after signing.
xattr -cr "$APP"

# Sign inside-out: nested code first, then the bundle itself (no --deep on the final signature).
# Today the only nested code is the main executable; frameworks/helpers would be signed here too.
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP/Contents/MacOS/LightCraft"
sign --options runtime --entitlements "$HERE/entitlements.plist" "$APP"
codesign --verify --strict --deep --verbose=2 "$APP"

if [ "$DEV_APP" = 1 ]; then
  echo "==> local development app only (ad-hoc signed, not notarized): $APP"
  exit 0
fi

if [ "$NOTARIZE" = 1 ]; then
  ditto -c -k --keepParent "$APP" "$WORK/LightCraft-notarize.zip"
  notarize "$WORK/LightCraft-notarize.zip"
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  spctl --assess --type execute -vvv "$APP"
fi

# ---- DMG ---------------------------------------------------------------------------------------
echo "==> building $DMG"
STAGE="$WORK/dmg"
mkdir -p "$STAGE"
ditto "$APP" "$STAGE/LightCraft R3D.app"
ln -s /Applications "$STAGE/Applications"
# Finder window layout: background, icon size and positions (packaging/macos/dmg/README.md).
mkdir -p "$STAGE/.background"
cp "$HERE/dmg/background.tiff" "$STAGE/.background/background.tiff"
cp "$HERE/dmg/dmg-layout.DS_Store" "$STAGE/.DS_Store"
rm -f "$DMG"
# hdiutil makehybrid -hfs injects FinderInfo metadata into the signed app.
# create -srcfolder preserves the signed app and the existing .DS_Store layout.
# hdiutil may transiently return "Resource busy" on CI; retry boundedly.
hdiutil_retry() {
  local attempt
  for attempt in 1 2 3 4 5; do
    if hdiutil "$@"; then
      return 0
    fi
    if [ "$attempt" = 5 ]; then
      echo "error: hdiutil $1 failed after five attempts" >&2
      return 1
    fi
    warn "hdiutil $1 attempt $attempt failed; retrying"
    sleep $((attempt * 10))
  done
}
hdiutil_retry create -srcfolder "$STAGE" -volname "LightCraft" -fs HFS+ -format UDZO \
  -imagekey zlib-level=9 -ov "$DMG"
sign "$DMG"
codesign --verify --strict --verbose=2 "$DMG"

# Verification must use the FINISHED image (and a normal ditto-installed copy).
# Signatures of the DMG container alone do not validate the bundle in the image.
verify_dmg_app() {
  local mount="$WORK/dmg-verify-mount" copy="$WORK/dmg-verify-copy" rc=0
  mkdir -p "$mount" "$copy"
  if ! hdiutil_retry attach -readonly -nobrowse -noautoopen -mountpoint "$mount" "$DMG" >/dev/null; then
    echo "error: unable to mount finished $DMG" >&2
    hdiutil detach "$mount" >/dev/null 2>&1 || true
    return 1
  fi
  echo "==> strict codesign of LightCraft R3D.app in finished DMG"
  codesign --verify --deep --strict --verbose=2 "$mount/LightCraft R3D.app" || rc=1
  if [ "$rc" = 0 ]; then
    ditto "$mount/LightCraft R3D.app" "$copy/LightCraft R3D.app" || rc=1
  fi
  if [ "$rc" = 0 ]; then
    echo "==> strict codesign of LightCraft R3D.app after ditto install-copy"
    codesign --verify --deep --strict --verbose=2 "$copy/LightCraft R3D.app" || rc=1
  fi
  if [ "$rc" = 0 ] && [ "$NOTARIZE" = 1 ]; then
    spctl --assess --type execute -vvv "$copy/LightCraft R3D.app" || rc=1
  fi
  if [ "$rc" != 0 ]; then
    echo "error: published-image app verification failed; reject $DMG" >&2
    xattr -lr "$mount/LightCraft R3D.app" >&2 || true
  fi
  if ! hdiutil_retry detach "$mount" >/dev/null; then
    echo "error: could not detach verification mount" >&2
    rc=1
  fi
  rm -rf "$copy"
  return "$rc"
}
verify_dmg_app
if [ "$NOTARIZE" = 1 ]; then
  notarize "$DMG"
  xcrun stapler staple "$DMG"
  xcrun stapler validate "$DMG"
  spctl --assess --type open --context context:primary-signature -vvv "$DMG"
fi

# ---- CLI ---------------------------------------------------------------------------------------
echo "==> building $CLI_ZIP"
CLI_DIR="$WORK/lightcraft-r3d-cli-$VERSION-macos-$ARCH"
mkdir -p "$CLI_DIR"
cp "$WORK/bin/lightcraft-cli" "$CLI_DIR/"
copy_docs "$CLI_DIR"
# Same entitlements as the app: `--opt addToPhotos=true` / `app.export {addToPhotos}` send Apple
# Events to Photos through osascript too (macOS asks about the app that started the CLI, usually
# the terminal).
sign --options runtime --entitlements "$HERE/entitlements.plist" "$CLI_DIR/lightcraft-cli"
codesign --verify --strict --verbose=2 "$CLI_DIR/lightcraft-cli"
rm -f "$CLI_ZIP"
ditto -c -k --keepParent "$CLI_DIR" "$CLI_ZIP"
# A bare Mach-O can't carry a stapled ticket; Gatekeeper looks the notarization up online.
if [ "$NOTARIZE" = 1 ]; then notarize "$CLI_ZIP"; fi

"$WORK/bin/lightcraft-cli" --version
echo "==> done"
ls -lh "$DMG" "$CLI_ZIP"
