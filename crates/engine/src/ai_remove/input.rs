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
    let scene = Scene::load(source, &info, &d, spot, segmented, continuous, false, refinements)?;
    scene.prepare(&scene.legacy_context()?, 1024, None, seed, cancel).map(|(prepared, _)| prepared)
}

#[path = "regions.rs"]
mod regions;

struct Scene<'a> {
    full: std::sync::Arc<lightcraft_raster::Rgb32f>,
    frame: lightcraft_pipeline::geometry::Frame,
    spot: Spot,
    segmented: Option<super::SelectionMask>,
    refinements: &'a [(Spot, bool)],
    pure_brush: bool,
    support: AiBrushSupport,
    bounds: [f64; 4],
}

impl<'a> Scene<'a> {
    #[allow(clippy::too_many_arguments)]
    fn load(
        source: crate::media::SourceRef,
        info: &lightcraft_pipeline::SourceInfo,
        d: &DevelopSettings,
        spot: Spot,
        segmented: Option<super::SelectionMask>,
        continuous: bool,
        pure_brush: bool,
        refinements: &'a [(Spot, bool)],
    ) -> Result<Self, String> {
        let decoded = source.load_source()?;
        if decoded.from_proxy {
            return Err("AI Remove needs the original photo online.".into());
        }
        let full = match lightcraft_pipeline::ai_edits::source_with_edits(&decoded.image, d) {
            std::borrow::Cow::Borrowed(_) => decoded.image.clone(),
            std::borrow::Cow::Owned(full) => std::sync::Arc::new(full),
        };
        let frame = lightcraft_pipeline::frame_for(&full, decoded.info.as_ref().unwrap_or(info), d, false);
        let mut bounds = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        let mut add_stroke = |spot: &Spot| -> Result<(), String> {
            let radius = spot.size * full.width.max(full.height) as f64;
            if !radius.is_finite() || radius <= 0.0 || spot.points.is_empty() || spot.points.len() > 4096 {
                return Err("Invalid brush mask.".into());
            }
            for p in &spot.points {
                if !p.x.is_finite() || !p.y.is_finite() {
                    return Err("Invalid brush point.".into());
                }
                let (x, y) = (p.x * frame.ow, p.y * frame.oh);
                bounds = [bounds[0].min(x - radius), bounds[1].min(y - radius), bounds[2].max(x + radius), bounds[3].max(y + radius)];
            }
            Ok(())
        };
        if pure_brush {
            for (stroke, subtract) in refinements {
                if !subtract {
                    add_stroke(stroke)?;
                }
            }
        } else if let Some(mask) = &segmented {
            let b = mask.selected_bounds();
            bounds = [b[0] * frame.ow, b[1] * frame.oh, b[2] * frame.ow, b[3] * frame.oh];
        } else {
            add_stroke(&spot)?;
        }
        let support = lightcraft_pipeline::ai_edits::brush_support(&frame, &spot, continuous);
        Ok(Self { full, frame, spot, segmented, refinements, pure_brush, support, bounds })
    }

    fn source_bounds(&self, bounds: [f64; 4]) -> [f64; 4] {
        let frame = &self.frame;
        let oriented = frame.warp.as_ref().map_or(Some(bounds), |warp| warp.source_bounds(bounds)).unwrap_or([0.0, 0.0, frame.ow, frame.oh]);
        let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
        for (x, y) in [(oriented[0], oriented[1]), (oriented[0], oriented[3]), (oriented[2], oriented[1]), (oriented[2], oriented[3])] {
            let (x, y) = frame.orient.inverse().map(x, y, frame.ow, frame.oh);
            b = [b[0].min(x), b[1].min(y), b[2].max(x), b[3].max(y)];
        }
        b
    }

    fn legacy_context(&self) -> Result<[f64; 4], String> {
        context_rect(self.source_bounds(self.bounds), self.full.width, self.full.height).ok_or_else(|| "The mask is outside the photo.".into())
    }

    fn prepare(
        &self,
        context: &[f64; 4],
        max_side: usize,
        region: Option<(&regions::Plan, usize, [usize; 2])>,
        seed: u32,
        cancel: &AtomicBool,
    ) -> Result<(PreparedInput, Vec<f32>), String> {
        let [x0, y0, x1, y1] = *context;
        let full = &self.full;
        let scale = (max_side as f64 / (x1 - x0).max(y1 - y0)).min(1.0);
        let [w, h] = region.map_or([((x1 - x0) * scale).ceil() as usize, ((y1 - y0) * scale).ceil() as usize], |(_, _, size)| size);
        let scaled = x1 - x0 > w as f64 + 1e-7 || y1 - y0 > h as f64 + 1e-7;
        let rect = [x0 / full.width as f64, y0 / full.height as f64, x1 / full.width as f64, y1 / full.height as f64];
        let mut src = lightcraft_raster::Rgb32f::new(w, h);
        let mut mask = vec![0; src.data.len()];
        let mut alpha = if self.pure_brush || region.is_some_and(|(p, i, _)| p.windows[i].cable) { vec![1.; mask.len()] } else { Vec::new() };
        let mut protected = vec![false; mask.len()];
        let segments = self.refinements.iter().fold(0usize, |n, (s, _)| n.saturating_add(s.points.len().max(1)));
        if !self.pure_brush && segments.saturating_mul(mask.len()) > 64_000_000 {
            return Err("Selection protection is too complex. Try fewer correction strokes.".into());
        }
        let brush = lightcraft_pipeline::ai_edits::BrushSupport::new(&self.support, full.width, full.height);
        let mut strokes: Vec<_> = self.refinements.iter().map(|(spot, subtract)| (spot, true, *subtract)).collect();
        if self.segmented.is_none() && region.is_some() {
            strokes.push((&self.spot, self.support.continuous, false));
        }
        let corrections = StrokeIndex::new([self.frame.ow, self.frame.oh], &strokes)?;
        let source_cells = |a: f64, b: f64, n: usize| ((a - 0.5).ceil().max(0.) as usize).min(n)..((b - 0.5).ceil().max(0.) as usize).min(n);
        let mut checked = 0usize;
        let has_subtractions = self.refinements.iter().any(|(_, subtract)| *subtract);
        for (i, pixel) in src.data.iter_mut().enumerate() {
            if i % 1024 == 0 && cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let (x, y) = (x0 + ((i % w) as f64 + 0.5) * (x1 - x0) / w as f64, y0 + ((i / w) as f64 + 0.5) * (y1 - y0) / h as f64);
            *pixel =
                if region.is_some() { full.sample_bilinear(x as f32, y as f32) } else { full.sample_bilinear((x - 0.5) as f32, (y - 0.5) as f32) };
            let point = [x / full.width as f64, y / full.height as f64];
            if self.segmented.is_none() && region.is_none() && !self.pure_brush && self.refinements.is_empty() {
                mask[i] = if brush.alpha(point, full.width, full.height) > 0. { 255 } else { 0 };
                continue;
            }
            let Some(transformed) = brush.brush_point(point, full.width, full.height) else {
                protected[i] = region.is_some();
                continue;
            };
            let mut owned = true;
            if let Some((plan, index, _)) = region {
                let owner = plan.owner(transformed);
                owned = owner == index;
                if !owned {
                    // Predict the continuation of the cable in the forward
                    // overlap. Its output is context only and is never saved.
                    if owner > index && plan.windows[index].cable && plan.windows[index].group == plan.windows[owner].group {
                        alpha[i] = 0.;
                    } else {
                        protected[i] = true;
                        continue;
                    }
                }
            }
            let correction = corrections.at(transformed);
            protected[i] = correction.is_some_and(|(subtract, _)| subtract);
            if self.pure_brush
                && owned
                && let Some((false, opacity)) = correction
            {
                alpha[i] = opacity;
            }
            let selected = if self.pure_brush || (self.segmented.is_none() && region.is_some()) {
                correction.is_some_and(|(subtract, _)| !subtract)
            } else if let Some(seg) = &self.segmented {
                seg.contains(lightcraft_geom::Point::new(transformed[0] / self.frame.ow, transformed[1] / self.frame.oh))
            } else {
                brush.alpha(point, full.width, full.height) > 0.
            };
            if !owned && !selected {
                protected[i] = true;
            }
            if selected {
                mask[i] = 255;
            }
            // A reduced raster cell may cover several original samples. Leave
            // it transparent if it crosses an authorization or protection edge.
            // Legacy direct brushes already carry exact analytic support.
            if scaled && (region.is_some() || self.pure_brush) && ((selected && self.segmented.is_some()) || has_subtractions) && !protected[i] {
                let dx = (x1 - x0) / w as f64;
                let dy = (y1 - y0) / h as f64;
                let cols = source_cells(x - dx * 0.5, x + dx * 0.5, full.width);
                let rows = source_cells(y - dy * 0.5, y + dy * 0.5, full.height);
                checked = checked.saturating_add(cols.len().saturating_mul(rows.len()));
                if checked > 96_000_000 {
                    return Err("Selection protection is too complex. Try a smaller selection.".into());
                }
                'cell: for row in rows {
                    for col in cols.clone() {
                        let source = [(col as f64 + 0.5) / full.width as f64, (row as f64 + 0.5) / full.height as f64];
                        let Some(p) = brush.brush_point(source, full.width, full.height) else {
                            protected[i] = true;
                            break 'cell;
                        };
                        let correction = corrections.at(p);
                        let authorized = if self.pure_brush {
                            correction.is_some_and(|(sub, _)| !sub)
                        } else {
                            self.segmented
                                .as_ref()
                                .is_none_or(|mask| mask.contains(lightcraft_geom::Point::new(p[0] / self.frame.ow, p[1] / self.frame.oh)))
                        };
                        if correction.is_some_and(|(sub, _)| sub) || (selected && !authorized) {
                            protected[i] = true;
                            break 'cell;
                        }
                    }
                }
            }
        }
        let has_selection = if region.is_some() || self.pure_brush {
            mask.iter().enumerate().any(|(i, &value)| value > 0 && !protected[i] && alpha.get(i).is_none_or(|a| *a > 0.))
        } else {
            mask.contains(&255)
        };
        if !has_selection {
            return Err("The selected mask is empty.".into());
        }
        Ok((
            PreparedInput {
                request: runner::RetouchInput { width: w, height: h, rgb: src.data, mask, rect, seed, steps: 20, effects: false, protected },
                support: (!self.pure_brush && self.segmented.is_none()).then(|| self.support.clone()),
            },
            alpha,
        ))
    }
}

#[path = "stroke_index.rs"]
mod stroke_index;
use stroke_index::StrokeIndex;

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

/// Generate full-source regions as one transaction. Overlapping model windows
/// share a sampling lattice, and every saved patch is bounded by stack capacity.
#[allow(clippy::too_many_arguments)]
pub(super) fn run_local(
    source: crate::media::SourceRef,
    info: lightcraft_pipeline::SourceInfo,
    settings: DevelopSettings,
    spot: Spot,
    segmented: Option<lightcraft_develop::SegMask>,
    pure_brush: bool,
    refinements: &[(Spot, bool)],
    effects: bool,
    continuous: bool,
    seed: u32,
    cancel: &AtomicBool,
    mut infer: impl FnMut(PreparedInput, super::patch::Layout<'_>) -> Result<super::LightingDrafts, String>,
) -> Result<Vec<super::LightingDrafts>, String> {
    use lightcraft_develop::ai_edit::{MAX_EDITS, MAX_SIDE, MAX_TOTAL_PIXELS};
    let slots = MAX_EDITS.saturating_sub(settings.ai_edits.len());
    let used = settings.ai_edits.iter().try_fold(0usize, |sum, e| e.pixel_count().map(|n| sum.saturating_add(n))).map_err(|e| e.to_string())?;
    let budget = MAX_TOTAL_PIXELS.saturating_sub(used);
    if slots == 0 || budget == 0 {
        return Err("AI retouch stack exceeds limits.".into());
    }
    let segmented = segmented.as_ref().map(super::SelectionMask::new).transpose()?;
    let scene = Scene::load(source, &info, &settings, spot, segmented, continuous, pure_brush, refinements)?;
    let strokes: Vec<_> = if pure_brush {
        refinements.iter().filter(|(_, subtract)| !subtract).map(|(s, _)| (s, true)).collect()
    } else if scene.segmented.is_none() {
        vec![(&scene.spot, continuous)]
    } else {
        Vec::new()
    };
    let mask = if pure_brush { None } else { scene.segmented.as_ref() };
    let plan = regions::plan([scene.frame.ow, scene.frame.oh], mask, &strokes, slots);
    if let Some(plan) = plan {
        let mut drafts = Vec::new();
        let mut prior = Vec::new();
        let mut used = 0;
        let mut complete = true;
        let (width, height) = (scene.full.width, scene.full.height);
        let contexts: Vec<_> = plan.windows.iter().map(|w| local_context(scene.source_bounds(w.bounds), width, height, w.cable)).collect();
        let grids = aligned_windows(&contexts, [width, height]);
        for (i, window) in plan.windows.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled.".into());
            }
            let Some((context, size)) = grids[i] else { continue };
            let (mut prepared, alpha) = match scene.prepare(&context, MAX_SIDE, Some((&plan, i, size)), seed, cancel) {
                Ok(result) => result,
                Err(e) if e == "The selected mask is empty." => continue, // conservative planning or entirely subtracted stroke
                Err(e) => return Err(e),
            };
            prepared.request.effects = effects;
            if window.cable {
                for previous in &prior {
                    apply_context(previous, &mut prepared.request, [width, height], cancel)?;
                }
            }
            let draft = infer(prepared, super::patch::Layout { trim: true, alpha: &alpha })?;
            used += draft.raw.pixel_count().map_err(|e| e.to_string())?;
            if used > budget {
                complete = false;
                break;
            }
            if window.cable {
                prior.push((
                    draft.raw.rect,
                    draft.raw.width as usize,
                    draft.raw.height as usize,
                    draft.raw.support.clone(),
                    draft.raw.decode_pixels().map_err(|e| e.to_string())?,
                ));
            }
            drafts.push(draft);
        }
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        if complete && !drafts.is_empty() {
            return Ok(drafts);
        }
    }
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    let max_side = budget.isqrt().min(MAX_SIDE);
    let (mut input, alpha) = scene.prepare(&scene.legacy_context()?, max_side, None, seed, cancel)?;
    input.request.effects = effects;
    infer(input, super::patch::Layout { trim: pure_brush || max_side < MAX_SIDE, alpha: &alpha }).map(|draft| vec![draft])
}

/// Integer source windows preserve native sample centres across overlapping tiles.
fn local_context(bounds: [f64; 4], width: usize, height: usize, cable: bool) -> Option<[f64; 4]> {
    let mut c = context_rect(bounds, width, height)?;
    if cable {
        let core = (bounds[2] - bounds[0]).max(bounds[3] - bounds[1]).ceil();
        let side = (core + 192.).clamp(512., 1024.).max(core + 64.);
        for (i, n) in [width, height].into_iter().enumerate() {
            let extent = side.min(n as f64);
            c[i] = ((bounds[i] + bounds[i + 2] - extent) * 0.5).floor().clamp(0., n as f64 - extent);
            c[i + 2] = c[i] + extent;
        }
    }
    Some(c)
}

/// Use the same sample lattice wherever model contexts meet. This preserves
/// single ownership across cable boundaries even when slots require scaling.
fn aligned_windows(contexts: &[Option<[f64; 4]>], source: [usize; 2]) -> Vec<Option<([f64; 4], [usize; 2])>> {
    let mut scales: Vec<f64> = contexts
        .iter()
        .map(|c| {
            c.map_or(1., |r| {
                let span = (r[2] - r[0]).max(r[3] - r[1]);
                if span <= 1024. { 1. } else { 1022. / span }
            })
        })
        .collect();
    for _ in 0..contexts.len() {
        for (i, a) in contexts.iter().enumerate() {
            for (j, b) in contexts.iter().enumerate().skip(i + 1) {
                if let (Some(a), Some(b)) = (a, b)
                    && a[0] < b[2]
                    && b[0] < a[2]
                    && a[1] < b[3]
                    && b[1] < a[3]
                {
                    let scale = scales[i].min(scales[j]);
                    scales[i] = scale;
                    scales[j] = scale;
                }
            }
        }
    }
    contexts
        .iter()
        .zip(scales)
        .map(|(context, scale)| {
            let mut r = (*context)?;
            let mut size = [0; 2];
            for axis in 0..2 {
                let count = (source[axis] as f64 * scale).floor().max(1.);
                let step = source[axis] as f64 / count;
                let lo = (r[axis] / step).floor().max(0.);
                let hi = (r[axis + 2] / step).ceil().min(count);
                size[axis] = (hi - lo) as usize;
                r[axis] = lo * step;
                r[axis + 2] = (hi * step).min(source[axis] as f64);
            }
            Some((r, size))
        })
        .collect()
}

type PriorTile = ([f64; 4], usize, usize, Option<AiBrushSupport>, Vec<[f32; 4]>);

fn apply_context(previous: &PriorTile, input: &mut runner::RetouchInput, size: [usize; 2], cancel: &AtomicBool) -> Result<(), String> {
    let (bounds, w, h, support, rgba) = previous;
    if input.rect[0] >= bounds[2] || input.rect[2] <= bounds[0] || input.rect[1] >= bounds[3] || input.rect[3] <= bounds[1] {
        return Ok(());
    }
    let support = support.as_ref().map(|s| lightcraft_pipeline::ai_edits::BrushSupport::new(s, size[0], size[1]));
    for (i, pixel) in input.rgb.iter_mut().enumerate() {
        if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let x = input.rect[0] + ((i % input.width) as f64 + 0.5) / input.width as f64 * (input.rect[2] - input.rect[0]);
        let y = input.rect[1] + ((i / input.width) as f64 + 0.5) / input.height as f64 * (input.rect[3] - input.rect[1]);
        if x < bounds[0] || x >= bounds[2] || y < bounds[1] || y >= bounds[3] {
            continue;
        }
        let col = (((x - bounds[0]) / (bounds[2] - bounds[0]) * *w as f64) as usize).min(w - 1);
        let row = (((y - bounds[1]) / (bounds[3] - bounds[1]) * *h as f64) as usize).min(h - 1);
        let p = rgba[row * w + col];
        if p[3] > 0. {
            let a = p[3] * support.as_ref().map_or(1., |s| s.alpha([x, y], size[0], size[1]));
            *pixel = std::array::from_fn(|k| pixel[k] * (1. - a) + p[k] * a);
        }
    }
    Ok(())
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
            SegMask { side: 1025, data: String::new(), rect: None, grid: None },
            SegMask { side: 1, data: "bad".into(), rect: None, grid: None },
        ] {
            assert!(prepare_segmented(source(Default::default()), Default::default(), Default::default(), seg, 0, &AtomicBool::new(false)).is_err());
        }
    }
}

#[cfg(test)]
#[path = "regional_tests.rs"]
mod regional_tests;
