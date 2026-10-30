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
        let support = edit.support.as_ref().map(|s| {
            (
                s.points.iter().map(|p| [p[0] * src.width as f64, p[1] * src.height as f64]).collect::<Vec<_>>(),
                s.radius * src.width.max(src.height) as f64,
                s.feather,
                s.continuous,
            )
        });
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
                if let Some((points, radius, feather, continuous)) = &support {
                    let distance = lightcraft_develop::ai_edit::brush_distance(points, [x as f64 + 0.5, y as f64 + 0.5], *continuous);
                    if distance >= *radius {
                        continue;
                    }
                    if *feather > 0.0 {
                        let t = ((radius - distance) / (radius * feather)).clamp(0.0, 1.0);
                        alpha *= (t * t * (3.0 - 2.0 * t)) as f32;
                    }
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
    #[test]
    fn continuous_support_fills_sparse_midpoint_and_preserves_legacy_circles() {
        let src = Arc::new(Rgb32f::from_fn(64, 64, |_, _| [0.0, 0.0, -0.0]));
        let mut s = settings();
        let mut edit = s.ai_edits.remove(0);
        edit.rect = [0.0, 0.0, 1.0, 1.0];
        edit.width = 1;
        edit.height = 1;
        edit.support = Some(lightcraft_develop::ai_edit::AiBrushSupport {
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
        edit.support =
            Some(lightcraft_develop::ai_edit::AiBrushSupport { continuous: false, points: vec![[0.5, 0.5]], radius: 17.0 / 1536.0, feather: 0.0 });
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
