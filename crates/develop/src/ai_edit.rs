//! Persisted raster retouches: rendering requires neither a model nor regeneration.
//!
//! Pixels are straight-alpha, scene-linear Rec.2020, in the same as-shot development space as
//! the EXIF-oriented source buffer, BEFORE user orientation, optics, local tone and white balance.
//! RGB is not premultiplied; finite values are bounded to the half-float HDR range (±65504). Rectangles refer to that whole source, not the cropped preview.
use serde::{Deserialize, Serialize};

pub const MAX_EDITS: usize = 8;
pub const MAX_SIDE: usize = 1024;
pub const MAX_TOTAL_PIXELS: usize = 2 * 1024 * 1024;
const MAX_ENCODED_BYTES: usize = 24 * 1024 * 1024;
const MAX_TOTAL_ENCODED_BYTES: usize = 48 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AiEditError(pub &'static str);
impl std::fmt::Display for AiEditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for AiEditError {}

/// Exact brush support, independent of inference resolution. With geometry, points live
/// in the frozen transformed frame; without it they retain legacy source-space semantics.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiBrushSupport {
    /// Resolved geometry at selection time. Missing preserves legacy source-space support.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub geometry: Option<AiSupportGeometry>,
    /// New brush paths connect points; missing/false preserves saved circle-only masks.
    #[serde(default, skip_serializing_if = "is_false")]
    pub continuous: bool,
    pub points: Vec<[f64; 2]>,
    /// Radius relative to the full source's long edge.
    pub radius: f64,
    /// Fraction of the radius blended inward; zero preserves a hard boundary.
    #[serde(default)]
    pub feather: f64,
}

/// Frozen transformed-brush coordinate system. Only the green-channel positional warp is
/// stored: gain and lateral CA affect rendering, never the authorization mask.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiSupportGeometry {
    pub orientation: lightcraft_geom::Orientation,
    pub source_size: [u32; 2],
    pub perspective: lightcraft_geom::Homography,
    pub distortion: f64,
    pub lens: Option<crate::EmbeddedLens>,
    pub lens_distortion: f64,
}

impl AiSupportGeometry {
    pub fn valid(&self) -> bool {
        self.source_size.iter().all(|n| *n > 0 && *n <= 100_000)
            && self.perspective.0.iter().all(|v| v.is_finite())
            && self.perspective.inverse().is_some()
            && self.distortion.is_finite()
            && self.distortion.abs() <= 1.0
            && self.lens_distortion.is_finite()
            && (0.0..=2.0).contains(&self.lens_distortion)
            && self.lens.is_none_or(|l| {
                let valid =
                    |center: lightcraft_geom::Point, radius: f64| center.x.is_finite() && center.y.is_finite() && radius.is_finite() && radius > 0.0;
                l.warp.is_none_or(|w| valid(w.center, w.radius) && w.planes.iter().flatten().all(|v| v.is_finite()))
                    && l.stages.iter().flatten().all(|w| {
                        valid(w.center, w.radius)
                            && w.planes.iter().flatten().all(|v| v.is_finite())
                            && w.planes.iter().all(|p| p[17] >= 0.0 && p[18] >= p[17])
                    })
            })
    }
}

fn is_false(value: &bool) -> bool {
    !value
}

/// Distance to the authorized brush path, in any common pixel coordinate system.
/// Legacy support is a union of circles; new continuous support is a union of capsules.
pub fn brush_distance(points: &[[f64; 2]], point: [f64; 2], continuous: bool) -> f64 {
    if !continuous {
        return points.iter().map(|p| (point[0] - p[0]).hypot(point[1] - p[1])).fold(f64::INFINITY, f64::min);
    }
    points
        .iter()
        .zip(points.iter().skip(1).chain(points.last()))
        .map(|(a, b)| {
            let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
            let length = dx * dx + dy * dy;
            let t = if length > 0.0 { (((point[0] - a[0]) * dx + (point[1] - a[1]) * dy) / length).clamp(0.0, 1.0) } else { 0.0 };
            (point[0] - a[0] - t * dx).hypot(point[1] - a[1] - t * dy)
        })
        .fold(f64::INFINITY, f64::min)
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AiEdit {
    pub id: u32,
    pub enabled: bool,
    /// Normalized source bounds [x0, y0, x1, y1], entirely in 0..1.
    pub rect: [f64; 4],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support: Option<AiBrushSupport>,
    pub width: u32,
    pub height: u32,
    /// Deflated little-endian RGBA f32, base64 encoded. Exact f32 values survive save/reopen.
    #[serde(deserialize_with = "deserialize_payload")]
    pub pixels: String,
    pub model: String,
    pub model_version: String,
    pub seed: u64,
}

impl AiEdit {
    /// Validate and encode a supplied raster. Metadata must identify the actual generator;
    /// this constructor does not download, start or select a model.
    pub fn from_pixels(mut metadata: Self, pixels: &[[f32; 4]]) -> Result<Self, AiEditError> {
        metadata.pixels.clear();
        if metadata.pixel_count()? != pixels.len() || !valid_pixels(pixels) {
            return Err(AiEditError("invalid AI retouch pixels"));
        }
        let bytes: Vec<u8> = pixels.iter().flatten().flat_map(|v| v.to_le_bytes()).collect();
        metadata.pixels = crate::segmask::base64_encode(&miniz_oxide::deflate::compress_to_vec(&bytes, 6));
        Ok(metadata)
    }

    pub fn pixel_count(&self) -> Result<usize, AiEditError> {
        let (w, h) = (self.width as usize, self.height as usize);
        if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
            return Err(AiEditError("AI retouch raster dimensions exceed limits"));
        }
        if let Some(support) = &self.support
            && (support.geometry.as_ref().is_some_and(|g| !g.valid())
                || support.points.is_empty()
                || support.points.len() > 4096
                || !support.points.iter().flatten().all(|v| v.is_finite())
                || !support.radius.is_finite()
                || support.radius <= 0.0
                || !support.feather.is_finite()
                || !(0.0..=1.0).contains(&support.feather))
        {
            return Err(AiEditError("invalid AI retouch brush support"));
        }
        let r = self.rect;
        if !r.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v)) || r[0] >= r[2] || r[1] >= r[3] {
            return Err(AiEditError("invalid AI retouch source rectangle"));
        }
        if self.model.is_empty() || self.model.len() > 256 || self.model_version.is_empty() || self.model_version.len() > 256 {
            return Err(AiEditError("invalid AI retouch model provenance"));
        }
        w.checked_mul(h).ok_or(AiEditError("AI retouch size overflow"))
    }

    pub fn decode_pixels(&self) -> Result<Vec<[f32; 4]>, AiEditError> {
        let count = self.pixel_count()?;
        let size = count.checked_mul(16).ok_or(AiEditError("AI retouch size overflow"))?;
        if self.pixels.len() > MAX_ENCODED_BYTES {
            return Err(AiEditError("AI retouch payload exceeds limits"));
        }
        let bad = AiEditError("damaged AI retouch raster");
        let compressed = crate::segmask::base64_decode(&self.pixels).ok_or(bad)?;
        let raw = miniz_oxide::inflate::decompress_to_vec_with_limit(&compressed, size).map_err(|_| bad)?;
        if raw.len() != size {
            return Err(bad);
        }
        let mut out = Vec::with_capacity(count);
        for pixel in raw.chunks_exact(16) {
            let mut rgba = [0.0; 4];
            for (v, bytes) in rgba.iter_mut().zip(pixel.chunks_exact(4)) {
                let bytes: [u8; 4] = bytes.try_into().map_err(|_| bad)?;
                *v = f32::from_le_bytes(bytes);
            }
            out.push(rgba);
        }
        if !valid_pixels(&out) {
            return Err(bad);
        }
        Ok(out)
    }
}

fn valid_pixels(pixels: &[[f32; 4]]) -> bool {
    pixels.iter().all(|p| p.iter().all(|v| v.is_finite() && v.abs() <= 65504.0) && (0.0..=1.0).contains(&p[3]))
}

/// Validate the entire stack before accepting a new edit. Reject excess data rather than
/// truncating it and silently losing an existing edit. Includes disabled edits.
pub fn validate_stack(edits: &[AiEdit]) -> Result<(), AiEditError> {
    if edits.len() > MAX_EDITS {
        return Err(AiEditError("too many AI retouches"));
    }
    let mut pixels = 0usize;
    let mut encoded = 0usize;
    for (i, edit) in edits.iter().enumerate() {
        pixels = pixels.checked_add(edit.pixel_count()?).ok_or(AiEditError("AI retouch size overflow"))?;
        encoded = encoded.checked_add(edit.pixels.len()).ok_or(AiEditError("AI retouch size overflow"))?;
        if pixels > MAX_TOTAL_PIXELS || encoded > MAX_TOTAL_ENCODED_BYTES || edit.pixels.len() > MAX_ENCODED_BYTES {
            return Err(AiEditError("AI retouch stack exceeds limits"));
        }
        if edits.iter().take(i).any(|previous| previous.id == edit.id) {
            return Err(AiEditError("duplicate AI retouch id"));
        }
    }
    Ok(())
}

fn deserialize_payload<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct Payload;
    impl serde::de::Visitor<'_> for Payload {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a bounded AI retouch payload")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<String, E> {
            if value.len() > MAX_ENCODED_BYTES {
                return Err(E::custom("AI retouch payload exceeds limits"));
            }
            Ok(value.to_owned())
        }
    }
    d.deserialize_str(Payload)
}

pub(crate) fn deserialize_edits<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<AiEdit>, D::Error> {
    struct Edits;
    impl<'de> serde::de::Visitor<'de> for Edits {
        type Value = Vec<AiEdit>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("a bounded AI retouch stack")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut edits = Vec::new();
            while let Some(edit) = seq.next_element::<AiEdit>()? {
                edits.push(edit);
                validate_stack(&edits).map_err(serde::de::Error::custom)?;
                if let Some(edit) = edits.last() {
                    edit.decode_pixels().map_err(serde::de::Error::custom)?;
                }
            }
            Ok(edits)
        }
    }
    d.deserialize_seq(Edits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DevelopSettings;
    pub fn edit() -> AiEdit {
        AiEdit::from_pixels(
            AiEdit {
                id: 1,
                enabled: true,
                rect: [0.25, 0.25, 0.75, 0.75],
                support: None,
                width: 2,
                height: 2,
                pixels: String::new(),
                model: "test-model".into(),
                model_version: "sha256:test".into(),
                seed: 42,
            },
            &[[0.4, 0.3, 0.2, 0.0], [0.4, 0.3, 0.2, 0.5], [0.4, 0.3, 0.2, 1.0], [0.4, 0.3, 0.2, 0.0]],
        )
        .unwrap()
    }
    #[test]
    fn old_settings_keep_json_and_edits_roundtrip_with_provenance() {
        let mut settings = DevelopSettings::from_json(&serde_json::json!({})).unwrap();
        assert!(settings.ai_edits.is_empty());
        assert!(settings.to_json().get("ai_edits").is_none());
        let hash = settings.hash64();
        settings.ai_edits.push(edit());
        assert_ne!(hash, settings.hash64());
        let restored = DevelopSettings::from_json(&settings.to_json()).unwrap();
        assert_eq!(settings, restored);
        let default = DevelopSettings::default();
        let copied = crate::apply_partial(&default, &settings.to_json(), 1.0);
        assert!(copied.ai_edits.is_empty(), "presets and sync cannot transfer raster retouches");
        let groups = crate::extract_groups(&settings, &crate::SettingsGroup::ALL);
        assert!(groups.get("ai_edits").is_none());
        assert_eq!(settings.ai_edits[0].decode_pixels().unwrap(), restored.ai_edits[0].decode_pixels().unwrap());
    }
    #[test]
    fn geometry_snapshot_rejects_malformed_dimensions_transforms_and_lens_metadata() {
        let valid = AiSupportGeometry {
            orientation: lightcraft_geom::Orientation::Normal,
            source_size: [6000, 4000],
            perspective: lightcraft_geom::Homography::IDENTITY,
            distortion: 0.0,
            lens: None,
            lens_distortion: 1.0,
        };
        assert!(valid.valid());
        let mut invalid = valid.clone();
        invalid.source_size[0] = 0;
        assert!(!invalid.valid());
        invalid = valid.clone();
        invalid.perspective.0[0] = f64::NAN;
        assert!(!invalid.valid());
        invalid = valid.clone();
        invalid.perspective.0 = [0.0; 9];
        assert!(!invalid.valid());
        invalid = valid.clone();
        invalid.lens_distortion = 2.01;
        assert!(!invalid.valid());
        invalid = valid.clone();
        invalid.distortion = f64::INFINITY;
        assert!(!invalid.valid());
        invalid = valid.clone();
        invalid.lens = Some(crate::EmbeddedLens { warp: Some(crate::EmbeddedWarp::default()), ..Default::default() });
        assert!(!invalid.valid(), "zero embedded radius is invalid");
        let mut stage = crate::EmbeddedWarpStage { radius: 0.5, ..Default::default() };
        stage.planes[1][17] = 1.0;
        stage.planes[1][18] = 0.5;
        invalid = valid;
        invalid.lens = Some(crate::EmbeddedLens { stages: [Some(stage), None, None], ..Default::default() });
        assert!(!invalid.valid(), "inverted radius range is invalid");
        let mut e = edit();
        e.support = Some(AiBrushSupport { geometry: Some(invalid), continuous: true, points: vec![[0.5, 0.5]], radius: 0.01, feather: 0.15 });
        assert!(e.pixel_count().is_err());
    }
    #[test]
    fn brush_support_roundtrips_and_rejects_invalid_bounds() {
        let mut e = edit();
        e.support = Some(AiBrushSupport { geometry: None, continuous: false, points: vec![[0.25, 0.5]], radius: 0.01, feather: 0.0 });
        let s = DevelopSettings { ai_edits: vec![e.clone()], ..Default::default() };
        assert_eq!(DevelopSettings::from_json(&s.to_json()).unwrap(), s);
        let mut legacy = s.to_json();
        legacy["ai_edits"][0]["support"].as_object_mut().unwrap().remove("feather");
        assert_eq!(DevelopSettings::from_json(&legacy).unwrap(), s);
        for feather in [-0.01, 1.01, f64::INFINITY, f64::NAN] {
            e.support.as_mut().unwrap().feather = feather;
            assert!(e.pixel_count().is_err());
        }
        e.support.as_mut().unwrap().feather = 0.0;
        for radius in [0.0, -1.0, f64::INFINITY, f64::NAN] {
            e.support.as_mut().unwrap().radius = radius;
            assert!(e.pixel_count().is_err());
        }
        let support = e.support.as_mut().unwrap();
        support.radius = 0.01;
        support.points = vec![[0.0, 0.0]; 4097];
        assert!(e.pixel_count().is_err());
        e.support.as_mut().unwrap().points = vec![[f64::NAN, 0.0]];
        assert!(e.pixel_count().is_err());
        e.support.as_mut().unwrap().points.clear();
        assert!(e.pixel_count().is_err());
    }
    #[test]
    fn malformed_rasters_are_bounded_and_rejected_without_panicking() {
        let mut e = edit();
        e.width = u32::MAX;
        assert!(e.decode_pixels().is_err());
        e = edit();
        e.rect[0] = f64::NAN;
        assert!(e.decode_pixels().is_err());
        e = edit();
        e.pixels = crate::segmask::base64_encode(&miniz_oxide::deflate::compress_to_vec(&[0; 1000], 6));
        assert!(e.decode_pixels().is_err(), "inflate must respect claimed dimensions");
        e = edit();
        e.width = 1;
        e.height = 1;
        assert!(AiEdit::from_pixels(e.clone(), &[[f32::NAN, 0.0, 0.0, 1.0]]).is_err());
        assert!(AiEdit::from_pixels(e.clone(), &[[0.0, 0.0, 0.0, 1.01]]).is_err());
        assert!(AiEdit::from_pixels(e, &[[f32::MAX, 0.0, 0.0, 1.0]]).is_err());
        let many: Vec<_> = (0..=MAX_EDITS)
            .map(|i| {
                let mut e = edit();
                e.id = i as u32;
                e
            })
            .collect();
        assert!(validate_stack(&many).is_err());
        let json = serde_json::json!({"ai_edits": many});
        assert!(DevelopSettings::from_json(&json).is_err());
        let mut large = edit();
        large.width = 1024;
        large.height = 1024;
        let mut edits = vec![large; 3];
        for (id, e) in edits.iter_mut().enumerate() {
            e.id = id as u32;
        }
        assert!(validate_stack(&edits).is_err(), "aggregate cap is stricter than per-edit cap");
    }
}
