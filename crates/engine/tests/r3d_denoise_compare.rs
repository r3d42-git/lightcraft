//! Opt-in CPU/Metal comparison on explicitly supplied RAW test copies.
//! Output is white-balanced camera RGB with gamma 2.2, not a final colour-managed export.
#![cfg(feature = "denoise")]
use lightcraft_denoise::bayer::Layout;
use lightcraft_denoise::run::{Control, Params, denoise_bayer};
use lightcraft_raster::Rgb32f;
use std::path::Path;
use std::time::Instant;

fn png(path: &Path, panels: &[&Rgb32f], wb: [f32; 3]) {
    use lightcraft_codecs::encode::{EncodeImage, EncodeMeta, Samples, encode_png};
    let (w, h) = (panels[0].width, panels[0].height);
    let mut data = Vec::with_capacity(w * h * panels.len() * 3);
    for y in 0..h {
        for p in panels {
            for x in 0..w {
                for (c, gain) in wb.iter().enumerate() {
                    data.push((p.data[y * w + x][c] * gain).clamp(0.0, 1.0).powf(1.0 / 2.2).mul_add(255.0, 0.5) as u8);
                }
            }
        }
    }
    let image = EncodeImage::new((w * panels.len()) as u32, h as u32, 3, Samples::U8(&data));
    std::fs::write(path, encode_png(&image, &EncodeMeta::default()).unwrap()).unwrap();
}

#[test]
#[ignore = "requires R3D_DENOISE_INPUT and R3D_DENOISE_OUT; reads authorized test copies only"]
fn authorized_raw_cpu_metal_comparison() {
    let input = std::env::var("R3D_DENOISE_INPUT").unwrap();
    let out = std::env::var("R3D_DENOISE_OUT").unwrap();
    let out = Path::new(&out);
    std::fs::create_dir_all(out).unwrap();
    let mut records = Vec::new();
    let files: Vec<_> = std::env::var("R3D_DENOISE_FILES").unwrap().split(',').map(str::to_string).collect();
    for name in files {
        let path = Path::new(&input).join(&name);
        let before = std::fs::read(&path).unwrap();
        let raw = lightcraft_raw::decode(&before).unwrap();
        let wb = raw.wb_multipliers.unwrap_or([1.0; 3]);
        let n = raw.normalized().unwrap();
        let cfa = n.cfa.clone().unwrap();
        assert!(cfa.is_bayer());
        let layout = Layout::from_cell([cfa.color_at(0, 0), cfa.color_at(1, 0), cfa.color_at(0, 1), cfa.color_at(1, 1)]).unwrap();
        let (w, h) = (768, 512);
        let (ox, oy) = (((n.width - w) / 2) & !1, ((n.height - h) / 2) & !1);
        let data: Vec<f32> = (0..w * h).map(|i| n.data[(oy + i / w) * n.width + ox + i % w]).collect();
        let crop = lightcraft_raw::Normalized { width: w, height: h, cpp: 1, data, cfa: Some(cfa) };
        let ahd = lightcraft_raw::demosaic::demosaic(&crop, lightcraft_raw::demosaic::Method::Ahd);
        let noise = lightcraft_denoise::builtin::estimate_noise(&n.data, n.width, n.height);
        let cpu = lightcraft_denoise::builtin::Builtin::with_noise(noise);
        let gpu = lightcraft_gpu::bm3d::BuiltinRunner::new(
            std::sync::Arc::new(lightcraft_gpu::bm3d::Bm3dGpu::new(&cpu.bm3d_params().unwrap(), lightcraft_denoise::builtin::TILE as usize).unwrap()),
            cpu,
        )
        .unwrap();
        let adapter = gpu.adapter();
        assert!(adapter.contains("Metal"), "expected actual Metal adapter: {adapter}");
        let (tile, overlap, gain) =
            (lightcraft_denoise::builtin::TILE, lightcraft_denoise::builtin::OVERLAP, lightcraft_denoise::manifest::Gain::None);
        let params = Params { tile: tile as usize, overlap: overlap as usize, gain, clip: Some(0.99), parallel: 1 };
        let start = Instant::now();
        let cpu_out = denoise_bayer(&crop.data, w, h, layout, &cpu, &params, &Control::default()).unwrap();
        let cpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        let gpu_out = denoise_bayer(&crop.data, w, h, layout, &gpu, &params, &Control::default()).unwrap();
        let gpu_ms = start.elapsed().as_secs_f64() * 1000.0;
        let mut worst = 0f32;
        let mut sum = 0f64;
        let scale = cpu_out.data.iter().flatten().fold(1e-9f32, |m, v| m.max(v.abs()));
        for (a, b) in cpu_out.data.iter().flatten().zip(gpu_out.data.iter().flatten()) {
            assert!(a.is_finite() && b.is_finite());
            worst = worst.max((a - b).abs());
            sum += f64::from((a - b).abs());
        }
        let mean = sum / (w * h * 3) as f64;
        assert!(mean / f64::from(scale) < 1e-3, "CPU/GPU disagreement: {name}, {mean}, scale {scale}");
        let mut half = ahd.clone();
        assert!(lightcraft_denoise::product::blend(&mut half, &cpu_out, 0.5));
        png(&out.join(format!("{name}-AHD-amount50-CPU-Metal.png")), &[&ahd, &half, &cpu_out, &gpu_out], wb);
        let cache = out.join(format!("{name}.lcdn"));
        lightcraft_denoise::product::write(&cache, &cpu_out, "r3d-comparison").unwrap();
        assert!(lightcraft_denoise::product::is_current(&cache, "r3d-comparison"));
        assert!(!lightcraft_denoise::product::is_current(&cache, "changed-model"));
        assert_eq!(std::fs::read(&path).unwrap(), before, "test copy changed");
        let record = serde_json::json!({"file": name, "adapter": adapter, "crop": [ox,oy,w,h], "cpu_ms":cpu_ms, "gpu_ms":gpu_ms, "worst_abs":worst, "mean_abs":mean,"scale":scale,"camera_rgb_only":true});
        eprintln!("{record}");
        records.push(record);
        std::fs::write(out.join("metrics.json"), serde_json::to_vec_pretty(&records).unwrap()).unwrap();
    }
}
