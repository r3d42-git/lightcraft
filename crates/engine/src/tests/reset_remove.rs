//! Develop reset restores the look while keeping Remove work, including stored raster patches.

use std::sync::Arc;

use lightcraft_catalog::{MemStore, Op, Photo, PhotoId, Source};
use lightcraft_develop::ai_edit::{AiBrushSupport, AiSupportGeometry};
use lightcraft_develop::{AiEdit, DevelopSettings, EmbeddedLens, EmbeddedWarp, ProcessVersion, Spot, SpotMode};
use lightcraft_geom::{Homography, Orientation, Point};
use serde_json::json;

use crate::Session;
use crate::library::LibraryStores;

fn lens() -> EmbeddedLens {
    EmbeddedLens {
        warp: Some(EmbeddedWarp { planes: [[1.0, 0.12, -0.03, 0.0, 0.0, 0.0]; 3], center: Point::new(0.48, 0.51), radius: 0.7 }),
        ..Default::default()
    }
}

fn patch(id: u32) -> AiEdit {
    AiEdit::from_pixels(
        AiEdit {
            id,
            enabled: id != 2,
            rect: [0.25, 0.25, 0.75, 0.75],
            support: Some(AiBrushSupport {
                geometry: Some(AiSupportGeometry {
                    orientation: Orientation::default(),
                    source_size: [32, 24],
                    perspective: Homography::IDENTITY,
                    distortion: 0.0,
                    lens: Some(lens()),
                    lens_distortion: 1.0,
                }),
                continuous: true,
                points: vec![[0.4, 0.5], [0.6, 0.5]],
                radius: 0.1,
                feather: 0.25,
            }),
            width: 1,
            height: 1,
            pixels: String::new(),
            model: "stored-test-patch".into(),
            model_version: "1".into(),
            seed: u64::from(id),
            stages: Vec::new(),
        },
        &[[0.123, 0.25, 1.75, 0.625]],
    )
    .unwrap()
}

fn add_photo(s: &mut Session, id: PhotoId, import_look: bool, removed: bool) -> DevelopSettings {
    let name = format!("reset-{}.ARW", id.0);
    let mut p = Photo::new(id, Source::File { path: name.clone() }, &name, "ARW", 32, 24, "");
    p.as_shot_wb = Some((6500.0, 0.0));
    p.embedded_lens = Some(lens());
    let mut expected = p.camera_defaults();
    if import_look {
        expected.light.contrast = 23.0;
        // Imported settings must not replace the photo's current Remove edits, even if empty.
        expected.spots = vec![Spot::default()];
        expected.ai_edits = vec![patch(7)];
        p.import_look = Some(Arc::new(expected.clone()));
    }
    let mut edited = expected.clone();
    edited.light.exposure = 1.5;
    edited.process = ProcessVersion(0);
    edited.mark_auto_tone();
    edited.crop.geometry.rect = lightcraft_geom::Rect::new(0.1, 0.15, 0.8, 0.7);
    edited.orientation = Orientation::Rotate90;
    edited.geometry.vertical = 12.0;
    edited.masks = vec![lightcraft_develop::Mask::default()];
    edited.red_eye = vec![lightcraft_develop::RedEye::default()];
    edited.lens_blur.enabled = true;
    edited.lens_blur.amount = 35.0;
    edited.enhance.raw_details = true;
    edited.enhance.super_resolution = true;
    edited.enhance.denoise = 40.0;
    edited.disabled_sections = vec!["light".into(), "geometry".into(), "lensBlur".into()];
    edited.spots = if removed {
        [SpotMode::Heal, SpotMode::Clone, SpotMode::Fill, SpotMode::Remove]
            .into_iter()
            .map(|mode| Spot {
                mode,
                points: vec![Point::new(0.4, 0.5), Point::new(0.6, 0.5)],
                source_offset: Some(Point::new(0.1, -0.1)),
                size: 0.03,
                feather: 35.0,
                opacity: 80.0,
            })
            .collect()
    } else {
        vec![]
    };
    edited.ai_edits = if removed { vec![patch(1), patch(2)] } else { vec![] };
    expected.process = edited.process;
    expected.crop = edited.crop;
    expected.orientation = edited.orientation;
    expected.geometry = edited.geometry.clone();
    expected.masks = edited.masks.clone();
    expected.red_eye = edited.red_eye.clone();
    expected.lens_blur = edited.lens_blur;
    expected.enhance.raw_details = true;
    expected.enhance.super_resolution = true;
    expected.disabled_sections = vec!["geometry".into(), "lensBlur".into()];
    expected.spots = edited.spots.clone();
    expected.ai_edits = edited.ai_edits.clone();
    p.develop = Arc::new(edited);
    s.commit("Add test photo", Op::AddPhoto { photo: Box::new(p) }).unwrap();
    expected
}

#[test]
fn panel_reset_preserves_other_tools_and_undo_redo() {
    let mut s = Session::with_demo();
    let ids = [PhotoId(900), PhotoId(901), PhotoId(902)];
    let expected = [add_photo(&mut s, ids[0], false, true), add_photo(&mut s, ids[1], true, true), add_photo(&mut s, ids[2], true, false)];
    s.execute("library.select", &json!({"ids": [900, 901, 902], "active": 900})).unwrap();
    s.execute("spot.select", &json!({"index": 1})).unwrap();
    let before: Vec<_> = ids.iter().map(|id| s.develop_of(*id).unwrap()).collect();
    let undo_steps = s.undo.len();
    s.execute("develop.resetPanel", &json!({})).unwrap();
    assert_eq!(s.undo.len(), undo_steps + 1, "one batch undo step");
    for (id, expected) in ids.iter().zip(&expected) {
        assert_eq!(s.develop_of(*id).unwrap().as_ref(), expected, "photo {id:?}");
        assert!(expected.optics.lens_profile, "embedded lens correction remains active");
        let frame = lightcraft_pipeline::geometry::Frame::with_lens(32, 24, expected, false, Some(&lens()));
        assert!(frame.warp.as_ref().unwrap().lens.is_some(), "actual resolved lens mapping remains active");
    }
    assert_eq!(s.active_spot, Some(1));
    s.execute("edit.undo", &json!({})).unwrap();
    for (id, before) in ids.iter().zip(&before) {
        assert_eq!(s.develop_of(*id).unwrap(), *before);
    }
    s.execute("edit.redo", &json!({})).unwrap();
    for (id, expected) in ids.iter().zip(&expected) {
        assert_eq!(s.develop_of(*id).unwrap().as_ref(), expected);
    }
    assert_eq!(s.active_spot, Some(1));
}

#[test]
fn reset_remove_patches_survive_journal_reopen_without_models() {
    let store = MemStore::new();
    let open = || {
        let mut s = Session::new();
        s.open_library_in(
            LibraryStores { dir: "reset-remove".into(), catalog: Box::new(store.clone()), files: Box::new(MemStore::new()), on_disk: false },
            false,
        )
        .unwrap();
        s
    };
    let mut s = open();
    let id = PhotoId(900);
    let expected = add_photo(&mut s, id, false, true);
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    s.execute("develop.resetPanel", &json!({"ids": [id.0]})).unwrap();
    drop(s);
    let s = open();
    let restored = s.develop_of(id).unwrap();
    assert_eq!(restored.as_ref(), &expected);
    for (actual, expected) in restored.ai_edits.iter().zip(&expected.ai_edits) {
        assert_eq!(actual.decode_pixels().unwrap(), expected.decode_pixels().unwrap());
    }
}

#[test]
fn global_reset_still_resets_all_tools_and_updates_the_process() {
    let mut s = Session::with_demo();
    let id = PhotoId(900);
    add_photo(&mut s, id, false, true);
    s.execute("library.select", &json!({"ids": [id.0], "active": id.0})).unwrap();
    let before = s.develop_of(id).unwrap();
    let mut expected = s.catalog.photo(id).unwrap().camera_defaults();
    expected.process = ProcessVersion::LATEST;
    s.execute("develop.reset", &json!({})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().as_ref(), &expected);
    s.execute("edit.undo", &json!({})).unwrap();
    assert_eq!(s.develop_of(id).unwrap(), before);
    s.execute("edit.redo", &json!({})).unwrap();
    assert_eq!(s.develop_of(id).unwrap().as_ref(), &expected);
}
