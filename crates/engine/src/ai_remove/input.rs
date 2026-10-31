//! Source-space preparation and frozen authorization remain owned by the engine.
use super::{DevelopSettings, Spot, runner};
use lightcraft_develop::ai_edit::AiBrushSupport;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct PreparedInput {
    pub request: runner::RetouchInput,
    pub support: AiBrushSupport,
}

pub(super) fn prepare(
    source: crate::media::SourceRef,
    info: lightcraft_pipeline::SourceInfo,
    d: DevelopSettings,
    spot: Spot,
    continuous: bool,
    seed: u32,
    cancel: &AtomicBool,
) -> std::result::Result<PreparedInput, String> {
    let decoded = source.load_source()?;
    if decoded.from_proxy {
        return Err("AI Remove needs the original photo online.".into());
    }
    let edited = lightcraft_pipeline::ai_edits::source_with_edits(&decoded.image, &d);
    let full = edited.as_ref();
    let frame = lightcraft_pipeline::frame_for(full, decoded.info.as_ref().unwrap_or(&info), &d, false);
    let radius = spot.size * full.width.max(full.height) as f64;
    if !radius.is_finite() || radius <= 0.0 || spot.points.is_empty() || spot.points.len() > 4096 {
        return Err("Invalid brush mask.".into());
    }
    let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in &spot.points {
        if !p.x.is_finite() || !p.y.is_finite() {
            return Err("Invalid brush point.".into());
        }
        let (x, y) = (p.x * frame.ow, p.y * frame.oh);
        bounds[0] = bounds[0].min(x - radius);
        bounds[1] = bounds[1].min(y - radius);
        bounds[2] = bounds[2].max(x + radius);
        bounds[3] = bounds[3].max(y + radius);
    }
    // Bound the entire transformed brush rectangle through the nonlinear warp. Mapping
    // only centres (or a few boundary points) could clip curved capsule support.
    let oriented = frame.warp.as_ref().map_or(Some(bounds), |warp| warp.source_bounds(bounds)).unwrap_or([0.0, 0.0, frame.ow, frame.oh]);
    bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for (x, y) in [(oriented[0], oriented[1]), (oriented[0], oriented[3]), (oriented[2], oriented[1]), (oriented[2], oriented[3])] {
        let (x, y) = d.orientation.inverse().map(x, y, frame.ow, frame.oh);
        bounds[0] = bounds[0].min(x);
        bounds[1] = bounds[1].min(y);
        bounds[2] = bounds[2].max(x);
        bounds[3] = bounds[3].max(y);
    }
    let support = lightcraft_pipeline::ai_edits::brush_support(&frame, &spot, continuous);
    let brush = lightcraft_pipeline::ai_edits::BrushSupport::new(&support, full.width, full.height);
    let [x0, y0, x1, y1] = context_rect(bounds, full.width, full.height).ok_or_else(|| "The mask is outside the photo.".to_string())?;
    let scale = (1024.0 / (x1 - x0).max(y1 - y0)).min(1.0);
    let (w, h) = (((x1 - x0) * scale).ceil() as usize, ((y1 - y0) * scale).ceil() as usize);
    let rect = [x0 / full.width as f64, y0 / full.height as f64, x1 / full.width as f64, y1 / full.height as f64];
    let mut src = lightcraft_raster::Rgb32f::new(w, h);
    for (i, p) in src.data.iter_mut().enumerate() {
        *p = full.sample_bilinear(
            (x0 + ((i % w) as f64 + 0.5) * (x1 - x0) / w as f64 - 0.5) as f32,
            (y0 + ((i / w) as f64 + 0.5) * (y1 - y0) / h as f64 - 0.5) as f32,
        );
    }
    let mut mask = vec![0u8; w.saturating_mul(h)];
    // Evaluate exact frozen transformed support at each source-space inference sample.
    for (i, m) in mask.iter_mut().enumerate() {
        if i % 1024 == 0 && cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let point = [
            (x0 + ((i % w) as f64 + 0.5) * (x1 - x0) / w as f64) / full.width as f64,
            (y0 + ((i / w) as f64 + 0.5) * (y1 - y0) / h as f64) / full.height as f64,
        ];
        if brush.alpha(point, full.width, full.height) > 0.0 {
            *m = 255;
        }
    }
    if !mask.contains(&255) {
        return Err("The selected mask is empty.".into());
    }
    Ok(PreparedInput { request: runner::RetouchInput { width: w, height: h, rgb: src.data, mask, rect, seed, steps: 20 }, support })
}

/// Use equal context on both axes, shifting at edges instead of clipping it.
/// Very narrow originals retain all available context on the shorter axis.
pub(super) fn context_rect(bounds: [f64; 4], width: usize, height: usize) -> Option<[f64; 4]> {
    let [left, top, right, bottom] = bounds;
    let (width, height) = (width as f64, height as f64);
    if !bounds.iter().all(|v| v.is_finite())
        || right <= left
        || bottom <= top
        || width == 0.0
        || height == 0.0
        || right <= 0.0
        || bottom <= 0.0
        || left >= width
        || top >= height
    {
        return None;
    }
    let half = (right - left).max(bottom - top).max(256.0);
    let side = (half * 2.0).ceil();
    let (w, h) = (side.min(width), side.min(height));
    let x = (((left + right) * 0.5 - w * 0.5).floor()).clamp(0.0, width - w);
    let y = (((top + bottom) * 0.5 - h * 0.5).floor()).clamp(0.0, height - h);
    Some([x, y, x + w, y + h])
}
