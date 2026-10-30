//! Source-space raster retouches. Compose before geometry so crop, orientation, perspective,
//! distortion and per-channel lens corrections affect original and generated pixels identically.
//! Model inference is deliberately absent. Source buffers and originals are never mutated.
use std::borrow::Cow;

use lightcraft_develop::DevelopSettings;
use lightcraft_raster::Rgb32f;

/// Whether sampling needs the CPU raster compositor. Disabled edits do not force a fallback.
pub fn active(s: &DevelopSettings) -> bool {
    s.ai_edits.iter().any(|e| e.enabled)
}

/// Compose enabled edits, in stored order, into a temporary scene-linear source.
/// The caller should validate incoming edits before persisting them. A damaged stack contributes
/// nothing; malformed raster payloads are skipped defensively, without a panic or model rerun.
pub fn source_with_edits<'a>(src: &'a Rgb32f, s: &DevelopSettings) -> Cow<'a, Rgb32f> {
    if !active(s) || lightcraft_develop::ai_edit::validate_stack(&s.ai_edits).is_err() || src.width == 0 || src.height == 0 {
        return Cow::Borrowed(src);
    }
    let mut out = Cow::Borrowed(src);
    for edit in s.ai_edits.iter().filter(|e| e.enabled) {
        let Ok(pixels) = edit.decode_pixels() else { continue };
        let r = edit.rect;
        let support = edit.support.as_ref().map(|s| BrushSupport::new(s, src.width, src.height));
        // Visit only source pixels whose centres fall inside the rectangle. Feather is inward
        // only: exact zero alpha and pixels outside source brush support never write.
        let x0 = ((r[0] * src.width as f64 - 0.5).ceil().max(0.0) as usize).min(src.width);
        let x1 = ((r[2] * src.width as f64 - 0.5).ceil().max(0.0) as usize).min(src.width);
        let y0 = ((r[1] * src.height as f64 - 0.5).ceil().max(0.0) as usize).min(src.height);
        let y1 = ((r[3] * src.height as f64 - 0.5).ceil().max(0.0) as usize).min(src.height);
        for y in y0..y1 {
            for x in x0..x1 {
                let u = ((x as f64 + 0.5) / src.width as f64 - r[0]) / (r[2] - r[0]);
                let v = ((y as f64 + 0.5) / src.height as f64 - r[1]) / (r[3] - r[1]);
                // Cell sampling preserves the stored mask support (interpolation would spread
                // nonzero alpha into neighbouring zero-mask cells). The final shared geometry
                // resampler performs filtering for preview/export just like the source image.
                let px = ((u * edit.width as f64).floor() as usize).min(edit.width as usize - 1);
                let py = ((v * edit.height as f64).floor() as usize).min(edit.height as usize - 1);
                let Some(index) = py.checked_mul(edit.width as usize).and_then(|i| i.checked_add(px)) else { continue };
                let Some(rgba) = pixels.get(index).filter(|p| p[3] > 0.0) else { continue };
                let mut alpha = rgba[3];
                if let Some(support) = &support {
                    alpha *= support.alpha([(x as f64 + 0.5) / src.width as f64, (y as f64 + 0.5) / src.height as f64], src.width, src.height);
                }
                if alpha <= 0.0 {
                    continue;
                }
                let Some(index) = y.checked_mul(src.width).and_then(|i| i.checked_add(x)) else { continue };
                let Some(old) = out.data.get(index).copied() else { continue };
                let rgb = std::array::from_fn(|c| if alpha == 1.0 { rgba[c] } else { old[c] * (1.0 - alpha) + rgba[c] * alpha });
                if let Some(dst) = out.to_mut().data.get_mut(index) {
                    *dst = rgb;
                }
            }
        }
    }
    out
}

/// Shared CPU geometry entry point, including raster edits. The GPU uses this path when a
/// retouch is active; later stages may still execute on the GPU.
pub fn sample(src: &Rgb32f, s: &DevelopSettings, frame: &crate::geometry::Frame, w: usize, h: usize) -> Rgb32f {
    frame.sample(&source_with_edits(src, s), w, h)
}

/// Map existing mask/spot coordinates (transformed whole-image normalized coordinates) to
/// the source grid used by raster retouches. Green-channel geometry defines the mask; the
/// generated RGB then follows each channel's actual correction during normal resampling.
pub fn transformed_to_source_norm(frame: &crate::geometry::Frame, point: lightcraft_geom::Point) -> Option<lightcraft_geom::Point> {
    let mut p = lightcraft_geom::Point::new(point.x * frame.ow, point.y * frame.oh);
    if let Some(warp) = &frame.warp {
        p = warp.to_source(p, 1);
    }
    let (x, y) = frame.orient.inverse().map(p.x, p.y, frame.ow, frame.oh);
    let p = lightcraft_geom::Point::new(x / frame.src_w as f64, y / frame.src_h as f64);
    (p.x.is_finite() && p.y.is_finite()).then_some(p)
}

/// Prepared exact authorization mask. The frozen warp is independent of current settings.
pub struct BrushSupport<'a> {
    support: &'a lightcraft_develop::ai_edit::AiBrushSupport,
    warp: Option<crate::optics::Warp>,
    points: Vec<[f64; 2]>,
    valid: bool,
}
impl<'a> BrushSupport<'a> {
    pub fn new(support: &'a lightcraft_develop::ai_edit::AiBrushSupport, width: usize, height: usize) -> Self {
        let warp = support.geometry.as_ref().map(|g| {
            let [sw, sh] = g.source_size.map(f64::from);
            let (w, h) = if g.orientation.swaps_axes() { (sh, sw) } else { (sw, sh) };
            let mut warp = crate::optics::Warp::identity(w, h);
            warp.persp = g.perspective;
            warp.persp_inv = g.perspective.inverse().unwrap_or(lightcraft_geom::Homography::IDENTITY);
            warp.k1 = g.distortion;
            warp.lens = g.lens;
            warp.lens_dist = g.lens_distortion;
            warp
        });
        let (w, h) = warp.as_ref().map_or((width as f64, height as f64), |w| (w.w, w.h));
        let points = support.points.iter().map(|p| [p[0] * w, p[1] * h]).collect();
        let valid = support.geometry.as_ref().is_none_or(|g| g.valid());
        Self { support, warp, points, valid }
    }

    /// Source-normalized point → frozen transformed brush coordinates. Failed inversions
    /// authorize nothing; a root is accepted only after a subpixel round-trip check.
    pub fn brush_point(&self, source: [f64; 2], width: usize, height: usize) -> Option<[f64; 2]> {
        let Some(g) = &self.support.geometry else { return Some([source[0] * width as f64, source[1] * height as f64]) };
        if !self.valid {
            return None;
        }
        let warp = self.warp.as_ref()?;
        let (x, y) = g.orientation.map(
            source[0] * g.source_size[0] as f64,
            source[1] * g.source_size[1] as f64,
            g.source_size[0] as f64,
            g.source_size[1] as f64,
        );
        let p = warp.source_to_transformed(lightcraft_geom::Point::new(x, y))?;
        Some([p.x, p.y])
    }

    pub fn alpha(&self, source: [f64; 2], width: usize, height: usize) -> f32 {
        let Some(p) = self.brush_point(source, width, height) else { return 0.0 };
        let (w, h) = self.warp.as_ref().map_or((width as f64, height as f64), |w| (w.w, w.h));
        // Points remain in their original transformed coordinate system; nonlinear lens
        // maps are evaluated for each source sample, never approximated by brush centres.
        let distance = lightcraft_develop::ai_edit::brush_distance(&self.points, p, self.support.continuous);
        let radius = self.support.radius * w.max(h);
        // An inward guard is much smaller than a source pixel and larger than the inverse
        // round-trip tolerance, preventing numerical boundary noise from expanding support.
        let guard = if self.warp.is_some() { w.max(h) * 1e-7 } else { 0.0 };
        if !distance.is_finite() || distance >= radius - guard {
            return 0.0;
        }
        if self.support.feather <= 0.0 {
            return 1.0;
        }
        let t = ((radius - distance) / (radius * self.support.feather)).clamp(0.0, 1.0);
        (t * t * (3.0 - 2.0 * t)) as f32
    }
}

/// Freeze the resolved embedded lens/perspective, rather than looking up changing profiles
/// or copying a radius into source space. Crop/straighten/flips are already resolved by UI
/// coordinates before a selection reaches this layer.
pub fn brush_support(
    frame: &crate::geometry::Frame,
    spot: &lightcraft_develop::Spot,
    continuous: bool,
) -> lightcraft_develop::ai_edit::AiBrushSupport {
    let warp = frame.warp.clone().unwrap_or_else(|| crate::optics::Warp::identity(frame.ow, frame.oh));
    lightcraft_develop::ai_edit::AiBrushSupport {
        geometry: Some(lightcraft_develop::ai_edit::AiSupportGeometry {
            orientation: frame.orient,
            source_size: [frame.src_w as u32, frame.src_h as u32],
            perspective: warp.persp,
            distortion: warp.k1,
            lens: warp.lens.map(|mut lens| {
                lens.vignette = None;
                lens
            }),
            lens_distortion: warp.lens_dist,
        }),
        points: spot.points.iter().map(|p| [p.x, p.y]).collect(),
        radius: spot.size,
        feather: 0.15,
        continuous,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{OutputDepth, RenderRequest, SourceInfo, StageCache};
    use lightcraft_develop::AiEdit;
    use lightcraft_geom::{Orientation, Point, Rect};
    use std::sync::Arc;

    fn settings() -> DevelopSettings {
        let mut s = DevelopSettings::default();
        s.ai_edits.push(
            AiEdit::from_pixels(
                AiEdit {
                    id: 1,
                    enabled: true,
                    rect: [0.25, 0.25, 0.75, 0.75],
                    support: None,
                    width: 2,
                    height: 2,
                    pixels: String::new(),
                    model: "test".into(),
                    model_version: "1".into(),
                    seed: 42,
                },
                &[[0.8, 0.2, 0.1, 0.0], [0.8, 0.2, 0.1, 1.0], [0.8, 0.2, 0.1, 0.5], [0.8, 0.2, 0.1, 0.0]],
            )
            .unwrap(),
        );
        s
    }
    fn radial_lens() -> lightcraft_develop::EmbeddedLens {
        lightcraft_develop::EmbeddedLens {
            warp: Some(lightcraft_develop::EmbeddedWarp {
                planes: [[1.0, 0.22, -0.015, 0.0, 0.0, 0.0]; 3],
                center: Point::new(0.5, 0.5),
                radius: 0.5,
            }),
            ..Default::default()
        }
    }

    #[test]
    fn embedded_edge_brush_has_exact_nonlinear_source_support_and_survives_geometry_changes() {
        let src = Rgb32f::from_fn(320, 200, |x, y| [x as f32 / 1000.0, y as f32 / 1000.0, -0.0]);
        let mut s = DevelopSettings::default();
        s.optics.lens_profile = true;
        let frame = crate::geometry::Frame::with_lens(320, 200, &s, false, Some(&radial_lens()));
        let spot = lightcraft_develop::Spot { points: vec![Point::new(0.85, 0.23), Point::new(0.87, 0.45)], size: 0.055, ..Default::default() };
        let support = brush_support(&frame, &spot, true);
        let mut edit = settings().ai_edits.remove(0);
        edit.rect = [0.0, 0.0, 1.0, 1.0];
        edit.width = 1;
        edit.height = 1;
        edit.support = Some(support.clone());
        s.ai_edits.push(AiEdit::from_pixels(edit, &[[0.9, 0.8, 0.7, 1.0]]).unwrap());
        let restored = DevelopSettings::from_json(&s.to_json()).unwrap();
        assert_eq!(restored, s);
        let result = source_with_edits(&src, &restored).into_owned();
        let mut changed = 0;
        let mut feathered = 0;
        // Independent scalar bisection of the synthetic radial polynomial is the oracle,
        // not the production numerical inverse or a mapped-centre circle approximation.
        let points = [[272.0, 46.0], [278.4, 90.0]];
        for y in 0..200 {
            for x in 0..320 {
                let (dx, dy) = (x as f64 + 0.5 - 160.0, y as f64 + 0.5 - 100.0);
                let source_r = dx.hypot(dy) / 160.0;
                let (mut lo, mut hi) = (0.0_f64, 2.0_f64);
                for _ in 0..60 {
                    let r = (lo + hi) * 0.5;
                    if r * (1.0 + 0.22 * r * r - 0.015 * r.powi(4)) < source_r {
                        lo = r;
                    } else {
                        hi = r;
                    }
                }
                let scale = if source_r > 0.0 { (lo + hi) * 0.5 / source_r } else { 1.0 };
                let distance = lightcraft_develop::ai_edit::brush_distance(&points, [160.0 + dx * scale, 100.0 + dy * scale], true);
                let radius = 17.6;
                if distance >= radius {
                    assert_eq!(result.get(x, y).map(f32::to_bits), src.get(x, y).map(f32::to_bits), "outside {x},{y}");
                } else {
                    let t = ((radius - distance) / (radius * 0.15)).clamp(0.0, 1.0);
                    let alpha = (t * t * (3.0 - 2.0 * t)) as f32;
                    let expected = src.get(x, y)[0] * (1.0 - alpha) + 0.9 * alpha;
                    assert!((result.get(x, y)[0] - expected).abs() < 1e-5, "inside {x},{y}");
                    changed += 1;
                    if alpha < 1.0 {
                        feathered += 1;
                    }
                }
            }
        }
        assert!(changed > 100 && feathered > 10);
        let key = crate::ai_edits_key(&restored);
        let mut later = restored.clone();
        later.orientation = Orientation::Rotate90;
        later.optics.lens_profile = false;
        later.optics.distortion = -30.0;
        later.geometry.vertical = 20.0;
        later.crop.geometry.rect = Rect::new(0.1, 0.2, 0.8, 0.9);
        assert_eq!(source_with_edits(&src, &later).data, result.data, "saved pixels stay attached to original source");
        later.ai_edits[0].support.as_mut().unwrap().geometry.as_mut().unwrap().distortion = 0.01;
        assert_ne!(crate::ai_edits_key(&later), key);
        assert_ne!(later.hash64(), restored.hash64());
    }

    #[test]
    fn frozen_geometry_handles_orientation_perspective_crop_and_gain_only() {
        for orientation in [Orientation::Normal, Orientation::Rotate90, Orientation::FlipH, Orientation::Transverse] {
            let mut s = DevelopSettings { orientation, ..Default::default() };
            s.geometry.vertical = 12.0;
            s.geometry.horizontal = -9.0;
            s.optics.distortion = 10.0;
            s.optics.lens_profile = true;
            s.crop.geometry.rect = Rect::new(0.15, 0.2, 0.9, 0.85);
            s.crop.geometry.angle = 4.0;
            s.crop.flip_h = true;
            let frame = crate::geometry::Frame::with_lens(320, 200, &s, true, Some(&radial_lens()));
            let p = frame.out_to_norm(200, 120).apply(Point::new(153.0, 38.0));
            let spot = lightcraft_develop::Spot { points: vec![p], size: 0.02, ..Default::default() };
            let support = brush_support(&frame, &spot, true);
            let brush = BrushSupport::new(&support, 320, 200);
            let src = transformed_to_source_norm(&frame, p).unwrap();
            let back = brush.brush_point([src.x, src.y], 320, 200).unwrap();
            assert!((back[0] - p.x * frame.ow).hypot(back[1] - p.y * frame.oh) < 1e-5);
            assert_eq!(brush.alpha([src.x, src.y], 320, 200), 1.0);
            let preview_brush = BrushSupport::new(&support, 80, 50);
            for point in [[src.x, src.y], [src.x + 0.01, src.y - 0.02], [0.1, 0.1]] {
                assert_eq!(
                    brush.alpha(point, 320, 200),
                    preview_brush.alpha(point, 80, 50),
                    "normalized support must not move between fit and full resolution"
                );
                assert_eq!(brush.brush_point(point, 320, 200), preview_brush.brush_point(point, 80, 50));
            }
        }
        let mut s = DevelopSettings::default();
        s.optics.vignetting = 80.0;
        s.optics.ca_red = 50.0;
        s.optics.ca_blue = -40.0;
        s.optics.remove_ca = true;
        let frame = crate::geometry::Frame::new(320, 200, &s, false);
        assert!(frame.warp.is_some());
        let spot = lightcraft_develop::Spot { points: vec![Point::new(0.8, 0.3)], size: 0.03, ..Default::default() };
        let support = brush_support(&frame, &spot, true);
        assert_eq!(BrushSupport::new(&support, 320, 200).brush_point([0.8, 0.3], 320, 200), Some([256.0, 60.0]));
        let mut legacy = support.clone();
        legacy.geometry = None;
        for y in 0..200 {
            for x in 0..320 {
                let point = [(x as f64 + 0.5) / 320.0, (y as f64 + 0.5) / 200.0];
                assert_eq!(BrushSupport::new(&support, 320, 200).alpha(point, 320, 200), BrushSupport::new(&legacy, 320, 200).alpha(point, 320, 200));
            }
        }
    }

    #[test]
    fn frozen_geometry_rejects_malformed_snapshot_and_nonconvergence_safely() {
        let frame = crate::geometry::Frame::new(80, 40, &DevelopSettings::default(), false);
        let spot = lightcraft_develop::Spot { points: vec![Point::new(0.5, 0.5)], ..Default::default() };
        let mut support = brush_support(&frame, &spot, true);
        support.geometry.as_mut().unwrap().perspective = lightcraft_geom::Homography([0.0; 9]);
        assert_eq!(BrushSupport::new(&support, 80, 40).alpha([0.5, 0.5], 80, 40), 0.0);
        let mut s = settings();
        s.ai_edits[0].support = Some(support);
        assert!(DevelopSettings::from_json(&s.to_json()).is_err());
        let mut warp = crate::optics::Warp::identity(80.0, 40.0);
        warp.k1 = f64::NAN;
        assert!(warp.source_to_transformed(Point::new(75.0, 35.0)).is_none());
        warp.k1 = -1.0;
        assert!(warp.source_to_transformed(Point::new(300.0, 300.0)).is_none());
    }

    #[test]
    fn nonlinear_interval_bounds_cover_entire_transformed_selection_rectangle() {
        let mut s = DevelopSettings::default();
        s.optics.lens_profile = true;
        s.optics.distortion = 40.0;
        s.geometry.horizontal = 15.0;
        let frame = crate::geometry::Frame::with_lens(320, 200, &s, false, Some(&radial_lens()));
        let warp = frame.warp.unwrap();
        let rect = [200.0, 20.0, 300.0, 140.0];
        let b = warp.source_bounds(rect).unwrap();
        for y in 0..101 {
            for x in 0..101 {
                let p = warp.to_source(Point::new(200.0 + x as f64, 20.0 + y as f64 * 1.2), 1);
                assert!(p.x >= b[0] && p.y >= b[1] && p.x <= b[2] && p.y <= b[3]);
            }
        }
    }

    #[test]
    fn continuous_support_fills_sparse_midpoint_and_preserves_legacy_circles() {
        let src = Arc::new(Rgb32f::from_fn(64, 64, |_, _| [0.0, 0.0, -0.0]));
        let mut s = settings();
        let mut edit = s.ai_edits.remove(0);
        edit.rect = [0.0, 0.0, 1.0, 1.0];
        edit.width = 1;
        edit.height = 1;
        edit.support = Some(lightcraft_develop::ai_edit::AiBrushSupport {
            geometry: None,
            points: vec![[16.5 / 64.0, 32.5 / 64.0], [48.5 / 64.0, 32.5 / 64.0]],
            radius: 5.0 / 64.0,
            feather: 0.2,
            continuous: false,
        });
        s.ai_edits.push(AiEdit::from_pixels(edit, &[[1.0, 1.0, 1.0, 1.0]]).unwrap());
        let legacy_json = s.to_json();
        assert!(legacy_json["ai_edits"][0]["support"].get("continuous").is_none());
        let legacy = DevelopSettings::from_json(&legacy_json).unwrap();
        let circles = source_with_edits(&src, &legacy);
        assert_eq!(circles.get(32, 32).map(f32::to_bits), src.get(32, 32).map(f32::to_bits));
        s.ai_edits[0].support.as_mut().unwrap().continuous = true;
        assert_ne!(legacy.hash64(), s.hash64(), "continuous support contributes to settings identity");
        assert_ne!(crate::ai_edits_key(&legacy), crate::ai_edits_key(&s), "geometry cache includes support mode");
        let cache = StageCache::default();
        let info = SourceInfo::default();
        let req = RenderRequest::fit(64, 64);
        let old_render = crate::render_cached(&src, &info, &legacy, &req, &cache);
        let new_render = crate::render_cached(&src, &info, &s, &req, &cache);
        assert_ne!(old_render.image.data, new_render.image.data);
        assert_eq!(new_render.image.data, crate::render(&src, &info, &s, &req).image.data);
        let result = source_with_edits(&src, &s);
        assert_eq!(result.get(32, 32), [1.0; 3]);
        assert_eq!(result.get(16, 32), [1.0; 3]);
        for y in 0..64 {
            for x in 0..64 {
                let support = s.ai_edits[0].support.as_ref().unwrap();
                let points: Vec<_> = support.points.iter().map(|p| [p[0] * 64.0, p[1] * 64.0]).collect();
                if lightcraft_develop::ai_edit::brush_distance(&points, [x as f64 + 0.5, y as f64 + 0.5], true) >= 5.0 {
                    assert_eq!(result.get(x, y).map(f32::to_bits), src.get(x, y).map(f32::to_bits));
                }
            }
        }
        assert_eq!(legacy.to_json(), legacy_json, "missing flag retains old serialization and semantics");
    }
    #[test]
    fn feather_blends_strictly_inward_and_preserves_full_strength_interior() {
        let src = Rgb32f::from_fn(64, 64, |_, _| [0.0, 0.0, -0.0]);
        let mut s = settings();
        let mut edit = s.ai_edits.remove(0);
        edit.rect = [0.0, 0.0, 1.0, 1.0];
        edit.width = 1;
        edit.height = 1;
        edit.support = Some(lightcraft_develop::ai_edit::AiBrushSupport {
            geometry: None,
            continuous: false,
            points: vec![[32.5 / 64.0, 32.5 / 64.0]],
            radius: 10.0 / 64.0,
            feather: 0.5,
        });
        // Even deliberately overbroad raster alpha cannot extend the source brush.
        s.ai_edits.push(AiEdit::from_pixels(edit, &[[1.0, 1.0, 1.0, 1.0]]).unwrap());
        let s = DevelopSettings::from_json(&s.to_json()).unwrap();
        let result = source_with_edits(&src, &s);
        let mut feathered = 0;
        for y in 0..64 {
            for x in 0..64 {
                let distance = (x as f64 - 32.0).hypot(y as f64 - 32.0);
                if distance >= 10.0 {
                    assert_eq!(result.get(x, y).map(f32::to_bits), src.get(x, y).map(f32::to_bits));
                } else if distance <= 5.0 {
                    assert_eq!(result.get(x, y), [1.0; 3]);
                } else {
                    let t = (10.0 - distance) / 5.0;
                    let expected = (t * t * (3.0 - 2.0 * t)) as f32;
                    assert!((result.get(x, y)[0] - expected).abs() < 1e-6);
                    assert!(result.get(x, y)[0] > 0.0 && result.get(x, y)[0] < 1.0);
                    feathered += 1;
                }
            }
        }
        assert!(feathered > 0);
        let mut hard = s.clone();
        hard.ai_edits[0].support.as_mut().unwrap().feather = 0.0;
        assert_eq!(source_with_edits(&src, &hard).get(40, 32), [1.0; 3]);
        assert!((result.get(40, 32)[0] - 0.352).abs() < 1e-6);
    }
    #[test]
    fn downsampled_patch_cannot_expand_original_brush_support() {
        let src = Rgb32f::from_fn(1536, 96, |_, _| [0.1, 0.2, -0.0]);
        let mut s = settings();
        let mut edit = s.ai_edits.remove(0);
        edit.rect = [0.0, 0.0, 1.0, 1.0];
        edit.width = 256;
        edit.height = 16;
        edit.support = Some(lightcraft_develop::ai_edit::AiBrushSupport {
            geometry: None,
            continuous: false,
            points: vec![[0.5, 0.5]],
            radius: 17.0 / 1536.0,
            feather: 0.0,
        });
        // A coarse boundary cell has a centre inside the brush but extends outside.
        let pixels: Vec<_> = (0..16)
            .flat_map(|y| {
                (0..256).map(move |x| {
                    let inside = (x as f64 * 6.0 + 3.0 - 768.0).hypot(y as f64 * 6.0 + 3.0 - 48.0) < 17.0;
                    [0.8, 0.7, 0.6, if inside { 1.0 } else { 0.0 }]
                })
            })
            .collect();
        s.ai_edits.push(AiEdit::from_pixels(edit, &pixels).unwrap());
        let restored = DevelopSettings::from_json(&s.to_json()).unwrap();
        let result = source_with_edits(&src, &restored);
        let mut changed = 0;
        for y in 0..96 {
            for x in 0..1536 {
                if (x as f64 + 0.5 - 768.0).hypot(y as f64 + 0.5 - 48.0) >= 17.0 {
                    assert_eq!(result.get(x, y).map(f32::to_bits), src.get(x, y).map(f32::to_bits));
                } else if result.get(x, y) != src.get(x, y) {
                    changed += 1;
                }
            }
        }
        assert!(changed > 0);
    }
    #[test]
    fn zero_alpha_and_outside_rectangle_are_bit_exact_and_disabled_is_borrowed() {
        let src = Rgb32f::from_fn(8, 8, |x, y| [x as f32 * 0.01, y as f32 * 0.01, -0.0]);
        let mut s = settings();
        let edited = source_with_edits(&src, &s);
        for y in 0..8 {
            for x in 0..8 {
                if !((4..6).contains(&x) && (2..4).contains(&y) || (2..4).contains(&x) && (4..6).contains(&y)) {
                    assert_eq!(src.get(x, y).map(f32::to_bits), edited.get(x, y).map(f32::to_bits), "{x},{y}");
                }
            }
        }
        assert_eq!(edited.get(4, 2), [0.8, 0.2, 0.1]);
        assert_eq!(src.get(4, 2), [0.04, 0.02, -0.0]);
        s.ai_edits[0].enabled = false;
        assert!(matches!(source_with_edits(&src, &s), Cow::Borrowed(_)));
    }
    #[test]
    fn previews_exports_windows_crop_rotation_and_cache_use_the_same_edited_source() {
        let src = Arc::new(Rgb32f::from_fn(32, 24, |x, y| [0.01 * x as f32, 0.01 * y as f32, 0.1]));
        let info = SourceInfo::default();
        let cache = StageCache::default();
        let mut s = settings();
        let mut plain = s.clone();
        plain.ai_edits.clear();
        for orientation in [Orientation::Normal, Orientation::Rotate90, Orientation::FlipH, Orientation::Transverse] {
            s.orientation = orientation;
            s.crop.geometry.rect = Rect::new(0.1, 0.1, 0.9, 0.9);
            s.crop.geometry.angle = 5.0;
            s.optics.distortion = 12.0;
            plain.optics = s.optics;
            plain.orientation = orientation;
            plain.crop = s.crop;
            let replacement = source_with_edits(&src, &s).into_owned();
            for depth in [OutputDepth::U8, OutputDepth::F32Hdr] {
                for edge in [12, 64] {
                    let req = RenderRequest { max_w: edge, max_h: edge, depth, ..RenderRequest::fit(edge, edge) };
                    let actual = crate::render_cached(&src, &info, &s, &req, &cache);
                    let expected = crate::render(&replacement, &info, &plain, &req);
                    assert_eq!(actual.image.data, expected.image.data);
                    assert_eq!(actual.deep, expected.deep);
                    let window_req = RenderRequest { window: Some(crate::PixelWindow { x: 2, y: 1, w: 5, h: 4 }), ..req };
                    let windowed = crate::render_cached(&src, &info, &s, &window_req, &cache);
                    let expected_window = crate::render(&replacement, &info, &plain, &window_req);
                    assert_eq!(windowed.image.data, expected_window.image.data);
                    assert_eq!(windowed.deep, expected_window.deep);
                    let disabled = {
                        let mut d = s.clone();
                        d.ai_edits[0].enabled = false;
                        d
                    };
                    let disabled_render = crate::render_cached(&src, &info, &disabled, &req, &cache);
                    assert_eq!(disabled_render.image.data, crate::render(&src, &info, &plain, &req).image.data);
                }
            }
        }
    }
    #[test]
    fn transformed_points_return_to_source_after_all_user_orientations() {
        for exif in 1..=8 {
            let mut s = settings();
            s.orientation = Orientation::from_exif(exif);
            let frame = crate::geometry::Frame::new(80, 40, &s, false);
            let original = Point::new(0.25, 0.7);
            let (x, y) = s.orientation.map(original.x * 80.0, original.y * 40.0, 80.0, 40.0);
            let back = transformed_to_source_norm(&frame, Point::new(x / frame.ow, y / frame.oh)).unwrap();
            assert!((back.x - original.x).abs() < 1e-12 && (back.y - original.y).abs() < 1e-12);
        }
    }
}
