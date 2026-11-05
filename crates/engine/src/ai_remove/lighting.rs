//! Optional cloud-only boundary illumination matching. No original masked RGB is
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
        return Err("Cloud lighting matching timed out. Turn off Match lighting to use the unadjusted result.".into());
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
