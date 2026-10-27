# R3D optics integration

This fork integrates the unchanged LightCraft `v0.14.0` release,
`03c88c166c054c009db2a47187d59991f6232b85`, into its tested R3D feature line. The machine-readable base is
`r3d/upstream-base.json`. This is a local development implementation, not a released
replacement for the official application.

## v0.14.0 integration

Upstream v0.14.0 is merged into the existing R3D history. The isolated native
identity, configuration/library environment variables, read-only optional optics
helper, conservative resolver and optional crop API remain. Existing geometry,
accepted RAW colour provenance, 1600% pixel inspection, native macOS ACL export
preservation and original/sidecar protection, owner-note exit cleanup, AHD blend,
German/layout corrections, low-latency surface request and zero-chroma GPU fix
remain in place.

Upstream owns keyword export (#2015) and the complete German terminology update
(#2800); their overlapping fork hunks are shared rather than duplicated. The
fixed histogram remains above the Edit scroll area and now uses upstream's tonal
drag controls and shared clipping-button hit rectangles. Both fixed-layout and
new drag regression modules remain registered. GPU integration tests move under
`tests/it` with the upstream declaration module, retaining the zero-chroma test.
The recent-library launch chooser uses `LIGHTCRAFT_R3D_LIBRARY` and the R3D default
library; it does not inherit the official application's library environment.

`RENDER_CACHE_VERSION = 73` invalidates both upstream cache 70 and older R3D cache
72 for the combined pipeline. `LOOK_VERSION = 13` stays above upstream 12: the
upstream change in camera fitting is a test correction only, so the existing R3D
geometry-gated fit semantics are unchanged. Local workspace crates in Cargo.lock
use 0.14.0. Historical version references below describe their original states.

The checkout-specific development defaults are now `.tools/target-update-0.14.0`
and `test-runtime/update-0.14.0/{config,library,input}`, with control port 18473.
Validation and native visual acceptance are recorded separately; source integration
alone is not evidence of a successful build, launch or image-quality check.

## Camera colour provenance candidate

Manually ported diagnostic feature from upstream [PR #1332](https://github.com/storytold/lightcraft/pull/1332),
immutable head `467213a58e7cb630e9161266bcd96ff18fac85f7`, onto tested fork
`6b8b5c47c946d6419dd692e7e892048f8a5fa1c5`. The original PR predates
this fork's DNG tone, near-black tone and geometry work; obsolete test-portability
patches are omitted and its diagnostic paths are adapted to the current source.

Edit and the profile browser display the selected original RAW's actual accepted
colour basis, separately from the editable creative profile. English and German
readouts distinguish file matrices, validated local/bundled camera profiles, full
or partial JPEG fits, spectral colour and neutral fallback; accepted monochrome,
near-black tone and Sony DRO removal have independent badges. A DNG's accepted
preview tone retains file-matrix attribution. Rejected fits never acquire a badge.

Diagnostics travel with decoded source facts or successful current-content worker
results; remote facts have a 64 KiB LRU budget and disappear on eviction, forget or
source-cache clear. Render-cache hits and old smart previews without decoded facts
honestly show unavailable until an original is decoded. No decode is forced for
the readout. Camera-profile data and validated origin are cached atomically.

This is diagnostic visibility only: pixel algorithms, geometry/export/zoom fixes,
`LOOK_VERSION = 13` and `RENDER_CACHE_VERSION = 70` remain unchanged. It does not
improve image colour or establish camera calibration parity. Validation is
synthetic, automated and windowless; desktop launch, personal library/media,
installation and publication are outside this preparation.

## Pixel inspection and 1600% zoom candidate

Prepared from the tested export-safety fork `789269c3`. Upstream references are
[issue #2643 / PR #2645](https://github.com/storytold/lightcraft/pull/2645) at
`91aaed72759f45b479823ee487f9efb58d6307a7` and
[issue #2652 / PR #2653](https://github.com/storytold/lightcraft/pull/2653) at
`a7d887c8582e20c9480b77251b985d60e004e03c`.

The local filtering policy is deliberately narrower than PR #2645: detail,
Before/After, Compare/Reference, second-window and hovered-look textures use
nearest-neighbour magnification with linear minification. Thumbnails, loading
stand-ins, variants, import previews and composed slides retain linear filtering.
Both texture uploads and the CPU texture overrides used by native headless
screenshots share that policy. A Fit view that enlarges a final render may also
show the new magnification; this is a texture sampling rule, not a label-based
100% switch. Fractional zoom can produce uneven pixel widths.

Keyboard steps, pinch/wheel and `view.navigate` share the new 1600% maximum.
Click/Z/Space ratios remain 1:1 through 8:1. Source resolution and export output
are unchanged. The existing native-size window renderer and #2499's display-fit
hold are retained, as are all colour, optics and export-safety changes.

Validation for this preparation is automated and windowless only, including
synthetic pixel output at 200/800/1600%, 1x/2x scaling, upload replacement,
minification, screenshot texture parity and zoom/navigation boundaries. No
desktop application launch, installation or publication is authorized here.
Automated checks are not a native-window/compositor visual acceptance test.

## Export safety candidate

Prepared on top of the installed fork's `15af97c584315f145532b192b236fb2925805819`,
including its camera-preview geometry correction. Sources are upstream
[#2015](https://github.com/storytold/lightcraft/pull/2015) at `3734c22fae0396da04cde4ff15e2b8f27db91344`,
[#2634](https://github.com/storytold/lightcraft/pull/2634) at `07d2556ead33726b3d32ec504bec4c5e805ead43`,
and [#2658](https://github.com/storytold/lightcraft/pull/2658) at `1aea62dde04c96912b690f6ed481e166870f81b6`.
These are a combined local candidate, not a new upstream release.

Keyword and mask-effect exports validate their final destination against catalog
originals and both XMP sidecar spellings, including absent sidecars, directory
aliases and Unix hard links. macOS canonical Unicode name equivalence also applies
to reserved sidecar names. An unrelated folder may use the same name. Keyword text
output and missing-parent errors remain; mask-effect exports still add the default
extension and create nested output folders.

The shared atomic writer retains existing regular files' POSIX mode, owner/group,
and native macOS ACLs. It checks existing write access without truncation, stages
replacement bytes inside a private directory cleared of inherited ACLs, and applies
the old access controls before filling the new file. Failure leaves the previous
destination and removes staging artifacts. Files with special mode bits are
refused, rather than transferring privilege bits onto generated content. Ordinary
symlink destinations are replaced atomically; their referents remain unchanged.
New-file permission/inheritance defaults are retained.

`crates/fs-security` is a dependency-free L0 helper around documented macOS system
APIs; its narrow unsafe boundary is listed in `AGENTS.md`. No registry dependencies
are introduced or upgraded. Native Windows/Linux ACL transfer is not supplied by
this macOS helper. Validation must include the native macOS ACL tests, engine/UI
export paths, failed-write cleanup, the complete CI gates, and an isolated native
app run. No application installation, push, PR, tag or release is part of this work.

## v0.13.0 integration

The seven committed local changes from the tested v0.11.0 fork were replayed in a
new checkout. The conservative resolver, optional crop validity, directional mount
compatibility, read-only metadata helper, accurate component controls and isolated
app identity remain necessary. Pinned Lensfun/ExifTool versions and archive hashes
are unchanged. No diagnostic-branch colour experiment or Adobe asset is included.

Upstream now owns the pinch display-fit hold through `SizeHold::display_fit` and
also shares it with the Before and hover views. The former fork-only texture hold
and its redundant modest-pinch test were removed; `detail.rs` and
`tests_preview_limit.rs` are unchanged from v0.13.0. Upstream regression tests remain.

The new upstream Nikon lens-specification fallback is retained: a missing lens
model may use `spec_name` only for profiles with an explicit exact alias, using
upstream `names_exactly`. Those eligible profiles still go through conservative
mount, identity, ambiguity and crop checks. A recorded model retains precedence.
The upstream adapter regression and the added duplicate-alias rejection cover this
integration. Upstream's reduced-DNG thumbnail path, full-image exports, DNG lens
opcode handling and all unrelated release changes are retained.

The local `r3d-optics` lock entry now uses workspace version `0.13.0`. The equivalent
`is_none_or` expression in export preset handling remains for the local Clippy gate.
Development scripts now default to a checkout-specific build target and runtime,
and preserve explicit caller overrides. For this checkout these are
`.tools/target-update-0.13.0`, `test-runtime/update-0.13.0/{config,library,input}` and
control port 18473, keeping the installed development app's older paths separate.
Validation evidence is recorded outside Git; a prepared tree is not proof of a
successful build, launch or image-quality check.

## Historical v0.11.0 integration

The six local functional commits were replayed onto the official release in a
separate worktree. Upstream's removal of the obsolete `.pcraft`, PSB and QOI UTI
declarations is retained; the fork keeps its own bundle identifier without
claiming those document types. The resolver, optional crop API and metadata helper
remain necessary: upstream has not superseded them. The Lensfun and ExifTool
versions and archive hashes remain unchanged.

The upstream RAW-preview format detection, background Reload from Disk, bounded
profile menus, export bit-depth validation, preset write-error recovery and merge
preferences remain intact. One upstream boolean in export preset handling is
expressed with `is_none_or` for the existing local Rust 1.95 Clippy gate; its
missing/null-value behavior is unchanged. The replayed local crate lock entry now
uses the workspace version `0.11.0`.

The official release's pinch regression was reproduced from byte-identical
upstream source before fixing it here. During a pinch the render plan stays held,
but its display-fit dimensions previously followed the changing zoom rectangle,
growing the texture despite the hold. The fork also holds the existing same-photo
display-fit dimensions until the gesture is quiet. The ordinary Preview source
floor remains intact, and normal rendering resumes after the hold expires.

## Boundaries

`crates/r3d-optics` owns provider-neutral identity and calibration selection. It has
no LightCraft, UI, filesystem or image-processing dependencies. Its inputs are
descriptors; its output is a unique selection or an explicit failure status.
Changing an upstream metadata or image type therefore does not change this API.

`crates/engine/src/lens_profiles.rs` is the adapter to the existing Lensfun-format
parser and correction pipeline. Database parameters are converted by upstream
code, not by a duplicate renderer. A compact camera is identified through the
provider's fixed-lens mount convention, never through a hardcoded camera list.
Top-level Lensfun mount declarations survive database-file aggregation. The adapter
passes only the camera mount's explicitly declared compatible lens mounts to the
resolver (for example, Canon EF-S accepts Canon EF). Reverse or transitive
compatibility is never inferred; fixed-lens camera-only matching requires the exact mount.
Calibration crop coverage is checked separately from lens identity.
The resolver records one effective frame crop in its resolution; the adapter passes
that exact value to correction conversion. A recorded crop overrides the body only
when it is more than 10% larger, preserving the existing whole-millimetre EXIF
rounding policy. Calibration coverage retains its separate 1% boundary tolerance.

Camera and calibration crops remain optional through XML parsing and adaptation;
missing or invalid values never become an assumed 1.0. An omitted calibration
attribute inherits its parent lens crop, while an explicitly invalid override stays
unknown. A valid calibration attribute may supply a crop when its parent lacks one.
Explicit 1.0 remains valid. Parsing, resolution and conversion accept only finite
crops in 0.1..=20. Conversion rejects profiles with unknown calibration crop even
when called directly. The parser uses `Option<f64>` rather than retaining a numeric
fallback plus provenance flag, so downstream consumers cannot accidentally trust
the fallback. This changes the parser's public crop fields but avoids parallel
representations of the same fact.

These rules follow the Lensfun format documentation: the
[camera crop](https://lensfun.github.io/manual/latest/el_camera.html) must be
accurate, [lens calibration crop](https://lensfun.github.io/manual/latest/el_lens.html)
has no documented 1.0 default, and
[calibration attributes](https://lensfun.github.io/manual/latest/structlfLensCalibAttributes.html)
require a crop factor. The documented default aspect ratio remains unchanged.


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
| `crates/lensdb/src/lib.rs` | Optional crop API preserves unknown values and distinguishes omitted inheritance from invalid overrides | XML crop validity/inheritance tests, direct conversion rejection of unknown calibration, adapter integration tests |
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
Only copies placed in `test-runtime/update-0.14.0/input` are passed to the test app.
The development launcher uses control port 18473. The official application is
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

## Isolated BM3D denoise candidate – 2026-10-27

Selective #2982 port (57f974c2) on 3098c66a, retaining egui 0.36 and the normal AHD path. The denoised component performs its own Bayer reconstruction. With no valid selected ONNX model, Builtin is used; selected models remain alternatives. Consequently previously enabled photos whose selected model is missing can now render differently. No original is rewritten. Builtin cache fingerprint is builtin@5; render cache 73 and look version 13 are retained. Pending model-setup UI is removed only in this variant. #2929 is prepared in a separate worktree. No installed-app or native photographic acceptance is implied.

## Culling core candidate (#3055)

Selectively backported upstream head `7fa770fb94eb8211ddd08e6008876f1c4785c876` onto adaptive Auto v0.14.0. Incremental browse, camera-JPEG look-ahead, CR3 preview sizing and source badges retain the fork renderer, colour/optics, pixel zoom and original-file safety. Module-split paths were mapped onto this checkout; no egui upgrade. Validation recorded separately; local user acceptance pending.

The dependent #3077 (`aa852a75`) and #3080 (`703f5727`) follow-ups add reusable built previews, bounded background preparation, a configurable disk budget and reserved stand-in workers. The fork Auto marker is ignored as presentation metadata in profile-only camera-preview eligibility. All three culling PRs are validated together as one feature chain before commit; no upstream module-layout or egui migration is included.

## Combined denoise/culling source identity – 2026-10-27

A deterministic completion-order test reproduces a late plain/previous-model decode repopulating the source cache after denoise readiness or a model switch. Decoded twins now retain their product key; Session acceptance requires both the original RAW identity and the current product identity. Plain sources remain reusable after product removal; unbound twins cannot claim to be plain. Render cache77 invalidates earlier derived previews; LOOK13 is unchanged. The separate optical-crop variant reserves78. Full combination CI and real cache-rebuild checks are required before commit.

## Local PR candidates — 2026-10-27

PR #3038, head `ff913833de19cbb017c79c596cc31d3cbe75f2b4`, declares the 18 shipped UI languages for macOS system dialogs. The R3D bundle identity remains. AppKit follows system/app language preferences; a LightCraft-only language switch does not force native dialog language. Native dialog acceptance remains pending.

PR #2981, head `daf46b16f13bd7c2a9713efbdd63eda20a98219c`, gates the retained live HDR histogram on HDR being enabled. The fixed histogram layout, tonal drag controls and active HDR button remain. No rendering/cache/LOOK version changes.

PR #2988, head `5490347d47a52d0cffc534302ae79134ade04091`, adds Lanczos-3 downscaling only to export jobs (CPU and GPU); preview/thumbnail requests retain Mitchell. Export request keys carry a distinct filter identity, and the resolved frame includes the filter in CPU/GPU stage-cache geometry keys. Output sharpening becomes a Gaussian luma unsharp mask with a local 10% overshoot bound and finite float-input handling. Original/sidecar protection and native ACL atomic writes are unchanged. Shared-stage-cache regressions exercise Smooth → Sharp → Smooth on CPU and Metal. Cache73 and LOOK13 remain unchanged at this conservative checkpoint because preview pixels are unchanged; the later #3012 crop variant is separate.

The combined conservative candidate keeps render cache77 and LOOK13. It includes #3038, #2981, #2988, #2982 and the #3055/#3077/#3080 chain with late-source protection. Automatic optical crop #3012 and the #2929 download catalog are not included; the verified optional ONNX import remains available.
