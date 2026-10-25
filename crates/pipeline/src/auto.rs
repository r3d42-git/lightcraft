//! Image-dependent, rendered-clipping-guarded Auto tone and auto white balance (grey-world
//! statistics on a proxy).

use lightcraft_color::cct::xy_to_temp_tint;
use lightcraft_color::{REC2020, Xy, bradford, luminance_2020};
use lightcraft_develop::DevelopSettings;
use lightcraft_raster::Rgb32f;
use serde::Serialize;

use crate::SourceInfo;
use crate::local::effective_wb;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
pub struct AutoTone {
    pub exposure: f64,
    pub contrast: f64,
    pub highlights: f64,
    pub shadows: f64,
    pub whites: f64,
    pub blacks: f64,
    pub vibrance: f64,
    pub saturation: f64,
}

impl AutoTone {
    /// Replace only the eight Auto-owned controls, preserving white balance and every other edit.
    pub fn apply_to(self, settings: &mut DevelopSettings) {
        settings.light.exposure = self.exposure;
        settings.light.contrast = self.contrast;
        settings.light.highlights = self.highlights;
        settings.light.shadows = self.shadows;
        settings.light.whites = self.whites;
        settings.light.blacks = self.blacks;
        settings.color.vibrance = self.vibrance;
        settings.color.saturation = self.saturation;
    }

    fn current(s: &DevelopSettings) -> Self {
        let finite = |v: f64, limit: f64| if v.is_finite() { v.clamp(-limit, limit) } else { 0.0 };
        Self {
            exposure: finite(s.light.exposure, 5.0),
            contrast: finite(s.light.contrast, 100.0),
            highlights: finite(s.light.highlights, 100.0),
            shadows: finite(s.light.shadows, 100.0),
            whites: finite(s.light.whites, 100.0),
            blacks: finite(s.light.blacks, 100.0),
            vibrance: finite(s.color.vibrance, 100.0),
            saturation: finite(s.color.saturation, 100.0),
        }
    }

    fn scaled(self, scale: f64) -> Self {
        let round = |v: f64| (v * scale * 100.0).round() / 100.0;
        Self {
            exposure: round(self.exposure),
            contrast: round(self.contrast),
            highlights: round(self.highlights),
            shadows: round(self.shadows),
            whites: round(self.whites),
            blacks: round(self.blacks),
            vibrance: round(self.vibrance),
            saturation: round(self.saturation),
        }
    }

    /// Keep the proposed midtone lift while progressively removing endpoint pressure.
    /// Amounts depend on the measured proposal, not a second fixed look. This sequence is
    /// independent of the starting edit, which preserves the fixed-point property of Auto.
    fn protected(self, strength: f64) -> Self {
        let brightening = self.exposure.max(0.0);
        let contrast = self.contrast.max(0.0);
        let white_lift = self.whites.max(0.0);
        Self {
            highlights: (self.highlights - strength * (brightening * 30.0 + contrast * 0.4 + white_lift)).max(-75.0),
            whites: (self.whites - strength * (brightening * 15.0 + contrast * 0.3 + white_lift)).max(-30.0),
            blacks: self.blacks + strength * (contrast * 0.05 + (-self.exposure).max(0.0)),
            contrast: self.contrast - contrast * strength.min(1.0),
            vibrance: self.vibrance - self.vibrance.max(0.0) * strength.min(1.0),
            ..self
        }
    }
}

/// Box statistics and an extrema-preserving spatial validation proxy. Each 2×2 validation tile
/// contains its mean, darkest pixel, brightest channel pixel, and most chromatic pixel. This
/// retains small lamps and saturated colours that a box average alone would hide. Both proxies
/// have at most 256² pixels; source traversal is linear and does not clone a full-resolution RAW.
fn tone_proxies(src: &Rgb32f) -> Option<(Rgb32f, Rgb32f)> {
    if src.width == 0
        || src.height == 0
        || src.width.checked_mul(src.height)? != src.data.len()
        || src.data.iter().any(|p| p.iter().any(|v| !v.is_finite()))
    {
        return None;
    }
    if src.width.max(src.height) <= 256 {
        return Some((src.clone(), src.clone()));
    }
    let long = src.width.max(src.height);
    let w = ((src.width as u128 * 128 / long as u128) as usize).max(1);
    let h = ((src.height as u128 * 128 / long as u128) as usize).max(1);
    let mut mean = Rgb32f::new(w, h);
    let mut guard = Rgb32f::new(w * 2, h * 2);
    for y in 0..h {
        for x in 0..w {
            let x0 = (x as u128 * src.width as u128 / w as u128) as usize;
            let x1 = ((x + 1) as u128 * src.width as u128 / w as u128) as usize;
            let y0 = (y as u128 * src.height as u128 / h as u128) as usize;
            let y1 = ((y + 1) as u128 * src.height as u128 / h as u128) as usize;
            let (mut sum, mut count) = ([0.0f64; 3], 0usize);
            let (mut dark, mut bright, mut colorful) = ([0.0; 3], [0.0; 3], [0.0; 3]);
            let (mut min_y, mut max_c, mut max_chroma) = (f32::INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
            for sy in y0..y1 {
                for sx in x0..x1 {
                    let p = *src.data.get(sy.checked_mul(src.width)?.checked_add(sx)?)?;
                    for (acc, v) in sum.iter_mut().zip(p) {
                        *acc += v as f64;
                    }
                    count += 1;
                    let luma = luminance_2020(p);
                    let hi = p.into_iter().fold(f32::NEG_INFINITY, f32::max);
                    let lo = p.into_iter().fold(f32::INFINITY, f32::min);
                    let chroma = (hi - lo) / hi.max(1e-6);
                    if luma < min_y {
                        min_y = luma;
                        dark = p;
                    }
                    if hi > max_c {
                        max_c = hi;
                        bright = p;
                    }
                    if chroma > max_chroma {
                        max_chroma = chroma;
                        colorful = p;
                    }
                }
            }
            let average = sum.map(|v| (v / count.max(1) as f64) as f32);
            *mean.data.get_mut(y * w + x)? = average;
            for (dx, dy, value) in [(0, 0, average), (1, 0, dark), (0, 1, bright), (1, 1, colorful)] {
                *guard.data.get_mut((y * 2 + dy) * (w * 2) + x * 2 + dx)? = value;
            }
        }
    }
    Some((mean, guard))
}

/// The scene's log-average luminance under the current white balance, in EV relative to middle grey
/// (the mean of log2(Y / 0.18) over a 512 px proxy, scene-linear, before any tone curve). Pixels that
/// aren't finite (a damaged decode) are left out: they get no weight when the proxy is made, and
/// proxy pixels made only of them are skipped. `None` when no pixel is valid.
pub fn scene_log_mean(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<f64> {
    let (mut clean, mut weight) = (src.clone(), Rgb32f::new(src.width, src.height));
    let mut any = false;
    for (p, w) in clean.data.iter_mut().zip(weight.data.iter_mut()) {
        if p.iter().all(|v| v.is_finite()) {
            *w = [1.0; 3];
            any = true;
        } else {
            *p = [0.0; 3];
        }
    }
    if !any {
        return None;
    }
    let fit = |img: &Rgb32f| lightcraft_raster::resample::fit(img, 512, 512, lightcraft_raster::resample::Filter::Box);
    let (img, weight) = (fit(&clean), fit(&weight));
    // the mean of the valid pixels under each proxy pixel
    let kept: Vec<[f32; 3]> = img.data.iter().zip(&weight.data).filter(|(_, w)| w[0] > 1e-6).map(|(p, w)| p.map(|v| v / w[0])).collect();
    let mut kept = Rgb32f { width: kept.len(), height: 1, data: kept };
    let base = DevelopSettings { wb: s.wb, process: s.process, ..DevelopSettings::default() };
    // (white balance is per pixel: the valid pixels can be balanced in a row of their own)
    crate::local::scene_linear_pre(&mut kept, info, &base);
    let (mut sum, mut n) = (0f64, 0usize);
    for c in &kept.data {
        let ev = (luminance_2020(*c).max(1e-6) / 0.18).log2();
        if ev.is_finite() {
            sum += ev as f64;
            n += 1;
        }
    }
    (n > 0).then(|| sum / n as f64)
}

/// Image-dependent, conservative Auto tone under the current white balance and other edits.
/// A neutral-Auto render supplies luminance percentiles and chroma; it does not aim every scene
/// at one white/black point. Dark scenes with small lights and high-key scenes retain their key.
/// The proposed controls are validated through the actual renderer against the current edit,
/// per pixel AND per channel: existing clipped lights may remain, new clipping is rejected.
///
/// At most 21 candidates (endpoint protection, then fixed strengths toward neutral) are rendered on
/// a ≤256px spatial proxy. No acceptable candidate means preserving the current Auto controls.
/// Fixed candidates and a strict clipping-support subset make repeated Auto idempotent, including
/// when refinement is necessary. This is a proxy guarantee, not a full-resolution clipping proof:
/// fine spatial edits, small crop boundaries and detail filters can differ at export resolution.
/// Invalid source buffers/pixels or non-finite rendered samples conservatively keep current controls.
/// HDR is checked against its configured linear headroom, never against SDR white at 1.0.
/// White balance is never changed.
pub fn auto_tone(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> AutoTone {
    let current = AutoTone::current(s);
    let Some((measurement, validation)) = tone_proxies(src) else {
        return current;
    };
    let mut neutral = s.clone();
    AutoTone::default().apply_to(&mut neutral);
    let Some(stats) = ToneStats::of(&measurement, info, &neutral) else {
        return current;
    };
    let candidate = stats.proposal();
    let Some(baseline) = tone_render(&validation, info, s) else {
        return current;
    };
    for scale in [1.0, 0.5, 0.25, 0.125, 0.0625, 0.03125, 0.0] {
        for protection in [0.0, 1.0, 2.0] {
            let value = candidate.protected(protection).scaled(scale);
            let mut settings = s.clone();
            value.apply_to(&mut settings);
            let Some(rendered) = tone_render(&validation, info, &settings) else {
                continue;
            };
            if no_new_clipping(&baseline, &rendered, s.hdr.peak(), s.hdr.enabled) {
                return value;
            }
        }
    }
    current
}

struct ToneStats {
    low: f64,
    lower: f64,
    median: f64,
    upper: f64,
    high: f64,
    log_mean: f64,
    chroma: f64,
}

impl ToneStats {
    fn of(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<Self> {
        let rendered = tone_render(src, info, s)?;
        let deep = rendered.deep?;
        let crate::DeepSamples::F32(samples) = deep.samples else {
            return None;
        };
        let (mut values, mut log_sum, mut chroma_sum) = (Vec::with_capacity(samples.len() / 3), 0.0, 0.0);
        for p in samples.chunks_exact(3) {
            let [r, g, b] = <[f32; 3]>::try_from(p).ok()?;
            let y = (0.2126 * r + 0.7152 * g + 0.0722 * b).max(0.0) as f64;
            log_sum += y.max(1e-6).log2();
            let hi = r.max(g).max(b) as f64;
            let lo = r.min(g).min(b) as f64;
            chroma_sum += if hi > 0.005 { (hi - lo) / hi } else { 0.0 };
            values.push(y);
        }
        if values.is_empty() {
            return None;
        }
        values.sort_by(f64::total_cmp);
        let quantile = |q: f64| values.get(((values.len() - 1) as f64 * q).round() as usize).copied().unwrap_or(0.0);
        Some(Self {
            low: quantile(0.01),
            lower: quantile(0.10),
            median: quantile(0.50),
            upper: quantile(0.90),
            high: quantile(0.99),
            log_mean: log_sum / values.len() as f64,
            chroma: chroma_sum / values.len() as f64,
        })
    }

    fn proposal(&self) -> AutoTone {
        // Uniform fields provide no evidence for a tonal correction. Confidence grows with
        // absolute spread, rather than amplifying noise in an almost-flat black/white image.
        let confidence = ((self.upper - self.lower) / 0.08).clamp(0.0, 1.0);
        let span_ev = ((self.upper + 0.003) / (self.lower + 0.003)).log2();
        let night = self.median < 0.045 && self.high > self.median.max(0.002) * 12.0;
        let high_key = self.median > 0.45;
        let intent = if night || high_key { 0.20 } else { 1.0 };
        let exposure = ((0.18f64.log2() - self.log_mean) * 0.32).clamp(-0.65, 1.15) * intent;
        let highlight_need = ((self.high - self.upper) / (self.high + 0.03)).clamp(0.0, 1.0);
        let shadow_need = ((self.median - self.lower) / (self.median + 0.03)).clamp(0.0, 1.0);
        AutoTone {
            exposure,
            contrast: ((3.2 - span_ev) * 9.0).clamp(-12.0, 28.0) * intent,
            highlights: -highlight_need * 48.0 * if high_key { 0.4 } else { 1.0 },
            shadows: shadow_need * 28.0 * intent,
            whites: ((0.82 - self.high) * 18.0).clamp(-18.0, 10.0) * intent,
            blacks: ((0.012 - self.low) * 100.0).clamp(-10.0, 2.0) * intent,
            vibrance: ((0.35 - self.chroma) * 30.0).clamp(-6.0, 10.0),
            saturation: -((self.chroma - 0.55) * 12.0).clamp(0.0, 5.0),
        }
        .scaled(confidence)
    }
}

fn tone_render(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> Option<crate::Rendered> {
    let mut request = crate::RenderRequest::fit(256, 256);
    request.depth = if s.hdr.enabled { crate::OutputDepth::F32Hdr } else { crate::OutputDepth::F32Linear };
    let rendered = crate::render(src, info, s, &request);
    let crate::DeepSamples::F32(samples) = &rendered.deep.as_ref()?.samples else {
        return None;
    };
    if samples.is_empty() || samples.iter().any(|v| !v.is_finite()) {
        return None;
    }
    Some(rendered)
}

fn no_new_clipping(before: &crate::Rendered, after: &crate::Rendered, peak: f32, hdr: bool) -> bool {
    let (Some(crate::DeepImage { samples: crate::DeepSamples::F32(a), .. }), Some(crate::DeepImage { samples: crate::DeepSamples::F32(b), .. })) =
        (&before.deep, &after.deep)
    else {
        return false;
    };
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    // One part per million of configured headroom allows for floating point endpoint rounding.
    // The test remains per sample; a clipped red lamp cannot pay for a newly clipped blue sky.
    let top = peak * (1.0 - 1e-6);
    if a.iter().zip(b).any(|(&old, &new)| !old.is_finite() || !new.is_finite() || (new <= 1e-7 && old > 1e-7) || (new >= top && old < top)) {
        return false;
    }
    if hdr {
        return true;
    }
    before.image.data.len() == after.image.data.len()
        && before
            .image
            .data
            .iter()
            .zip(&after.image.data)
            .all(|(a, b)| a.iter().take(3).zip(b.iter().take(3)).all(|(&old, &new)| (new != 0 || old == 0) && (new != 255 || old == 255)))
}

/// An automatic black & white mix (slider values, red … magenta) for `src` under `s`'s white
/// balance and tone: each hue band's colourful pixels are pushed away from the image's mean
/// lightness — bands brighter than average get brighter, darker ones darker — so areas that
/// differ only in colour stay apart in grey. Bands with almost no colourful pixels stay at 0.
pub fn auto_bw_mix(src: &Rgb32f, info: &SourceInfo, s: &DevelopSettings) -> [f64; 8] {
    use lightcraft_color::perceptual::{lab_to_lch, oklab_from_2020};
    let mut img = lightcraft_raster::resample::fit(src, 512, 512, lightcraft_raster::resample::Filter::Box);
    let base = DevelopSettings { wb: s.wb, light: s.light, process: s.process, ..DevelopSettings::default() };
    crate::local::scene_linear_pre(&mut img, info, &base);
    let gain = 2f32.powf(base.light.exposure as f32);
    let (mut mass, mut sum_l) = ([0f64; 8], [0f64; 8]);
    let (mut all_l, mut n) = (0f64, 0usize);
    for p in &img.data {
        let lch = lab_to_lch(oklab_from_2020(p.map(|v| (v * gain).max(0.0))));
        all_l += lch[0] as f64;
        n += 1;
        let k = (lch[1] / 0.2).min(1.0) as f64; // as in the B&W conversion
        if k < 0.05 {
            continue;
        }
        let w = crate::colorops::band_weights(lch[2]);
        for i in 0..8 {
            mass[i] += w[i] as f64 * k;
            sum_l[i] += w[i] as f64 * k * lch[0] as f64;
        }
    }
    if n == 0 {
        return [0.0; 8];
    }
    let mean = all_l / n as f64;
    let total: f64 = mass.iter().sum();
    std::array::from_fn(|i| {
        if total <= 0.0 || mass[i] / total < 0.01 {
            return 0.0;
        }
        let sep = sum_l[i] / mass[i] - mean;
        (sep * 400.0).clamp(-60.0, 60.0).round()
    })
}

/// Grey-world white balance weighted towards mid-tone, low-chroma pixels. Returns (temp, tint).
pub fn auto_wb(src: &Rgb32f, info: &SourceInfo) -> (f64, f64) {
    let img = lightcraft_raster::resample::fit(src, 256, 256, lightcraft_raster::resample::Filter::Box);
    let (mut acc, mut wsum) = ([0.0f64; 3], 0.0f64);
    for c in &img.data {
        let y = luminance_2020(*c);
        if !(0.01..=2.0).contains(&y) {
            continue;
        }
        let mx = c[0].max(c[1]).max(c[2]);
        let mn = c[0].min(c[1]).min(c[2]);
        let chroma = (mx - mn) / (mx + 1e-6);
        let w = (1.0 - chroma).powi(2) as f64 * (1.0 - ((y.log2() + 2.5) / 4.0).abs().min(1.0)) as f64;
        for i in 0..3 {
            acc[i] += c[i] as f64 * w;
        }
        wsum += w;
    }
    if wsum <= 0.0 {
        return (info.as_shot_temp, info.as_shot_tint);
    }
    let avg = acc.map(|v| v / wsum);
    // the white that makes `avg` neutral, through the same model the white balance renders with
    // (`local::wb_matrix_for`): the camera's own colour model, else a Bradford adaptation
    let camera = info.camera_color.as_deref().filter(|_| info.raw && !info.relative_wb);
    let seen = match camera.and_then(|cc| lightcraft_raw::color::neutral_white(&cc.tags, cc.developed_for, avg)) {
        Some(white) => white,
        None => {
            let shot = lightcraft_color::cct::temp_tint_to_xy(info.as_shot_temp, info.as_shot_tint);
            Xy::from_xyz(bradford(REC2020.white, shot).apply(REC2020.to_xyz().apply(avg)))
        }
    };
    let (t, tint) = xy_to_temp_tint(seen);
    (t.clamp(2000.0, 50000.0).round(), tint.clamp(-150.0, 150.0).round())
}

/// Temperature/tint currently in effect (for UI display).
pub fn current_wb(info: &SourceInfo, s: &DevelopSettings) -> (f64, f64) {
    effective_wb(info, s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_image_keeps_as_shot_wb() {
        let img = Rgb32f::from_fn(32, 32, |x, y| [0.05 + (x + y) as f32 * 0.004; 3]);
        let (t, tint) = auto_wb(&img, &SourceInfo::default());
        assert!((t - 6500.0).abs() < 150.0, "{t}");
        assert!(tint.abs() < 6.0, "{tint}");
    }

    #[test]
    fn blue_cast_is_corrected_by_higher_temp() {
        // A bluish cast should be neutralised by telling the pipeline the light was bluer (higher K).
        let img = Rgb32f::from_fn(32, 32, |_, _| [0.16, 0.18, 0.24]);
        let (t, _) = auto_wb(&img, &SourceInfo::default());
        assert!(t > 7000.0, "{t}");
        let mut s = DevelopSettings::default();
        s.wb.mode = lightcraft_develop::WbMode::Custom;
        s.wb.temp = t;
        let (t2, tint2) = auto_wb(&img, &SourceInfo::default());
        let _ = (t2, tint2);
        let mut out = img.clone();
        let (tt, ti) = auto_wb(&img, &SourceInfo::default());
        s.wb.temp = tt;
        s.wb.tint = ti;
        crate::local::scene_linear_pre(&mut out, &SourceInfo::default(), &s);
        let c = out.get(0, 0);
        assert!((c[0] - c[2]).abs() < 0.02, "{c:?}");
    }
}

#[cfg(test)]
mod bw_tests {
    use super::*;

    #[test]
    fn auto_bw_mix_pushes_light_and_dark_hues_apart() {
        // left: a light yellow, right: a dark blue (scene-linear Rec. 2020)
        let src = Rgb32f::from_fn(64, 32, |x, _| if x < 32 { [0.55, 0.5, 0.05] } else { [0.01, 0.02, 0.12] });
        let m = auto_bw_mix(&src, &SourceInfo::default(), &DevelopSettings::default());
        let (yellow, blue) = (m[2], m[5]);
        assert!(yellow > 0.0 && blue < 0.0, "{m:?}");
        // an image without colour leaves the mix alone
        let grey = Rgb32f::from_fn(16, 16, |x, _| [x as f32 / 16.0; 3]);
        assert_eq!(auto_bw_mix(&grey, &SourceInfo::default(), &DevelopSettings::default()), [0.0; 8]);
    }
}

#[cfg(test)]
mod tint_tests {
    use super::*;

    #[test]
    fn auto_wb_and_picker_correct_green_with_positive_tint() {
        // The picker delegates a sampled patch to this same auto_wb implementation.
        for (rgb, sign) in [([0.18, 0.24, 0.18], 1.0), ([0.24, 0.18, 0.24], -1.0)] {
            let img = Rgb32f::filled(16, 16, rgb);
            let info = SourceInfo { raw: true, relative_wb: true, ..Default::default() };
            let (temp, tint) = auto_wb(&img, &info);
            assert!(tint * sign > 0.0, "{rgb:?}: temp {temp}, tint {tint}");
            let mut s = DevelopSettings::default();
            s.wb.mode = lightcraft_develop::WbMode::Custom;
            s.wb.temp = temp;
            s.wb.tint = tint;
            let mut corrected = img;
            crate::local::white_balance(&mut corrected, &info, &s);
            let p = corrected.get(0, 0);
            let spread = p[0].max(p[1]).max(p[2]) - p[0].min(p[1]).min(p[2]);
            assert!(spread < 0.003, "{rgb:?} -> {p:?}");
        }
    }
}

#[cfg(test)]
mod adaptive_tests {
    use super::*;

    fn scenes() -> Vec<(&'static str, Rgb32f)> {
        vec![
            ("low contrast", Rgb32f::from_fn(64, 48, |x, y| [0.12 + (x + y) as f32 * 0.001; 3])),
            ("dark interior", Rgb32f::from_fn(64, 48, |x, _| [0.003 + (x as f32 / 63.0).powi(2) * 0.22; 3])),
            ("high key", Rgb32f::from_fn(64, 48, |x, _| [0.55 + x as f32 / 63.0 * 0.38; 3])),
            (
                "night lights",
                Rgb32f::from_fn(64, 48, |x, y| if (x == 31 || x == 32) && y < 10 { [4.0, 0.7, 0.05] } else { [0.002 + x as f32 * 0.0004; 3] }),
            ),
            (
                "saturated",
                Rgb32f::from_fn(64, 48, |x, y| {
                    let v = 0.01 + y as f32 / 47.0 * 1.6;
                    match x / 16 {
                        0 => [v, 0.004, 0.002],
                        1 => [0.002, v, 0.004],
                        2 => [0.004, 0.002, v],
                        _ => [v, v * 0.5, 0.004],
                    }
                }),
            ),
            ("already clipped", Rgb32f::from_fn(64, 48, |x, _| [if x > 54 { 20.0 } else { x as f32 / 63.0 * 0.5 }; 3])),
            ("full range", Rgb32f::from_fn(64, 48, |x, _| [(x as f32 / 63.0).powi(2); 3])),
        ]
    }

    #[test]
    fn varied_scenes_are_adaptive_safe_and_idempotent_in_sdr_and_hdr() {
        let mut distinct = Vec::new();
        for hdr in [false, true] {
            for raw in [false, true] {
                let info = SourceInfo { raw, ..Default::default() };
                for (name, image) in scenes() {
                    let mut settings = DevelopSettings::default();
                    settings.hdr.enabled = hdr;
                    let before = tone_render(&image, &info, &settings).unwrap();
                    let preview_before = crate::render(&image, &info, &settings, &crate::RenderRequest::fit(256, 256));
                    let auto = auto_tone(&image, &info, &settings);
                    auto.apply_to(&mut settings);
                    let after = tone_render(&image, &info, &settings).unwrap();
                    assert!(no_new_clipping(&before, &after, settings.hdr.peak(), hdr), "{name}, raw={raw}, hdr={hdr}: {auto:?}");
                    if !hdr {
                        let preview_after = crate::render(&image, &info, &settings, &crate::RenderRequest::fit(256, 256));
                        assert!(
                            preview_before.image.data.iter().zip(&preview_after.image.data).all(|(a, b)| a
                                .iter()
                                .take(3)
                                .zip(b.iter().take(3))
                                .all(|(&a, &b)| (b != 0 || a == 0) && (b != 255 || a == 255))),
                            "direct preview {name}: {auto:?}"
                        );
                    }
                    assert_eq!(auto, auto_tone(&image, &info, &settings), "repeated {name}, raw={raw}, hdr={hdr}");
                    if !raw && !hdr {
                        distinct.push(auto);
                    }
                }
            }
        }
        assert!(distinct.windows(2).filter(|w| w[0] != w[1]).count() >= 5, "{distinct:?}");
        assert!(distinct.iter().filter(|a| **a != AutoTone::default()).count() >= 5, "{distinct:?}");
    }

    fn mean_luma(image: &crate::Rendered) -> f64 {
        let crate::DeepSamples::F32(samples) = &image.deep.as_ref().unwrap().samples else {
            panic!("float render");
        };
        samples.chunks_exact(3).map(|p| (p[0] * 0.2126 + p[1] * 0.7152 + p[2] * 0.0722) as f64).sum::<f64>() / (samples.len() / 3) as f64
    }

    #[test]
    fn low_contrast_gains_separation_and_extreme_scenes_keep_their_key() {
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        let low = &scenes()[0].1;
        let before = tone_render(low, &info, &s).unwrap();
        let auto = auto_tone(low, &info, &s);
        auto.apply_to(&mut s);
        let after = tone_render(low, &info, &s).unwrap();
        let range = |r: &crate::Rendered| {
            let lo = r.image.data.iter().map(|p| p[0]).min().unwrap();
            let hi = r.image.data.iter().map(|p| p[0]).max().unwrap();
            hi as i32 - lo as i32
        };
        assert!(range(&after) > range(&before), "{auto:?}: {} -> {}", range(&before), range(&after));
        for idx in [2, 3] {
            let image = &scenes()[idx].1;
            let mut s = DevelopSettings::default();
            let before = tone_render(image, &info, &s).unwrap();
            auto_tone(image, &info, &s).apply_to(&mut s);
            let after = tone_render(image, &info, &s).unwrap();
            if idx == 2 {
                assert!(mean_luma(&after) > 0.5);
            } else {
                assert!(mean_luma(&after) < 0.06);
            }
            assert!((mean_luma(&before) - mean_luma(&after)).abs() < 0.10);
        }
    }

    #[test]
    fn uniform_and_invalid_sources_do_not_invent_a_recipe() {
        let s = DevelopSettings::default();
        for v in [0.0, 1e-7, 0.18, 0.8, 8.0] {
            assert_eq!(auto_tone(&Rgb32f::filled(32, 32, [v; 3]), &SourceInfo::default(), &s), AutoTone::default());
        }
        let mut s = s;
        s.light.exposure = -0.75;
        s.light.shadows = 12.0;
        for image in [
            Rgb32f::new(0, 0),
            Rgb32f { width: usize::MAX, height: 2, data: vec![] },
            Rgb32f { width: 4, height: 4, data: vec![[0.18; 3]] },
            Rgb32f::filled(8, 8, [f32::NAN; 3]),
            Rgb32f::filled(8, 8, [f32::INFINITY; 3]),
            Rgb32f::from_fn(8, 8, |x, y| if x == y { [f32::NEG_INFINITY; 3] } else { [0.18; 3] }),
        ] {
            assert_eq!(auto_tone(&image, &SourceInfo::default(), &s), AutoTone::current(&s));
        }
        s.light.exposure = f64::NAN;
        assert!(auto_tone(&Rgb32f::new(0, 0), &SourceInfo::default(), &s).exposure.is_finite());
    }

    #[test]
    fn current_edits_are_preserved_and_custom_start_is_stable() {
        let image = &scenes()[4].1;
        let info = SourceInfo::default();
        let mut settings = DevelopSettings::default();
        settings.light.exposure = -1.8;
        settings.light.highlights = -80.0;
        settings.wb.mode = lightcraft_develop::WbMode::Custom;
        settings.wb.temp = 4800.0;
        settings.curve.red = vec![lightcraft_geom::Point::new(0.0, 0.04), lightcraft_geom::Point::new(1.0, 0.92)];
        settings.vignette.amount = -24.0;
        let original = settings.clone();
        let before = tone_render(image, &info, &settings).unwrap();
        let auto = auto_tone(image, &info, &settings);
        auto.apply_to(&mut settings);
        let after = tone_render(image, &info, &settings).unwrap();
        assert!(no_new_clipping(&before, &after, 1.0, false));
        assert_eq!(auto, auto_tone(image, &info, &settings));
        AutoTone::current(&original).apply_to(&mut settings);
        assert_eq!(settings, original);
    }

    #[test]
    fn hdr_superwhite_is_valid_and_new_channel_clips_cannot_trade_places() {
        let image = Rgb32f::from_fn(32, 16, |x, _| [0.1 + x as f32 / 31.0 * 4.0; 3]);
        let mut s = DevelopSettings::default();
        s.hdr.enabled = true;
        let hdr = tone_render(&image, &SourceInfo::default(), &s).unwrap();
        let crate::DeepSamples::F32(samples) = &hdr.deep.as_ref().unwrap().samples else {
            panic!("float");
        };
        assert!(samples.iter().any(|&v| v > 1.0 && v < s.hdr.peak()));
        assert!(no_new_clipping(&hdr, &hdr, s.hdr.peak(), true));
        let mut changed = tone_render(&image, &SourceInfo::default(), &s).unwrap();
        let crate::DeepSamples::F32(v) = &mut changed.deep.as_mut().unwrap().samples else {
            panic!("float");
        };
        v[0] = s.hdr.peak();
        assert!(!no_new_clipping(&hdr, &changed, s.hdr.peak(), true));
        let crate::DeepSamples::F32(v) = &mut changed.deep.as_mut().unwrap().samples else {
            panic!("float");
        };
        v[0] = 0.0;
        assert!(!no_new_clipping(&hdr, &changed, s.hdr.peak(), true));
    }

    #[test]
    fn previous_fixed_recipe_adds_clipping_that_adaptive_auto_avoids() {
        let info = SourceInfo::default();
        let settings = DevelopSettings::default();
        let mut saw_old_clipping = false;
        for (name, image) in scenes() {
            let before = tone_render(&image, &info, &settings).unwrap();
            let log_mean = scene_log_mean(&image, &info, &settings).unwrap();
            let old = AutoTone {
                exposure: ((-0.58 - 0.41 * log_mean).clamp(-2.0, 3.0) * 100.0).round() / 100.0,
                contrast: 68.0,
                highlights: -66.0,
                shadows: 52.0,
                vibrance: 30.0,
                ..Default::default()
            };
            let mut old_settings = settings.clone();
            old.apply_to(&mut old_settings);
            let old_rendered = tone_render(&image, &info, &old_settings).unwrap();
            if !no_new_clipping(&before, &old_rendered, 1.0, false) {
                saw_old_clipping = true;
                let mut adaptive = settings.clone();
                auto_tone(&image, &info, &settings).apply_to(&mut adaptive);
                assert!(no_new_clipping(&before, &tone_render(&image, &info, &adaptive).unwrap(), 1.0, false), "{name}");
            }
        }
        assert!(saw_old_clipping, "the regression scene must actually expose the old recipe's clipping");
    }

    #[test]
    fn bounded_proxy_keeps_single_pixel_lights_and_dark_details() {
        let src = Rgb32f::from_fn(1024, 768, |x, y| {
            if x == 401 && y == 299 {
                [12.0, 0.2, 0.04]
            } else if x == 403 && y == 300 {
                [0.0; 3]
            } else {
                [0.1 + x as f32 * 0.0001; 3]
            }
        });
        let (mean, guard) = tone_proxies(&src).unwrap();
        assert!(mean.data.len() <= 128 * 128 && guard.data.len() <= 256 * 256);
        assert!(guard.data.contains(&[12.0, 0.2, 0.04]));
        assert!(guard.data.contains(&[0.0; 3]));
        let info = SourceInfo::default();
        let mut s = DevelopSettings::default();
        let before = tone_render(&guard, &info, &s).unwrap();
        let auto = auto_tone(&src, &info, &s);
        auto.apply_to(&mut s);
        let after = tone_render(&guard, &info, &s).unwrap();
        assert!(no_new_clipping(&before, &after, 1.0, false));
        assert_eq!(auto, auto_tone(&src, &info, &s));
    }
}
