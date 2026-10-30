//! Real headless slider input; the diagnostic must follow live modifier/button ownership.
use crate::{LightcraftApp, Services, headless::Headless, panels::detail::slider_mask_preview, state::RightPanel};
use egui::{Event, Modifiers, PointerButton, Pos2};
use lightcraft_pipeline::Overlay;
use serde_json::json;
use std::time::Duration;

fn modifiers(alt: bool) -> Modifiers {
    Modifiers { alt, ..Modifiers::NONE }
}

#[test]
fn sharpen_mask_headless_capture_keeps_the_real_drag_and_history_open() {
    let (mut h, _, history) = start_drag(true);
    h.settle(Duration::from_secs(20));
    assert!(h.app.headless_screenshot(&h.view.ctx, true).is_some());
    assert!(h.app.session.interaction.is_some(), "capturing must not release the real mouse button");
    assert_eq!(h.app.ui.dragging_control.as_deref(), Some("detail.sharpenMasking"));
    assert_eq!(h.app.session.undo.len(), history, "capturing must not commit the slider gesture");
    h.step();
    assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), Some(Overlay::SharpenMask));
}

#[test]
fn sharpen_mask_logic_uses_refreshed_host_focus_not_previous_ui_focus() {
    let (mut h, _, _) = start_drag(true);
    // run_logic refreshes raw.focused while InputState::focused still belongs to the last UI pass.
    h.view.ctx.input_mut(|i| {
        i.focused = false;
        i.raw.focused = true;
    });
    h.app.logic(&h.view.ctx);
    assert_eq!(h.app.ui.dragging_control.as_deref(), Some("detail.sharpenMasking"));
    h.view.ctx.input_mut(|i| {
        i.focused = true;
        i.raw.focused = false;
    });
    h.app.logic(&h.view.ctx);
    assert!(h.app.ui.dragging_control.is_none());
}

fn start_drag(alt: bool) -> (Headless, Pos2, usize) {
    let app = LightcraftApp::new(lightcraft_engine::Session::with_demo(), Services { png: None, ..Default::default() });
    let mut h = Headless::new(app, [1400.0, 1000.0], 1.0);
    h.app.renderer.keep_pixels = true;
    let t = Duration::from_secs(20);
    let id = h.app.session.visible_cloned()[0].0;
    assert_eq!(h.request("engine.execute", json!({"command": "library.select", "params": {"ids": [id]}}), t)["ok"], true);
    assert_eq!(h.request("ui.set", json!({"view": "detail", "right": "edit"}), t)["ok"], true);
    assert_eq!(
        h.request("engine.execute", json!({"command": "develop.set", "params": {"control": "detail.sharpenAmount", "value": 80.0}}), t)["ok"],
        true
    );
    h.app.ui.open_sections = vec!["detail".to_string()];
    h.step();
    h.step();
    let track = h.app.widgets.iter().find(|(id, _)| id == "slider:detail.sharpenMasking").unwrap().1;
    let from = egui::pos2(track.left() + track.width() * 0.15, track.center().y);
    let to = from + egui::vec2(track.width() * 0.35, 0.0);
    let history = h.app.session.undo.len();
    h.events.push(Event::ModifiersChanged(modifiers(alt)));
    h.events.push(Event::PointerMoved(from));
    h.events.push(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: modifiers(alt) });
    h.step();
    h.events.push(Event::PointerMoved(to));
    h.step();
    h.step();
    assert_eq!(h.app.ui.dragging_control.as_deref(), Some("detail.sharpenMasking"));
    assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), alt.then_some(Overlay::SharpenMask));
    assert_eq!(h.app.session.undo.len(), history, "diagnostic adds no history");
    (h, to, history)
}

#[test]
fn sharpen_mask_preview_tracks_modifier_and_button_release_without_extra_history() {
    let (mut h, to, history) = start_drag(true);
    h.events.push(Event::ModifiersChanged(Modifiers::NONE));
    h.step();
    assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), None, "modifier release returns to photo while dragging");
    h.events.push(Event::ModifiersChanged(modifiers(true)));
    h.step();
    assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), Some(Overlay::SharpenMask));
    h.events.push(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: modifiers(true) });
    h.step();
    h.step();
    assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), None, "button release returns despite held Alt");
    assert!(h.app.session.interaction.is_none());
    assert_eq!(h.app.session.undo.len(), history + 1, "one ordinary slider undo step");
}

#[test]
fn sharpen_mask_preview_rejects_closed_section_panel_changed_photo_cancel_and_focus_loss() {
    for cancellation in 0..9 {
        let (mut h, to, _) = start_drag(true);
        match cancellation {
            0 => h.app.ui.open_sections.clear(),
            1 => h.app.ui.right = RightPanel::Masking,
            2 => h.app.ui.dialog = Some(crate::state::Dialog::About),
            3 => h.app.notices.push("test blocking notice".to_string()),
            4 => {
                let other = h.app.session.visible_cloned()[1];
                h.app.run("library.select", json!({"ids": [other.0]})).unwrap();
            }
            5 => {} // A faithful unfocused host frame is supplied below.
            6 => h.events.push(Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: modifiers(true) }),
            7 => h.events.push(Event::PointerGone),
            _ => {
                h.app.run("develop.endInteraction", json!({})).unwrap();
            }
        }
        if cancellation == 5 {
            let mut raw = crate::headless::HeadlessView::raw_input(h.size, 1.0, h.time(), vec![Event::WindowFocused(false)]);
            raw.focused = false;
            raw.viewports.entry(egui::ViewportId::ROOT).or_default().focused = Some(false);
            h.app.raw_input_hook(&h.view.ctx, &mut raw);
            let app = &mut h.app;
            h.view.run(raw, |ui| {
                app.logic(ui.ctx());
                app.ui(ui);
            });
            h.time += 1.0 / 60.0;
        } else {
            h.step();
        }
        assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), None, "cancellation {cancellation}");
        h.app.ui.dialog = None;
        h.app.notices.clear();
        h.app.ui.right = RightPanel::Edit;
        h.app.ui.open_sections = vec!["detail".to_string()];
        h.events.push(Event::WindowFocused(true));
        h.events.push(Event::PointerMoved(to));
        h.events.push(Event::Key { key: egui::Key::Escape, physical_key: None, pressed: false, repeat: false, modifiers: modifiers(true) });
        h.step();
        assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), None, "gesture {cancellation} stays retired");
    }
}

/// Removing the show() wiring must fail this pixel-level end-to-end check.
#[test]
fn sharpen_mask_preview_can_start_mid_drag_and_replaces_actual_loupe_pixels() {
    let (mut h, to, _) = start_drag(false);
    let settle = Duration::from_secs(60);
    assert!(h.settle(settle));
    let colored = |h: &Headless| {
        let texture = h.app.renderer.textures.get(&crate::render::Slot::Main).expect("main loupe texture");
        let pixels = &texture.pixels.as_ref().expect("headless CPU pixel copy").pixels;
        pixels.iter().filter(|p| p.r().abs_diff(p.g()) > 3 || p.g().abs_diff(p.b()) > 3).count()
    };
    assert!(colored(&h) > 100, "ordinary photo has colour");
    h.events.push(Event::ModifiersChanged(modifiers(true)));
    assert!(h.settle(settle));
    assert_eq!(slider_mask_preview(&h.app, &h.view.ctx), Some(Overlay::SharpenMask));
    assert_eq!(colored(&h), 0, "the actual loupe texture is the grayscale weight");
    let texture = h.app.renderer.textures.get(&crate::render::Slot::Main).unwrap();
    let pixels = &texture.pixels.as_ref().unwrap().pixels;
    assert!(pixels.iter().any(|p| p.r() < 10));
    h.events.push(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers: modifiers(true) });
    assert!(h.settle(settle));
    assert!(colored(&h) > 100, "release immediately retires diagnostic and restores normal pixels");
}
