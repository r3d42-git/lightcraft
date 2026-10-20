//! Provider-neutral identity resolution. No file access or image processing occurs here.
//! Ambiguous identities and known incompatible mounts never produce a selected profile.
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use serde::{Deserialize, Serialize};

/// Provider-adapted camera identity; fixed-lens knowledge belongs to the provider.
#[derive(Clone, Debug)]
pub struct CameraDescriptor {
    pub maker: String,
    pub model: String,
    pub mount: String,
    /// Lens mounts directly accepted by this camera mount, explicitly declared by the provider.
    /// Never inferred in reverse or transitively; ignored for fixed-lens camera-only resolution.
    pub compatible_mounts: Vec<String>,
    pub fixed_lens: bool,
    /// Sensor crop relative to 35 mm; missing or invalid values cannot establish calibration coverage.
    pub crop_factor: Option<f64>,
}

#[derive(Clone, Debug)]
pub struct ProfileDescriptor {
    pub maker: String,
    pub names: Vec<String>,
    pub mounts: Vec<String>,
    /// Crop factor of the frame on which this profile was calibrated.
    pub calibration_crop_factor: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Request<'a> {
    pub camera_make: Option<&'a str>,
    pub camera_model: Option<&'a str>,
    pub lens_make: Option<&'a str>,
    pub lens_model: Option<&'a str>,
    /// Optional recorded-frame crop, including in-camera crop modes.
    pub frame_crop_factor: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Selected,
    MissingIdentity,
    NoMatch,
    AmbiguousCamera,
    AmbiguousProfile,
}

/// Indices reference the supplied descriptor slices, never provider-global identifiers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resolution {
    pub status: Status,
    pub camera_index: Option<usize>,
    pub profile_index: Option<usize>,
    pub candidates: Vec<usize>,
    pub via_fixed_lens: bool,
    /// The validated crop used for both calibration coverage and correction conversion.
    pub frame_crop_factor: Option<f64>,
}

/// Resolve only a unique best candidate. Camera-only resolution requires a provider-declared fixed lens.
pub fn resolve(request: Request<'_>, cameras: &[CameraDescriptor], profiles: &[ProfileDescriptor]) -> Resolution {
    let mut result = Resolution {
        status: Status::NoMatch,
        camera_index: None,
        profile_index: None,
        candidates: Vec::new(),
        via_fixed_lens: false,
        frame_crop_factor: None,
    };
    let camera_model = camera_name(request.camera_model.unwrap_or_default(), request.camera_make.unwrap_or_default());
    let matching_cameras: Vec<_> = cameras
        .iter()
        .enumerate()
        .filter(|(_, camera)| {
            !camera_model.is_empty()
                && maker_agrees(&camera.maker, request.camera_make.unwrap_or_default())
                && camera_name(&camera.model, &camera.maker) == camera_model
        })
        .collect();
    if matching_cameras.len() > 1 {
        result.status = Status::AmbiguousCamera;
        return result;
    }
    let camera = matching_cameras.first().map(|(index, camera)| {
        result.camera_index = Some(*index);
        *camera
    });
    let lens_name = request.lens_model.unwrap_or_default();
    let fixed = camera.is_some_and(|camera| camera.fixed_lens && !camera.mount.is_empty());
    result.via_fixed_lens = fixed;
    if !fixed && (tokens(lens_name).is_empty() || tokens(lens_name).iter().any(|word| matches!(word.as_str(), "unknown" | "undefined"))) {
        result.status = Status::MissingIdentity;
        return result;
    }
    let mut best = 0usize;
    for (index, profile) in profiles.iter().enumerate() {
        if camera.is_some_and(|camera| {
            !camera.mount.is_empty()
                && !profile.mounts.iter().any(|mount| {
                    mount.eq_ignore_ascii_case(&camera.mount)
                        || (!fixed
                            && camera.compatible_mounts.iter().any(|compatible| !compatible.is_empty() && mount.eq_ignore_ascii_case(compatible)))
                })
        }) {
            continue;
        }
        if request.lens_make.is_some_and(|maker| !maker.trim().is_empty() && !maker_agrees(&profile.maker, maker)) {
            continue;
        }
        let score = if fixed { 1000 } else { profile.names.iter().map(|name| name_score(name, lens_name, &profile.maker)).max().unwrap_or(0) };
        if score == 0 || score < best {
            continue;
        }
        if score > best {
            result.candidates.clear();
            best = score;
        }
        result.candidates.push(index);
    }
    // Resolve optical identity before considering calibration coverage. A better crop match
    // cannot prove that two different makers or lens models describe the same optics.
    if let Some(first) = result.candidates.first().and_then(|index| profiles.get(*index))
        && result.candidates.iter().filter_map(|index| profiles.get(*index)).any(|profile| !same_identity(first, profile))
    {
        result.status = Status::AmbiguousProfile;
        return result;
    }
    let frame_crop = effective_frame_crop_factor(camera.and_then(|camera| camera.crop_factor), request.frame_crop_factor);
    result.frame_crop_factor = frame_crop;
    let Some(frame_crop) = frame_crop else {
        result.status = if result.candidates.len() > 1 { Status::AmbiguousProfile } else { Status::NoMatch };
        return result;
    };
    // A larger calibration crop sees a smaller field. Never extrapolate it to a
    // wider recorded frame; permit only 1% database/tag rounding at the boundary.
    result.candidates.retain(|index| {
        profiles.get(*index).and_then(|profile| valid_crop(profile.calibration_crop_factor)).is_some_and(|crop| crop <= frame_crop * 1.01)
    });
    let best_crop = result
        .candidates
        .iter()
        .filter_map(|index| profiles.get(*index).and_then(|profile| valid_crop(profile.calibration_crop_factor)))
        .max_by(f64::total_cmp);
    if let Some(best_crop) = best_crop {
        result.candidates.retain(|index| {
            profiles.get(*index).and_then(|profile| valid_crop(profile.calibration_crop_factor)).is_some_and(|crop| (crop - best_crop).abs() <= 1e-9)
        });
    }
    result.status = match result.candidates.as_slice() {
        [index] => {
            result.profile_index = Some(*index);
            Status::Selected
        }
        [] => Status::NoMatch,
        _ => Status::AmbiguousProfile,
    };
    result
}

/// A recorded crop narrows the body's field only when it exceeds the body crop by more than 10%.
/// Smaller differences are treated as whole-millimetre EXIF rounding, retaining the body's crop.
/// Both inputs use the same finite 0.1..=20 validity range as calibration coverage.
pub fn effective_frame_crop_factor(body: Option<f64>, recorded: Option<f64>) -> Option<f64> {
    match (valid_crop(body), valid_crop(recorded)) {
        (Some(body), Some(recorded)) if recorded > body * 1.1 => Some(recorded),
        (body, recorded) => body.or(recorded),
    }
}

fn valid_crop(crop: Option<f64>) -> Option<f64> {
    crop.filter(|crop| crop.is_finite() && (0.1..=20.0).contains(crop))
}

fn same_identity(a: &ProfileDescriptor, b: &ProfileDescriptor) -> bool {
    let names = |profile: &ProfileDescriptor| {
        let maker = maker_name(&profile.maker);
        let mut names = profile
            .names
            .iter()
            .map(|name| {
                let mut words = tokens(name).into_iter().filter(|word| !maker.contains(word)).collect::<Vec<_>>();
                words.sort();
                words
            })
            .collect::<Vec<_>>();
        names.sort();
        names.dedup();
        names
    };
    maker_agrees(&a.maker, &b.maker) && names(a) == names(b)
}

fn maker_name(name: &str) -> Vec<String> {
    tokens(name)
        .into_iter()
        .filter(|token| !matches!(token.as_str(), "corporation" | "corp" | "inc" | "co" | "ltd" | "limited" | "company" | "ag"))
        .collect()
}

fn maker_agrees(a: &str, b: &str) -> bool {
    let (a, b) = (maker_name(a), maker_name(b));
    !a.is_empty() && !b.is_empty() && a == b
}

fn camera_name(model: &str, maker: &str) -> String {
    let mut words = tokens(model);
    let make = maker_name(maker);
    if !make.is_empty() && words.starts_with(&make) {
        words.drain(..make.len());
    }
    words.concat()
}

fn tokens(name: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut previous = None;
    for c in name.chars().flat_map(char::to_lowercase) {
        let numeric = c.is_ascii_digit() || (c == '.' && previous == Some(true));
        if c.is_alphanumeric() || numeric {
            if previous.is_some_and(|value| value != numeric) && !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            current.push(c);
            previous = Some(numeric);
        } else {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            previous = None;
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    for word in &mut words {
        if word.contains('.') && word.starts_with(|c: char| c.is_ascii_digit()) {
            *word = word.trim_end_matches('0').trim_end_matches('.').to_string();
        }
    }
    words.retain(|word| !word.is_empty());
    words
}

fn name_score(profile: &str, shot: &str, maker: &str) -> usize {
    let make = maker_name(maker);
    let clean = |name| tokens(name).into_iter().filter(|word| !make.contains(word)).collect::<Vec<_>>();
    let (profile, shot) = (clean(profile), clean(shot));
    if profile.is_empty() || shot.is_empty() {
        return 0;
    }
    let numbers = |words: &[String]| {
        let mut numbers = words.iter().filter(|word| word.starts_with(|c: char| c.is_ascii_digit())).cloned().collect::<Vec<_>>();
        numbers.sort();
        numbers
    };
    if numbers(&profile) != numbers(&shot) {
        return 0;
    }
    // Do not fuzzy-match revision/series suffixes: "II" and "IS" can change the optics.
    // Word order, punctuation, maker prefixes and numeric spelling are normalized; aliases
    // beyond that must be supplied as provider names rather than guessed.
    if profile.iter().all(|word| shot.contains(word)) && shot.iter().all(|word| profile.contains(word)) { 1000 } else { 0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn camera(fixed: bool) -> CameraDescriptor {
        CameraDescriptor {
            maker: "Example Corporation".into(),
            model: "EX-1".into(),
            mount: "Example mount".into(),
            compatible_mounts: Vec::new(),
            fixed_lens: fixed,
            crop_factor: Some(1.5),
        }
    }
    fn profile() -> ProfileDescriptor {
        ProfileDescriptor {
            maker: "Example".into(),
            names: vec!["Example 18-55mm f/3.5-5.6".into()],
            mounts: vec!["Example mount".into()],
            calibration_crop_factor: Some(1.5),
        }
    }
    fn request() -> Request<'static> {
        Request { camera_make: Some("EXAMPLE"), camera_model: Some("Example EX1"), lens_model: Some("18-55mm F3.5-5.6"), ..Default::default() }
    }
    #[test]
    fn unique_normalized_identity() {
        let result = resolve(request(), &[camera(false)], &[profile()]);
        assert_eq!(result.profile_index, Some(0));
        assert_eq!(result.camera_index, Some(0));
    }
    #[test]
    fn missing_and_unknown() {
        assert_eq!(resolve(Request::default(), &[], &[profile()]).status, Status::MissingIdentity);
        assert_eq!(resolve(Request { lens_model: Some("70-200mm f4"), ..request() }, &[], &[profile()]).status, Status::NoMatch);
    }
    #[test]
    fn ties_never_choose_first() {
        let result = resolve(request(), &[camera(false)], &[profile(), profile()]);
        assert_eq!(result.status, Status::AmbiguousProfile);
        assert_eq!(result.profile_index, None);
    }
    #[test]
    fn fixed_lens_requires_provider_knowledge() {
        let request = Request { lens_model: None, ..request() };
        assert_eq!(resolve(request, &[camera(true)], &[profile()]).profile_index, Some(0));
        assert_eq!(resolve(request, &[camera(false)], &[profile()]).status, Status::MissingIdentity);
    }
    #[test]
    fn incompatible_mount_is_rejected() {
        let mut other = profile();
        other.mounts = vec!["Other mount".into()];
        assert_eq!(resolve(request(), &[camera(false)], &[other]).status, Status::NoMatch);
    }
    #[test]
    fn provider_mount_compatibility_is_directional_and_explicit() {
        let mut body = camera(false);
        body.mount = "Canon EF-S".into();
        body.compatible_mounts = vec!["Canon EF".into()];
        let mut lens = profile();
        lens.mounts = vec!["Canon EF".into()];
        assert_eq!(resolve(request(), &[body.clone()], &[lens.clone()]).status, Status::Selected);
        lens.mounts = vec!["Other mount".into()];
        assert_eq!(resolve(request(), &[body.clone()], &[lens.clone()]).status, Status::NoMatch);
        body.mount = "Canon EF".into();
        body.compatible_mounts.clear();
        lens.mounts = vec!["Canon EF-S".into()];
        assert_eq!(resolve(request(), &[body], &[lens]).status, Status::NoMatch);
    }

    #[test]
    fn fixed_lens_requires_its_exact_mount_even_with_declared_compatibility() {
        let mut body = camera(true);
        body.compatible_mounts = vec!["Other mount".into()];
        let mut other = profile();
        other.mounts = vec!["Other mount".into()];
        let request = Request { lens_model: None, ..request() };
        assert_eq!(resolve(request, &[body.clone()], &[other]).status, Status::NoMatch);
        assert_eq!(resolve(request, &[body], &[profile()]).status, Status::Selected);
    }

    #[test]
    fn ambiguous_generic_name_is_rejected() {
        let mut other = profile();
        other.maker = "Other".into();
        other.names = vec!["18-55mm f/3.5-5.6".into()];
        assert_eq!(resolve(request(), &[camera(false)], &[profile(), other]).status, Status::AmbiguousProfile);
    }
    #[test]
    fn ambiguous_camera_is_rejected() {
        assert_eq!(resolve(request(), &[camera(false), camera(false)], &[profile()]).status, Status::AmbiguousCamera);
    }
    #[test]
    fn revision_suffix_is_not_discarded() {
        assert_eq!(resolve(Request { lens_model: Some("18-55mm f/3.5-5.6 II"), ..request() }, &[], &[profile()]).status, Status::NoMatch);
    }
    #[test]
    fn placeholder_lens_identity_is_not_a_match() {
        assert_eq!(
            resolve(Request { lens_model: Some("Unknown 18-55mm f/3.5-5.6"), ..request() }, &[], &[profile()]).status,
            Status::MissingIdentity
        );
    }
    #[test]
    fn calibration_uses_closest_covering_frame() {
        let mut full = profile();
        full.calibration_crop_factor = Some(1.0);
        let mut aps = profile();
        aps.calibration_crop_factor = Some(1.611);
        let profiles = [full, aps];
        let mut body = camera(false);
        body.crop_factor = Some(1.0);
        assert_eq!(resolve(request(), &[body.clone()], &profiles).profile_index, Some(0));
        assert_eq!(resolve(Request { frame_crop_factor: Some(1.6), ..request() }, &[body.clone()], &profiles).profile_index, Some(1));
        assert_eq!(resolve(request(), &[body], &profiles[1..]).status, Status::NoMatch);
    }

    #[test]
    fn effective_crop_retains_body_rounding_and_respects_crop_mode() {
        let body = camera(false);
        let mut too_narrow = profile();
        too_narrow.calibration_crop_factor = Some(1.611);
        let result = resolve(Request { frame_crop_factor: Some(1.6), ..request() }, &[body], &[too_narrow]);
        assert_eq!(result.status, Status::NoMatch, "1.6 EXIF rounding cannot qualify a 1.611 calibration for a 1.5 body");
        assert_eq!(result.frame_crop_factor, Some(1.5));
        let boundary = 1.5 * 1.1;
        for recorded in [1.0, 1.5, boundary - 1e-9, boundary] {
            assert_eq!(effective_frame_crop_factor(Some(1.5), Some(recorded)), Some(1.5));
        }
        assert_eq!(effective_frame_crop_factor(Some(1.5), Some(boundary + 1e-9)), Some(boundary + 1e-9));
        assert_eq!(effective_frame_crop_factor(Some(1.0), Some(1.6)), Some(1.6));
        assert_eq!(effective_frame_crop_factor(None, Some(1.6)), Some(1.6));
        for invalid in [f64::NAN, f64::INFINITY, -1.0, 0.0, 0.099, 20.001] {
            assert_eq!(effective_frame_crop_factor(Some(invalid), Some(invalid)), None);
            assert_eq!(effective_frame_crop_factor(Some(1.5), Some(invalid)), Some(1.5));
        }
        for boundary in [0.1, 20.0] {
            assert_eq!(effective_frame_crop_factor(None, Some(boundary)), Some(boundary));
        }
    }

    #[test]
    fn calibration_needs_known_valid_crop_and_keeps_equal_ties() {
        let mut body = camera(false);
        body.crop_factor = None;
        assert_eq!(resolve(request(), &[body.clone()], &[profile(), profile()]).status, Status::AmbiguousProfile);
        assert_eq!(resolve(request(), &[body.clone()], &[profile()]).profile_index, None);
        for invalid in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            assert_eq!(resolve(Request { frame_crop_factor: Some(invalid), ..request() }, &[body.clone()], &[profile()]).profile_index, None);
        }
        assert_eq!(resolve(request(), &[camera(false)], &[profile(), profile()]).status, Status::AmbiguousProfile);
    }

    #[test]
    fn crop_does_not_resolve_different_makers() {
        let mut other = profile();
        other.maker = "Other".into();
        other.names = vec!["18-55mm f/3.5-5.6".into()];
        other.calibration_crop_factor = Some(1.0);
        assert_eq!(resolve(request(), &[camera(false)], &[profile(), other]).status, Status::AmbiguousProfile);
    }

    #[test]
    fn invalid_or_missing_profile_crop_is_rejected() {
        let mut lens = profile();
        for crop in [None, Some(f64::NAN), Some(-1.0)] {
            lens.calibration_crop_factor = crop;
            assert_eq!(resolve(request(), &[camera(false)], &[lens.clone()]).status, Status::NoMatch);
        }
    }

    #[test]
    fn mismatched_maker_and_extra_numbers_are_rejected() {
        assert_eq!(resolve(Request { lens_make: Some("Other"), ..request() }, &[], &[profile()]).status, Status::NoMatch);
        assert_eq!(resolve(Request { lens_model: Some("18-55mm f/3.5-5.6 II 2"), ..request() }, &[], &[profile()]).status, Status::NoMatch);
    }
}
