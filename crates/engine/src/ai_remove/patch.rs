//! The engine freezes geometry and bounds explicitly opted-in model coverage.
use super::{input::PreparedInput, runner::RetouchOutput};
use lightcraft_develop::AiEdit;

pub(super) fn construct(prepared: PreparedInput, output: RetouchOutput) -> Result<AiEdit, String> {
    construct_with_layout(prepared, output, Layout::default())
}

#[derive(Clone, Copy, Default)]
pub(super) struct Layout<'a> {
    pub trim: bool,
    pub alpha: &'a [f32],
}

pub(super) fn construct_with_layout(prepared: PreparedInput, output: RetouchOutput, layout: Layout<'_>) -> Result<AiEdit, String> {
    let input = prepared.request;
    output.validate(&input)?;
    let coverage = output.coverage.as_ref().unwrap_or(&input.mask);
    let mut bounds = [0, 0, input.width, input.height];
    if layout.trim {
        bounds = [input.width, input.height, 0, 0];
        for (i, (&v, &protected)) in coverage.iter().zip(&input.protected).enumerate() {
            if v > 0 && !protected && layout.alpha.get(i).is_none_or(|v| *v > 0.) {
                let (x, y) = (i % input.width, i / input.width);
                bounds = [bounds[0].min(x), bounds[1].min(y), bounds[2].max(x + 1), bounds[3].max(y + 1)];
            }
        }
        if bounds[0] >= bounds[2] {
            return Err("The selected mask is empty.".into());
        }
    }
    let [x0, y0, x1, y1] = bounds;
    let mut pixels = Vec::with_capacity((x1 - x0) * (y1 - y0));
    for y in y0..y1 {
        for x in x0..x1 {
            let i = y * input.width + x;
            let mask = if input.protected[i] || layout.alpha.get(i) == Some(&0.) { 0 } else { coverage[i] };
            let rgb = if mask > 0 {
                output.rgb[i]
            } else if layout.trim {
                [0.; 3]
            } else {
                input.rgb[i]
            };
            let brush_alpha = if input.effects { 1. } else { layout.alpha.get(i).copied().unwrap_or(1.) };
            pixels.push([rgb[0], rgb[1], rgb[2], f32::from(mask) / 255. * brush_alpha]);
        }
    }
    let [l, t, r, b] = input.rect;
    let rect = if layout.trim {
        [
            l + (r - l) * x0 as f64 / input.width as f64,
            t + (b - t) * y0 as f64 / input.height as f64,
            l + (r - l) * x1 as f64 / input.width as f64,
            t + (b - t) * y1 as f64 / input.height as f64,
        ]
        .map(|v| v.clamp(0., 1.))
    } else {
        input.rect
    };
    AiEdit::from_pixels(
        AiEdit {
            id: 0,
            group: None,
            enabled: true,
            rect,
            support: if input.effects { None } else { prepared.support },
            width: (x1 - x0) as u32,
            height: (y1 - y0) as u32,
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
