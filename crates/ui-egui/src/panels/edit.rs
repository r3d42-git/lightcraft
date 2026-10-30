//! The Edit panel: histogram, Auto/B&W, profile, and the Light / Color / Effects / Detail / Optics
//! sections, built from the engine's control specs.

use egui::epaint::{Mesh, Vertex};
use egui::{Align2, Color32, Pos2, Rect, RichText, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::{ControlSpec, DevelopSettings, Section, Track, WbMode, controls};
use lightcraft_geom::Point;
use lightcraft_pipeline::{PARAM_AMOUNTS, ParametricCurve};
use lightcraft_raster::Histogram;
use serde_json::{Value, json};

use crate::LightcraftApp;
use crate::icons::{Icon, paint};
use crate::render::Slot;
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{BAND_COLORS, SliderOut, divider, flyout_row, hex, register, section_header, slider, text_button};

mod curve;
mod grading;
mod histogram;
mod mixer;
mod sliders;

use curve::curve_editor;
use grading::grading;
pub(crate) use grading::wheel_pointer_frame_end;
#[cfg(test)]
pub(crate) use grading::{WheelGrab, WheelLock, WheelMods, wheel_pick};
pub use histogram::histogram;
use histogram::soft_proofing;
use mixer::{mixer, point_color};
use sliders::{REL_TEMP, REL_TINT, hdr_row, k_to_rel, profile_menu, quick_develop, rel_to_k, reset_tip, section};
pub use sliders::{apply_slider_out, sub_title};
pub(crate) use sliders::{control, reload_preview_only};

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let d = app.session.develop_of(id).unwrap_or_default();
    // a raw shown from its embedded JPEG (preview only) gets the rendered-file white balance scale
    let raw = app.session.catalog.photo(id).is_some_and(|p| p.develops_raw() && !p.relative_wb());
    let mut d = d;
    if d.wb.mode == WbMode::AsShot && app.session.catalog.photo(id).is_some_and(|p| p.relative_wb()) {
        let wb = &mut std::sync::Arc::make_mut(&mut d).wb;
        wb.temp = 6500.0;
        wb.tint = 0.0;
    } else if raw {
        // the Kelvin sliders show the white balance in effect: As Shot reads the file's as-shot white (the stored
        // numbers can predate a decoder update, or the camera's matrices giving the file its Kelvin scale), a
        // preset its Kelvin value, and a value written on the old relative scale the Kelvin it renders as
        let info = app.session.source_info(id);
        let (temp, tint) = lightcraft_engine::pipeline::local::effective_wb(&info, &d);
        let wb = &mut std::sync::Arc::make_mut(&mut d).wb;
        wb.temp = temp;
        wb.tint = tint;
    }
    let preview_only = app.session.catalog.photo(id).and_then(|p| p.preview_only.clone());

    if app.ui.soft_proof {
        soft_proofing(app, ui, id);
    }
    // header: Edit + Auto / B&W / HDR
    let mut reset_clicked = false;
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 40.0), Sense::hover());
    ui.painter().text(pos2(hr.left() + 24.0, hr.bottom() - 10.0), Align2::LEFT_CENTER, crate::i18n::tr("Edit"), t.semibold(15.0), t.text);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 14 }).show(ui, |ui| {
        let tip = reset_tip(&app.ui.settings.keymap, ui.ctx().os() == egui::os::OperatingSystem::Mac);
        let clicks = header_buttons(ui, d.auto_tone_active(), crate::is_bw(&d), d.hdr.enabled, &tip);
        if clicks.auto {
            let _ = app.run("develop.auto", json!({}));
            app.toast(ui.ctx(), crate::i18n::tr("Auto settings applied"));
        }
        if clicks.bw {
            let _ = app.run("develop.treatment", json!({}));
        }
        if clicks.hdr {
            let _ = app.run("develop.hdr", json!({"enabled": !d.hdr.enabled}));
        }
        reset_clicked = clicks.reset;
    });
    if let Some(why) = &preview_only {
        let reload = egui::Frame::NONE
            .inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 12 })
            .show(ui, |ui| crate::widgets::preview_only_notice(ui, "edit", why))
            .inner;
        if reload {
            reload_preview_only(app, ui.ctx(), id);
        }
    }
    let n = app.session.selection.ids.len();
    if n > 1 && matches!(app.ui.view, crate::state::ViewMode::PhotoGrid | crate::state::ViewMode::SquareGrid) {
        quick_develop(app, ui, n);
    }
    if app.session.auto_sync && n > 1 {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(crate::i18n::tr_format!("Auto Sync: edits apply to {n} photos", n = n)).color(t.accent).size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if text_button(ui, "autoSyncOff", crate::i18n::tr("Turn Off"), false).clicked() {
                        let _ = app.run("develop.autoSync", json!({"on": false}));
                    }
                });
            });
        });
    }
    divider(ui);
    // profile row
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 14, bottom: 14 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Profile")).font(t.font(13.0)).color(t.text_dim));
            let name = app.session.profile_info(&d.profile.id).map(|(name, _)| name).unwrap_or("Color");
            // a long name is cut short before Browse Profiles: a wider row would widen the whole panel's
            // content, and the sliders below would run past its edge
            let max = ui.available_width() - 28.0 - ui.spacing().item_spacing.x;
            let r = crate::widgets::dropdown_within(ui, "profile", crate::i18n::profile_label(&d.profile.id, name), t.font(15.0), t.text_label, max);
            let below = r.rect.bottom();
            egui::Popup::menu(&r).show(|ui| profile_menu(app, ui, &d, below));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if crate::widgets::icon_button(
                    ui,
                    "profileBrowser",
                    Icon::ProfileGrid,
                    vec2(28.0, 28.0),
                    false,
                    true,
                    crate::i18n::tr("Browse Profiles"),
                )
                .clicked()
                {
                    let _ = app.run("panel.profiles", json!({}));
                }
            });
        });
        super::profiles::colour_source(app, ui, id);
    });
    if d.profile.id != "lc.color" {
        control(app, ui, &d, "profile.amount", true);
        ui.add_space(6.0);
    }
    // a raw carrying its own local tone mapping (Apple ProRAW's gain table map): off renders it as
    // Lightroom Classic does, on as the camera does
    if app.session.source_info(id).local_tone.is_some() {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
            let mut on = d.profile.camera_local_tone;
            let r = ui.checkbox(&mut on, crate::i18n::tr("Camera local tone mapping"));
            crate::widgets::register(ui, "check:profile.cameraLocalTone", r.rect);
            if r.on_hover_text(crate::i18n::tr("Apply the local tone mapping stored in the raw (Apple ProRAW), as the camera renders it")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"profile": {"camera_local_tone": on}}, "label": "Camera Local Tone Mapping"}));
            }
        });
    }
    divider(ui);

    section(app, ui, &d, "light", "Light", |app, ui, d| {
        hdr_row(app, ui, d);
        for c in ["light.exposure", "light.contrast", "light.highlights", "light.shadows", "light.whites", "light.blacks"] {
            control(app, ui, d, c, true);
        }
        ui.add_space(6.0);
        if d.hdr.enabled {
            let open = app.ui.flyout_open("sdrRendition");
            if flyout_row(ui, "sdrRendition", "SDR Rendition", Icon::Sliders, open).clicked() {
                app.ui.toggle_flyout("sdrRendition");
            }
            if open {
                for c in ["hdr.sdrBrightness", "hdr.sdrContrast", "hdr.sdrHighlights", "hdr.sdrShadows", "hdr.sdrWhites", "hdr.sdrClarity"] {
                    control(app, ui, d, c, true);
                }
                ui.add_space(6.0);
            }
        }
        let open = app.ui.flyout_open("curve");
        if flyout_row(ui, "curve", crate::i18n::tr("Curve"), Icon::Curve, open).clicked() {
            app.ui.toggle_flyout("curve");
        }
        if open {
            curve_editor(app, ui, id, d);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "color", "Color", |app, ui, d| {
        // White balance row
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 2 }).show(ui, |ui| {
            // a wider row widens the whole panel's content, so label and mode are cut short before the eyedropper, like
            // the profile row. The label gives way first (down to `LABEL_MIN`), so the mode, which says what is set,
            // stays whole where it can; both show their full text on hover. The row never wraps: a layout that changed
            // with the mode would move the menu under the pointer.
            const LABEL_MIN: f32 = 40.0;
            ui.horizontal(|ui| {
                let gap = ui.spacing().item_spacing.x;
                let title = crate::i18n::tr("White Balance");
                let mode = crate::i18n::tr(d.wb.mode.label());
                let mode_w = ui.painter().layout_no_wrap(mode.to_string(), t.font(14.0), t.text_label).size().x + 20.0;
                let label_max = (ui.available_width() - 28.0 - 2.0 * gap - mode_w).max(LABEL_MIN);
                ui.label(crate::widgets::one_line(ui.painter(), title, t.font(13.0), t.text_dim, label_max));
                let max = ui.available_width() - 28.0 - gap;
                let r = crate::widgets::dropdown_within(ui, "wbMode", mode, t.font(14.0), t.text_label, max);
                egui::Popup::menu(&r).show(|ui| {
                    for m in WbMode::ALL {
                        if m == WbMode::Custom {
                            continue;
                        }
                        if ui.selectable_label(d.wb.mode == m, crate::i18n::tr(m.label())).clicked() {
                            let mode = serde_json::to_value(m).unwrap_or_default();
                            let _ = app.run("develop.wb", json!({"mode": mode}));
                        }
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let active = app.ui.tool == "wbPicker";
                    if crate::widgets::icon_button(
                        ui,
                        "wbPicker",
                        Icon::Picker,
                        vec2(28.0, 28.0),
                        active,
                        true,
                        crate::i18n::tr("White Balance Selector (W)"),
                    )
                    .clicked()
                    {
                        if active {
                            app.ui.tool.clear();
                        } else if let Err(e) = app.run("tool.wbPicker", json!({})) {
                            // the same as W: from Compare, Survey or the grid it opens the photo in Detail
                            app.toast(ui.ctx(), e);
                        }
                    }
                });
            });
        });
        if raw {
            control(app, ui, d, "wb.temp", true);
            control(app, ui, d, "wb.tint", true);
        } else {
            let out = slider(ui, &REL_TEMP, k_to_rel(d.wb.temp), true, None);
            apply_slider_out(app, &REL_TEMP, out, |app, v| app.run("develop.set", json!({"control": "wb.temp", "value": rel_to_k(v)})));
            let out = slider(ui, &REL_TINT, d.wb.tint.clamp(-100.0, 100.0), true, None);
            apply_slider_out(app, &REL_TINT, out, |app, v| app.run("develop.set", json!({"control": "wb.tint", "value": v})));
        }
        control(app, ui, d, "color.vibrance", true);
        control(app, ui, d, "color.saturation", true);
        ui.add_space(6.0);
        let open = app.ui.flyout_open("mixer");
        if flyout_row(ui, "mixer", if crate::is_bw(d) { "B&W Mixer" } else { "Color Mixer" }, Icon::Radial, open).clicked() {
            app.ui.toggle_flyout("mixer");
        }
        if open {
            mixer(app, ui, d);
        }
        if !crate::is_bw(d) {
            let open = app.ui.flyout_open("pointColor");
            if flyout_row(ui, "pointColor", crate::i18n::tr("Point Color"), Icon::Picker, open).clicked() {
                app.ui.toggle_flyout("pointColor");
            }
            if open {
                point_color(app, ui, d);
            }
        }
        let open = app.ui.flyout_open("grading");
        if flyout_row(ui, "grading", crate::i18n::tr("Color Grading"), Icon::Presets, open).clicked() {
            app.ui.toggle_flyout("grading");
        }
        if open {
            grading(app, ui, d, id);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "effects", "Effects", |app, ui, d| {
        for c in ["effects.texture", "effects.clarity", "effects.dehaze"] {
            control(app, ui, d, c, true);
        }
        sub_title(ui, crate::i18n::tr("Vignette"));
        {
            use lightcraft_develop::VignetteStyle as V;
            let styles = [
                (V::HighlightPriority, "Highlight", "highlightPriority"),
                (V::ColorPriority, "Color", "colorPriority"),
                (V::PaintOverlay, "Paint", "paintOverlay"),
            ];
            let items: Vec<(&str, &str)> = styles.iter().map(|(_, l, k)| (*l, *k)).collect();
            let active = styles.iter().position(|(v, _, _)| *v == d.vignette.style);
            let mut chosen = None;
            egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 4 }).show(ui, |ui| {
                chosen = crate::widgets::segmented(ui, "vignetteStyle", &items, active, 3);
            });
            if let Some(i) = chosen {
                let _ = app.run(
                    "develop.merge",
                    serde_json::json!({"settings": {"vignette": {"style": serde_json::to_value(styles[i].0).unwrap_or_default()}}}),
                );
            }
        }
        for c in ["vignette.amount", "vignette.midpoint", "vignette.feather", "vignette.roundness", "vignette.highlights"] {
            let enabled = c == "vignette.amount" || d.vignette.amount != 0.0;
            control(app, ui, d, c, enabled);
        }
        sub_title(ui, crate::i18n::tr("Grain"));
        for c in ["grain.amount", "grain.size", "grain.roughness"] {
            control(app, ui, d, c, c == "grain.amount" || d.grain.amount != 0.0);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "detail", "Detail", |app, ui, d| {
        for c in ["detail.sharpenAmount", "detail.sharpenRadius", "detail.sharpenDetail", "detail.sharpenMasking"] {
            control(app, ui, d, c, c == "detail.sharpenAmount" || d.detail.sharpen_amount > 0.0);
        }
        // AI Denoise is its own tool, apart from the manual noise reduction sliders below it
        sub_title(ui, crate::i18n::tr("AI Denoise"));
        // AI Denoise: how much of the photo's cleaned picture to mix in (raw files only), and what that picture is doing
        let applicable = !matches!(app.session.denoise_photo_state(id), lightcraft_engine::denoise::PhotoState::NotApplicable);
        let on = d.enhance.denoise_enabled();
        super::denoise::detail_toggle(app, ui, id, on, applicable);
        super::denoise::detail_model(app, ui, id, applicable);
        control(app, ui, d, "enhance.denoise", applicable && on);
        super::denoise::detail_status(app, ui, id, if on { d.enhance.denoise } else { 0.0 });
        sub_title(ui, crate::i18n::tr("Noise Reduction"));
        for c in ["detail.nrLuminance", "detail.nrDetail", "detail.nrContrast", "detail.nrColor", "detail.nrColorDetail", "detail.nrColorSmoothness"]
        {
            control(app, ui, d, c, true);
        }
        ui.add_space(8.0);
    });
    section(app, ui, &d, "optics", "Optics", |app, ui, d| {
        let lens = app.session.catalog.photo(id).and_then(|p| p.embedded_lens.as_ref());
        let has_lens = lens.is_some();
        let has_warp = lens.is_some_and(|l| l.warp.is_some() || l.has_stages());
        let has_vignette = lens.is_some_and(|l| l.vignette.is_some());
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 4 }).show(ui, |ui| {
            let mut ca = d.optics.remove_ca;
            if ui.checkbox(&mut ca, crate::i18n::tr("Remove Chromatic Aberration")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"optics": {"remove_ca": ca}}, "label": "Remove CA"}));
            }
            let mut lp = d.optics.lens_profile;
            if ui.checkbox(&mut lp, crate::i18n::tr("Enable Lens Corrections")).changed() {
                let _ = app.run("develop.merge", json!({"settings": {"optics": {"lens_profile": lp}}, "label": "Lens Corrections"}));
            }
            if lp && !has_lens {
                ui.label(
                    egui::RichText::new(crate::i18n::tr("No supported lens correction available.")).size(11.0).color(Tokens::get(ui.ctx()).text_dim),
                );
            }
            if lp && has_lens && !has_vignette {
                ui.label(
                    egui::RichText::new(crate::i18n::tr("No supported vignetting correction available."))
                        .size(11.0)
                        .color(Tokens::get(ui.ctx()).text_dim),
                );
            }
        });
        if d.optics.lens_profile {
            control(app, ui, d, "optics.profileDistortion", has_warp);
            control(app, ui, d, "optics.profileVignetting", has_vignette);
        }
        sub_title(ui, crate::i18n::tr("Manual"));
        for c in ["optics.distortion", "optics.vignetting"] {
            control(app, ui, d, c, true);
        }
        control(app, ui, d, "optics.vignettingMidpoint", d.optics.vignetting != 0.0);
        ui.add_space(6.0);
        let open = app.ui.flyout_open("defringe");
        if flyout_row(ui, "defringe", crate::i18n::tr("Defringe"), Icon::Picker, open).clicked() {
            app.ui.toggle_flyout("defringe");
        }
        if open {
            control(app, ui, d, "optics.defringePurple", true);
            let on = d.optics.defringe_purple_amount > 0.0;
            control(app, ui, d, "optics.defringePurpleHueLo", on);
            control(app, ui, d, "optics.defringePurpleHueHi", on);
            control(app, ui, d, "optics.defringeGreen", true);
            let on = d.optics.defringe_green_amount > 0.0;
            control(app, ui, d, "optics.defringeGreenHueLo", on);
            control(app, ui, d, "optics.defringeGreenHueHi", on);
            sub_title(ui, crate::i18n::tr("Lateral Chromatic Aberration"));
            control(app, ui, d, "optics.caRed", true);
            control(app, ui, d, "optics.caBlue", true);
        }
        ui.add_space(8.0);
    });
    // Lightroom Classic's Calibration panel (the cloud app hides it): last, like there.
    section(app, ui, &d, "calibration", "Calibration", |app, ui, d| {
        sub_title(ui, crate::i18n::tr("Shadows"));
        control(app, ui, d, "calibration.shadowsTint", true);
        for (title, k) in [("Red Primary", "red"), ("Green Primary", "green"), ("Blue Primary", "blue")] {
            sub_title(ui, title);
            control(app, ui, d, &format!("calibration.{k}Hue"), true);
            control(app, ui, d, &format!("calibration.{k}Sat"), true);
        }
        ui.add_space(8.0);
    });
    ui.add_space(40.0);
    // A pending numeric edit commits on blur before the explicit Reset takes precedence.
    if reset_clicked {
        let _ = app.run("develop.reset", json!({}));
    }
}

/// Which of the Edit header's buttons were clicked.
#[derive(Default)]
struct HeaderClicks {
    auto: bool,
    bw: bool,
    hdr: bool,
    reset: bool,
}

/// The Edit header's buttons: Auto, B&W and HDR on the left, Reset on the right. A button that doesn't fit on the
/// row starts the next one (long translations in a narrow panel, such as German *Zurücksetzen* beside HDR), so none
/// covers another; Reset stays right-aligned on whichever row it lands.
fn header_buttons(ui: &mut egui::Ui, auto: bool, bw: bool, hdr: bool, reset_tip: &str) -> HeaderClicks {
    const GAP: f32 = 8.0;
    let mut clicks = HeaderClicks::default();
    let avail = ui.available_width();
    let left = [("auto", "Auto", auto), ("bw", "B&W", bw), ("hdr", "HDR", hdr)];
    // rows of `left` indices, filled greedily; `used` is the width taken on the current (last) row
    let mut rows: Vec<Vec<usize>> = Vec::new();
    let mut row: Vec<usize> = Vec::new();
    let mut used = 0.0;
    for (i, (_, label, _)) in left.iter().enumerate() {
        let w = crate::widgets::text_button_size(ui, label);
        if row.is_empty() {
            used = w;
        } else if used + GAP + w > avail {
            rows.push(std::mem::take(&mut row));
            used = w;
        } else {
            used += GAP + w;
        }
        row.push(i);
    }
    rows.push(row);
    let reset_on_last = used + GAP + crate::widgets::text_button_size(ui, "Reset") <= avail;
    let reset = |ui: &mut egui::Ui, clicks: &mut HeaderClicks| {
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            clicks.reset = text_button(ui, "reset", crate::i18n::tr("Reset"), false).on_hover_text(reset_tip).clicked();
        });
    };
    let last = rows.len() - 1;
    for (r, row) in rows.iter().enumerate() {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for &i in row {
                let (id, label, active) = left[i];
                let mut resp = crate::widgets::filled_text_button(ui, id, crate::i18n::tr(label), active);
                if id == "hdr" {
                    // HDR editing, next to B&W as Lightroom has it
                    resp = resp.on_hover_text(crate::i18n::tr("Edit in HDR: highlights above SDR white, for HDR exports and displays"));
                }
                let clicked = resp.clicked();
                match id {
                    "auto" => clicks.auto = clicked,
                    "bw" => clicks.bw = clicked,
                    _ => clicks.hdr = clicked,
                }
            }
            if r == last && reset_on_last {
                reset(ui, &mut clicks);
            }
        });
    }
    if !reset_on_last {
        ui.horizontal(|ui| reset(ui, &mut clicks));
    }
    clicks
}
