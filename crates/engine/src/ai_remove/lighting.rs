//! Optional boundary illumination matching with provider-specific guidance. No original masked RGB is
//! used in the equations; authorization and geometry remain frozen by the engine.
use super::runner::RetouchInput;
use rayon::prelude::*;
use std::{
    sync::atomic::{AtomicBool, Ordering},
    time::Instant,
};

const SIGMA: f64 = 32.0;
const RADIUS: usize = 128;
const NONE: usize = usize::MAX;
const MAX_ITERATIONS: usize = 2500;
const MIN_BOUNDARY: usize = 64;

fn check(cancel: &AtomicBool, deadline: Instant) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    if Instant::now() >= deadline {
        return Err("Lighting matching timed out. Turn off Match lighting to use the unadjusted result.".into());
    }
    Ok(())
}

/// Exact separable sigma32/truncate4 Gaussian with clamped ROI edges, matching
/// the validated offline reference. Padding from the square upload is excluded.
fn gaussian(generated: &[[f32; 3]], width: usize, height: usize, cancel: &AtomicBool, deadline: Instant) -> Result<Vec<[f64; 3]>, String> {
    let mut kernel: Vec<f64> = (0..=2 * RADIUS)
        .map(|i| {
            let x = i as f64 - RADIUS as f64;
            (-0.5 * x * x / (SIGMA * SIGMA)).exp()
        })
        .collect();
    let sum: f64 = kernel.iter().sum();
    for value in &mut kernel {
        *value /= sum;
    }
    let mut horizontal = vec![[0.0; 3]; generated.len()];
    horizontal.par_chunks_mut(width).enumerate().try_for_each(|(y, row)| -> Result<(), String> {
        check(cancel, deadline)?;
        let source = generated.get(y * width..(y + 1) * width).ok_or("Invalid lighting ROI.")?;
        for (x, pixel) in row.iter_mut().enumerate() {
            for (k, weight) in kernel.iter().enumerate() {
                let at = (x + k).saturating_sub(RADIUS).min(width - 1);
                let p = source.get(at).ok_or("Invalid lighting row.")?;
                for channel in 0..3 {
                    pixel[channel] += f64::from(p[channel]) * weight;
                }
            }
        }
        Ok(())
    })?;
    let mut blurred = vec![[0.0; 3]; generated.len()];
    blurred.par_chunks_mut(width).enumerate().try_for_each(|(y, row)| -> Result<(), String> {
        check(cancel, deadline)?;
        for (x, pixel) in row.iter_mut().enumerate() {
            for (k, weight) in kernel.iter().enumerate() {
                let yy = (y + k).saturating_sub(RADIUS).min(height - 1);
                let p = horizontal.get(yy * width + x).ok_or("Invalid lighting column.")?;
                for channel in 0..3 {
                    pixel[channel] += p[channel] * weight;
                }
            }
        }
        Ok(())
    })?;
    Ok(blurred)
}

fn adjacent(i: usize, width: usize, height: usize) -> [Option<usize>; 4] {
    let (x, y) = (i % width, i / width);
    [
        if y > 0 { Some(i - width) } else { None },
        if y + 1 < height { Some(i + width) } else { None },
        if x > 0 { Some(i - 1) } else { None },
        if x + 1 < width { Some(i + 1) } else { None },
    ]
}

struct Domain {
    pixels: Vec<usize>,
    neighbours: Vec<[usize; 4]>,
    degree: Vec<f64>,
    boundary: Vec<usize>,
    width: usize,
    height: usize,
}
impl Domain {
    fn new(input: &RetouchInput) -> Result<Self, String> {
        let mut index = vec![NONE; input.rgb.len()];
        let mut pixels = Vec::new();
        for (i, id) in index.iter_mut().enumerate() {
            if input.mask.get(i) == Some(&255) && input.protected.get(i) == Some(&false) {
                *id = pixels.len();
                pixels.push(i);
            }
        }
        if pixels.is_empty() {
            return Err("No unprotected pixels to match.".into());
        }
        let mut boundary = vec![false; index.len()];
        let mut neighbours = Vec::with_capacity(pixels.len());
        let mut degree = Vec::with_capacity(pixels.len());
        for &pixel in &pixels {
            let mut ids = [NONE; 4];
            let mut count = 0u8;
            for (slot, neighbour) in adjacent(pixel, input.width, input.height).into_iter().enumerate() {
                if let Some(at) = neighbour {
                    count += 1;
                    let id = *index.get(at).ok_or("Invalid lighting neighbour.")?;
                    if id == NONE {
                        if let Some(v) = boundary.get_mut(at) {
                            *v = true;
                        }
                    } else {
                        ids[slot] = id;
                    }
                }
            }
            if count == 0 {
                return Err("Lighting matching has no boundary support.".into());
            }
            neighbours.push(ids);
            degree.push(f64::from(count));
        }
        let boundary: Vec<usize> = boundary.into_iter().enumerate().filter_map(|(i, v)| v.then_some(i)).collect();
        if boundary.len() < MIN_BOUNDARY {
            return Err("Lighting matching needs more surrounding context. Turn off Match lighting to use the unadjusted result.".into());
        }
        Ok(Self { pixels, neighbours, degree, boundary, width: input.width, height: input.height })
    }
    fn multiply(&self, x: &[f64], result: &mut [f64]) {
        for (i, value) in result.iter_mut().enumerate() {
            let mut sum = self.degree[i] * x[i];
            for &neighbour in &self.neighbours[i] {
                if neighbour != NONE {
                    sum -= x[neighbour];
                }
            }
            *value = sum;
        }
    }
    fn solve(&self, b: &[f64], initial: f64, cancel: &AtomicBool, deadline: Instant) -> Result<Vec<f64>, String> {
        let n = b.len();
        let mut x = vec![initial; n];
        let mut ap = vec![0.0; n];
        self.multiply(&x, &mut ap);
        let mut r: Vec<f64> = b.iter().zip(&ap).map(|(b, a)| b - a).collect();
        let mut z: Vec<f64> = r.iter().zip(&self.degree).map(|(v, d)| v / d).collect();
        let mut p = z.clone();
        let mut rho = dot(&r, &z);
        let tolerance = (1e-7 * dot(b, b).sqrt()).max(1e-10);
        for _ in 0..MAX_ITERATIONS {
            check(cancel, deadline)?;
            if dot(&r, &r).sqrt() <= tolerance {
                self.multiply(&x, &mut ap);
                let error = b.iter().zip(&ap).map(|(b, a)| (b - a).powi(2)).sum::<f64>().sqrt();
                if error <= tolerance * 4.0 && x.iter().all(|v| v.is_finite()) {
                    return Ok(x);
                }
                return Err("Lighting matching did not reach a stable solution. Turn off Match lighting to use the unadjusted result.".into());
            }
            self.multiply(&p, &mut ap);
            let denominator = dot(&p, &ap);
            if !denominator.is_finite() || denominator <= 0.0 || !rho.is_finite() || rho <= 0.0 {
                break;
            }
            let alpha = rho / denominator;
            for i in 0..n {
                x[i] += alpha * p[i];
                r[i] -= alpha * ap[i];
                z[i] = r[i] / self.degree[i];
            }
            let next = dot(&r, &z);
            let beta = next / rho;
            for i in 0..n {
                p[i] = z[i] + beta * p[i];
            }
            rho = next;
        }
        Err("Lighting matching did not converge. Turn off Match lighting to use the unadjusted result.".into())
    }
}
fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(a, b)| a * b).sum()
}
fn median(values: &mut [f64]) -> Result<f64, String> {
    values.sort_unstable_by(f64::total_cmp);
    let mid = values.len() / 2;
    let upper = *values.get(mid).ok_or("Missing lighting boundary.")?;
    if values.len().is_multiple_of(2) {
        Ok((upper + *values.get(mid.saturating_sub(1)).ok_or("Missing lighting boundary.")?) * 0.5)
    } else {
        Ok(upper)
    }
}

pub(super) fn apply(
    input: &RetouchInput,
    generated: &[[f32; 3]],
    gain: f32,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<[f32; 3]>, String> {
    check(cancel, deadline)?;
    let count = input
        .width
        .checked_mul(input.height)
        .filter(|n| *n > 0 && input.width <= 1024 && input.height <= 1024)
        .ok_or("Invalid lighting dimensions.")?;
    if generated.len() != count
        || input.rgb.len() != count
        || input.mask.len() != count
        || input.protected.len() != count
        || !gain.is_finite()
        || gain < 1.0
        || !input.mask.iter().all(|v| *v == 0 || *v == 255)
        || !generated.iter().flatten().all(|v| v.is_finite())
        || !input.rgb.iter().flatten().all(|v| v.is_finite())
    {
        return Err("Invalid lighting input.".into());
    }
    let domain = Domain::new(input)?;
    let mut high = gaussian(generated, input.width, input.height, cancel, deadline)?;
    for (h, g) in high.iter_mut().zip(generated) {
        for channel in 0..3 {
            h[channel] = f64::from(g[channel]) - h[channel];
        }
    }
    let selected = |i: usize| input.mask.get(i) == Some(&255) && input.protected.get(i) == Some(&false);
    let mut output = input.rgb.clone();
    for channel in 0..3 {
        check(cancel, deadline)?;
        let mut boundary_values = vec![0.0; count];
        let mut samples = Vec::with_capacity(domain.boundary.len());
        for &i in &domain.boundary {
            let original = input.rgb.get(i).ok_or("Invalid original lighting boundary.")?;
            let h = high.get(i).ok_or("Invalid generated lighting boundary.")?;
            let value = f64::from(original[channel]) - h[channel];
            if !value.is_finite() {
                return Err("Non-finite original lighting boundary.".into());
            }
            boundary_values[i] = value;
            samples.push(value);
        }
        let initial = median(&mut samples)?;
        let b: Vec<f64> = domain
            .pixels
            .iter()
            .map(|&pixel| {
                adjacent(pixel, domain.width, domain.height).into_iter().flatten().filter(|&i| !selected(i)).map(|i| boundary_values[i]).sum()
            })
            .collect();
        let solution = domain.solve(&b, initial, cancel, deadline)?;
        for (&pixel, value) in domain.pixels.iter().zip(solution) {
            let h = high.get(pixel).ok_or("Invalid lighting solution pixel.")?;
            let g = generated.get(pixel).ok_or("Invalid generated lighting pixel.")?;
            let result = h[channel] + value;
            if !result.is_finite() || !(result as f32).is_finite() || (result - f64::from(g[channel])).abs() > 0.125 * f64::from(gain) {
                return Err("Lighting matching would change the result too much. Turn off Match lighting to use the unadjusted result.".into());
            }
            if let Some(p) = output.get_mut(pixel) {
                p[channel] = result as f32;
            }
        }
    }
    check(cancel, deadline)?;
    Ok(output)
}

// A local worker has already restored original RGB outside the mask. Never
// differentiate that composited image: its artificial step would become guide
// texture. Split generated pixels with a masked affine Gaussian instead, then
// harmonically extend an independently fitted exterior/interior residual boundary.
const LOCAL_RADIUS: usize = 12;
const MAX_LOCAL_BOUNDARY: usize = 16_384;

fn local_fit<T: Copy + Into<f64>>(
    values: &[[T; 3]],
    selected: &[bool],
    inside: bool,
    pixel: usize,
    width: usize,
    height: usize,
) -> Result<[f64; 3], String> {
    let (x, y) = (pixel % width, pixel / width);
    // Weighted normal equations for [1, dx/radius, dy/radius], RGB RHS.
    let mut matrix = [[0.0f64; 6]; 3];
    let mut samples = 0usize;
    for yy in y.saturating_sub(LOCAL_RADIUS)..=(y + LOCAL_RADIUS).min(height - 1) {
        for xx in x.saturating_sub(LOCAL_RADIUS)..=(x + LOCAL_RADIUS).min(width - 1) {
            let at = yy * width + xx;
            if selected[at] != inside {
                continue;
            }
            let dx = (xx as f64 - x as f64) / LOCAL_RADIUS as f64;
            let dy = (yy as f64 - y as f64) / LOCAL_RADIUS as f64;
            let weight = (-2.0 * (dx * dx + dy * dy)).exp();
            let basis = [1.0, dx, dy];
            for row in 0..3 {
                for column in 0..3 {
                    matrix[row][column] += weight * basis[row] * basis[column];
                    matrix[row][3 + column] += weight * basis[row] * values[at][column].into();
                }
            }
            samples += 1;
        }
    }
    let scale = matrix[0][0];
    if samples < 6 || !scale.is_finite() || scale <= 0.0 {
        return Err("Local lighting matching needs more surrounding context. Turn off Match lighting to use the unadjusted result.".into());
    }
    for column in 0..3 {
        let pivot =
            (column..3).max_by(|&a, &b| matrix[a][column].abs().total_cmp(&matrix[b][column].abs())).ok_or("Missing local lighting support.")?;
        if !matrix[pivot][column].is_finite() || matrix[pivot][column].abs() < scale * 1e-6 {
            return Err("Local lighting matching needs two-dimensional context. Turn off Match lighting to use the unadjusted result.".into());
        }
        matrix.swap(column, pivot);
        let divisor = matrix[column][column];
        for value in &mut matrix[column][column..] {
            *value /= divisor;
        }
        let pivot_row = matrix[column];
        for (row, values) in matrix.iter_mut().enumerate() {
            if row != column {
                let factor = values[column];
                for k in column..6 {
                    values[k] -= factor * pivot_row[k];
                }
            }
        }
    }
    let value = [matrix[0][3], matrix[0][4], matrix[0][5]];
    if !value.iter().all(|v| v.is_finite()) {
        return Err("Non-finite local lighting fit.".into());
    }
    Ok(value)
}

/// Generated-only affine Gaussian high-frequency residual. Zero padding and
/// normalized sigma32/truncate4 moments match the independent SciPy reference.
/// Only selected pixels require a fit; originals inside the mask are not inputs.
/// The full-resolution horizontal buffer has nine f64 scalars per pixel (72 MiB
/// at 1024²), and is dropped before the boundary fits/CG. No 2-D kernel loop.
fn local_high(
    generated: &[[f32; 3]],
    selected: &[bool],
    width: usize,
    height: usize,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<[f64; 3]>, String> {
    let mut kernel: Vec<[f64; 3]> = (0..=2 * RADIUS)
        .map(|k| {
            let offset = (k as f64 - RADIUS as f64) / SIGMA;
            let weight = (-0.5 * offset * offset).exp();
            [weight, weight * offset, weight * offset * offset]
        })
        .collect();
    let normalization: f64 = kernel.iter().map(|k| k[0]).sum();
    for weights in &mut kernel {
        for value in weights {
            *value /= normalization;
        }
    }
    // [mask0, maskx, maskxx, RGB0(3), RGBx(3)] in sigma-normalized coordinates.
    let mut horizontal = vec![[0.0f64; 9]; generated.len()];
    horizontal.par_chunks_mut(width).enumerate().try_for_each(|(y, row)| -> Result<(), String> {
        check(cancel, deadline)?;
        let first = y * width;
        if !selected[first..first + width].iter().any(|v| *v) {
            return Ok(());
        }
        for (x, moments) in row.iter_mut().enumerate() {
            for xx in x.saturating_sub(RADIUS)..=(x + RADIUS).min(width - 1) {
                let at = first + xx;
                if !selected[at] {
                    continue;
                }
                let weights = kernel[xx + RADIUS - x];
                for k in 0..3 {
                    moments[k] += weights[k];
                    let value = f64::from(generated[at][k]);
                    moments[3 + k] += weights[0] * value;
                    moments[6 + k] += weights[1] * value;
                }
            }
        }
        Ok(())
    })?;
    let mut high = vec![[0.0; 3]; generated.len()];
    high.par_chunks_mut(width).enumerate().try_for_each(|(y, row)| -> Result<(), String> {
        check(cancel, deadline)?;
        for (x, pixel) in row.iter_mut().enumerate() {
            let at = y * width + x;
            if !selected[at] {
                continue;
            }
            // [s, sx, sy, sxx, sxy, syy] and [b, bx, by] for each channel.
            let mut moments = [0.0; 6];
            let mut rhs = [[0.0; 3]; 3];
            for yy in y.saturating_sub(RADIUS)..=(y + RADIUS).min(height - 1) {
                let weights = kernel[yy + RADIUS - y];
                let h = horizontal[yy * width + x];
                moments[0] += weights[0] * h[0];
                moments[1] += weights[0] * h[1];
                moments[2] += weights[1] * h[0];
                moments[3] += weights[0] * h[2];
                moments[4] += weights[1] * h[1];
                moments[5] += weights[2] * h[0];
                for channel in 0..3 {
                    rhs[channel][0] += weights[0] * h[3 + channel];
                    rhs[channel][1] += weights[0] * h[6 + channel];
                    rhs[channel][2] += weights[1] * h[3 + channel];
                }
            }
            let [s, sx, sy, sxx, sxy, syy] = moments;
            if !s.is_finite() || s <= 1e-5 {
                return Err("Local lighting matching needs two-dimensional context. Turn off Match lighting to use the unadjusted result.".into());
            }
            let vx = sxx - sx * sx / s;
            let vy = syy - sy * sy / s;
            let vxy = sxy - sx * sy / s;
            let determinant = vx * vy - vxy * vxy;
            if !determinant.is_finite() || determinant <= 1e-6 * s * s {
                return Err("Local lighting matching needs two-dimensional context. Turn off Match lighting to use the unadjusted result.".into());
            }
            for channel in 0..3 {
                let [b, bx, by] = rhs[channel];
                let ex = bx - sx * b / s;
                let ey = by - sy * b / s;
                let ax = (ex * vy - ey * vxy) / determinant;
                let ay = (ey * vx - ex * vxy) / determinant;
                let low = (b - sx * ax - sy * ay) / s;
                pixel[channel] = f64::from(generated[at][channel]) - low;
                if !pixel[channel].is_finite() {
                    return Err("Non-finite local lighting frequency split.".into());
                }
            }
        }
        Ok(())
    })?;
    check(cancel, deadline)?;
    Ok(high)
}

/// Replace broad generated illumination using surrounding context, preserving
/// the generated-only high-frequency residual. Independent radius12 boundary
/// fits retain local mean brightness continuity without filtering the composite
/// seam. Real interior light curvature/material transitions remain a limitation.
/// Unsupported geometry returns an error so the raw draft stays usable.
pub(super) fn apply_local(
    input: &RetouchInput,
    generated: &[[f32; 3]],
    gain: f32,
    cancel: &AtomicBool,
    deadline: Instant,
) -> Result<Vec<[f32; 3]>, String> {
    check(cancel, deadline)?;
    let count = input
        .width
        .checked_mul(input.height)
        .filter(|n| *n > 0 && input.width <= 1024 && input.height <= 1024)
        .ok_or("Invalid local lighting dimensions.")?;
    if generated.len() != count
        || input.rgb.len() != count
        || input.mask.len() != count
        || input.protected.len() != count
        || !gain.is_finite()
        || gain < 1.0
        || !input.mask.iter().all(|v| *v == 0 || *v == 255)
        || !generated.iter().flatten().all(|v| v.is_finite())
    {
        return Err("Invalid local lighting input.".into());
    }
    let selected: Vec<bool> = input.mask.iter().zip(&input.protected).map(|(mask, protected)| *mask == 255 && !protected).collect();
    // Masked original RGB is deliberately neither sampled nor validated.
    if input.rgb.iter().zip(&selected).any(|(p, inside)| !inside && !p.iter().all(|v| v.is_finite())) {
        return Err("Invalid original local lighting context.".into());
    }
    let domain = Domain::new(input)?;
    if domain.boundary.len() > MAX_LOCAL_BOUNDARY {
        return Err("Local lighting matching has too many boundary pixels. Turn off Match lighting to use the unadjusted result.".into());
    }
    let high = local_high(generated, &selected, input.width, input.height, cancel, deadline)?;
    let mut boundary_values = vec![[0.0; 3]; count];
    for &pixel in &domain.boundary {
        check(cancel, deadline)?;
        let outside = local_fit(&input.rgb, &selected, false, pixel, input.width, input.height)?;
        let inside = local_fit(&high, &selected, true, pixel, input.width, input.height)?;
        for channel in 0..3 {
            boundary_values[pixel][channel] = outside[channel] - inside[channel];
        }
    }
    let mut output = input.rgb.clone();
    for channel in 0..3 {
        check(cancel, deadline)?;
        let mut samples: Vec<f64> = domain.boundary.iter().map(|&i| boundary_values[i][channel]).collect();
        let initial = median(&mut samples)?;
        let b: Vec<f64> = domain
            .pixels
            .iter()
            .map(|&pixel| {
                adjacent(pixel, input.width, input.height).into_iter().flatten().filter(|&i| !selected[i]).map(|i| boundary_values[i][channel]).sum()
            })
            .collect();
        let basis = domain.solve(&b, initial, cancel, deadline)?;
        for (&pixel, value) in domain.pixels.iter().zip(basis) {
            let result = high[pixel][channel] + value;
            let delta = result - f64::from(generated[pixel][channel]);
            if !result.is_finite() || !(result as f32).is_finite() || delta.abs() > 0.125 * f64::from(gain) {
                return Err("Local lighting matching would change the result too much. Turn off Match lighting to use the unadjusted result.".into());
            }
            output[pixel][channel] = result as f32;
        }
    }
    check(cancel, deadline)?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    fn fixture() -> RetouchInput {
        let (width, height) = (96, 80);
        RetouchInput {
            width,
            height,
            rgb: vec![[0.4; 3]; width * height],
            mask: (0..width * height).map(|i| if (24..72).contains(&(i % width)) && (20..60).contains(&(i / width)) { 255 } else { 0 }).collect(),
            protected: vec![false; width * height],
            rect: [0., 0., 1., 1.],
            seed: 0,
            steps: 20,
            effects: false,
        }
    }
    fn run(input: &RetouchInput, g: &[[f32; 3]]) -> Result<Vec<[f32; 3]>, String> {
        apply(input, g, 1., &AtomicBool::new(false), Instant::now() + Duration::from_secs(30))
    }
    #[test]
    fn constant_boundary_protection_and_poisoned_original_interior() {
        let mut input = fixture();
        input.protected[40 * input.width + 48] = true;
        let generated = vec![[0.47; 3]; input.rgb.len()];
        let first = run(&input, &generated).unwrap();
        for (i, p) in first.iter().enumerate() {
            if input.mask[i] == 0 || input.protected[i] {
                assert_eq!(*p, input.rgb[i]);
            } else {
                assert!(p.iter().all(|v| (*v - 0.4).abs() < 2e-6));
            }
        }
        for (i, p) in input.rgb.iter_mut().enumerate() {
            if input.mask[i] == 255 && !input.protected[i] {
                *p = [100., -100., 50.];
            }
        }
        let second = run(&input, &generated).unwrap();
        for (i, p) in first.iter().enumerate() {
            if input.mask[i] == 255 && !input.protected[i] {
                assert_eq!(*p, second[i]);
            }
        }
    }
    #[test]
    fn hdr_outside_and_protected_hole_stay_bit_exact() {
        let mut input = fixture();
        let protected = 40 * input.width + 48;
        input.rgb[0] = [4., 2., 1.];
        input.rgb[protected] = [3., 1.5, 0.5];
        input.protected[protected] = true;
        let generated: Vec<_> = input.rgb.iter().map(|p| p.map(|v| v + 0.02)).collect();
        let result = run(&input, &generated).unwrap();
        for (i, p) in result.iter().enumerate() {
            if input.mask[i] == 0 || input.protected[i] {
                assert_eq!(*p, input.rgb[i]);
            }
        }
    }
    #[test]
    fn gradient_blob_and_known_strong_edge_limit() {
        let mut input = fixture();
        for (i, p) in input.rgb.iter_mut().enumerate() {
            *p = [0.2 + (i % input.width) as f32 * 0.001 + (i / input.width) as f32 * 0.0003; 3];
        }
        let generated: Vec<_> = input.rgb.iter().map(|p| p.map(|v| v + 0.05)).collect();
        let result = run(&input, &generated).unwrap();
        assert!(result.iter().zip(&input.rgb).all(|(a, b)| a.iter().zip(b).all(|(a, b)| (*a - *b).abs() < 0.001)));
        input = fixture();
        let generated: Vec<_> = (0..input.rgb.len())
            .map(|i| {
                let x = (i % input.width) as f32 - 48.;
                let y = (i / input.width) as f32 - 40.;
                [0.4 + 0.07 * (-(x * x + y * y) / (2. * 35. * 35.)).exp(); 3]
            })
            .collect();
        let result = run(&input, &generated).unwrap();
        let selected: Vec<_> = (0..input.rgb.len()).filter(|&i| input.mask[i] == 255).collect();
        let before: f32 = selected.iter().map(|&i| (generated[i][0] - 0.4).abs()).sum();
        let after: f32 = selected.iter().map(|&i| (result[i][0] - 0.4).abs()).sum();
        assert!(after < before * 0.4);
        input.width = 256;
        input.height = 256;
        input.rgb = vec![[0.; 3]; 256 * 256];
        input.protected = vec![false; 256 * 256];
        input.mask = (0..256 * 256).map(|i| if (72..184).contains(&(i % 256)) && (72..184).contains(&(i / 256)) { 255 } else { 0 }).collect();
        for (i, p) in input.rgb.iter_mut().enumerate() {
            *p = [if i % input.width < 128 { 0.25 } else { 0.65 }; 3];
        }
        let generated: Vec<_> = input.rgb.iter().map(|p| p.map(|v| v + 0.04)).collect();
        let edge = run(&input, &generated);
        // A characterization, not a requirement to damage a particular edge:
        // broad material steps can be altered by high-frequency-only guidance.
        match edge {
            Ok(result) => {
                let mut error = 0.0;
                let mut count = 0;
                for (i, p) in result.iter().enumerate() {
                    if input.mask[i] == 0 {
                        assert_eq!(*p, input.rgb[i]);
                    } else {
                        assert!(p.iter().all(|v| v.is_finite()));
                        error += f64::from((p[0] - input.rgb[i][0]).abs());
                        count += 1;
                    }
                }
                println!("Known material-edge characterization: selected_MAE={:.8}", error / count as f64);
            }
            Err(message) => assert!(message.contains("too much"), "{message}"),
        }
    }
    #[test]
    fn unsupported_cancel_deadline_and_boundaryless_masks_fail_safely() {
        let mut input = fixture();
        let generated = vec![[0.45; 3]; input.rgb.len()];
        assert!(apply(&input, &generated, 1., &AtomicBool::new(true), Instant::now() + Duration::from_secs(1)).unwrap_err().contains("Cancelled"));
        assert!(apply(&input, &generated, 1., &AtomicBool::new(false), Instant::now()).unwrap_err().contains("timed out"));
        input.mask.fill(255);
        assert!(run(&input, &generated).unwrap_err().contains("context"));
        input.mask.fill(0);
        input.mask[100] = 255;
        assert!(run(&input, &generated).is_err());
        input = fixture();
        let bad = vec![[0.9; 3]; input.rgb.len()];
        assert!(run(&input, &bad).unwrap_err().contains("too much"));
    }
}

#[cfg(test)]
mod local_tests {
    use super::*;
    use std::time::Duration;

    fn fixture() -> RetouchInput {
        let (width, height) = (96, 80);
        RetouchInput {
            width,
            height,
            rgb: (0..width * height).map(|i| [0.3 + (i % width) as f32 * 0.001 + (i / width) as f32 * 0.0007; 3]).collect(),
            mask: (0..width * height).map(|i| if (24..72).contains(&(i % width)) && (20..60).contains(&(i / width)) { 255 } else { 0 }).collect(),
            protected: vec![false; width * height],
            rect: [0., 0., 1., 1.],
            seed: 0,
            steps: 20,
            effects: false,
        }
    }
    fn run(input: &RetouchInput, generated: &[[f32; 3]]) -> Result<Vec<[f32; 3]>, String> {
        apply_local(input, generated, 1., &AtomicBool::new(false), Instant::now() + Duration::from_secs(30))
    }
    fn generated(input: &RetouchInput, affine: bool) -> Vec<[f32; 3]> {
        input
            .rgb
            .iter()
            .enumerate()
            .map(|(i, p)| {
                if input.mask[i] == 255 && !input.protected[i] {
                    let offset = 0.04 + if affine { (i % input.width) as f32 * 0.0002 + (i / input.width) as f32 * 0.0001 } else { 0. };
                    p.map(|v| v + offset)
                } else {
                    *p
                }
            })
            .collect()
    }
    #[test]
    fn local_offset_and_affine_ramp_do_not_inherit_composite_boundary_step() {
        let input = fixture();
        let correct = run(&input, &input.rgb).unwrap();
        assert!(correct.iter().zip(&input.rgb).all(|(a, b)| a.iter().zip(b).all(|(a, b)| (*a - *b).abs() < 2e-6)));
        for affine in [false, true] {
            let result = run(&input, &generated(&input, affine)).unwrap();
            for (i, pixel) in result.iter().enumerate() {
                if input.mask[i] == 0 {
                    assert_eq!(*pixel, input.rgb[i]);
                } else {
                    assert!((pixel[0] - input.rgb[i][0]).abs() < 2e-6, "{i}: {pixel:?}");
                }
            }
        }
    }
    #[test]
    fn local_poisoned_original_and_protected_hole_are_not_references() {
        let mut input = fixture();
        for y in 34..46 {
            for x in 42..54 {
                input.protected[y * input.width + x] = true;
            }
        }
        let prediction = generated(&input, false);
        let first = run(&input, &prediction).unwrap();
        for (i, p) in input.rgb.iter_mut().enumerate() {
            if input.mask[i] == 255 && !input.protected[i] {
                *p = [f32::NAN, -100., 100.];
            }
        }
        let second = run(&input, &prediction).unwrap();
        for (i, pixel) in first.iter().enumerate() {
            assert_eq!(*pixel, second[i]);
        }
        for (i, pixel) in second.iter().enumerate() {
            if input.mask[i] == 0 || input.protected[i] {
                assert_eq!(*pixel, input.rgb[i]);
            }
        }
    }
    #[test]
    fn local_disconnected_and_border_masks_stay_finite_and_preserve_context() {
        for border in [false, true] {
            let mut input = fixture();
            for (i, mask) in input.mask.iter_mut().enumerate() {
                let (x, y) = (i % input.width, i / input.width);
                *mask = if (16..64).contains(&y) && if border { x < 48 } else { (12..36).contains(&x) || (60..84).contains(&x) } { 255 } else { 0 };
            }
            let prediction = generated(&input, false);
            let result = run(&input, &prediction).unwrap();
            for (i, pixel) in result.iter().enumerate() {
                if input.mask[i] == 0 {
                    assert_eq!(*pixel, input.rgb[i]);
                } else {
                    assert!(pixel.iter().all(|v| v.is_finite()));
                    if !border {
                        assert!((pixel[0] - input.rgb[i][0]).abs() < 2e-6);
                    }
                    // At an image edge there is no exterior sample along the
                    // missing normal: harmonic extension need not reproduce
                    // a ramp in that direction. It must still remain bounded.
                    assert!((pixel[0] - prediction[i][0]).abs() <= 0.125);
                }
            }
        }
    }
    #[test]
    fn local_broad_interior_cloud_is_removed_without_original_interior_guidance() {
        let mut input = fixture();
        input.width = 384;
        input.height = 384;
        input.rgb.clear();
        input.mask.clear();
        input.protected = vec![false; input.width * input.height];
        let mut prediction = Vec::with_capacity(input.width * input.height);
        let mut truth = Vec::with_capacity(input.width * input.height);
        for y in 0..input.height {
            for x in 0..input.width {
                let u = x as f64 / input.width as f64;
                let v = y as f64 / input.height as f64;
                let texture = 0.008 * (std::f64::consts::TAU * x as f64 / 9.0).sin()
                    + 0.005 * (std::f64::consts::TAU * (y as f64 + 0.27 * x as f64) / 5.0).sin();
                let clean = (0.2 + 0.06 * u + 0.02 * v + texture) as f32;
                let inside = ((u - 0.5) / 0.30).powi(2) + ((v - 0.51) / 0.34).powi(2) < 1.0;
                let cloud = 0.065 * (-0.5 * (((u - 0.52) / 0.18).powi(2) + ((v - 0.51) / 0.20).powi(2))).exp();
                input.mask.push(if inside { 255 } else { 0 });
                // No clean original interior is available to the algorithm.
                input.rgb.push(if inside { [f32::NAN; 3] } else { [clean; 3] });
                truth.push(clean);
                prediction.push([clean + if inside { cloud as f32 } else { 0.0 }; 3]);
            }
        }
        let result = run(&input, &prediction).unwrap();
        let mut before = 0.0;
        let mut after = 0.0;
        for (i, pixel) in result.iter().enumerate() {
            if input.mask[i] == 255 {
                before += f64::from((prediction[i][0] - truth[i]).abs());
                after += f64::from((pixel[0] - truth[i]).abs());
            } else {
                assert_eq!(*pixel, input.rgb[i]);
            }
        }
        // Boundary-only harmonic offsets retain this broad interior error;
        // frequency replacement must remove at least 85% of its mean error.
        assert!(after < before * 0.15, "before={before}; after={after}");
    }
    #[test]
    fn local_frequency_contract_changes_broad_material_but_preserves_generated_high_pass() {
        let mut input = fixture();
        for (i, p) in input.rgb.iter_mut().enumerate() {
            if i % input.width >= 48 {
                for value in p {
                    *value += 0.08;
                }
            }
        }
        let mut prediction = generated(&input, false);
        for (i, pixel) in prediction.iter_mut().enumerate() {
            if input.mask[i] == 255 {
                for value in pixel {
                    *value += 0.012 * ((i % input.width) as f32).sin();
                }
            }
        }
        let result = run(&input, &prediction).unwrap();
        let selected: Vec<_> = input.mask.iter().map(|v| *v == 255).collect();
        let high =
            local_high(&prediction, &selected, input.width, input.height, &AtomicBool::new(false), Instant::now() + Duration::from_secs(30)).unwrap();
        let mut max_material_change = 0.0f32;
        for (i, pixel) in result.iter().enumerate() {
            if !selected[i] {
                assert_eq!(*pixel, input.rgb[i]);
            } else {
                max_material_change = max_material_change.max((pixel[0] - input.rgb[i][0]).abs());
                assert!(pixel.iter().all(|v| v.is_finite()));
            }
        }
        // Legitimate broad material structure is no longer an exact invariant.
        assert!(max_material_change > 0.001);
        // The basis is harmonic: the output retains the *masked high-pass*
        // Laplacian, not the entire generated image's low-frequency curvature.
        // Check several textured interior points, including the material edge.
        for y in [30, 40, 50] {
            for x in [35, 47, 48, 60] {
                let i = y * input.width + x;
                let result_lap = 4.0 * f64::from(result[i][0])
                    - adjacent(i, input.width, input.height).into_iter().flatten().map(|j| f64::from(result[j][0])).sum::<f64>();
                let high_lap = 4.0 * high[i][0] - adjacent(i, input.width, input.height).into_iter().flatten().map(|j| high[j][0]).sum::<f64>();
                assert!((result_lap - high_lap).abs() < 2e-6, "{x},{y}: {result_lap} vs {high_lap}");
            }
        }
        // Away from the material edge, the generated fine oscillation's
        // curvature is retained (the broad fitted baseline is much smoother).
        let i = 40 * input.width + 60;
        let texture_lap = 0.012 * (2.0 * 60.0f64.sin() - 59.0f64.sin() - 61.0f64.sin());
        let high_lap = 4.0 * high[i][0] - adjacent(i, input.width, input.height).into_iter().flatten().map(|j| high[j][0]).sum::<f64>();
        assert!((high_lap - texture_lap).abs() < 2e-4, "{high_lap} vs {texture_lap}");
    }
    #[test]
    fn local_unsupported_geometry_limits_and_interruptions_fail_safely() {
        let mut input = fixture();
        let prediction = generated(&input, false);
        assert!(
            apply_local(&input, &prediction, 1., &AtomicBool::new(true), Instant::now() + Duration::from_secs(1)).unwrap_err().contains("Cancelled")
        );
        assert!(apply_local(&input, &prediction, 1., &AtomicBool::new(false), Instant::now()).unwrap_err().contains("timed out"));
        assert!(run(&input, &generated(&input, false).iter().map(|p| p.map(|v| v + 0.3)).collect::<Vec<_>>()).unwrap_err().contains("too much"));
        input.mask.fill(255);
        assert!(run(&input, &prediction).unwrap_err().contains("context"));
        for (i, mask) in input.mask.iter_mut().enumerate() {
            *mask = if i % input.width == 48 && (10..70).contains(&(i / input.width)) { 255 } else { 0 };
        }
        assert!(run(&input, &prediction).unwrap_err().contains("two-dimensional"));
        input = fixture();
        input.mask[0] = 1;
        assert!(run(&input, &prediction).unwrap_err().contains("Invalid"));
    }
}

#[cfg(test)]
mod saved_local_reference {
    use super::*;
    #[test]
    #[ignore = "explicit offline archived local RAW patch parity; no model inference"]
    fn local_match_agrees_with_independent_saved_reference() {
        let root = std::path::PathBuf::from(std::env::var_os("LIGHTCRAFT_LOCAL_LIGHTING_REFERENCE_DIR").expect("reference directory"));
        let settings: serde_json::Value = serde_json::from_slice(&std::fs::read(root.join("local-settings.json")).unwrap()).unwrap();
        let baseline: lightcraft_develop::AiEdit = serde_json::from_value(settings["ai_edits"][0].clone()).unwrap();
        let pixels = baseline.decode_pixels().unwrap();
        let input = RetouchInput {
            width: baseline.width as usize,
            height: baseline.height as usize,
            rgb: pixels.iter().map(|p| if p[3] > 0. { [f32::NAN; 3] } else { [p[0], p[1], p[2]] }).collect(),
            mask: pixels.iter().map(|p| if p[3] > 0. { 255 } else { 0 }).collect(),
            protected: vec![false; pixels.len()],
            rect: baseline.rect,
            seed: baseline.seed.try_into().unwrap(),
            steps: 20,
            effects: false,
        };
        let generated: Vec<_> = pixels.iter().map(|p| [p[0], p[1], p[2]]).collect();
        let start = Instant::now();
        let result = apply_local(&input, &generated, 1., &AtomicBool::new(false), Instant::now() + std::time::Duration::from_secs(90)).unwrap();
        let bytes = std::fs::read(root.join("candidate.rgba")).unwrap();
        assert_eq!(bytes.len(), pixels.len() * 16);
        let mut max_error = 0.0f32;
        let mut out = Vec::with_capacity(bytes.len());
        for (i, (rgb, old)) in result.iter().zip(&pixels).enumerate() {
            if old[3] == 0. {
                assert_eq!(*rgb, [old[0], old[1], old[2]]);
            }
            for c in 0..3 {
                let at = i * 16 + c * 4;
                let expected = f32::from_le_bytes(bytes[at..at + 4].try_into().unwrap());
                max_error = max_error.max((rgb[c] - expected).abs());
            }
            for value in [rgb[0], rgb[1], rgb[2], old[3]] {
                out.extend_from_slice(&value.to_le_bytes());
            }
        }
        assert!(max_error < 3e-6, "max error {max_error}");
        std::fs::write(root.join("rust-candidate.rgba"), out).unwrap();
        println!("archived local reference max_RGB_error={max_error}; elapsed={:?}", start.elapsed());
    }
}
