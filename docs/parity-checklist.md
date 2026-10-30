# Lightroom parity checklist

| 2026-10-25 | patch | LR-BEHAV-TITLEBAR: the top bar's title-bar gestures come from craft-window's `title_bar`: a double-click follows the system setting, and a press on a button, the search field or a panel floating over the bar never moves the window; status counts unchanged |

## R3D test candidate — 2026-10-30

These targeted upstream fixes preserve existing feature status and counts; separate runtime acceptance is pending.

- LR-VIEW-HISTOGRAM: clipping-button tile and chevron use theme tokens in every theme; the R3D spatial overlay is retained.
  Evidence: `crates/ui-egui/src/panels/edit/histogram.rs` (`the_clipping_chevron_stands_3_to_1_against_its_tile_in_every_theme`).
- LR-VIEW-PHOTOGRID and LR-VIEW-FILMSTRIP: pending thumbnails show three still dots; unreadable thumbnails show "!".
  A replacement request suppresses the previous failure while pending. Evidence: `crates/ui-egui/src/tests/loading_cell.rs`
  (widgets `badge:loading:{id}`, `film:badge:loading:{id}`), `crates/ui-egui/src/render/thumbnail_tests.rs`.
- White Balance preset choices use the existing interface translation catalog: `crates/ui-egui/src/panels/edit.rs`.

## R3D sharpening mask preview — 2026-10-30

- LR-EDIT-DETAIL-SHARPEN: Option/Alt while dragging Masking (including its numeric readout) shows the actual sharpening weight in white on black. Releasing the modifier or button restores the photo; the gesture adds no history step.
  Evidence: `crates/ui-egui/src/tests/sharpen_mask_preview.rs`, `crates/pipeline/src/tests.rs`, `crates/engine/src/display.rs`.
- CPU diagnostic with padded zoom regions; the normal GPU path resumes afterward. The sharpening formula stays unchanged and the histogram stays the photo's. Headless captures preserve the held gesture without replaying input.
- The owner accepted holding Option and releasing Option/mouse separately on 2026-10-30. Personal RAW latency, crop/rotation, and a physical profiled/HDR display are not separately confirmed.
- Usage and limits: `docs/sharpen-mask-preview.md`. This fork's abbreviated checklist has no feature rows to recount; no headline parity percentages are changed.
