use super::super::{
    LightingDrafts, patch,
    runner::{RetouchIdentity, RetouchOutput, RetouchProvenance},
};
use super::*;
use lightcraft_geom::Point;
use lightcraft_raster::Rgb32f;
use std::sync::Arc;

fn source(width: usize, height: usize) -> (Arc<Rgb32f>, crate::media::SourceRef) {
    let image = Arc::new(Rgb32f::from_fn(width, height, |x, y| [0.1, x as f32 / width as f32, y as f32 / height as f32]));
    let source = crate::media::SourceRef::Loaded(Box::new(crate::media::DecodedSource::new(image.clone(), None)));
    (image, source)
}

fn stamp(input: PreparedInput, layout: patch::Layout<'_>) -> Result<LightingDrafts, String> {
    let req = &input.request;
    let output = RetouchOutput {
        width: req.width,
        height: req.height,
        rgb: vec![[0.8; 3]; req.rgb.len()],
        coverage: req.effects.then(|| vec![255; req.mask.len()]),
        provenance: RetouchProvenance {
            model: "test".into(),
            revision: "test".into(),
            identity: RetouchIdentity { model: "fixture".into(), runtime: "fixture".into(), adapter: "lama" },
        },
    };
    let raw = patch::construct_with_layout(input, output, layout)?;
    Ok(LightingDrafts { matched: Ok(raw.clone()), raw })
}

fn spot(xy: &[(f64, f64)], radius: f64) -> Spot {
    Spot { points: xy.iter().map(|&(x, y)| Point::new(x, y)).collect(), size: radius, ..Default::default() }
}

#[test]
fn nearby_disconnected_parts_share_context_but_far_objects_get_source_native_crops() {
    let (image, src) = source(2048, 128);
    let points = spot(&[(0.1, 0.5), (0.14, 0.5), (0.85, 0.5)], 0.005);
    let mut calls = Vec::new();
    let result = run_local(
        src,
        Default::default(),
        Default::default(),
        points.clone(),
        None,
        false,
        &[],
        false,
        false,
        0,
        &AtomicBool::new(false),
        |input, layout| {
            let r = &input.request;
            assert_eq!(r.width, 512);
            assert_eq!(r.height, 128);
            let left = (r.rect[0] * 2048.).round() as usize;
            assert_eq!(r.rgb[5 * r.width + 1], image.data[5 * 2048 + left + 1]);
            calls.push((r.rect, r.mask.iter().filter(|v| **v == 255).count()));
            stamp(input, layout)
        },
    )
    .unwrap();
    assert_eq!(result.len(), 2);
    assert_eq!(calls.len(), 2);
    assert!(calls[0].1 > calls[1].1);
    assert!(calls[0].0[2] < calls[1].0[0]);
    let mut d = DevelopSettings::default();
    for (i, v) in result.into_iter().enumerate() {
        let mut edit = v.raw;
        edit.id = i as u32 + 1;
        d.ai_edits.push(edit);
    }
    let rendered = lightcraft_pipeline::ai_edits::source_with_edits(&image, &d);
    assert_eq!(rendered.get(246, 64), image.get(246, 64)); // gap between the two nearby components
    for x in [205, 287, 1741] {
        assert_eq!(rendered.get(x, 64), [0.8; 3]);
    }
}

#[test]
fn cable_uses_previous_fill_and_persists_each_authorized_pixel_once() {
    let (image, src) = source(2400, 80);
    let path = spot(&[(0.04, 0.35), (0.55, 0.62), (0.95, 0.48)], 0.003);
    let mut calls = 0;
    let result = run_local(
        src,
        Default::default(),
        Default::default(),
        path.clone(),
        None,
        false,
        &[],
        false,
        true,
        0,
        &AtomicBool::new(false),
        |input, layout| {
            if calls < 2 {
                assert!(input.request.mask.iter().zip(layout.alpha).any(|(&m, &a)| m != 0 && a == 0.), "missing forward inference overlap");
            }
            let prior = input.request.rgb.iter().zip(&input.request.mask).filter(|(p, m)| p[0] > 0.79 && **m == 0).count();
            if calls > 0 {
                assert!(prior > 100, "missing prior cable fill");
            }
            calls += 1;
            stamp(input, layout)
        },
    )
    .unwrap();
    assert_eq!(calls, 3);
    let geom = lightcraft_pipeline::frame_for(&image, &Default::default(), &Default::default(), false);
    let support = lightcraft_pipeline::ai_edits::brush_support(&geom, &path, true);
    let support = lightcraft_pipeline::ai_edits::BrushSupport::new(&support, image.width, image.height);
    let mut d = DevelopSettings::default();
    let mut covered = vec![0u8; image.data.len()];
    for (i, v) in result.into_iter().enumerate() {
        let mut e = v.raw;
        e.id = i as u32 + 1;
        let x = (e.rect[0] * image.width as f64).round() as usize;
        let y = (e.rect[1] * image.height as f64).round() as usize;
        assert_eq!(((e.rect[2] - e.rect[0]) * image.width as f64).round() as u32, e.width);
        for (i, p) in e.decode_pixels().unwrap().into_iter().enumerate() {
            if p[3] > 0. {
                covered[(y + i / e.width as usize) * image.width + x + i % e.width as usize] += 1;
            }
        }
        d.ai_edits.push(e);
    }
    let after = lightcraft_pipeline::ai_edits::source_with_edits(&image, &d);
    for (i, p) in after.data.iter().enumerate() {
        let point = [((i % image.width) as f64 + 0.5) / image.width as f64, ((i / image.width) as f64 + 0.5) / image.height as f64];
        let a = support.alpha(point, image.width, image.height);
        assert_eq!(covered[i], u8::from(a > 0.));
        assert!((p[0] - (0.1 * (1. - a) + 0.8 * a)).abs() < 1e-6);
    }
}

#[test]
fn scaled_contexts_preserve_subtracted_manual_source_pixels_and_geometry() {
    let (image, src) = source(4000, 256);
    let stroke = spot(&[(0.10, 0.5), (0.36, 0.5)], 0.040);
    let other = spot(&[(0.85, 0.5)], 0.008);
    let subtract = spot(&[(0.25, 0.49)], 0.004);
    let refinements = vec![(stroke.clone(), false), (other.clone(), false), (subtract.clone(), true)];
    let d = DevelopSettings::default();
    let seg = lightcraft_develop::SegMask::from_logits(1, &[8.]);
    let mut count = 0;
    let result = run_local(
        src,
        Default::default(),
        d.clone(),
        Spot::default(),
        Some(seg),
        true,
        &refinements,
        false,
        true,
        0,
        &AtomicBool::new(false),
        |input, layout| {
            if count == 0 {
                assert!((input.request.rect[2] - input.request.rect[0]) * 4000. > 1024.);
            }
            count += 1;
            stamp(input, layout)
        },
    )
    .unwrap();
    assert_eq!(count, 2);
    let frame = lightcraft_pipeline::frame_for(&image, &Default::default(), &d, false);
    let supports: Vec<_> = refinements.iter().map(|(s, _)| lightcraft_pipeline::ai_edits::brush_support(&frame, s, true)).collect();
    let brushes: Vec<_> = supports.iter().map(|s| lightcraft_pipeline::ai_edits::BrushSupport::new(s, image.width, image.height)).collect();
    let mut d = d;
    for (i, v) in result.into_iter().enumerate() {
        let mut e = v.raw;
        e.id = i as u32 + 1;
        d.ai_edits.push(e);
    }
    let after = lightcraft_pipeline::ai_edits::source_with_edits(&image, &d);
    assert_eq!(after.get(700, 128), [0.8; 3]);
    assert_eq!(after.get(3400, 128), [0.8; 3]);
    for (i, p) in after.data.iter().enumerate() {
        let point = [((i % image.width) as f64 + 0.5) / image.width as f64, ((i / image.width) as f64 + 0.5) / image.height as f64];
        if brushes[2].alpha(point, image.width, image.height) > 0. || brushes[..2].iter().all(|b| b.alpha(point, image.width, image.height) == 0.) {
            assert_eq!(*p, image.data[i]);
        }
    }
}

#[test]
fn very_long_cable_shares_a_scaled_lattice_across_available_slots() {
    let (_, src) = source(10000, 32);
    let path = spot(&[(0.01, 0.5), (0.99, 0.5)], 0.001);
    let mut supports = vec![];
    let drafts =
        run_local(src, Default::default(), Default::default(), path, None, false, &[], false, true, 0, &AtomicBool::new(false), |input, layout| {
            let r = &input.request;
            assert!(r.width <= 1024);
            let step = (r.rect[2] - r.rect[0]) * 10000. / r.width as f64;
            assert!(step > 1. && step < 2.);
            supports.push((r.rect, step));
            stamp(input, layout)
        })
        .unwrap();
    assert_eq!(drafts.len(), 8);
    for pair in supports.windows(2) {
        assert!(pair[0].0[2] > pair[1].0[0]);
        assert!((pair[0].1 - pair[1].1).abs() < 1e-9);
    }
}

#[test]
fn distant_wires_keep_individual_axes_and_parallel_bundles_use_local_windows() {
    let first = spot(&[(0.10, 0.04), (0.19, 0.70)], 0.007);
    let second = spot(&[(0.85, 0.07), (0.80, 0.78)], 0.007);
    let plan = regions::plan([5184., 3456.], None, &[(&first, true), (&second, true)], 8).unwrap();
    assert_eq!(plan.windows.iter().map(|w| w.group).max(), Some(1));
    assert!(plan.windows.iter().all(|w| w.cable && w.bounds[3] - w.bounds[1] < 900.));
    let wires: Vec<_> = (0..3).map(|i| spot(&[(0.59 + i as f64 * 0.012, 0.), (0.57 + i as f64 * 0.012, 0.59)], 0.0058)).collect();
    let paths: Vec<_> = wires.iter().map(|w| (w, true)).collect();
    let plan = regions::plan([3456., 5184.], None, &paths, 8).unwrap();
    assert!(plan.windows.len() >= 4);
    assert!(plan.windows.iter().all(|w| w.cable && w.group == 0));
}

#[test]
fn erased_tile_with_only_forward_overlap_skips_inference_and_retains_later_tiles() {
    let (image, src) = source(2400, 80);
    let path = spot(&[(0.04, 0.5), (0.95, 0.5)], 0.003);
    let plan = regions::plan([2400., 80.], None, &[(&path, true)], 8).unwrap();
    assert_eq!(plan.windows.len(), 3);
    let cut = plan.windows[0].bounds[2] / 2400.;
    let eraser = spot(&[(0., 0.5), (cut, 0.5)], 0.012);
    let seg = lightcraft_develop::SegMask::from_logits(1, &[8.]);
    let mut calls = 0;
    let drafts = run_local(
        src,
        Default::default(),
        Default::default(),
        Spot::default(),
        Some(seg),
        true,
        &[(path, false), (eraser, true)],
        false,
        true,
        0,
        &AtomicBool::new(false),
        |input, layout| {
            calls += 1;
            stamp(input, layout)
        },
    )
    .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(drafts.len(), 2);
    let mut d = DevelopSettings::default();
    for (i, draft) in drafts.into_iter().enumerate() {
        let mut e = draft.raw;
        e.id = i as u32 + 1;
        d.ai_edits.push(e);
    }
    let after = lightcraft_pipeline::ai_edits::source_with_edits(&image, &d);
    assert_eq!(after.get(1800, 40), [0.8; 3]);
    for x in 0..(cut * 2400.) as usize {
        assert_eq!(after.get(x, 40), image.get(x, 40));
    }
}
