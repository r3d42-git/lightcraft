# R3D adaptive Auto

Auto computes a new set of six tonal controls plus vibrance and saturation from the selected image. White balance is an input, never an output of this command. Other edits remain in place and participate in the rendered assessment. HDR/panorama merge Auto uses the same implementation.

## Algorithm and acceptance criteria

1. Validate the source buffer and finite samples. Invalid input preserves the current controls (non-finite control values have safe finite fallbacks).
2. Create bounded measurement and validation proxies. Tile means measure the scene; local dark, bright-channel and chromatic representatives retain small extreme features for validation.
3. Render with neutral Auto controls and all remaining edits. Luminance percentiles, log-average luminance, dynamic range and chroma drive restrained exposure, contrast, highlights, shadows, whites, blacks, vibrance and saturation. Near-uniform fields have little confidence. Dark scenes with sparse lights and high-key scenes receive weaker tonal shifts. This is a heuristic, not semantic scene recognition.
4. Render the current edit and candidate through the CPU reference pipeline. Reject a candidate that adds an endpoint at any previously unclipped channel/sample in the validation render. Existing clipping is allowed; one clipped region cannot offset a newly clipped region elsewhere. SDR checks float and encoded 8-bit endpoints. HDR checks zero and configured linear headroom, not SDR white.
5. Before weakening the whole adjustment, try bounded protection of highlights/whites, contrast and positive vibrance, proportional to the proposal. Then try fixed strengths toward neutral (at most 21 candidate renders). If none is safe, keep the current Auto controls. Fixed candidates plus the clipping-support subset rule make repeated Auto stable.

The command remains a single undoable edit. Existing saved settings render identically: only newly invoking Auto changes the generated settings, so no render cache/process-version bump is needed.

## Limits and validation

The command currently supplies a 512 px decoded source; validation renders are bounded to 256 px. Subpixel lights, spatial filtering, masks and crop boundaries can differ at larger resolution. No full-resolution no-clipping guarantee is claimed. Monitor-profile gamut and HDR display appearance need physical display validation. The UI clipping warning remains unchanged and is not RAW sensor saturation analysis.

Synthetic regression tests cover different scene ranges, saturated colors, existing clipping, HDR superwhite, invalid input, fixed-point behavior and edit preservation. The engine regression covers the command, one undo/redo step, custom WB and other edits. `cargo run -p lightcraft-engine --example auto_review -- OUTPUT_DIR INPUT_COPY...` creates local before/old-recipe/new-Auto PNGs and per-channel clipping measurements at 1600 px, with no catalog or sidecars. Supply only authorized test copies.

Manual acceptance should include a backlit subject, lamps at night, intentional silhouette, high-key image, fog, saturated flowers, skin tones, and the user's original reported image. Judge useful improvement, preservation of intent, halos and new clipping; verify WB, repeat Auto, undo, crop/masks and preview/export agreement. These human checks are separate from headless tests.

This fork no longer uses the old fixed recipe or inherits its Lightroom calibration scores.

## Filled header state

Auto, B&W and HDR use a contrasting solid fill while selected. B&W/HDR reflect their actual modes. Auto stores the last eight applied values as optional per-photo metadata and is selected only while those values are unchanged. Altering an Auto-owned control deselects it; undo or restoring the same values reselects it. Other controls do not invalidate the indication. Reset clears it. Catalog/history persistence retains it across reopening and image changes. Existing photos have no marker until Auto is applied.

The marker does not affect render hashes, the unedited predicate or camera-preview eligibility. Presets, copy groups and Auto Sync do not copy another image's Auto provenance.
