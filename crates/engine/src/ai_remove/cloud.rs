//! Cloud reconstruction keeps local geometry, authorization and raster persistence.
//! Shadow coverage is determined by the existing trusted ObjectClear stage; only
//! original RGB and the final authorized mask are sent to ChatGPT through Codex.
use super::{AiRemove, CloudDrafts, Job, Phase, SelectionMask, codex_client, error, input, lighting, patch, protocol, runner};
use crate::{Result, Session};
use lightcraft_codecs::{DecodeOptions, EncodeImage, EncodeMeta};
use lightcraft_develop::Spot;
use lightcraft_raster::Rgba8;
use runner::{RetouchIdentity, RetouchInput, RetouchOutput, RetouchProvenance, RetouchRequest, RetouchRunner};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::{Duration, Instant},
};

impl Session {
    pub(super) fn ai_remove_start_cloud(&mut self, include_effects: Option<bool>, match_lighting: Option<bool>) -> Result<Value> {
        self.ai_remove_poll();
        if self.ai_remove.busy() {
            return Err(error("AI Remove is already running."));
        }
        if self.ai_remove.selecting {
            return Err(error("Wait for the object selection."));
        }
        if self.ai_models.busy() {
            return Err(error("Wait for model management to finish."));
        }
        let segmented = self.ai_remove.object_selection.as_ref().and_then(|o| o.seg.clone());
        if self.ai_remove.object_selection.is_some() && segmented.is_none() {
            return Err(error("Select an object first."));
        }
        if let Some(seg) = &segmented
            && SelectionMask::new(seg).map_err(error)?.is_empty()
        {
            return Err(error("The selected mask is empty."));
        }
        let id = self.active().ok_or_else(|| error("Select a photo."))?;
        let d = self.develop_of(id).unwrap_or_default();
        let (spot, index) = if segmented.is_some() {
            (Spot::default(), None)
        } else if let Some(selection) = self.ai_remove.selection.clone() {
            (selection, None)
        } else {
            let index = self.active_spot.ok_or_else(|| error("Paint an AI selection first."))?;
            (d.spots.get(index).cloned().ok_or_else(|| error("Select a spot first."))?, Some(index))
        };
        if d.ai_edits.len() >= 8 {
            return Err(error("This photo already has eight AI patches."));
        }
        let match_lighting = match_lighting.unwrap_or(self.ai_remove.match_lighting.unwrap_or(true));
        let executable = codex_client::find_executable().map_err(error)?;
        let requested_effects = include_effects.or(self.ai_remove.include_effects).unwrap_or(true);
        // No local model is needed for object-only cloud reconstruction.
        let effects_package = if requested_effects {
            let resolved = crate::ai_models::root()
                .and_then(|root| crate::ai_models::resolve(&root, Some(crate::ai_models::EFFECTS_MODEL_ID), Some("objectclear"), false));
            match resolved {
                Ok(package) if package.supports_effects() => Some(package),
                _ if include_effects == Some(true) => return Err(error("Cloud shadow removal needs local ObjectClear shadow detection.")),
                _ => None,
            }
        } else {
            None
        };
        let effects = effects_package.is_some();
        let refinements = self.ai_remove.object_selection.as_ref().map(|o| o.refinements.clone()).unwrap_or_default();
        let photo = self.catalog.photo(id).cloned().ok_or_else(|| error("Photo missing."))?;
        let source = self.media.source_ref(&photo, crate::media::SourceLevel::Full);
        let info = self.source_info(id);
        let settings = (*d).clone();
        let job = Job::spawn_cloud(&self.activity, move |cancel, progress| {
            let deadline = Instant::now() + Duration::from_secs(1200);
            progress(Phase::Preparing);
            let mut prepared = match segmented {
                Some(seg) => input::prepare_segmented_protected(source, info, settings, seg, &refinements, 0, cancel)?,
                None => input::prepare(source, info, settings, spot, index.is_none(), 0, cancel)?,
            };
            prepared.request.effects = effects;
            let cloud = CloudRunner { executable, deadline };
            if let Some(package) = effects_package {
                let root = crate::ai_models::root().map_err(|e| e.to_string())?;
                let coverage = {
                    let _store_lock = crate::ai_models::store_lock(&root).map_err(|e| e.to_string())?;
                    package.verify_execution().map_err(|e| e.to_string())?;
                    progress(Phase::Effects);
                    let detector = protocol::ProtocolRunner {
                        runtime: &package.runtime,
                        backend: &package.backend,
                        package: Some(&package),
                        deadline: Some(deadline),
                    };
                    runner::invoke_checked(&prepared.request, &detector, cancel)?.coverage.ok_or("Shadow detection returned no coverage.")?
                };
                execute_variants(prepared, Some(coverage), &cloud, cancel, deadline, progress)
            } else {
                execute_variants(prepared, None, &cloud, cancel, deadline, progress)
            }
        })
        .map_err(error)?;
        self.ai_remove = AiRemove {
            use_cloud: true,
            match_lighting: Some(match_lighting),
            cloud_drafts: None,
            include_effects: self.ai_remove.include_effects,
            reconstruct: self.ai_remove.reconstruct,
            effects_active: effects,
            reconstruction_active: false,
            object_selection: self.ai_remove.object_selection.clone(),
            selecting: false,
            selection_revision: self.ai_remove.selection_revision,
            phase: Phase::Loading,
            cancel_reason: None,
            message: "Sending selected region to ChatGPT through Codex…".into(),
            draft: None,
            preview_settings: None,
            preview_original: false,
            tag: self.ai_remove_tag(),
            spot: index,
            selection: self.ai_remove.selection.clone(),
            selection_tag: self.ai_remove.selection_tag.clone(),
            job: Some(job),
        };
        Ok(json!({"running":true,"provider":"chatgpt","effectsActive":effects,"matchLighting":match_lighting}))
    }
}

/// One runner invocation produces both drafts. Changing the preference never
/// runs a model again, and matching failure cannot discard the valid raw result.
fn execute_variants(
    prepared: input::PreparedInput,
    coverage: Option<Vec<u8>>,
    cloud: &dyn RetouchRunner,
    cancel: &AtomicBool,
    deadline: Instant,
    progress: &dyn Fn(Phase),
) -> std::result::Result<CloudDrafts, String> {
    let mut reconstruction = prepared.request.clone();
    if let Some(coverage) = &coverage {
        if coverage.len() != reconstruction.mask.len()
            || !coverage.iter().zip(&reconstruction.mask).all(|(c, m)| (*c == 0 || *c == 255) && (*m == 0 || *c == 255))
        {
            return Err("Invalid shadow coverage.".into());
        }
        reconstruction.mask = coverage.iter().zip(&reconstruction.protected).map(|(v, p)| if *p { 0 } else { *v }).collect();
    }
    reconstruction.effects = false;
    if !reconstruction.mask.contains(&255) {
        return Err("No unprotected pixels to reconstruct.".into());
    }
    progress(Phase::Reconstruction);
    // CloudRunner returns the entire decoded generated ROI, not a composite
    // whose artificial mask edge could contaminate the high-frequency guide.
    let mut output = runner::invoke_checked(&reconstruction, cloud, cancel)?;
    let gain = inference_gain(&reconstruction)?;
    progress(Phase::Finalizing);
    let matched_rgb = lighting::apply(&reconstruction, &output.rgb, gain, cancel, deadline);
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    output.coverage = coverage;
    if output.coverage.is_some() {
        output.provenance.model = "ObjectClear coverage + ChatGPT image generation".into();
    }
    let matched_output = matched_rgb.map(|rgb| {
        let mut revision = output.provenance.revision.clone();
        let suffix = "; lighting=boundary-sigma32-v1";
        if revision.len() + suffix.len() <= 256 {
            revision.push_str(suffix);
        }
        RetouchOutput {
            width: output.width,
            height: output.height,
            rgb,
            coverage: output.coverage.clone(),
            provenance: RetouchProvenance { model: output.provenance.model.clone(), revision, identity: output.provenance.identity.clone() },
        }
    });
    let raw = patch::construct(input::PreparedInput { request: prepared.request.clone(), support: prepared.support.clone() }, output)?;
    let matched = matched_output.and_then(|output| patch::construct(prepared, output));
    Ok(CloudDrafts { raw, matched })
}

fn inference_gain(input: &RetouchInput) -> std::result::Result<f32, String> {
    let matrix = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB);
    let mut gain = 1.0f32;
    for pixel in &input.rgb {
        for value in matrix.apply_f32(*pixel) {
            if !value.is_finite() {
                return Err("Non-finite cloud input.".into());
            }
            gain = gain.max(value);
        }
    }
    Ok(gain)
}

struct CloudRunner {
    executable: PathBuf,
    deadline: Instant,
}
impl RetouchRunner for CloudRunner {
    fn identity(&self) -> RetouchIdentity {
        RetouchIdentity { model: "chatgpt-image-generation".into(), runtime: self.executable.clone(), adapter: "codex-app-server" }
    }
    fn run(&self, request: RetouchRequest<'_>, cancel: &AtomicBool) -> std::result::Result<RetouchOutput, String> {
        if request.input.effects {
            return Err("Cloud reconstruction requires resolved local shadow coverage.".into());
        }
        let canvas = Canvas::new(request.input)?;
        let scratch = Scratch::new()?;
        let image = scratch.0.join("photo.png");
        let mask = scratch.0.join("edit-mask.png");
        std::fs::write(&image, png(&canvas.image)?).map_err(|e| e.to_string())?;
        std::fs::write(&mask, png(&canvas.mask)?).map_err(|e| e.to_string())?;
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let prompt = "Edit image 1 (the photo) with built-in image generation exactly once. Image 2 is an edit mask: WHITE pixels identify the object and any associated shadow to remove, BLACK pixels must remain unchanged. Remove the indicated object and reconstruct natural background only in the white region. Keep the exact square canvas, framing, alignment, lighting, color and every other object. Black padding is intentional; do not crop, zoom or move anything. Do not draw the mask into the result. Return one opaque edited photo, no text. Use the supplied photo as the edit target. Do not use any API-key fallback or other tools.";
        let generated = codex_client::edit(&self.executable, &[image, mask], prompt, cancel, self.deadline)?;
        let rgb = canvas.decode_generated(&generated.png, request.input)?;
        Ok(RetouchOutput {
            width: request.input.width,
            height: request.input.height,
            rgb,
            coverage: None,
            provenance: RetouchProvenance {
                model: "ChatGPT image generation".into(),
                revision: generated.provenance,
                identity: request.identity.clone(),
            },
        })
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> std::result::Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("lightcraft-cloud-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder.create(&path).map_err(|e| e.to_string())?;
        Ok(Self(path))
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn png(image: &Rgba8) -> std::result::Result<Vec<u8>, String> {
    lightcraft_codecs::encode_png(&EncodeImage::rgba8(image), &EncodeMeta::default()).map_err(|e| e.to_string())
}

/// A square envelope avoids model output aspect-ratio changes stretching the patch.
/// Only image pixels are serialized: no source filename, EXIF, GPS or catalog data.
struct Canvas {
    image: Rgba8,
    mask: Rgba8,
    x: usize,
    y: usize,
    gain: f32,
}
impl Canvas {
    fn new(input: &RetouchInput) -> std::result::Result<Self, String> {
        let count = input.width.checked_mul(input.height).filter(|n| *n > 0).ok_or("Invalid cloud input dimensions.")?;
        if input.width > 1024 || input.height > 1024 || input.rgb.len() != count || input.mask.len() != count || input.protected.len() != count {
            return Err("Invalid cloud input pixels or mask.".into());
        }
        let matrix = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB);
        let colors: Vec<[f32; 3]> = input.rgb.iter().map(|p| matrix.apply_f32(*p)).collect();
        if !colors.iter().flatten().all(|v| v.is_finite()) {
            return Err("Non-finite cloud input.".into());
        }
        // One reversible exposure scale prevents HDR clipping. Gamut conversion is
        // clipped only for the inference preview, never for unselected original pixels.
        let gain = colors.iter().flatten().copied().fold(1.0f32, f32::max);
        let side = input.width.max(input.height);
        let (x, y) = ((side - input.width) / 2, (side - input.height) / 2);
        let mut image = Rgba8::new(side, side);
        let mut mask = Rgba8::new(side, side);
        image.data.fill([0, 0, 0, 255]);
        mask.data.fill([0, 0, 0, 255]);
        for (i, color) in colors.into_iter().enumerate() {
            let at = (y + i / input.width) * side + x + i % input.width;
            let encode = |v| lightcraft_color::transfer::encode_srgb8(v / gain);
            if let Some(pixel) = image.data.get_mut(at) {
                *pixel = [encode(color[0]), encode(color[1]), encode(color[2]), 255];
            }
            if input.mask.get(i) == Some(&255)
                && input.protected.get(i) == Some(&false)
                && let Some(pixel) = mask.data.get_mut(at)
            {
                *pixel = [255; 4];
            }
        }
        Ok(Self { image, mask, x, y, gain })
    }
    fn decode_generated(&self, bytes: &[u8], input: &RetouchInput) -> std::result::Result<Vec<[f32; 3]>, String> {
        if bytes.len() > 32 * 1024 * 1024 || !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err("Cloud result must be a bounded PNG image.".into());
        }
        let decoded = lightcraft_codecs::decode(bytes, DecodeOptions { max_size: None, max_pixels: 16 * 1024 * 1024 }).map_err(|e| e.to_string())?;
        if decoded.image.width != decoded.image.height || decoded.image.width < 64 || decoded.orientation != 1 {
            return Err("ChatGPT changed the image canvas. Try again; no retouch was applied.".into());
        }
        if decoded.alpha.as_ref().is_some_and(|a| a.data.iter().any(|v| !v.is_finite() || *v < 0.999)) {
            return Err("ChatGPT returned a transparent result.".into());
        }
        let working = decoded.to_working();
        let scale = working.width as f32 / self.image.width as f32;
        let sample = |i: usize| {
            let x = ((self.x + i % input.width) as f32 + 0.5) * scale;
            let y = ((self.y + i / input.width) as f32 + 0.5) * scale;
            working.sample_bilinear(x, y).map(|v| v * self.gain)
        };
        let rgb: Vec<[f32; 3]> = (0..input.rgb.len()).map(sample).collect();
        if !rgb.iter().flatten().all(|v| v.is_finite()) {
            return Err("ChatGPT returned non-finite pixel data.".into());
        }
        Ok(rgb)
    }
    #[cfg(test)]
    fn decode(&self, bytes: &[u8], input: &RetouchInput) -> std::result::Result<Vec<[f32; 3]>, String> {
        let generated = self.decode_generated(bytes, input)?;
        Ok(generated
            .into_iter()
            .enumerate()
            .map(|(i, p)| {
                if input.mask.get(i) == Some(&255) && input.protected.get(i) == Some(&false) {
                    p
                } else {
                    input.rgb.get(i).copied().unwrap_or([0.; 3])
                }
            })
            .collect())
    }
    #[cfg(test)]
    fn decode_matched(&self, bytes: &[u8], input: &RetouchInput) -> std::result::Result<Vec<[f32; 3]>, String> {
        let generated = self.decode_generated(bytes, input)?;
        lighting::apply(input, &generated, self.gain, &AtomicBool::new(false), Instant::now() + Duration::from_secs(60))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn input() -> RetouchInput {
        RetouchInput {
            width: 128,
            height: 64,
            rgb: vec![[0.2, 0.2, 0.2]; 128 * 64],
            mask: vec![255; 128 * 64],
            protected: vec![false; 128 * 64],
            rect: [0.0, 0.0, 1.0, 1.0],
            seed: 0,
            steps: 20,
            effects: false,
        }
    }
    #[test]
    fn cloud_roundtrip_preserves_unselected_hdr_and_protected_pixels_exactly() {
        let mut input = input();
        input.rgb[0] = [4.0, 2.0, 1.0];
        input.mask[0] = 0;
        input.rgb[1] = [0.9, 0.4, 0.1];
        input.protected[1] = true;
        let canvas = Canvas::new(&input).unwrap();
        assert_eq!(canvas.mask.data[canvas.y * 128 + 1], [0, 0, 0, 255]);
        let result = canvas.decode(&png(&canvas.image).unwrap(), &input).unwrap();
        assert_eq!(result[0], input.rgb[0]);
        assert_eq!(result[1], input.rgb[1]);
        for v in result[2] {
            assert!((v - 0.2).abs() < 0.012, "{v}");
        }
    }
    fn context_fixture() -> RetouchInput {
        let mut input = input();
        input.rgb.fill([0.4; 3]);
        input.mask.fill(0);
        let red = lightcraft_color::SRGB.to_space(&lightcraft_color::REC2020).apply_f32([0.8, 0.0, 0.0]);
        for y in 20..44 {
            for x in 48..80 {
                input.rgb[y * input.width + x] = red;
                input.mask[y * input.width + x] = 255;
            }
        }
        input
    }
    fn generated_background(canvas: &Canvas, color: [f32; 3]) -> Rgba8 {
        let mut generated = canvas.image.clone();
        let srgb = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB).apply_f32(color);
        let encoded = srgb.map(|v| lightcraft_color::transfer::encode_srgb8(v / canvas.gain));
        for y in canvas.y..canvas.image.height - canvas.y {
            for x in canvas.x..canvas.image.width - canvas.x {
                generated.data[y * generated.width + x] = [encoded[0], encoded[1], encoded[2], 255];
            }
        }
        generated
    }
    #[test]
    fn cloud_boundary_brightness_matches_context_without_restoring_removed_object() {
        let mut input = context_fixture();
        input.rgb[0] = [4.0, 2.0, 1.0];
        let protected = 32 * input.width + 64;
        input.rgb[protected] = [0.4; 3];
        input.protected[protected] = true;
        let canvas = Canvas::new(&input).unwrap();
        let generated = generated_background(&canvas, [0.4 + 0.025 * canvas.gain; 3]);
        let result = canvas.decode_matched(&png(&generated).unwrap(), &input).unwrap();
        for (i, pixel) in result.iter().enumerate() {
            if input.mask[i] == 0 || input.protected[i] {
                assert_eq!(*pixel, input.rgb[i]);
            } else {
                assert!(pixel.iter().all(|v| (*v - 0.4).abs() < 0.0001), "{pixel:?}");
            }
        }
        assert_eq!(result[protected], [0.4; 3]);
    }
    #[test]
    fn cloud_context_normal_roundtrip_and_generated_texture_remain_stable() {
        let input = context_fixture();
        let canvas = Canvas::new(&input).unwrap();
        let roundtrip = canvas.decode(&png(&canvas.image).unwrap(), &input).unwrap();
        let selected = 32 * input.width + 64;
        for channel in 0..3 {
            assert!((roundtrip[selected][channel] - input.rgb[selected][channel]).abs() < 0.005);
        }
        let mut generated = generated_background(&canvas, [0.44; 3]);
        let at = (canvas.y + 32) * generated.width + canvas.x + 64;
        generated.data[at] = [190, 180, 170, 255];
        let working = lightcraft_codecs::decode(&png(&generated).unwrap(), DecodeOptions::default()).unwrap().to_working();
        let base = working.sample_bilinear(canvas.x as f32 + 1.5, canvas.y as f32 + 1.5);
        let texture = working.sample_bilinear((canvas.x + 64) as f32 + 0.5, (canvas.y + 32) as f32 + 0.5);
        let result = canvas.decode_matched(&png(&generated).unwrap(), &input).unwrap();
        for channel in 0..3 {
            assert!((result[selected][channel] - (0.4 + texture[channel] - base[channel])).abs() < 0.0001);
        }
    }
    #[test]
    fn cloud_matching_off_never_applies_the_old_global_offset() {
        let input = context_fixture();
        let canvas = Canvas::new(&input).unwrap();
        let result = canvas.decode(&png(&generated_background(&canvas, [0.45; 3])).unwrap(), &input).unwrap();
        for (i, p) in result.iter().enumerate() {
            if input.mask[i] == 0 {
                assert_eq!(*p, input.rgb[i]);
            } else {
                assert!(p.iter().all(|v| *v > 0.44));
            }
        }
        let mut sparse = input.clone();
        sparse.mask.fill(255);
        assert!(canvas.decode_matched(&png(&generated_background(&canvas, [0.45; 3])).unwrap(), &sparse).is_err());
        assert!(canvas.decode(&png(&generated_background(&canvas, [0.45; 3])).unwrap(), &sparse).is_ok());
    }
    #[test]
    fn cloud_rejects_changed_aspect_transparency_and_invalid_input() {
        let mut input = input();
        let canvas = Canvas::new(&input).unwrap();
        assert!(canvas.decode(&png(&Rgba8::new(128, 64)).unwrap(), &input).is_err());
        assert!(canvas.decode(&png(&Rgba8::new(128, 128)).unwrap(), &input).is_err());
        input.rgb[0][0] = f32::NAN;
        assert!(Canvas::new(&input).is_err());
    }

    #[test]
    fn provider_switch_discards_draft_preserves_selection_and_never_starts_upload() {
        let mut s = Session::with_demo();
        super::super::tests::draft(&mut s);
        let selection = Spot::default();
        s.ai_remove.selection = Some(selection.clone());
        let before = s.develop_of(s.active().unwrap()).unwrap();
        s.execute("aiRemove.provider", &json!({"provider":"chatgpt"})).unwrap();
        assert!(s.ai_remove.use_cloud);
        assert!(!s.ai_remove.busy());
        assert!(s.ai_remove.draft.is_none());
        assert_eq!(s.ai_remove.selection.as_ref().unwrap().points, selection.points);
        assert_eq!(*s.develop_of(s.active().unwrap()).unwrap(), *before);
        assert_eq!(s.execute("aiRemove.status", &json!({})).unwrap()["provider"], "chatgpt");
        assert!(s.execute("aiRemove.provider", &json!({"provider":"unknown"})).is_err());
        assert!(s.execute("aiRemove.provider", &json!({})).is_err());
        assert!(s.ai_remove.use_cloud);
        s.ai_remove.selecting = true;
        assert!(s.execute("aiRemove.provider", &json!({"provider":"local"})).is_err());
        assert!(s.ai_remove.use_cloud);
    }

    #[test]
    fn cloud_effects_reconstruct_original_rgb_and_keep_protected_pixels() {
        struct Fake;
        impl RetouchRunner for Fake {
            fn identity(&self) -> RetouchIdentity {
                RetouchIdentity { model: "fake".into(), runtime: "fake".into(), adapter: "test" }
            }
            fn run(&self, r: RetouchRequest<'_>, _: &AtomicBool) -> std::result::Result<RetouchOutput, String> {
                assert_eq!(r.input.rgb, vec![[0.2, 0.3, 0.4]; 4]);
                assert_eq!(r.input.mask, vec![0, 255, 255, 0]);
                assert!(!r.input.effects);
                Ok(RetouchOutput {
                    width: 4,
                    height: 1,
                    rgb: vec![[0.7, 0.6, 0.5]; 4],
                    coverage: None,
                    provenance: RetouchProvenance { model: "fake".into(), revision: "1".into(), identity: r.identity.clone() },
                })
            }
        }
        let prepared = || input::PreparedInput {
            support: None,
            request: RetouchInput {
                width: 4,
                height: 1,
                rgb: vec![[0.2, 0.3, 0.4]; 4],
                mask: vec![0, 255, 0, 0],
                protected: vec![true, false, false, false],
                rect: [0.0, 0.0, 1.0, 1.0],
                seed: 0,
                steps: 20,
                effects: true,
            },
        };
        let edit = execute_variants(
            prepared(),
            Some(vec![255, 255, 255, 0]),
            &Fake,
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(30),
            &|_| {},
        )
        .unwrap()
        .raw;
        let pixels = edit.decode_pixels().unwrap();
        assert_eq!(pixels[0], [0.2, 0.3, 0.4, 0.0]);
        assert_eq!(pixels[1], [0.7, 0.6, 0.5, 1.0]);
        assert_eq!(pixels[2], [0.7, 0.6, 0.5, 1.0]);
        assert_eq!(pixels[3], [0.2, 0.3, 0.4, 0.0]);
        assert!(
            execute_variants(
                prepared(),
                Some(vec![255, 0, 255, 0]),
                &Fake,
                &AtomicBool::new(false),
                Instant::now() + Duration::from_secs(30),
                &|_| {}
            )
            .is_err()
        );
        assert!(
            execute_variants(
                prepared(),
                Some(vec![255, 255, 255, 0]),
                &Fake,
                &AtomicBool::new(true),
                Instant::now() + Duration::from_secs(30),
                &|_| {}
            )
            .is_err()
        );
    }

    fn fake_variants(calls: &std::sync::atomic::AtomicUsize) -> CloudDrafts {
        fake_variants_for(context_fixture(), calls)
    }
    fn fake_variants_for(input: RetouchInput, calls: &std::sync::atomic::AtomicUsize) -> CloudDrafts {
        struct Fake<'a>(&'a std::sync::atomic::AtomicUsize);
        impl RetouchRunner for Fake<'_> {
            fn identity(&self) -> RetouchIdentity {
                RetouchIdentity { model: "synthetic-cloud".into(), runtime: "fake".into(), adapter: "fake" }
            }
            fn run(&self, r: RetouchRequest<'_>, _: &AtomicBool) -> std::result::Result<RetouchOutput, String> {
                self.0.fetch_add(1, Ordering::Relaxed);
                Ok(RetouchOutput {
                    width: r.input.width,
                    height: r.input.height,
                    rgb: vec![[0.45; 3]; r.input.rgb.len()],
                    coverage: None,
                    provenance: RetouchProvenance { model: "synthetic-cloud".into(), revision: "fixture-1".into(), identity: r.identity.clone() },
                })
            }
        }
        execute_variants(
            input::PreparedInput { request: input, support: None },
            None,
            &Fake(calls),
            &AtomicBool::new(false),
            Instant::now() + Duration::from_secs(30),
            &|_| {},
        )
        .unwrap()
    }
    fn completed_session(variants: CloudDrafts, matching: bool) -> Session {
        let mut session = Session::with_demo();
        let (tx, reply) = std::sync::mpsc::channel();
        let (_, phases) = std::sync::mpsc::channel();
        session.ai_remove.use_cloud = true;
        session.ai_remove.match_lighting = Some(matching);
        session.ai_remove.tag = session.ai_remove_tag();
        session.ai_remove.job = Some(Job { reply, phases, cancel: std::sync::Arc::new(AtomicBool::new(false)) });
        tx.send(Ok(super::super::JobResult::Cloud(Box::new(variants)))).unwrap();
        session.ai_remove_poll();
        session
    }
    #[test]
    fn cloud_pair_toggle_reuses_one_runner_result_and_accepts_only_chosen_variant() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let variants = fake_variants(&calls);
        assert!(variants.matched.as_ref().unwrap().model_version.contains("lighting=boundary-sigma32-v1"));
        let raw = variants.raw.clone();
        let matched = variants.matched.clone().unwrap();
        let mut s = completed_session(variants, true);
        let id = s.active().unwrap();
        let before = s.develop_of(id).unwrap();
        assert_eq!(s.ai_remove.draft.as_ref(), Some(&matched));
        let _ = s.ai_remove_preview_settings().unwrap();
        assert!(s.ai_remove.preview_settings.is_some());
        s.execute("aiRemove.matchLighting", &json!({"enabled":false})).unwrap();
        assert_eq!(s.ai_remove.draft.as_ref(), Some(&raw));
        assert!(s.ai_remove.preview_settings.is_none());
        assert_eq!(*s.develop_of(id).unwrap(), *before);
        s.execute("aiRemove.matchLighting", &json!({"enabled":true})).unwrap();
        assert_eq!(s.ai_remove.draft.as_ref(), Some(&matched));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        s.execute("aiRemove.accept", &json!({})).unwrap();
        assert!(s.ai_remove.cloud_drafts.is_none());
        assert!(s.ai_remove.draft.is_none());
        let accepted = s.develop_of(id).unwrap();
        assert_eq!(accepted.ai_edits.len(), before.ai_edits.len() + 1);
        assert_eq!(accepted.ai_edits.last().unwrap().pixels, matched.pixels);
        s.execute("aiRemove.matchLighting", &json!({"enabled":false})).unwrap();
        assert_eq!(*s.develop_of(id).unwrap(), *accepted);
    }
    #[test]
    fn bright_protected_island_can_reject_matching_without_losing_raw_hdr() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let mut input = context_fixture();
        let protected = 32 * input.width + 64;
        input.rgb[0] = [4., 2., 1.];
        input.rgb[protected] = [3., 1.5, 0.5];
        input.protected[protected] = true;
        let variants = fake_variants_for(input, &calls);
        assert!(variants.matched.as_ref().unwrap_err().contains("too much"));
        let raw = variants.raw.decode_pixels().unwrap();
        assert_eq!(raw[0], [4., 2., 1., 0.]);
        assert_eq!(raw[protected], [3., 1.5, 0.5, 0.]);
        let mut s = completed_session(variants, true);
        assert!(s.ai_remove.draft.is_none());
        s.execute("aiRemove.matchLighting", &json!({"enabled":false})).unwrap();
        assert_eq!(s.ai_remove.draft.as_ref().unwrap().decode_pixels().unwrap()[protected], [3., 1.5, 0.5, 0.]);
        assert_eq!(calls.load(Ordering::Relaxed), 1);
    }
    #[test]
    fn cloud_pair_matching_error_keeps_raw_and_wrong_label_never_shows_raw() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let mut variants = fake_variants(&calls);
        let raw = variants.raw.clone();
        variants.matched = Err("Lighting fixture unsupported; turn matching off.".into());
        let mut s = completed_session(variants, true);
        assert_eq!(s.ai_remove.phase, Phase::Failed);
        assert!(s.ai_remove.draft.is_none());
        assert!(s.ai_remove.tag.is_some());
        s.execute("aiRemove.matchLighting", &json!({"enabled":false})).unwrap();
        assert_eq!(s.ai_remove.draft.as_ref(), Some(&raw));
        assert!(s.execute("aiRemove.matchLighting", &json!({"enabled":true})).is_err());
        assert_eq!(s.ai_remove.match_lighting, Some(false));
        assert_eq!(s.ai_remove.draft.as_ref(), Some(&raw));
        assert_eq!(calls.load(Ordering::Relaxed), 1);
        s.ai_remove.selecting = true;
        assert!(s.execute("aiRemove.matchLighting", &json!({"enabled":true})).is_err());
    }
    #[test]
    fn cloud_pair_clears_on_cancel_provider_photo_change_and_rejects_busy_toggle() {
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let variants = fake_variants(&calls);
        for action in ["cancel", "provider", "photo", "edit"] {
            let mut s = completed_session(variants.clone(), true);
            let _ = s.ai_remove_preview_settings();
            match action {
                "cancel" => {
                    s.execute("aiRemove.cancel", &json!({})).unwrap();
                }
                "provider" => {
                    s.execute("aiRemove.provider", &json!({"provider":"local"})).unwrap();
                }
                "photo" => {
                    s.execute("library.down", &json!({})).unwrap();
                }
                _ => {
                    s.execute("develop.set", &json!({"control":"light.exposure","value":0.5})).unwrap();
                }
            }
            s.ai_remove_poll();
            assert!(s.ai_remove.cloud_drafts.is_none(), "{action}");
            assert!(s.ai_remove.draft.is_none());
            assert!(s.ai_remove.preview_settings.is_none());
        }
        let mut s = Session::with_demo();
        let (_tx, reply) = std::sync::mpsc::channel();
        let (_, phases) = std::sync::mpsc::channel();
        s.ai_remove.job = Some(Job { reply, phases, cancel: std::sync::Arc::new(AtomicBool::new(false)) });
        assert!(s.execute("aiRemove.matchLighting", &json!({"enabled":false})).is_err());
        assert_eq!(s.ai_remove.match_lighting, None);
        assert!(s.execute("aiRemove.start", &json!({"provider":"chatgpt","matchLighting":"false"})).is_err());
    }
    #[test]
    #[ignore = "explicit saved real-RAW/CodexPNG parity test; offline only"]
    fn saved_real_boundary_matches_frozen_python_reference() {
        let root = PathBuf::from(std::env::var_os("LIGHTCRAFT_CLOUD_REFERENCE_DIR").expect("reference root"));
        let image = PathBuf::from(std::env::var_os("LIGHTCRAFT_CLOUD_REFERENCE_PNG").expect("saved generated PNG"));
        let mut settings: Value = serde_json::from_slice(&std::fs::read(root.join("cloud-accepted-settings.json")).unwrap()).unwrap();
        let baseline: lightcraft_develop::AiEdit = serde_json::from_value(settings["ai_edits"][0].clone()).unwrap();
        let pixels = baseline.decode_pixels().unwrap();
        let input = RetouchInput {
            width: baseline.width as usize,
            height: baseline.height as usize,
            rgb: pixels.iter().map(|p| if p[3] > 0.0 { [0.; 3] } else { [p[0], p[1], p[2]] }).collect(),
            mask: pixels.iter().map(|p| if p[3] > 0.0 { 255 } else { 0 }).collect(),
            protected: vec![false; pixels.len()],
            rect: baseline.rect,
            seed: 0,
            steps: 20,
            effects: false,
        };
        let reference_settings: Value =
            serde_json::from_slice(&std::fs::read(root.join("spatial-prototype-boundary/candidate-settings.json")).unwrap()).unwrap();
        let reference: lightcraft_develop::AiEdit = serde_json::from_value(reference_settings["ai_edits"][0].clone()).unwrap();
        let reference_pixels = reference.decode_pixels().unwrap();
        let metadata: Value =
            serde_json::from_slice(&std::fs::read(root.join("spatial-prototype-boundary/candidate-metadata.json")).unwrap()).unwrap();
        let mut canvas = Canvas::new(&input).unwrap();
        canvas.gain = metadata["gain"].as_f64().unwrap() as f32;
        let started = Instant::now();
        let generated = canvas.decode_generated(&std::fs::read(image).unwrap(), &input).unwrap();
        let result = lighting::apply(&input, &generated, canvas.gain, &AtomicBool::new(false), Instant::now() + Duration::from_secs(180)).unwrap();
        let mut max = 0.0f32;
        let mut count = 0;
        for (i, p) in result.iter().enumerate() {
            if input.mask[i] == 0 {
                assert_eq!(*p, input.rgb[i]);
            } else {
                count += 1;
                for c in 0..3 {
                    max = max.max((p[c] - reference_pixels[i][c]).abs());
                }
            }
        }
        println!("Rust boundary parity: selected={count}; maximum_float_delta={max:.9}; seconds={:.3}", started.elapsed().as_secs_f64());
        assert!(max < 3e-6, "frozen Python/Rust mismatch {max}");
        if let Some(path) = std::env::var_os("LIGHTCRAFT_CLOUD_REFERENCE_SETTINGS_OUTPUT") {
            let output = RetouchOutput {
                width: input.width,
                height: input.height,
                rgb: result,
                coverage: None,
                provenance: RetouchProvenance {
                    model: baseline.model.clone(),
                    revision: format!("{}; lighting=boundary-sigma32-v1", baseline.model_version),
                    identity: RetouchIdentity { model: "offline-reference".into(), runtime: PathBuf::new(), adapter: "offline" },
                },
            };
            let edit = patch::construct(input::PreparedInput { request: input, support: None }, output).unwrap();
            settings["ai_edits"][0] = serde_json::to_value(edit).unwrap();
            std::fs::write(path, serde_json::to_vec(&settings).unwrap()).unwrap();
        }
    }

    #[test]
    #[ignore = "explicit offline reference from the procedural cloud edit, no network"]
    fn saved_synthetic_reference_corrects_observed_brightness_drift() {
        let root = PathBuf::from(std::env::var_os("LIGHTCRAFT_CLOUD_REFERENCE_DIR").expect("explicit synthetic reference directory"));
        let image = PathBuf::from(std::env::var_os("LIGHTCRAFT_CLOUD_REFERENCE_PNG").expect("explicit generated reference PNG"));
        let settings: Value = serde_json::from_slice(&std::fs::read(root.join("synthetic-accepted-settings.json")).unwrap()).unwrap();
        let edit: lightcraft_develop::AiEdit = serde_json::from_value(settings["ai_edits"][0].clone()).unwrap();
        assert_eq!((edit.width, edit.height), (687, 512));
        let pixels = edit.decode_pixels().unwrap();
        let neutral = [0.4564111f32; 3];
        let input = RetouchInput {
            width: edit.width as usize,
            height: edit.height as usize,
            rgb: pixels.iter().map(|p| if p[3] > 0.0 { neutral } else { [p[0], p[1], p[2]] }).collect(),
            mask: pixels.iter().map(|p| if p[3] > 0.0 { 255 } else { 0 }).collect(),
            protected: vec![false; pixels.len()],
            rect: edit.rect,
            seed: u32::try_from(edit.seed).unwrap(),
            steps: 20,
            effects: false,
        };
        let canvas = Canvas::new(&input).unwrap();
        assert_eq!(canvas.gain, 1.0);
        let corrected = canvas.decode_matched(&std::fs::read(image).unwrap(), &input).unwrap();
        let mut old_error = 0.0f64;
        let mut new_error = 0.0f64;
        let mut selected = 0usize;
        for (i, result) in corrected.iter().enumerate() {
            if input.mask[i] == 0 {
                assert_eq!(*result, input.rgb[i]);
            } else {
                selected += 1;
                for channel in 0..3 {
                    old_error += f64::from((pixels[i][channel] - neutral[channel]).abs());
                    new_error += f64::from((result[channel] - neutral[channel]).abs());
                }
            }
        }
        assert!(selected > 0);
        let old_mae = old_error / (selected * 3) as f64;
        let new_mae = new_error / (selected * 3) as f64;
        println!(
            "Offline synthetic reference: selected={selected}; previous_MAE={old_mae:.8}; corrected_MAE={new_mae:.8}; ratio={:.6}",
            new_mae / old_mae
        );
        assert!(new_mae < old_mae * 0.25 && new_mae < 0.01, "old={old_mae}, corrected={new_mae}");
        if let Some(path) = std::env::var_os("LIGHTCRAFT_CLOUD_REFERENCE_OUTPUT") {
            let output = RetouchInput { rgb: corrected, ..input };
            std::fs::write(path, png(&Canvas::new(&output).unwrap().image).unwrap()).unwrap();
        }
    }

    #[test]
    #[ignore = "explicit live ChatGPT subscription test; consumes included usage"]
    fn live_chatgpt_masked_edit() {
        assert_eq!(std::env::var("LIGHTCRAFT_CLOUD_LIVE").as_deref(), Ok("1"));
        let mut input = input();
        input.mask.fill(0);
        let red = lightcraft_color::SRGB.to_space(&lightcraft_color::REC2020).apply_f32([0.8, 0.0, 0.0]);
        for y in 20..44 {
            for x in 48..80 {
                let index = y * input.width + x;
                input.rgb[index] = red;
                input.mask[index] = 255;
            }
        }
        let runner = CloudRunner { executable: codex_client::find_executable().unwrap(), deadline: Instant::now() + Duration::from_secs(600) };
        let result = execute_variants(
            input::PreparedInput { request: input.clone(), support: None },
            None,
            &runner,
            &AtomicBool::new(false),
            runner.deadline,
            &|_| {},
        )
        .unwrap();
        let pixels = result.raw.decode_pixels().unwrap();
        assert_eq!(pixels.len(), input.rgb.len());
        for (i, pixel) in pixels.iter().enumerate() {
            if input.mask[i] == 0 {
                assert_eq!([pixel[0], pixel[1], pixel[2]], input.rgb[i]);
            }
        }
        let center = pixels[32 * input.width + 64];
        assert!((center[0] - center[1]).abs() < 0.12, "object removal failed: {center:?}");
        println!("Cloud masked edit completed; protected/outside pixels identical; provenance: {}", result.raw.model_version);
    }
}
