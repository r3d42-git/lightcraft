use super::proxy::{Cr3Framing, reference_at, resample_correspondence};
use super::{Filter, Rgb32f, fit, luminance_2020};

/// Compare the actual lens correction with the unwarped reference on the same
/// structural features. A mismatched source edge contributes zero agreement:
/// requiring the raw to already have an edge there would remove the evidence for a warp.
pub(super) fn measured_geometry(
    sensor: &Rgb32f,
    mask: &Rgb32f,
    reference: &Rgb32f,
    correction: &lightcraft_develop::EmbeddedLens,
    orientation: lightcraft_geom::Orientation,
) -> Option<(Rgb32f, Rgb32f)> {
    use lightcraft_geom::Point;
    let (w, h) = (sensor.width as f64, sensor.height as f64);
    let (ow, oh) = if orientation.swaps_axes() { (h, w) } else { (w, h) };
    let mut warp = lightcraft_pipeline::optics::Warp::identity(w, h);
    warp.lens = Some(lightcraft_pipeline::optics::reorient_lens(correction, orientation.inverse(), ow, oh));
    warp.lens_dist = 1.;
    let source = |x, y| warp.corrected_to_source(Point::new(x, y), 1);
    let corrected = resample_correspondence(sensor, source);
    let reference = fit(reference, 128, 128, Filter::Box);
    fn features(image: &Rgb32f, reference: &Rgb32f) -> Rgb32f {
        let mut img = reference_at(image, reference);
        img.map_in_place(|p| {
            let y = luminance_2020(p);
            [if y.is_finite() { y.max(0.0001).ln() } else { f32::NAN }; 3]
        });
        let (w, h) = (img.width, img.height);
        let mut out = Rgb32f::new(w, h);
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                let get = |xx: usize, yy: usize| img.data[yy * w + xx][0];
                let dx = get(x + 1, y) - get(x - 1, y);
                let dy = get(x, y + 1) - get(x, y - 1);
                let norm = dx.hypot(dy).max(0.08);
                out.data[y * w + x] = [dx / norm, dy / norm, get(x, y)];
            }
        }
        out
    }
    if reference.width < 16 || reference.height < 16 {
        return None;
    }
    let target = features(&reference, &reference);
    let ordinary = features(sensor, &reference);
    let proposed = features(&corrected, &reference);
    let (sw, sh) = (reference.width, reference.height);
    let pts: Vec<_> = (3..sh - 3)
        .step_by(2)
        .flat_map(|y| (3..sw - 3).step_by(2).map(move |x| (x, y)))
        .filter_map(|(x, y)| {
            let v = target.data[y * sw + x];
            (v[0].hypot(v[1]) > 0.65 && v[2].is_finite()).then_some((x, y, v, ((x / 8) + 3 * (y / 8)) % 4 == 0))
        })
        .collect();
    if pts.len() < 100 {
        return None;
    }
    let score = |im: &Rgb32f, f: Cr3Framing, held: bool| {
        let mut sum = 0.;
        let mut n = 0;
        for &(x, y, v, holdout) in &pts {
            if holdout != held {
                continue;
            }
            n += 1;
            let (sx, sy) = f.source(x as f32 + 0.5, y as f32 + 0.5, sw, sh);
            if !(2.5..sw as f32 - 2.5).contains(&sx) || !(2.5..sh as f32 - 2.5).contains(&sy) {
                continue;
            }
            let g = im.sample_bilinear(sx, sy);
            let dot = v[0] * g[0] + v[1] * g[1];
            if dot.is_finite() {
                sum += dot;
            }
        }
        sum / (n.max(1) as f32)
    };
    let baseline = score(&ordinary, Cr3Framing::IDENTITY, true);
    let mut best = (f32::NEG_INFINITY, Cr3Framing::IDENTITY);
    for (stride, range) in [(0.02, 5), (0.005, 3), (0.00125, 2)] {
        let center = best.1;
        for ds in -range..=range {
            for dx in -2..=2 {
                for dy in -2..=2 {
                    let f = Cr3Framing {
                        scale: center.scale + stride * ds as f32,
                        offset: [center.offset[0] + stride * 0.25 * dx as f32, center.offset[1] + stride * 0.25 * dy as f32],
                    };
                    if !(0.88..=1.12).contains(&f.scale) || f.offset.iter().any(|x| x.abs() > 0.035) {
                        continue;
                    }
                    let value = score(&proposed, f, false);
                    if value > best.0 {
                        best = (value, f);
                    }
                }
            }
        }
    }
    let verified = score(&proposed, best.1, true);
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] measured framing {:?}, structure {baseline:.3} -> {verified:.3}", best.1);
    }
    if verified < 0.40 || verified <= baseline + 0.015 {
        return None;
    }
    let map = |x: f64, y: f64| {
        let (sx, sy) = best.1.source(x as f32, y as f32, sensor.width, sensor.height);
        source(sx as f64, sy as f64)
    };
    Some((resample_correspondence(sensor, map), resample_correspondence(mask, map)))
}
