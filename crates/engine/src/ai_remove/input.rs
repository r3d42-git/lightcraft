//! Source-space preparation and frozen authorization remain owned by the engine.
use super::{DevelopSettings, Spot, runner};
use lightcraft_develop::ai_edit::AiBrushSupport;
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) struct PreparedInput {
    pub request: runner::RetouchInput,
    pub support: Option<AiBrushSupport>,
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
    prepare_inner(source, info, d, spot, None, &[], continuous, seed, cancel)
}

#[cfg(test)]
pub(super) fn prepare_segmented(
    source: crate::media::SourceRef,
    info: lightcraft_pipeline::SourceInfo,
    d: DevelopSettings,
    seg: lightcraft_develop::SegMask,
    seed: u32,
    cancel: &AtomicBool,
) -> Result<PreparedInput, String> {
    prepare_segmented_protected(source, info, d, seg, &[], seed, cancel)
}

#[allow(clippy::too_many_arguments)]
pub(super) fn prepare_segmented_protected(
    source: crate::media::SourceRef,
    info: lightcraft_pipeline::SourceInfo,
    d: DevelopSettings,
    seg: lightcraft_develop::SegMask,
    refinements: &[(Spot, bool)],
    seed: u32,
    cancel: &AtomicBool,
) -> Result<PreparedInput, String> {
    let mask = super::SelectionMask::new(&seg)?;
    if mask.is_empty() {
        return Err("The selected mask is empty.".into());
    }
    prepare_inner(source, info, d, Spot::default(), Some(mask), refinements, true, seed, cancel)
}

#[allow(clippy::too_many_arguments)]
fn prepare_inner(
    source: crate::media::SourceRef,
    info: lightcraft_pipeline::SourceInfo,
    d: DevelopSettings,
    spot: Spot,
    segmented: Option<super::SelectionMask>,
    refinements: &[(Spot, bool)],
    continuous: bool,
    seed: u32,
    cancel: &AtomicBool,
) -> Result<PreparedInput, String> {
    let decoded = source.load_source()?;
    if decoded.from_proxy {
        return Err("AI Remove needs the original photo online.".into());
    }
    let edited = lightcraft_pipeline::ai_edits::source_with_edits(&decoded.image, &d);
    let full = edited.as_ref();
    let frame = lightcraft_pipeline::frame_for(full, decoded.info.as_ref().unwrap_or(&info), &d, false);
    let radius = spot.size * full.width.max(full.height) as f64;
    if segmented.is_none() && (!radius.is_finite() || radius <= 0.0 || spot.points.is_empty() || spot.points.len() > 4096) {
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
    if let Some(mask) = &segmented {
        let b = mask.selected_bounds();
        bounds = [b[0] * frame.ow, b[1] * frame.oh, b[2] * frame.ow, b[3] * frame.oh];
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
        if segmented.as_ref().map_or_else(
            || brush.alpha(point, full.width, full.height) > 0.0,
            |mask| {
                brush
                    .brush_point(point, full.width, full.height)
                    .is_some_and(|p| mask.contains(lightcraft_geom::Point::new(p[0] / frame.ow, p[1] / frame.oh)))
            },
        ) {
            *m = 255;
        }
    }
    let mut protected = vec![false; mask.len()];
    let protection_segments = refinements.iter().fold(0usize, |n, (spot, _)| n.saturating_add(spot.points.len().max(1)));
    if protection_segments.saturating_mul(mask.len()) > 64_000_000 {
        return Err("Selection protection is too complex. Try fewer correction strokes.".into());
    }
    // Replay ordered corrections with the same frozen lens/orientation mapping.
    // Addition strokes deliberately release protection established by earlier subtraction.
    for (spot, subtract) in refinements {
        let support = lightcraft_pipeline::ai_edits::brush_support(&frame, spot, true);
        let brush = lightcraft_pipeline::ai_edits::BrushSupport::new(&support, full.width, full.height);
        for (i, protected) in protected.iter_mut().enumerate() {
            if i % 1024 == 0 && cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let point = [
                (x0 + ((i % w) as f64 + 0.5) * (x1 - x0) / w as f64) / full.width as f64,
                (y0 + ((i / w) as f64 + 0.5) * (y1 - y0) / h as f64) / full.height as f64,
            ];
            if brush.alpha(point, full.width, full.height) > 0.0 {
                *protected = *subtract;
            }
        }
    }
    if !mask.contains(&255) {
        return Err("The selected mask is empty.".into());
    }
    Ok(PreparedInput {
        request: runner::RetouchInput { width: w, height: h, rgb: src.data, mask, rect, seed, steps: 20, effects: false, protected },
        support: segmented.is_none().then_some(support),
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_develop::{EmbeddedLens, EmbeddedWarp, SegMask};
    use lightcraft_geom::Point;
    fn source(info: lightcraft_pipeline::SourceInfo) -> crate::media::SourceRef {
        crate::media::SourceRef::Loaded(Box::new(crate::media::DecodedSource::new(
            std::sync::Arc::new(lightcraft_raster::Rgb32f::from_fn(128, 96, |_, _| [0.1, 0.2, 0.3])),
            Some(info),
        )))
    }
    #[test]
    fn segmented_source_mask_uses_resolved_lens_and_persisted_alpha_preserves_outside_pixels() {
        let info = lightcraft_pipeline::SourceInfo {
            lens: Some(EmbeddedLens {
                warp: Some(EmbeddedWarp { planes: [[1.0, 0.22, -0.015, 0.0, 0.0, 0.0]; 3], center: Point::new(0.5, 0.5), radius: 0.5 }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let mut d = DevelopSettings::default();
        d.optics.lens_profile = true;
        d.optics.remove_ca = true;
        d.geometry.vertical = 12.;
        d.geometry.horizontal = -8.;
        d.orientation = lightcraft_geom::Orientation::Rotate90;
        let image = lightcraft_raster::Rgb32f::from_fn(128, 96, |_, _| [0.1, 0.2, 0.3]);
        let frame = lightcraft_pipeline::frame_for(&image, &info, &d, false);
        assert!(frame.warp.as_ref().unwrap().lens.is_some());
        let logits: Vec<f32> = (0..64 * 64).map(|i| if (20..38).contains(&(i % 64)) && (14..45).contains(&(i / 64)) { 8. } else { -8. }).collect();
        let seg = SegMask::from_logits(64, &logits);
        let mask = super::super::SelectionMask::new(&seg).unwrap();
        let prepared = prepare_segmented(source(info.clone()), info.clone(), d.clone(), seg.clone(), 7, &AtomicBool::new(false)).unwrap();
        assert!(prepared.support.is_none());
        assert_eq!(prepared.request.rect, [0., 0., 1., 1.]);
        let support = lightcraft_pipeline::ai_edits::brush_support(&frame, &Spot::default(), true);
        let mapping = lightcraft_pipeline::ai_edits::BrushSupport::new(&support, 128, 96);
        let mut different_from_unwarped = 0;
        for (i, a) in prepared.request.mask.iter().enumerate() {
            let source = [((i % 128) as f64 + 0.5) / 128., ((i / 128) as f64 + 0.5) / 96.];
            let expected = mapping.brush_point(source, 128, 96).is_some_and(|p| mask.contains(Point::new(p[0] / frame.ow, p[1] / frame.oh)));
            assert_eq!(*a == 255, expected);
            if expected != mask.contains(Point::new(source[0], source[1])) {
                different_from_unwarped += 1;
            }
        }
        assert!(different_from_unwarped > 100);
        let authorization = prepared.request.mask.clone();
        let output = super::super::runner::RetouchOutput {
            coverage: None,
            width: 128,
            height: 96,
            rgb: vec![[0.8, 0.7, 0.6]; 128 * 96],
            provenance: super::super::runner::RetouchProvenance {
                model: "test".into(),
                revision: "1".into(),
                identity: super::super::runner::RetouchIdentity { model: "test".into(), runtime: Default::default(), adapter: "lama" },
            },
        };
        let edit = super::super::patch::construct(prepared, output).unwrap();
        let saved: lightcraft_develop::AiEdit = serde_json::from_value(serde_json::to_value(edit).unwrap()).unwrap();
        let clean_settings = d.clone();
        d.ai_edits.push(saved);
        let result = lightcraft_pipeline::ai_edits::source_with_edits(&image, &d);
        for (i, p) in result.data.iter().enumerate() {
            assert_eq!(*p, if authorization[i] == 0 { [0.1, 0.2, 0.3] } else { [0.8, 0.7, 0.6] });
        }
        // Real lens mapping also governs subtractive protection outside the object.
        let subtract = Spot { points: vec![Point::new(0.7, 0.5)], size: 0.08, ..Default::default() };
        let refinements = vec![(subtract.clone(), true)];
        let mut prepared = prepare_segmented_protected(
            source(info.clone()),
            info.clone(),
            clean_settings.clone(),
            seg.clone(),
            &refinements,
            7,
            &AtomicBool::new(false),
        )
        .unwrap();
        prepared.request.effects = true;
        let support = lightcraft_pipeline::ai_edits::brush_support(&frame, &subtract, true);
        let protection = lightcraft_pipeline::ai_edits::BrushSupport::new(&support, 128, 96);
        for (i, protected) in prepared.request.protected.iter().enumerate() {
            let p = [((i % 128) as f64 + 0.5) / 128., ((i / 128) as f64 + 0.5) / 96.];
            assert_eq!(*protected, protection.alpha(p, 128, 96) > 0.0);
        }
        assert!(prepared.request.protected.iter().any(|v| *v));
        let mut coverage = authorization.clone();
        // Synthetic model identifies effects in a known source-space strip outside object.
        for (i, alpha) in coverage.iter_mut().enumerate() {
            if (70..105).contains(&(i % 128)) {
                *alpha = 255;
            }
        }
        assert!(coverage.iter().zip(&authorization).any(|(a, b)| *a == 255 && *b == 0));
        let protected = prepared.request.protected.clone();
        let output = super::super::runner::RetouchOutput {
            width: 128,
            height: 96,
            rgb: vec![[0.8, 0.7, 0.6]; 128 * 96],
            coverage: Some(coverage.clone()),
            provenance: super::super::runner::RetouchProvenance {
                model: "test".into(),
                revision: "1".into(),
                identity: super::super::runner::RetouchIdentity { model: "test".into(), runtime: Default::default(), adapter: "test" },
            },
        };
        let edit = super::super::patch::construct(prepared, output).unwrap();
        let saved = serde_json::from_value(serde_json::to_value(edit).unwrap()).unwrap();
        let mut settings = clean_settings.clone();
        settings.ai_edits.push(saved);
        let result = lightcraft_pipeline::ai_edits::source_with_edits(&image, &settings);
        for (i, p) in result.data.iter().enumerate() {
            assert_eq!(*p, if coverage[i] == 0 || protected[i] { [0.1, 0.2, 0.3] } else { [0.8, 0.7, 0.6] });
        }
        // Later additions explicitly release earlier protection.
        let restored = prepare_segmented_protected(
            source(info.clone()),
            info,
            clean_settings,
            seg,
            &[(subtract.clone(), true), (subtract, false)],
            7,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(restored.request.protected.iter().all(|v| !v));
    }
    #[test]
    fn protection_work_budget_counts_segments_not_only_strokes() {
        let stroke = Spot { points: vec![Point::new(0.5, 0.5); 4096], size: 0.04, ..Default::default() };
        let error = prepare_segmented_protected(
            source(Default::default()),
            Default::default(),
            Default::default(),
            SegMask::from_logits(1, &[8.]),
            &[(stroke.clone(), true), (stroke, false)],
            0,
            &AtomicBool::new(false),
        )
        .err()
        .unwrap();
        assert!(error.contains("protection is too complex"));
    }
    #[test]
    fn empty_or_corrupt_segment_masks_are_rejected() {
        for seg in [
            SegMask::from_logits(1, &[-8.]),
            SegMask { side: 1025, data: String::new(), rect: None },
            SegMask { side: 1, data: "bad".into(), rect: None },
        ] {
            assert!(prepare_segmented(source(Default::default()), Default::default(), Default::default(), seg, 0, &AtomicBool::new(false)).is_err());
        }
    }
}
