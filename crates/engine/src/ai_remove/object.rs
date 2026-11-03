//! Temporary object selections: no develop mask, catalog entry or model-dependent edit.
use super::{Session, error, selection_param};
use lightcraft_develop::{SegMask, Spot};
use lightcraft_geom::Point;
use serde_json::{Value, json};

#[derive(Clone, Debug)]
pub struct ObjectSelection {
    pub seg: Option<SegMask>,
    pub include: Vec<Point>,
    pub exclude: Vec<Point>,
    pub margin: f64,
    raw_seg: Option<SegMask>,
    refinements: Vec<(Spot, bool)>,
}

impl Default for ObjectSelection {
    fn default() -> Self {
        Self { seg: None, include: Vec::new(), exclude: Vec::new(), margin: 0.003, raw_seg: None, refinements: Vec::new() }
    }
}

/// Decode once per overlay or inference preparation. Selected cells use logits > 0;
/// nearest-cell sampling preserves exactly-zero authorization outside the selection.
pub struct SelectionMask {
    logits: Vec<f32>,
    side: usize,
    rect: [f64; 4],
}
impl SelectionMask {
    pub fn new(seg: &SegMask) -> Result<Self, String> {
        let rect = seg.bounds().filter(|r| r.iter().all(|v| (0.0..=1.0).contains(v))).ok_or("Invalid object mask bounds.")?;
        let logits = seg.logits().ok_or("Invalid object mask data.")?;
        Ok(Self { logits, side: seg.side as usize, rect })
    }
    pub fn is_empty(&self) -> bool {
        !self.logits.iter().any(|v| *v > 0.0)
    }
    pub fn contains(&self, p: Point) -> bool {
        if !p.x.is_finite() || !p.y.is_finite() || p.x < self.rect[0] || p.y < self.rect[1] || p.x >= self.rect[2] || p.y >= self.rect[3] {
            return false;
        }
        let x = (((p.x - self.rect[0]) / (self.rect[2] - self.rect[0]) * self.side as f64).floor() as usize).min(self.side - 1);
        let y = (((p.y - self.rect[1]) / (self.rect[3] - self.rect[1]) * self.side as f64).floor() as usize).min(self.side - 1);
        self.logits.get(y * self.side + x).is_some_and(|v| *v > 0.0)
    }
    pub(super) fn selected_bounds(&self) -> [f64; 4] {
        let mut b = [1.0f64, 1.0, 0.0, 0.0];
        for (i, l) in self.logits.iter().enumerate().filter(|(_, l)| **l > 0.0) {
            let _ = l;
            let x = (i % self.side) as f64 / self.side as f64;
            let y = (i / self.side) as f64 / self.side as f64;
            b[0] = b[0].min(x);
            b[1] = b[1].min(y);
            b[2] = b[2].max(x + 1.0 / self.side as f64);
            b[3] = b[3].max(y + 1.0 / self.side as f64);
        }
        [
            self.rect[0] + b[0] * (self.rect[2] - self.rect[0]),
            self.rect[1] + b[1] * (self.rect[3] - self.rect[1]),
            self.rect[0] + b[2] * (self.rect[2] - self.rect[0]),
            self.rect[1] + b[3] * (self.rect[3] - self.rect[1]),
        ]
    }
}
impl ObjectSelection {
    pub fn decoded_mask(&self) -> Result<SelectionMask, String> {
        SelectionMask::new(self.seg.as_ref().ok_or("No object mask yet.")?)
    }
    fn refined(&self, seg: SegMask, aspect: f64) -> Result<SegMask, String> {
        if !self.margin.is_finite() || !(0.0..=0.02).contains(&self.margin) || !aspect.is_finite() || aspect <= 0.0 {
            return Err("Invalid object selection margin or aspect.".into());
        }
        if self.refinements.is_empty() && self.margin == 0.0 {
            SelectionMask::new(&seg)?;
            return Ok(seg);
        }
        let mask = SelectionMask::new(&seg)?;
        // Keep manual corrections finer than the model grid; the source-space result is
        // frozen later, after applying the actual resolved lens geometry.
        let side = 1024usize;
        let (w, h) = if aspect >= 1.0 { (aspect, 1.0) } else { (1.0, 1.0 / aspect) };
        // Preserve a detail crop's resolution when painting. Extend it only as far as
        // corrections need, rather than downsampling the refined object over the full photo.
        let mut rect = if mask.is_empty() { mask.rect } else { mask.selected_bounds() };
        let margin = self.margin * w.max(h);
        rect = [(rect[0] - margin / w).max(0.0), (rect[1] - margin / h).max(0.0), (rect[2] + margin / w).min(1.0), (rect[3] + margin / h).min(1.0)];
        for (spot, _) in &self.refinements {
            let radius = spot.size * w.max(h);
            for p in &spot.points {
                rect[0] = rect[0].min((p.x - radius / w).max(0.0));
                rect[1] = rect[1].min((p.y - radius / h).max(0.0));
                rect[2] = rect[2].max((p.x + radius / w).min(1.0));
                rect[3] = rect[3].max((p.y + radius / h).min(1.0));
            }
        }
        let (rw, rh) = (rect[2] - rect[0], rect[3] - rect[1]);
        let mut logits: Vec<f32> = (0..side * side)
            .map(|i| {
                if mask.contains(Point::new(
                    rect[0] + ((i % side) as f64 + 0.5) / side as f64 * rw,
                    rect[1] + ((i / side) as f64 + 0.5) / side as f64 * rh,
                )) {
                    8.0
                } else {
                    -8.0
                }
            })
            .collect();
        if margin > 0.0 {
            dilate(&mut logits, side, rw * w / side as f64, rh * h / side as f64, margin);
        }
        let mut work = 0usize;
        // Visit only each capsule's bounding cells, rather than every image cell for
        // every point. Cap the complete replay before doing any raster work.
        let mut capsules = Vec::new();
        for (spot, subtract) in &self.refinements {
            let points: Vec<[f64; 2]> = spot.points.iter().map(|p| [p.x * w, p.y * h]).collect();
            let radius = spot.size * w.max(h);
            for i in 0..points.len().max(1) {
                let Some(a) = points.get(i).copied() else { continue };
                let b = points.get(i + 1).copied().unwrap_or(a);
                let x0 = ((((a[0].min(b[0]) - radius) / w - rect[0]) / rw * side as f64).floor().max(0.0) as usize).min(side);
                let x1 = ((((a[0].max(b[0]) + radius) / w - rect[0]) / rw * side as f64).ceil().max(0.0) as usize).min(side);
                let y0 = ((((a[1].min(b[1]) - radius) / h - rect[1]) / rh * side as f64).floor().max(0.0) as usize).min(side);
                let y1 = ((((a[1].max(b[1]) + radius) / h - rect[1]) / rh * side as f64).ceil().max(0.0) as usize).min(side);
                work = work.saturating_add((x1 - x0).saturating_mul(y1 - y0));
                if work > 16_000_000 {
                    return Err("Selection refinements are too complex. Clear the selection and try fewer strokes.".into());
                }
                capsules.push((a, b, radius, *subtract, [x0, y0, x1, y1]));
            }
        }
        for (a, b, radius, subtract, [x0, y0, x1, y1]) in capsules {
            for y in y0..y1 {
                for x in x0..x1 {
                    let p = [(rect[0] + (x as f64 + 0.5) / side as f64 * rw) * w, (rect[1] + (y as f64 + 0.5) / side as f64 * rh) * h];
                    if lightcraft_develop::ai_edit::brush_distance(&[a, b], p, true) < radius
                        && let Some(l) = logits.get_mut(y * side + x)
                    {
                        *l = if subtract { -8.0 } else { 8.0 };
                    }
                }
            }
        }
        Ok(SegMask::from_logits_in(side, &logits, rect))
    }
}
/// Exact squared Euclidean distance on the bounded raster, with independent physical
/// x/y steps. Row nearest sites plus column parabola envelopes take O(side²) work,
/// independent of the margin radius (no per-pixel neighbourhood search).
fn dilate(logits: &mut [f32], side: usize, dx: f64, dy: f64, radius: f64) {
    let mut distances = vec![f64::INFINITY; logits.len()];
    for y in 0..side {
        let mut last = None;
        for x in 0..side {
            let i = y * side + x;
            if logits.get(i).is_some_and(|l| *l > 0.0) {
                last = Some(x);
            }
            if let Some(site) = last {
                distances[i] = ((x - site) as f64 * dx).powi(2);
            }
        }
        last = None;
        for x in (0..side).rev() {
            let i = y * side + x;
            if logits.get(i).is_some_and(|l| *l > 0.0) {
                last = Some(x);
            }
            if let Some(site) = last {
                distances[i] = distances[i].min(((site - x) as f64 * dx).powi(2));
            }
        }
    }
    let mut sites = Vec::<usize>::with_capacity(side);
    let mut boundaries = Vec::<f64>::with_capacity(side);
    for x in 0..side {
        sites.clear();
        boundaries.clear();
        for q in 0..side {
            let fq = distances[q * side + x];
            if !fq.is_finite() {
                continue;
            }
            let mut boundary = f64::NEG_INFINITY;
            while let Some(&p) = sites.last() {
                let py = p as f64 * dy;
                let qy = q as f64 * dy;
                boundary = (fq + qy * qy - distances[p * side + x] - py * py) / (2.0 * (qy - py));
                if boundary > *boundaries.last().unwrap_or(&f64::NEG_INFINITY) {
                    break;
                }
                sites.pop();
                boundaries.pop();
            }
            if sites.is_empty() {
                boundary = f64::NEG_INFINITY;
            }
            sites.push(q);
            boundaries.push(boundary);
        }
        if sites.is_empty() {
            continue;
        }
        let mut k = 0;
        for y in 0..side {
            let yp = y as f64 * dy;
            while boundaries.get(k + 1).is_some_and(|b| *b < yp) {
                k += 1;
            }
            let Some(&p) = sites.get(k) else { continue };
            let distance = distances[p * side + x] + ((y as f64 - p as f64) * dy).powi(2);
            if distance <= radius * radius
                && let Some(l) = logits.get_mut(y * side + x)
            {
                *l = 8.0;
            }
        }
    }
}

impl Session {
    pub(super) fn ai_remove_clear_object(&mut self) {
        self.ai_remove.object_selection = None;
        self.ai_remove.selecting = false;
        self.ai_remove.selection_revision = self.ai_remove.selection_revision.wrapping_add(1);
    }
    pub(super) fn ai_remove_object_point(&mut self, p: &Value) -> crate::Result<Value> {
        let coordinate = |name| {
            p.get(name)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && (0.0..=1.0).contains(v))
                .ok_or_else(|| error("Invalid object selection point."))
        };
        let at = Point::new(coordinate("x")?, coordinate("y")?);
        let exclude = match p.get("exclude") {
            None => false,
            Some(v) => v.as_bool().ok_or_else(|| error("exclude must be a boolean."))?,
        };
        self.ai_remove_poll();
        let tag = self.ai_remove_tag().ok_or_else(|| error("Select a photo."))?;
        let mut selection = self.ai_remove.object_selection.clone().unwrap_or_default();
        if selection.include.len() + selection.exclude.len() >= crate::segment::MAX_CLICKS {
            return Err(error("At most 64 object selection clicks."));
        }
        if exclude {
            if selection.include.is_empty() {
                return Err(error("Click an object to include first."));
            }
            selection.exclude.push(at);
        } else {
            selection.include.push(at);
        }
        let revision = self.ai_remove.selection_revision.wrapping_add(1);
        self.segmenter.require_model()?;
        self.segment_ai_remove_later(tag.0, revision, &selection.include, &selection.exclude).map_err(error)?;
        self.ai_remove_cancel();
        self.ai_remove.selection = None;
        self.ai_remove.selection_tag = Some(tag);
        self.ai_remove.object_selection = Some(selection);
        self.ai_remove.selection_revision = revision;
        self.ai_remove.selecting = true;
        self.ai_remove.message = "Selecting object…".into();
        Ok(json!({"pending":true}))
    }
    pub(super) fn ai_remove_refine_brush(&mut self, p: &Value) -> crate::Result<Value> {
        let spot = selection_param(p)?;
        let subtract = match p.get("subtract") {
            None => false,
            Some(v) => v.as_bool().ok_or_else(|| error("subtract must be a boolean."))?,
        };
        self.ai_remove_poll();
        if self.ai_remove.selecting {
            return Err(error("Wait for the object selection."));
        }
        let mut selection = self.ai_remove.object_selection.clone().ok_or_else(|| error("Select an object first."))?;
        if selection.refinements.len() >= 256 {
            return Err(error("At most 256 brush refinements."));
        }
        let seg = selection.raw_seg.clone().or_else(|| selection.seg.clone()).ok_or_else(|| error("Select an object first."))?;
        selection.raw_seg = Some(seg.clone());
        // Rebuild from the raw model result so neither margin nor corrections accumulate.
        selection.refinements.push((spot, subtract));
        selection.seg = Some(selection.refined(seg, self.ai_remove_aspect()).map_err(error)?);
        self.ai_remove_cancel();
        self.ai_remove.selection_revision = self.ai_remove.selection_revision.wrapping_add(1);
        self.ai_remove.object_selection = Some(selection);
        self.ai_remove.message = "Object selection refined.".into();
        Ok(json!({"selected":true}))
    }
    pub(super) fn ai_remove_object_margin(&mut self, p: &Value) -> crate::Result<Value> {
        let margin = p
            .get("size")
            .and_then(Value::as_f64)
            .filter(|v| v.is_finite() && (0.0..=0.02).contains(v))
            .ok_or_else(|| error("Object margin must be between 0 and 0.02."))?;
        self.ai_remove_poll();
        if self.ai_remove.selecting {
            return Err(error("Wait for the object selection."));
        }
        let mut selection = self.ai_remove.object_selection.clone().ok_or_else(|| error("Select an object first."))?;
        let raw = selection.raw_seg.clone().or_else(|| selection.seg.clone()).ok_or_else(|| error("Select an object first."))?;
        selection.margin = margin;
        selection.seg = Some(selection.refined(raw.clone(), self.ai_remove_aspect()).map_err(error)?);
        selection.raw_seg = Some(raw);
        self.ai_remove_cancel();
        self.ai_remove.object_selection = Some(selection);
        self.ai_remove.message = "Object selection margin updated.".into();
        Ok(json!({"size":margin,"selected":true}))
    }
    fn ai_remove_aspect(&self) -> f64 {
        self.active().and_then(|id| self.catalog.photo(id)).map_or(1.0, |p| {
            let (w, h) = if p.develop.orientation.swaps_axes() { (p.height, p.width) } else { (p.width, p.height) };
            if w > 0 && h > 0 { f64::from(w) / f64::from(h) } else { 1.0 }
        })
    }
    #[cfg_attr(not(feature = "sam"), allow(dead_code))]
    pub(crate) fn ai_remove_apply_seg(&mut self, revision: u64, detail: bool, result: Result<Option<SegMask>, String>) -> Option<[f64; 4]> {
        if revision != self.ai_remove.selection_revision
            || !self
                .ai_remove
                .selection_tag
                .as_ref()
                .zip(self.ai_remove_tag())
                .is_some_and(|((id, d, g), (now, current, generation))| *id == now && *g == generation && std::sync::Arc::ptr_eq(d, &current))
        {
            return None;
        }
        let aspect = self.ai_remove_aspect();
        let result = result.and_then(|s| s.ok_or_else(|| "Nothing was selected.".to_string())).and_then(|seg| {
            let mask = SelectionMask::new(&seg)?;
            if mask.is_empty() {
                return Err("The selected mask is empty.".into());
            }
            Ok(seg)
        });
        match result {
            Ok(seg) => {
                // The detail region follows the model's raw selection; the visible and
                // executable fallback already includes every manual correction.
                let region = if detail { None } else { crate::segment::detail_region(&seg, aspect) };
                let refined = self
                    .ai_remove
                    .object_selection
                    .as_ref()
                    .ok_or_else(|| "Selection cleared.".to_string())
                    .and_then(|o| o.refined(seg.clone(), aspect));
                let raw_seg = seg;
                match refined {
                    Ok(seg) => {
                        if let Some(o) = &mut self.ai_remove.object_selection {
                            o.raw_seg = Some(raw_seg.clone());
                            o.seg = Some(seg);
                        }
                        if let Some(region) = region {
                            self.ai_remove.selecting = true;
                            self.ai_remove.message = "Object selected. Refining object detail…".into();
                            return Some(region);
                        }
                        self.ai_remove.message = "Object selected. Refine or run AI Remove.".into();
                    }
                    Err(e) => {
                        // New prompts must never authorize an older mask if corrections
                        // cannot be replayed onto their result, even during a detail pass.
                        if let Some(o) = &mut self.ai_remove.object_selection {
                            o.seg = None;
                            o.raw_seg = None;
                        }
                        self.ai_remove.message = format!("Object mask refinement failed: {e}");
                    }
                }
            }
            Err(e) => {
                if !detail && let Some(o) = &mut self.ai_remove.object_selection {
                    o.seg = None;
                    o.raw_seg = None;
                }
                self.ai_remove.message = if detail { format!("Object detail refinement failed: {e} Coarse selection retained.") } else { e };
            }
        }
        self.ai_remove.selecting = false;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn seg() -> SegMask {
        SegMask::from_logits(4, &[-8., -8., -8., -8., -8., 8., -8., -8., -8., 8., 8., -8., -8., -8., -8., -8.])
    }
    fn selected(s: &mut Session) {
        s.ai_remove.selection_tag = s.ai_remove_tag();
        s.ai_remove.object_selection = Some(ObjectSelection { seg: Some(seg()), include: vec![Point::new(0.4, 0.4)], ..Default::default() });
    }
    #[test]
    fn temporary_object_refinement_and_clear_never_edit_photo() {
        let mut s = Session::with_demo();
        selected(&mut s);
        let id = s.active().unwrap();
        let original = s.develop_of(id).unwrap();
        let undo = s.undo.len();
        s.execute("aiRemove.refineBrush", &json!({"points":[[0.4,0.4]],"size":0.08,"subtract":true})).unwrap();
        assert!(!s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().contains(Point::new(0.4, 0.4)));
        s.execute("aiRemove.refineBrush", &json!({"points":[[0.4,0.4]],"size":0.04})).unwrap();
        assert!(s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().contains(Point::new(0.4, 0.4)));
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().include.len(), 1);
        assert!(std::sync::Arc::ptr_eq(&original, &s.develop_of(id).unwrap()));
        assert_eq!(s.undo.len(), undo);
        s.ai_remove.selecting = true;
        let revision = s.ai_remove.selection_revision;
        s.execute("aiRemove.clearSelection", &json!({})).unwrap();
        let _ = s.ai_remove_apply_seg(revision, false, Ok(Some(seg())));
        assert!(s.ai_remove.object_selection.is_none());
        assert!(!s.ai_remove.selecting);
    }
    #[test]
    fn stale_object_outcomes_are_rejected_for_every_context_boundary() {
        for kind in 0..5 {
            let mut s = Session::with_demo();
            selected(&mut s);
            s.ai_remove.selecting = true;
            let revision = s.ai_remove.selection_revision;
            match kind {
                0 => {
                    s.execute("develop.set", &json!({"control":"light.exposure","value":0.7})).unwrap();
                }
                1 => {
                    s.execute("library.down", &json!({})).unwrap();
                }
                2 => {
                    s.ai_remove.selection_tag.as_mut().unwrap().2 = s.library_generation().wrapping_add(1);
                }
                3 => {
                    s.execute("aiRemove.cancel", &json!({})).unwrap();
                }
                _ => {
                    s.ai_remove.selection_revision = revision.wrapping_add(1);
                }
            }
            let old = s.ai_remove.object_selection.as_ref().and_then(|o| o.seg.clone());
            let _ = s.ai_remove_apply_seg(revision, false, Ok(Some(SegMask::from_logits(1, &[8.]))));
            assert_eq!(s.ai_remove.object_selection.as_ref().and_then(|o| o.seg.clone()), old);
            if kind < 3 {
                s.ai_remove_poll();
                assert!(s.ai_remove.object_selection.is_none());
                assert!(!s.ai_remove.selecting);
            }
        }
    }
    #[test]
    fn coarse_then_detail_retains_corrections_and_rejects_old_detail() {
        let mut s = Session::with_demo();
        selected(&mut s);
        s.execute("aiRemove.refineBrush", &json!({"points":[[0.4,0.4]],"size":0.04,"subtract":true})).unwrap();
        let revision = s.ai_remove.selection_revision;
        let region = s.ai_remove_apply_seg(revision, false, Ok(Some(seg()))).unwrap();
        assert!(s.ai_remove.selecting);
        assert!(
            !s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().contains(Point::new(0.4, 0.4)),
            "visible coarse mask already retains corrections"
        );
        let detail = SegMask::from_logits_in(32, &vec![8.; 32 * 32], region);
        assert!(s.ai_remove_apply_seg(revision, true, Ok(Some(detail.clone()))).is_none());
        assert!(!s.ai_remove.selecting);
        let corrected_rect = s.ai_remove.object_selection.as_ref().unwrap().seg.as_ref().unwrap().rect.unwrap();
        assert!(corrected_rect[0] <= region[0] && corrected_rect[2] >= region[2]);
        assert!(
            !s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().contains(Point::new(0.4, 0.4)),
            "brush correction replayed after detail"
        );
        let final_mask = s.ai_remove.object_selection.as_ref().unwrap().seg.clone();
        s.execute("aiRemove.cancel", &json!({})).unwrap();
        let _ = s.ai_remove_apply_seg(revision, true, Ok(Some(detail)));
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, final_mask);
    }
    #[test]
    fn detail_failure_retains_coarse_with_explicit_notice_and_large_objects_finish_once() {
        let mut s = Session::with_demo();
        selected(&mut s);
        s.execute("aiRemove.refineBrush", &json!({"points":[[0.4,0.4]],"size":0.04,"subtract":true})).unwrap();
        let revision = s.ai_remove.selection_revision;
        assert!(s.ai_remove_apply_seg(revision, false, Ok(Some(seg()))).is_some());
        assert!(s.ai_remove.selecting);
        assert!(!s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().contains(Point::new(0.4, 0.4)));
        let coarse = s.ai_remove.object_selection.as_ref().unwrap().seg.clone();
        let _ = s.ai_remove_apply_seg(revision, true, Err("synthetic detail failure".into()));
        assert!(!s.ai_remove.selecting);
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, coarse);
        assert!(s.ai_remove.message.contains("detail refinement failed"));
        assert!(s.ai_remove.message.contains("Coarse selection retained"));
        assert!(s.ai_remove_apply_seg(revision, false, Ok(Some(SegMask::from_logits(1, &[8.])))).is_none());
        assert!(!s.ai_remove.selecting);
    }
    #[test]
    fn failed_new_click_clears_old_mask_but_keeps_prompts_without_editing_photo() {
        let mut s = Session::with_demo();
        selected(&mut s);
        let id = s.active().unwrap();
        let before = s.develop_of(id).unwrap();
        let undo = s.undo.len();
        s.ai_remove.object_selection.as_mut().unwrap().include.push(Point::new(0.6, 0.6));
        s.ai_remove.selecting = true;
        let revision = s.ai_remove.selection_revision;
        let _ = s.ai_remove_apply_seg(revision, false, Err("synthetic new-click failure".into()));
        assert!(!s.ai_remove.selecting);
        let object = s.ai_remove.object_selection.as_ref().unwrap();
        assert!(object.seg.is_none());
        assert_eq!(object.include.len(), 2);
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["selected"], false);
        #[cfg(not(target_arch = "wasm32"))]
        assert!(s.execute("aiRemove.start", &json!({})).unwrap_err().to_string().contains("Select an object first"));
        assert!(std::sync::Arc::ptr_eq(&before, &s.develop_of(id).unwrap()));
        assert_eq!(s.undo.len(), undo);
    }
    #[test]
    fn replay_failure_clears_old_mask_for_both_coarse_and_detail_results() {
        for detail in [false, true] {
            let mut s = Session::with_demo();
            selected(&mut s);
            let object = s.ai_remove.object_selection.as_mut().unwrap();
            object.include.push(Point::new(0.6, 0.6));
            object.refinements.push((Spot { points: vec![Point::new(0.5, 0.5); 4096], size: 0.25, ..Default::default() }, false));
            s.ai_remove.selecting = true;
            let revision = s.ai_remove.selection_revision;
            assert!(s.ai_remove_apply_seg(revision, detail, Ok(Some(seg()))).is_none());
            assert!(!s.ai_remove.selecting);
            assert!(s.ai_remove.object_selection.as_ref().unwrap().seg.is_none());
            assert!(s.ai_remove.message.contains("mask refinement failed"));
            assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["selected"], false);
        }
    }
    #[test]
    fn margin_is_visible_reversible_and_never_accumulates() {
        let mut s = Session::with_demo();
        selected(&mut s);
        let raw = s.ai_remove.object_selection.as_ref().unwrap().seg.clone().unwrap();
        s.execute("aiRemove.objectMargin", &json!({"size":0.0})).unwrap();
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, Some(raw.clone()));
        s.execute("aiRemove.objectMargin", &json!({"size":0.003})).unwrap();
        let expanded = s.ai_remove.object_selection.as_ref().unwrap().seg.clone();
        let sampled = s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap();
        assert!(sampled.contains(Point::new(0.248, 0.4)), "visible raster contains added border");
        s.execute("aiRemove.objectMargin", &json!({"size":0.003})).unwrap();
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, expanded);
        s.execute("aiRemove.objectMargin", &json!({"size":0.0})).unwrap();
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, Some(raw));
    }
    #[test]
    fn margin_precedes_subtraction_and_model_results_replay_both() {
        let mut s = Session::with_demo();
        selected(&mut s);
        s.execute("aiRemove.refineBrush", &json!({"points":[[0.4,0.4]],"size":0.04,"subtract":true})).unwrap();
        s.execute("aiRemove.objectMargin", &json!({"size":0.01})).unwrap();
        assert!(!s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().contains(Point::new(0.4, 0.4)));
        let revision = s.ai_remove.selection_revision;
        let _ = s.ai_remove_apply_seg(revision, true, Ok(Some(seg())));
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().margin, 0.01);
        assert!(!s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().contains(Point::new(0.4, 0.4)));
    }
    #[test]
    fn circular_margin_respects_photo_aspect_and_localized_roi() {
        let raw = SegMask::from_logits_in(64, &vec![8.; 64 * 64], [0.4, 0.4, 0.6, 0.6]);
        let o = ObjectSelection::default();
        let refined = o.refined(raw, 2.0).unwrap();
        let mask = SelectionMask::new(&refined).unwrap();
        assert!(mask.contains(Point::new(0.602, 0.5)));
        assert!(mask.contains(Point::new(0.5, 0.604)));
        assert!(!mask.contains(Point::new(0.604, 0.5)));
        assert!(!mask.contains(Point::new(0.5, 0.607)));
        assert!(!mask.contains(Point::new(0.6028, 0.6056)), "disk, not rectangular dilation");
        let r = refined.rect.unwrap();
        assert!(r[0] > 0.39 && r[1] > 0.39 && r[2] < 0.61 && r[3] < 0.61);
    }
    #[test]
    fn invalid_margin_and_pending_changes_preserve_selection() {
        let mut s = Session::with_demo();
        selected(&mut s);
        let before = s.ai_remove.object_selection.as_ref().unwrap().seg.clone();
        for p in [json!({"size":-0.1}), json!({"size":0.021}), json!({"size":null}), json!({"size":"NaN"})] {
            assert!(s.execute("aiRemove.objectMargin", &p).is_err());
            assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, before);
            assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().margin, 0.003);
        }
        s.ai_remove.selecting = true;
        assert!(s.execute("aiRemove.objectMargin", &json!({"size":0.01})).is_err());
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, before);
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().margin, 0.003);
        let invalid = ObjectSelection { margin: f64::NAN, ..Default::default() };
        assert!(invalid.refined(seg(), 1.0).is_err());
    }
    #[test]
    fn empty_mask_can_be_repainted_and_hostile_work_is_rejected_without_mutation() {
        let mut s = Session::with_demo();
        selected(&mut s);
        s.ai_remove.object_selection.as_mut().unwrap().seg = Some(SegMask::from_logits(1, &[-8.]));
        s.execute("aiRemove.refineBrush", &json!({"points":[[0.4,0.4]],"size":0.04})).unwrap();
        assert!(!s.ai_remove.object_selection.as_ref().unwrap().decoded_mask().unwrap().is_empty());
        let before = s.ai_remove.object_selection.as_ref().unwrap().seg.clone();
        assert!(s.execute("aiRemove.refineBrush", &json!({"points":vec![[0.5,0.5];4096],"size":0.25})).is_err());
        assert_eq!(s.ai_remove.object_selection.as_ref().unwrap().seg, before);
    }
}
