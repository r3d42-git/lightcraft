//! Recover colour when a Sony file's starting look is measurably less colourful than its JPEG.
//! The fit works on as-shot sensor values. It never reads the user's Develop settings.
use super::hue_sat::{HueSat, fit_hue_sat};
use super::pairs::{EDGE_CONTRAST, colour_pair, displayed, local_contrast};
use super::proxy::structure_agrees;
use super::{CameraLook, CameraTone, LookProvenance, Mat3, Rgb32f, ToneMap};
use lightcraft_color::perceptual::oklab_from_2020;

const MIN_SAMPLES: usize = 64;
const MIN_CHROMA: f32 = 0.025;

fn median(values: &mut [f32]) -> f32 {
    let middle = values.len() / 2;
    *values.select_nth_unstable_by(middle, f32::total_cmp).1
}

fn lab(scene: [f64; 3], tone: &ToneMap) -> [f32; 3] {
    oklab_from_2020(displayed(scene, tone).map(|v| v as f32))
}

fn chroma_error(a: [f32; 3], b: [f32; 3]) -> f32 {
    (a[1] - b[1]).hypot(a[2] - b[2])
}

// Entire 8×8 tiles are reserved for validation. Neighbouring pixels within each tile
// share JPEG processing and are poor independent witnesses for a gain learned there.
fn samples<'a>(sensor: &'a Rgb32f, reference: &'a Rgb32f, clipped: &'a [bool]) -> impl Iterator<Item = (bool, [f32; 3], [f32; 3])> + 'a {
    sensor.data.iter().zip(&reference.data).zip(clipped).enumerate().filter_map(|(i, ((&x, &y), &clip))| {
        if clip || !colour_pair(x, y) || local_contrast(reference, i) > EDGE_CONTRAST {
            return None;
        }
        let tile = (i % sensor.width) / 8 + (i / sensor.width) / 8;
        Some((tile.is_multiple_of(2), x, y))
    })
}

pub(super) fn recover_chroma(look: &CameraLook, sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool]) -> Option<CameraTone> {
    if sensor.width == 0
        || (sensor.width, sensor.height) != (reference.width, reference.height)
        || sensor.width.checked_mul(sensor.height) != Some(sensor.data.len())
        || sensor.data.len() != reference.data.len()
        || clipped.len() != sensor.data.len()
    {
        return None;
    }
    let table = look.hue_sat.as_ref().and_then(HueSat::new);
    let tone = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
    let (mut scales, mut held) = (Vec::new(), Vec::new());
    for (validate, x, y) in samples(sensor, reference, clipped) {
        let scene = look.matrix.apply_f32(x);
        let scene = table.as_ref().map_or(scene, |t| t.apply(scene)).map(f64::from);
        let target = oklab_from_2020(y);
        let predicted = lab(scene, &tone);
        let (wanted, actual) = (target[1].hypot(target[2]), predicted[1].hypot(predicted[2]));
        if wanted < MIN_CHROMA || actual < 0.005 {
            continue;
        }
        if validate {
            held.push((scene, target, chroma_error(predicted, target)));
        } else {
            scales.push(wanted / actual);
        }
    }
    if scales.len() < MIN_SAMPLES || held.len() < MIN_SAMPLES {
        return None;
    }
    let scale = median(&mut scales);
    if scale < 1.25 || !structure_agrees(sensor, reference) {
        return None;
    }
    // Shrink the estimate towards unity because the proxy averages fine colour detail.
    // Fade through the upper midtones, keeping the existing chroma of near-white highlights.
    let strength = (scale.min(3.0) - 1.0) * 0.85;
    let mut nodes = *look.tone.chroma();
    let last = (nodes.len() - 1) as f32;
    for (index, value) in nodes.iter_mut().enumerate() {
        let fade = ((0.90 - index as f32 / last) / 0.40).clamp(0.0, 1.0);
        *value = (*value * (1.0 + strength * fade)).min(4.0);
    }
    let candidate = look.tone.with_chroma(nodes)?;
    let after = ToneMap::camera(&candidate, 0.0, 0.0, 0.0);
    let mut before_errors = held.iter().map(|(_, _, error)| *error).collect::<Vec<_>>();
    let mut after_errors = held.iter().map(|(scene, target, _)| chroma_error(lab(*scene, &after), *target)).collect::<Vec<_>>();
    let (old, new) = (median(&mut before_errors), median(&mut after_errors));
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] Sony chroma estimate {scale:.3}, held-out colour error {old:.4} -> {new:.4}");
    }
    (new < old * 0.8).then_some(candidate)
}

/// If no full look was accepted, keep the existing fallback luminance. The camera's
/// colour can also differ in hue (e.g. orange skin rendered yellow). Try a hue-only
/// table when the independent tiles confirm an improvement over the chroma gain alone.
pub(super) fn recover_fallback(sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool]) -> Option<CameraLook> {
    let mut look = CameraLook {
        tone: CameraTone::generic(),
        matrix: Mat3::IDENTITY,
        hue_sat: None,
        provenance: LookProvenance { partial: true, ..Default::default() },
    };
    look.tone = recover_chroma(&look, sensor, reference, clipped)?;
    let (mut train, mut held) = (Vec::new(), Vec::new());
    for (validate, x, y) in samples(sensor, reference, clipped) {
        if validate {
            let target = oklab_from_2020(y);
            if target[1].hypot(target[2]) >= MIN_CHROMA {
                held.push((x, target));
            }
        } else {
            train.push((x.map(f64::from), y.map(f64::from)));
        }
    }
    if let Some(mut hue) = fit_hue_sat(&train, &look.matrix) {
        for entry in &mut hue.data {
            entry[1] = 1.0;
        }
        if let Some(table) = HueSat::new(&hue) {
            let map = ToneMap::camera(&look.tone, 0.0, 0.0, 0.0);
            let error = |rgb: [f32; 3], target| chroma_error(lab(rgb.map(f64::from), &map), target);
            let mut old: Vec<_> = held.iter().map(|&(x, y)| error(x, y)).collect();
            let mut new: Vec<_> = held.iter().map(|&(x, y)| error(table.apply(x), y)).collect();
            if held.len() >= MIN_SAMPLES && median(&mut new) < median(&mut old) * 0.85 {
                look.hue_sat = Some(hue);
            }
        }
    }
    Some(look)
}

#[cfg(test)]
mod tests;
