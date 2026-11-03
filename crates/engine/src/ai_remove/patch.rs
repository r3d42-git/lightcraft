//! The engine freezes geometry and bounds explicitly opted-in model coverage.
use super::{input::PreparedInput, runner::RetouchOutput};
use lightcraft_develop::AiEdit;

pub(super) fn construct(prepared: PreparedInput, output: RetouchOutput) -> Result<AiEdit, String> {
    let input = prepared.request;
    output.validate(&input)?;
    let coverage = output.coverage.as_ref().unwrap_or(&input.mask);
    let pixels: Vec<[f32; 4]> = output
        .rgb
        .iter()
        .zip(&input.rgb)
        .zip(coverage.iter().zip(&input.protected))
        .map(|((rgb, original), (mask, protected))| {
            let mask = if *protected { 0 } else { *mask };
            let rgb = if mask == 0 { original } else { rgb };
            [rgb[0], rgb[1], rgb[2], f32::from(mask) / 255.0]
        })
        .collect();
    AiEdit::from_pixels(
        AiEdit {
            id: 0,
            enabled: true,
            rect: input.rect,
            support: if input.effects { None } else { prepared.support },
            width: input.width as u32,
            height: input.height as u32,
            pixels: String::new(),
            model: output.provenance.model,
            model_version: output.provenance.revision,
            seed: u64::from(input.seed),
            stages: Vec::new(),
        },
        &pixels,
    )
    .map_err(|e| e.to_string())
}
