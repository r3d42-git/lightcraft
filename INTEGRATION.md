# R3D optics integration

This fork starts at the unchanged LightCraft `v0.9.1` release,
`4f0b110bcbd2e350c6d97b47f1d824c2222295c4`. The machine-readable base is
`r3d/upstream-base.json`. This is a local development implementation, not a released
replacement for the official application.

## Boundaries

`crates/r3d-optics` owns provider-neutral identity and calibration selection. It has
no LightCraft, UI, filesystem or image-processing dependencies. Its inputs are
descriptors; its output is a unique selection or an explicit failure status.
Changing an upstream metadata or image type therefore does not change this API.

`crates/engine/src/lens_profiles.rs` is the adapter to the existing Lensfun-format
parser and correction pipeline. Database parameters are converted by upstream
code, not by a duplicate renderer. A compact camera is identified through the
provider's fixed-lens mount convention, never through a hardcoded camera list.
Calibration crop coverage is checked separately from lens identity.

`crates/engine/src/optics_metadata.rs` supplements the upstream optical metadata
using a separately executed, optional ExifTool helper. This is an explicit native
fork integration choice for broad MakerNote support: the application remains
Rust, but this optional external helper uses Perl. No ExifTool implementation is
copied into Rust or committed here. The web build uses the existing metadata reader.
The helper receives bytes on stdin and fixed read-only arguments with config
loading disabled. It receives no writable photograph path. Output and input are
bounded, work is serialized and cached, and the process has a deadline.

External profiles and helper files live outside the source checkout. The initial
versions and archive hashes are pinned in `r3d/optics-sources.json`.
`r3d/bootstrap-optics.py` verifies archives and installs them into an explicit,
previously nonexistent directory. It refuses to replace an existing different
installation. Lensfun source code is not extracted. Network access requires the
explicit `--download` option. Downloading these sources is not part of app startup.

## Original files touched and update checks

| Original file | Reason | Required update check |
| --- | --- | --- |
| Root and engine Cargo manifests, Cargo.lock, xtask layers | Register isolated module; no upstream dependency upgrades | Locked build, layer checker |
| `crates/engine/src/lens_profiles.rs` | Generic resolver, external-data default, diagnostic report | Resolver and adapter tests, real profile reports, same geometry normalization |
| `crates/engine/src/files.rs` | Supplement metadata on RAW probe and RAW render paths | Import metadata and export agree; no JPEG correction |
| `crates/engine/src/lib.rs` | Expose metadata helper module | Native and WASM builds |
| `crates/engine/src/config.rs` | Separate settings defaults, override and portable marker | Config tests, official override must be ignored |
| `crates/engine/src/library.rs` | Separate default library name and override | Native `library.info`, restart with explicit test library |
| `apps/lightcraft/src/main.rs` | Separate app ID, title, settings/library override references | Actual native launch, correct settings and library paths |
| `apps/lightcraft-cli/src/main.rs` | Read-only `optics-inspect FILE` diagnostic | Report on test copies; no Session or library opened |
| `crates/ui-egui/src/panels/edit.rs`, German locale | Available component controls and accurate missing-correction text | Native optics panel; missing vignette does not enable its slider |
| `packaging/macos/Info.plist.in`, `package.sh` | Distinct bundle/artifact names; local dev-app mode; ToolBoxS option | Plist, shell syntax, signature, correct bundle ID; no official app replacement |

The catalog schema and rendering pipeline are unchanged. Available profile
parameters already fit the existing persisted lens field. The upstream import
behavior enables supported corrections for new RAWs. Its historical camera-default
calculation is deliberately preserved so reopening an older catalog does not
reclassify its edits. Automatic crop remains available through the existing import
preset mechanism; it is not imposed on historical photos.

## Isolated development workflow

The commands below run in Bash from this checkout. They use the project-local
toolchain and explicit project test paths:

```bash
source r3d/dev-env.sh
python3 r3d/bootstrap-optics.py \
  --root "$LIGHTCRAFT_R3D_CONFIG_DIR/optics" --cache "$R3D_PROJECT/.tools"
cargo build --locked -p lightcraft -p lightcraft-cli
./r3d/run-dev.sh
```

If the pinned archives are absent, add `--download` to the setup command.
Only copies placed in `test-runtime/input` are passed to the test app.
The development launcher uses control port 18431. The official application is
neither stopped nor launched by these commands.

The application and CLI use `LIGHTCRAFT_R3D_CONFIG_DIR` and
`LIGHTCRAFT_R3D_LIBRARY`. They intentionally do not inherit the official app's
`LIGHTCRAFT_CONFIG_DIR` or `LIGHTCRAFT_LIBRARY`. Platform defaults are also distinct.
Paths explicitly selected by a human in the app remain possible; the separation
does not pretend to be a filesystem sandbox.

For an ad-hoc signed local app bundle, use the existing packaging script:

```bash
source r3d/dev-env.sh
bash packaging/macos/package.sh --dev-app
```

This mode stops after assembling and verifying a local app. It creates no DMG,
does not notarize, does not upload, and does not install in `/Applications`.
Its generated plist embeds the explicit test configuration and library paths
for Finder launches. Release mode keeps the native packaging workflow and accepts
`MACOS_NOTARY_PROFILE`, defaulting to the user's existing `ToolBoxS` profile.

## Diagnostics and validation

```bash
source r3d/dev-env.sh
"$CARGO_TARGET_DIR/debug/lightcraft-cli" optics-inspect /absolute/path/to/copy.CR2
```

The report distinguishes the effective embedded/external source from a candidate
external profile. Component status `applied` means successfully converted and
available, not that a user has left the correction toggle enabled. Optical identity,
crop calibration, availability and conversion failure are separate facts.

Run upstream CI without external profile data or helper execution so its synthetic
tests remain hermetic:

```bash
source r3d/dev-env.sh
LIGHTCRAFT_R3D_CONFIG_DIR='' LIGHTCRAFT_R3D_EXIFTOOL='' \
LIGHTCRAFT_LENSFUN_DIR=/nonexistent-r3d-hermetic-ci cargo xtask ci
```

Native socket and GPU tests require a local execution context that permits them.
Corpus/model/font skips must be retained in the report. They are not evidence of
image quality. Supplement this gate with the local real-file reports and visual
CPU/GPU checks outside Git; personal pictures never enter the repository.

## Update procedure

1. Fetch the desired official release from `upstream`, leaving existing tags intact.
2. Start from a clean, committed and tested development state.
3. Run `python3 r3d/prepare-update.py NEW_TAG --destination /new/worktree` for a preview.
4. Add `--prepare` to create the new worktree and replay our commits. A conflict stops
   the script and leaves the worktree available for review.
5. Review every integration point above, even when replay had no conflicts. Remove
   patches superseded by upstream rather than keeping redundant implementations.
6. Run CI, the same real-file reports, native launch and image checks. Review the
   updated base manifest and commit the tested result on a new development branch.
7. Build and verify a new own app. Install or publish only in a separately authorized
   step; retain the old app and a compatible library backup.

The script does not fetch, install, publish, run the app or open a library. It
automates preparation, not approval of a build. Keep each functional area in its
own commit so the replay stays reviewable. It requires a linear patch series;
merge commits are refused before creating a worktree, since replaying only their
parents would lose merge-specific integration fixes.

## Current limits and next work

- This first implementation uses the existing Lensfun XML provider. Adobe LCP
  remains a planned additional provider, not a working feature.
- Embedded corrections retain precedence. Mixing an embedded warp with external
  shading is deferred until coordinate compatibility has been verified.
- Strong vignette profiles can fail the upstream polynomial fit. The G7X II at
  8.8 mm/f2.5 is such a real case. Its geometry is available; its vignette status
  is `rejected_conversion`. The error limit is not relaxed to hide this.
- Unknown crop, uncertain identifiers, adapters and genuinely ambiguous matches
  remain uncorrected. A lens name alone is never proof of usable calibration.
- No retrospective batch recognition UI, manual profile override UI or in-app
  profile downloader is implemented yet. Setup is currently provided by the
  project-local script.
- Profiles are pinned for this prototype. Switching data under a running or
  previously edited library needs explicit version/migration handling before
  profile updates can become a normal user operation.
