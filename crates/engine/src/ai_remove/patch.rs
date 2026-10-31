//! Only the engine can grant patch coverage; runners return RGB without alpha.
use super::{input::PreparedInput, runner::RetouchOutput};
use lightcraft_develop::AiEdit;

pub(super) fn construct(prepared: PreparedInput, output: RetouchOutput) -> Result<AiEdit, String> {
    let input = prepared.request;
    output.validate(&input)?;
    let pixels: Vec<[f32; 4]> = output
        .rgb
        .iter()
        .zip(&input.rgb)
        .zip(&input.mask)
        .map(|((rgb, original), mask)| {
            let rgb = if *mask == 0 { original } else { rgb };
            [rgb[0], rgb[1], rgb[2], f32::from(*mask) / 255.0]
        })
        .collect();
    AiEdit::from_pixels(
        AiEdit {
            id: 0,
            enabled: true,
            rect: input.rect,
            support: Some(prepared.support),
            width: input.width as u32,
            height: input.height as u32,
            pixels: String::new(),
            model: output.provenance.model,
            model_version: output.provenance.revision,
            seed: u64::from(input.seed),
        },
        &pixels,
    )
    .map_err(|e| e.to_string())
}
