# Sharpening mask preview

In Detail → Edit → Detail, hold Option (macOS) or Alt while dragging Sharpening Masking.
White receives sharpening, black is protected, and grey receives a partial weight.
Release the modifier or mouse button to return to the photo. This also works when scrubbing
the numeric Masking value. The preview does not add a history step or affect exports.

The view reads the same fine-detail plane and weight helper as CPU sharpening; Masking zero
is entirely white. Zoomed views use
the existing padded region pipeline, including crop, rotation and lens geometry.
The diagnostic uses CPU rendering because GPU detail planes are not read back; releasing
it restores the normal GPU eligibility. Existing clipping/HDR and mask visualizations resume
according to their switches. The photo histogram is retained and diagnostics bypass the
normal view cache.

Manual acceptance: drag with Option/Alt, release each independently, change focus/photo/panel,
try Fit and 100% with crop/rotation, and check a profiled/HDR display. Physical display and
real-mouse acceptance require the user's confirmation.
