use super::pairs::{displayed, local_contrast};
use super::proxy::{LensGeometry, photometric_proxies};
use super::{CameraLook, CameraTone, CameraTransform, HueSat, LookProvenance, RawImage, Rgb32f, ToneMap, luminance_2020};
use crate::camera_profiles::CameraProfile;
use lightcraft_color::perceptual::oklab_from_2020;

/// Fit on three spatial folds. The fourth checks both developments against the same
/// camera pixels, including shadow detail that is rare in the image as a whole.
fn reserved(i: usize, w: usize) -> bool {
    ((i % w) / 6 + 3 * ((i / w) / 6)).is_multiple_of(4)
}

pub(super) fn with_reference(
    raw: &RawImage,
    bytes: &[u8],
    transform: &CameraTransform,
    lens: Option<&LensGeometry>,
    embedded: Option<&lightcraft_develop::EmbeddedLens>,
    p: &CameraProfile,
    legacy: Option<&CameraLook>,
) -> Option<CameraLook> {
    let matrix = p.matrix().mul(&transform.matrix.inverse()?);
    let (sensor, reference, clipped) = photometric_proxies(raw, bytes, transform, lens, embedded)?;
    let mut pairs = Vec::new();
    let mut clean = Vec::new();
    let mut colours = Vec::new();
    for (i, ((x, y), clip)) in sensor.data.iter().zip(&reference.data).zip(&clipped).enumerate() {
        if *clip || reserved(i, sensor.width) || !x.iter().all(|v| v.is_finite()) {
            continue;
        }
        let target = luminance_2020(*y);
        if !(0.00005..0.98).contains(&target) {
            continue;
        }
        let lum = luminance_2020(matrix.apply_f32(*x).map(|v| v.max(0.)));
        if lum <= 0.00002 || !lum.is_finite() {
            continue;
        }
        pairs.push((f64::from(lum), f64::from(target)));
        if local_contrast(&sensor, i) < 1.65 && local_contrast(&reference, i) < 1.65 {
            clean.push((f64::from(lum), f64::from(target)));
            if i % 5 == 0 && y.iter().all(|v| *v > 0.0004 && *v < 0.93) {
                colours.push((x.map(f64::from), y.map(f64::from)));
            }
        }
    }
    let tone = super::tone::fit_tone(pairs)?;
    let regional = super::tone::fit_registered(&clean, tone).unwrap_or(tone);
    let hue_sat = super::hue_sat::fit_hue_sat_prior(&colours, &matrix, p.hue_sat.as_ref()).or_else(|| p.hue_sat.clone());
    let base = CameraLook { matrix, tone: regional, hue_sat, provenance: LookProvenance { profile: true, ..Default::default() } };
    let prior = legacy.cloned().unwrap_or_else(|| CameraLook {
        matrix: lightcraft_color::Mat3::IDENTITY,
        tone: CameraTone::generic(),
        hue_sat: None,
        provenance: Default::default(),
    });
    let evaluate = |look: &CameraLook| score(look, &sensor, &reference, &clipped);
    let before = evaluate(&prior)?;
    let calibrated = evaluate(&base)?;
    // Colour is compared at common reference luminance. Hence a better toe cannot
    // buy a worse hue or a loss of chroma. Near ties keep the existing rendition.
    let use_profile_colour = calibrated[1] + 0.00035 < before[1];
    let colour = if use_profile_colour { &base } else { &prior };
    let mut best = colour.clone();
    best.tone = prior.tone.with_chroma(*colour.tone.chroma())?;
    let mut loss = evaluate(&best)?[0];
    for luminance in [tone, regional] {
        let mut proposal = colour.clone();
        proposal.tone = luminance.with_chroma(*colour.tone.chroma())?;
        let error = evaluate(&proposal)?[0];
        if error + 0.0008 < loss {
            loss = error;
            best = proposal;
        }
    }
    if lightcraft_pipeline::profiling() {
        eprintln!("[profile] calibrated reference L/C {:?} -> {:?}, selected {:?} / {}", before, calibrated, loss, use_profile_colour);
    }
    if !use_profile_colour && best.tone == prior.tone {
        return None;
    }
    // A referenced profile supplied parts of this look even when the photo supplied the colour.
    // Keep the basis precise: profile provenance denotes its matrix/table.
    if best.tone.knots() != colour.tone.knots() {
        best.provenance.dro_tone_removed = false;
        best.provenance.dark_tone = false;
    }
    Some(best)
}

/// Measure the perceptual error over the same spatial holdout. Account for
/// the dominant subject and for less abundant dark and colourful surfaces.
fn score(look: &CameraLook, sensor: &Rgb32f, reference: &Rgb32f, clipped: &[bool]) -> Option<[f32; 2]> {
    let hue = look.hue_sat.as_ref().and_then(HueSat::new);
    let curve = ToneMap::camera(&look.tone, 0., 0., 0.);
    let mut groups: [Vec<[f32; 2]>; 10] = Default::default();
    for (i, ((x, y), clip)) in sensor.data.iter().zip(&reference.data).zip(clipped).enumerate() {
        if *clip || !reserved(i, sensor.width) || local_contrast(sensor, i) > 2. || local_contrast(reference, i) > 2. {
            continue;
        }
        let target = oklab_from_2020(*y);
        if !(0.035..0.97).contains(&target[0]) {
            continue;
        }
        let rgb = look.matrix.apply_f32(*x);
        let rgb = hue.as_ref().map_or(rgb, |h| h.apply(rgb));
        let predicted = displayed(rgb.map(f64::from), &curve).map(|v| v as f32);
        let seen = luminance_2020(predicted);
        if seen <= 0.000001 {
            continue;
        }
        let neutral = oklab_from_2020(predicted.map(|v| v * luminance_2020(*y) / seen));
        let got = oklab_from_2020(predicted);
        let err = [(got[0] - target[0]).abs(), (neutral[1] - target[1]).hypot(neutral[2] - target[2])];
        if !err.iter().all(|v| v.is_finite()) {
            continue;
        }
        groups[0].push(err);
        let zone = if target[0] < 0.37 {
            1
        } else if target[1].hypot(target[2]) > 0.04 {
            2
        } else {
            3
        };
        groups[zone].push(err);
        if target[1].hypot(target[2]) > 0.065 * target[0].max(0.25) {
            let turn = target[2].atan2(target[1]).rem_euclid(std::f32::consts::TAU) / std::f32::consts::TAU;
            groups[4 + ((6. * turn) as usize).min(5)].push(err);
        }
    }
    if groups[0].len() < 64 {
        return None;
    }
    let mut total = [0.; 2];
    let mut weights = [0.; 2];
    for (index, group) in groups.iter_mut().enumerate() {
        if group.len() < 16 {
            continue;
        }
        for axis in 0..2 {
            let w = match (index, axis) {
                (0, _) => 3.,
                (1..=3, _) => 1.,
                (_, 0) => 0.,
                _ => 1.,
            };
            if w == 0. {
                continue;
            }
            weights[axis] += w;
            group.sort_unstable_by(|a, b| a[axis].total_cmp(&b[axis]));
            let limit = group[group.len() * 95 / 100][axis];
            total[axis] += w * group.iter().map(|v| v[axis].min(limit)).sum::<f32>() / group.len() as f32;
        }
    }
    Some(std::array::from_fn(|i| total[i] / weights[i]))
}
