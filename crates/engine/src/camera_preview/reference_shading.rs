//! Verify that a database lens's radial gain belongs in a camera-JPEG tone fit.
//! The database describes an optical correction, not what processing this JPEG received.
//! Compare two otherwise identical interpretations on held-out regions. An unverified
//! difference keeps the existing profile; no fitted proxy pixel becomes output.
use super::pairs::local_contrast;
use super::{Rgb32f, luminance_2020};

struct Sample {
    scene: [f32; 2],
    light: f32,
    tile: usize,
    quadrant: usize,
    held: bool,
}

fn percentile(values: &mut [f32], fraction: f32) -> f32 {
    let index = ((values.len() - 1) as f32 * fraction) as usize;
    *values.select_nth_unstable_by(index, f32::total_cmp).1
}

/// Paired conditional medians allow each hypothesis its own global tone response.
/// Otherwise a difference in exposure or JPEG contrast can look like radial shading.
struct Response {
    first: f32,
    step: f32,
    light: [f32; 24],
}

impl Response {
    fn fit(samples: &[Sample], model: usize) -> Option<Self> {
        let mut exposure: Vec<_> = samples.iter().filter(|s| !s.held).map(|s| s.scene[model]).collect();
        if exposure.len() < 512 {
            return None;
        }
        let first = percentile(&mut exposure, 0.01);
        let last = percentile(&mut exposure, 0.99);
        if last - first < 1. {
            return None;
        }
        let step = (last - first) / 23.;
        let mut bins: [Vec<f32>; 24] = Default::default();
        for s in samples.iter().filter(|s| !s.held) {
            let bin = ((s.scene[model] - first) / step).round();
            if (0.0..24.0).contains(&bin) {
                bins[bin as usize].push(s.light);
            }
        }
        let mut light = [0.; 24];
        // Empty exposure bands cannot establish a response for their held-out pixels.
        for (k, bin) in bins.iter_mut().enumerate() {
            light[k] = if bin.len() >= 8 { percentile(bin, 0.5) } else { f32::NAN };
        }
        Some(Self { first, step, light })
    }

    fn at(&self, scene: f32) -> Option<f32> {
        let p = (scene - self.first) / self.step;
        if !(0.0..23.0).contains(&p) {
            return None;
        }
        let i = p as usize;
        let y = self.light[i] + (p - i as f32) * (self.light[i + 1] - self.light[i]);
        y.is_finite().then_some(y)
    }
}

/// Both proxies have gone through the same sensor sampling and the verified geometry
/// for their respective shading. Index 0 is the usual profile, 1 omits only its gain.
pub(super) fn use_unshaded(profiled: &Rgb32f, plain: &Rgb32f, jpeg: &Rgb32f, mask: &[bool], other_mask: &[bool]) -> bool {
    let (w, h) = (jpeg.width, jpeg.height);
    if w < 32 || h < 32 || [profiled, plain].iter().any(|p| (p.width, p.height) != (w, h)) || mask.len() != w * h || other_mask.len() != w * h {
        return false;
    }
    let mut samples = Vec::new();
    let tiles_w = w.div_ceil(12);
    for y in (2..h - 2).step_by(2) {
        for x in (2..w - 2).step_by(2) {
            let i = y * w + x;
            if mask[i] || other_mask[i] || local_contrast(plain, i) > 2. || local_contrast(jpeg, i) > 2. {
                continue;
            }
            let target = luminance_2020(jpeg.data[i]);
            let input = [profiled, plain].map(|img| luminance_2020(img.data[i].map(|v| v.max(0.))));
            if !(0.003..0.94).contains(&target) || !input.iter().all(|v| v.is_finite() && *v > 0.0002) {
                continue;
            }
            let (tx, ty) = (x / 12, y / 12);
            samples.push(Sample {
                scene: input.map(f32::log2),
                light: 116. * target.cbrt() - 16.,
                tile: ty * tiles_w + tx,
                quadrant: usize::from(x >= w / 2) + 2 * usize::from(y >= h / 2),
                held: (tx + 3 * ty).is_multiple_of(4),
            });
        }
    }
    let (Some(a), Some(b)) = (Response::fit(&samples, 0), Response::fit(&samples, 1)) else {
        return false;
    };
    let mut errors: [Vec<f32>; 2] = Default::default();
    let mut regions: [[Vec<f32>; 2]; 4] = Default::default();
    let mut tiles = vec![Vec::new(); tiles_w * h.div_ceil(12)];
    for s in samples.iter().filter(|s| s.held) {
        if let (Some(v), Some(u)) = (a.at(s.scene[0]), b.at(s.scene[1])) {
            let err = [(v - s.light).abs(), (u - s.light).abs()];
            for k in 0..2 {
                errors[k].push(err[k]);
                regions[s.quadrant][k].push(err[k]);
            }
            tiles[s.tile].push([s.light, v, u]);
        }
    }
    if errors[0].len() < 256 || regions.iter().any(|r| r[0].len() < 32) {
        return false;
    }
    // Insist on a material gain in median and tail brightness, supported independently
    // in three quadrants. A small sky patch must not choose the rendering of a whole frame.
    let med = errors.each_mut().map(|e| percentile(e, 0.5));
    let tail = errors.each_mut().map(|e| percentile(e, 0.9));
    if med[0] - med[1] < 1.2 || med[1] > 0.72 * med[0] || tail[0] - tail[1] < 2. {
        return false;
    }
    let wins = regions
        .iter_mut()
        .filter_map(|r| {
            let m = r.each_mut().map(|e| percentile(e, 0.5));
            (m[0] - m[1] > 0.3).then_some(())
        })
        .count();
    if wins < 3 {
        return false;
    }
    // Absolute errors can look good while a curve flattens transitions. Compare the
    // 10–90% lightness span inside each textured holdout tile to the camera's span.
    let mut contrast: [Vec<f32>; 2] = Default::default();
    for tile in tiles.iter().filter(|t| t.len() >= 10) {
        let range: [f32; 3] = std::array::from_fn(|k| {
            let mut vals: Vec<_> = tile.iter().map(|v| v[k]).collect();
            percentile(&mut vals, 0.9) - percentile(&mut vals, 0.1)
        });
        if range[0] > 3. {
            for k in 0..2 {
                contrast[k].push((range[k + 1] - range[0]).abs());
            }
        }
    }
    if contrast[0].len() < 12 {
        return false;
    }
    let c = contrast.each_mut().map(|v| percentile(v, 0.5));
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] camera shading held-out median {med:?}, p90 {tail:?}, local contrast {c:?}");
    }
    c[1] <= c[0] + 0.3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keep_shading_unless_the_camera_tone_predicts_independent_regions_without_it() {
        let (w, h) = (192, 144);
        let mut plain = Rgb32f::new(w, h);
        let mut profiled = plain.clone();
        let mut unshaded_jpeg = plain.clone();
        let mut shaded_jpeg = plain.clone();
        for y in 0..h {
            for x in 0..w {
                let i = y * w + x;
                let (u, v) = (x as f32 / w as f32 - 0.5, y as f32 / h as f32 - 0.5);
                let raw = 0.10 * (1.2 * (u * 36.).cos() + 0.75 * (v * 21.).sin()).exp();
                let raised = raw * 2f32.powf(4. * (u * u + v * v));
                let display = |s: f32| s / (0.24 + s);
                plain.data[i] = [raw; 3];
                profiled.data[i] = [raised; 3];
                unshaded_jpeg.data[i] = [display(raw); 3];
                shaded_jpeg.data[i] = [display(raised); 3];
            }
        }
        let mask = vec![false; w * h];
        assert!(use_unshaded(&profiled, &plain, &unshaded_jpeg, &mask, &mask));
        assert!(!use_unshaded(&profiled, &plain, &shaded_jpeg, &mask, &mask));
        assert!(!use_unshaded(&plain, &plain, &unshaded_jpeg, &mask, &mask));
        let partial: Vec<_> = (0..w * h).map(|i| i % w > w / 2 || i / w > h / 2).collect();
        assert!(!use_unshaded(&profiled, &plain, &unshaded_jpeg, &partial, &partial));
    }
}
