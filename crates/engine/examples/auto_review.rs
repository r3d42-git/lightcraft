//! Local read-only Auto comparison: auto_review OUTPUT_DIR INPUT_COPY...
//! Decodes only explicitly supplied files; creates no catalog or sidecars.
use std::path::Path;
use std::time::Instant;

use lightcraft_develop::DevelopSettings;
use lightcraft_engine::export::{ExportFormat, ExportOptions, encode_image};
use lightcraft_pipeline::{RenderRequest, auto::auto_tone, render};
use serde_json::json;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let out = args.next().ok_or("expected output directory and input copies")?;
    std::fs::create_dir_all(&out)?;
    for file in args {
        let name = Path::new(&file).file_stem().ok_or("missing file stem")?.to_string_lossy();
        let (src, info) = lightcraft_engine::files::load_vec(std::fs::read(&file)?, 1600)?;
        let before = DevelopSettings::default();
        // Same 512 px input size as the command; assess at a larger independent size.
        let proxy = lightcraft_raster::resample::fit(&src, 512, 512, lightcraft_raster::resample::Filter::Box);
        let start = Instant::now();
        let a = auto_tone(&proxy, &info, &before);
        let elapsed = start.elapsed().as_secs_f64() * 1000.0;
        let mut after = before.clone();
        a.apply_to(&mut after);
        let repeated = auto_tone(&proxy, &info, &after);
        let mut old = before.clone();
        old.light.exposure = lightcraft_pipeline::auto::scene_log_mean(&proxy, &info, &old)
            .map_or(0.0, |m| ((-0.58 - 0.41 * m).clamp(-2.0, 3.0) * 100.0).round() / 100.0);
        old.light.contrast = 68.0;
        old.light.highlights = -66.0;
        old.light.shadows = 52.0;
        old.color.vibrance = 30.0;
        let req = RenderRequest::fit(1600, 1600);
        let base = render(&src, &info, &before, &req);
        let mut records = Vec::new();
        for (label, settings) in [("before", &before), ("old-auto", &old), ("adaptive", &after)] {
            let r = render(&src, &info, settings, &req);
            let mut new_black = [0usize; 3];
            let mut new_white = [0usize; 3];
            for (b, p) in base.image.data.iter().zip(&r.image.data) {
                for (i, (&b, &p)) in b.iter().zip(p).take(3).enumerate() {
                    if let (Some(black), Some(white)) = (new_black.get_mut(i), new_white.get_mut(i)) {
                        *black += usize::from(p == 0 && b != 0);
                        *white += usize::from(p == 255 && b != 255);
                    }
                }
            }
            let opts = ExportOptions { format: ExportFormat::Png, ..Default::default() };
            std::fs::write(Path::new(&out).join(format!("{name}-{label}.png")), encode_image(&r.image, &opts)?)?;
            records.push(json!({"variant": label, "clipping": r.histogram.clipping(), "new_black_channels": new_black, "new_white_channels": new_white, "pixels": r.image.width*r.image.height}));
        }
        println!("{}", json!({"input": name, "auto": a, "ms": elapsed, "repeat_identical": a == repeated, "renders": records}));
    }
    Ok(())
}
