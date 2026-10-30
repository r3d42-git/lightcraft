//! A middle-button drag (the mouse wheel pressed) pans the zoomed photo in Detail whatever tool is
//! active: the healing brush doesn't paint meanwhile, and the hand cursor shows.

use std::time::Duration;

use egui::{Event, PointerButton, Pos2, vec2};
use serde_json::json;

use crate::headless::Headless;
use crate::{LightcraftApp, Services};

const T: Duration = Duration::from_secs(20);

fn spots(h: &Headless) -> usize {
    let id = h.app.session.active().expect("active photo");
    h.app.session.develop_of(id).unwrap_or_default().spots.len()
}

/// Press `button` at `from`, move to `to` over a few frames and release; the cursor seen mid-drag.
fn drag(h: &mut Headless, button: PointerButton, from: Pos2, to: Pos2) -> egui::CursorIcon {
    let modifiers = egui::Modifiers::NONE;
    h.app.synthetic.push(Event::PointerMoved(from));
    h.app.synthetic.push(Event::PointerButton { pos: from, button, pressed: true, modifiers });
    h.step();
    for i in 1..=6 {
        h.app.synthetic.push(Event::PointerMoved(from + (to - from) * (i as f32 / 6.0)));
        h.step();
    }
    let cursor = h.last_cursor;
    h.app.synthetic.push(Event::PointerButton { pos: to, button, pressed: false, modifiers });
    h.step();
    h.step();
    cursor
}

#[test]
fn a_middle_drag_pans_with_the_healing_brush_and_adds_no_spot() {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.view.ctx.data_mut(|d| d.insert_temp(egui::Id::unique("ai-remove-brush-mode"), false));
    for (method, params) in [
        ("ui.set", json!({"view": "detail"})),
        ("engine.execute", json!({"command": "panel.remove"})),
        ("engine.execute", json!({"command": "view.zoom100"})),
    ] {
        let r = h.request(method, params, T);
        assert_eq!(r["ok"], true, "{r}");
        h.step();
    }
    h.step();
    let at = h.app.image_rect.expect("the photo is shown").center();
    let pan = h.app.ui.pan;
    let cursor = drag(&mut h, PointerButton::Middle, at, at + vec2(-150.0, -100.0));
    assert_eq!(cursor, egui::CursorIcon::Grabbing, "the hand shows while panning");
    assert_ne!(h.app.ui.pan, pan, "the middle drag pans");
    assert_eq!(spots(&h), 0, "the healing brush doesn't paint during a middle drag");
    // the left button still paints a spot, and doesn't pan
    let pan = h.app.ui.pan;
    let at = h.app.image_rect.expect("the photo is shown").center();
    drag(&mut h, PointerButton::Primary, at, at + vec2(40.0, 0.0));
    assert_eq!(spots(&h), 1, "a left drag paints a spot");
    assert_eq!(h.app.ui.pan, pan);
}

fn setup(ai: bool) -> Headless {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1200.0, 800.0], 1.0);
    h.view.ctx.data_mut(|d| d.insert_temp(egui::Id::unique("ai-remove-brush-mode"), ai));
    for (method, params) in [
        ("ui.set", json!({"view":"detail"})),
        ("engine.execute", json!({"command":"panel.remove"})),
        ("engine.execute", json!({"command":"view.zoom100"})),
    ] {
        assert_eq!(h.request(method, params, T)["ok"], true);
        h.step();
    }
    h.step();
    h
}

fn space(h: &mut Headless, pressed: bool) {
    h.app.synthetic.push(Event::Key { key: egui::Key::Space, physical_key: None, pressed, repeat: false, modifiers: egui::Modifiers::NONE });
    h.step();
}

#[test]
fn space_hand_pans_and_restores_classic_and_ai_remove_without_edits() {
    for ai in [false, true] {
        let mut h = setup(ai);
        if ai {
            let r = h.request("engine.execute", json!({"command":"aiRemove.select", "params":{"points":[[0.3,0.3],[0.4,0.4]],"size":0.02}}), T);
            assert_eq!(r["ok"], true, "{r}");
        }
        let selection = h.app.session.ai_remove.selection.clone();
        let undo = h.app.session.undo.len();
        let at = h.app.canvas_rect.expect("canvas").center();
        h.app.synthetic.push(Event::PointerMoved(at));
        h.step();
        let zoom = h.app.ui.zoom;
        space(&mut h, true);
        assert_eq!(h.app.ui.zoom, zoom, "Space down must not toggle zoom");
        assert_eq!(h.last_cursor, egui::CursorIcon::Grab);
        let pan = h.app.ui.pan;
        let cursor = drag(&mut h, PointerButton::Primary, at, at + vec2(-120.0, -70.0));
        assert_eq!(cursor, egui::CursorIcon::Grabbing);
        assert_ne!(h.app.ui.pan, pan);
        space(&mut h, false);
        assert_eq!(spots(&h), 0);
        assert_eq!(h.app.session.undo.len(), undo, "navigation adds no edit history");
        assert_eq!(h.app.session.ai_remove.selection, selection);
        let at = h.app.canvas_rect.expect("canvas").center();
        drag(&mut h, PointerButton::Primary, at, at + vec2(25.0, 5.0));
        if ai {
            assert_ne!(h.app.session.ai_remove.selection, selection, "AI brush resumes");
        } else {
            assert_eq!(spots(&h), 1, "classic brush resumes");
        }
    }
}

#[test]
fn releasing_space_mid_drag_does_not_resume_a_brush_or_commit_an_interrupted_stroke() {
    for ai in [false, true] {
        let mut h = setup(ai);
        let at = h.app.canvas_rect.expect("canvas").center();
        h.app.synthetic.push(Event::PointerMoved(at));
        h.step();
        h.app.synthetic.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        h.step();
        h.app.synthetic.push(Event::PointerMoved(at + vec2(25.0, 0.0)));
        h.step();
        assert!(h.app.gesture.is_some(), "brush started");
        space(&mut h, true);
        h.app.synthetic.push(Event::PointerMoved(at + vec2(-50.0, -20.0)));
        h.step();
        space(&mut h, false);
        let end = at + vec2(-100.0, -40.0);
        h.app.synthetic.push(Event::PointerMoved(end));
        h.step();
        h.app.synthetic.push(Event::PointerButton { pos: end, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        h.step();
        h.step();
        assert_eq!(spots(&h), 0);
        assert!(h.app.session.ai_remove.selection.is_none());
        assert!(h.app.gesture.is_none());
        assert!(h.app.session.interaction.is_none());
    }
}

#[test]
fn space_point_wheel_zooms_and_plain_point_wheel_still_pans() {
    let mut h = setup(true);
    let at = h.app.canvas_rect.expect("canvas").center() + vec2(40.0, 20.0);
    h.app.synthetic.push(Event::PointerMoved(at));
    h.step();
    space(&mut h, true);
    let zoom = h.app.ui.zoom;
    let before = h.app.image_rect.expect("image");
    let anchor = (at - before.min) / before.size();
    h.app.synthetic.push(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        delta: vec2(0.0, 70.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    assert_ne!(h.app.ui.zoom, zoom);
    let after = h.app.image_rect.expect("image");
    assert!(((at - after.min) / after.size() - anchor).length() < 0.001, "zoom stays anchored at the pointer");
    space(&mut h, false);
    for _ in 0..120 {
        h.step();
    }
    let zoom = h.app.ui.zoom;
    let pan = h.app.ui.pan;
    h.app.synthetic.push(Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point,
        phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Unknown,
        delta: vec2(-70.0, -30.0),
        modifiers: egui::Modifiers::NONE,
    });
    h.step();
    assert_eq!(h.app.ui.zoom, zoom);
    assert_ne!(h.app.ui.pan, pan);
    assert!(h.app.session.ai_remove.selection.is_none());
}

#[test]
fn space_keeps_browsing_zoom_and_text_field_ownership() {
    let mut h = setup(false);
    h.app.ui.right = crate::state::RightPanel::None;
    h.app.ui.tool.clear();
    h.step();
    let zoom = h.app.ui.zoom;
    space(&mut h, true);
    assert_ne!(h.app.ui.zoom, zoom, "Space still toggles zoom while browsing");
    space(&mut h, false);
    h.app.ui.right = crate::state::RightPanel::Remove;
    h.app.ui.tool = "remove".into();
    h.step();
    let r = h.request("ui.clickWidget", json!({"id":"field:search"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    assert!(h.view.ctx.egui_wants_keyboard_input());
    let at = h.app.canvas_rect.expect("canvas").center();
    h.app.synthetic.push(Event::PointerMoved(at));
    h.step();
    let zoom = h.app.ui.zoom;
    space(&mut h, true);
    assert_eq!(h.app.ui.zoom, zoom);
    assert_ne!(h.last_cursor, egui::CursorIcon::Grab, "Space in a text field does not activate the hand");
    space(&mut h, false);
}

#[test]
fn space_hand_works_after_a_panel_checkbox_kept_keyboard_focus() {
    let mut h = setup(false);
    let r = h.request("ui.clickWidget", json!({"id":"aiRemoveBrushMode"}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let mode = h.view.ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("ai-remove-brush-mode")));
    let at = h.app.canvas_rect.expect("canvas").center();
    h.app.synthetic.push(Event::PointerMoved(at));
    h.step();
    space(&mut h, true);
    assert_eq!(h.last_cursor, egui::CursorIcon::Grab);
    assert_eq!(
        h.view.ctx.data(|d| d.get_temp::<bool>(egui::Id::unique("ai-remove-brush-mode"))),
        mode,
        "Space over canvas does not toggle the focused checkbox"
    );
    let pan = h.app.ui.pan;
    drag(&mut h, PointerButton::Primary, at, at + vec2(-100.0, -50.0));
    assert_ne!(h.app.ui.pan, pan);
    space(&mut h, false);
}

#[test]
fn taking_the_hand_rolls_back_an_in_progress_spot_move_without_history() {
    let mut h = setup(false);
    h.app.ui.tool_overlay = crate::state::ToolOverlay::Always;
    let r = h.request("engine.execute", json!({"command":"spot.add","params":{"points":[[0.5,0.5]],"size":0.02,"mode":"heal"}}), T);
    assert_eq!(r["ok"], true, "{r}");
    h.step();
    let id = h.app.session.active().expect("photo");
    let original = h.app.session.develop_of(id).expect("develop");
    let undo = h.app.session.undo.len();
    let at = h.app.canvas_rect.expect("canvas").center();
    h.app.synthetic.push(Event::PointerMoved(at));
    h.step();
    h.app.synthetic.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    h.step();
    h.app.synthetic.push(Event::PointerMoved(at + vec2(40.0, 20.0)));
    h.step();
    assert!(h.app.session.interaction.is_some(), "moving an existing spot starts an interaction");
    assert_ne!(h.app.session.develop_of(id).expect("develop").spots, original.spots);
    space(&mut h, true);
    assert_eq!(h.app.session.develop_of(id).expect("develop").spots, original.spots);
    assert!(h.app.session.interaction.is_none());
    space(&mut h, false);
    h.app.synthetic.push(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
    h.step();
    assert_eq!(h.app.session.undo.len(), undo);
}
