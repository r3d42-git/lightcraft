use super::curve::set_tat;
use super::sliders::{control, sub_title};
use super::{BAND_COLORS, Color32, DevelopSettings, Icon, LightcraftApp, Sense, Stroke, Tokens, hex, json, paint, register, text_button, vec2};

// ------------------------------------------------------------------------------ colour mixer

pub(super) fn mixer(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let bands = lightcraft_develop::MIXER_BANDS;
    let sel = bands.iter().position(|b| *b == app.ui.mixer_mode).unwrap_or(0);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 }).show(ui, |ui| {
        // the gaps narrow before the swatches would run into the margin (the narrowest panel)
        let n = bands.len() as f32;
        let gap = ((ui.available_width() - 22.0 * n) / (n - 1.0).max(1.0)).clamp(0.0, 5.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = gap;
            for (i, b) in bands.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, crate::i18n::tr(b));
                register(ui, format!("mixerBand:{b}"), r);
                ui.painter().circle_filled(r.center(), 9.0, hex(BAND_COLORS[i]));
                if i == sel {
                    ui.painter().circle_stroke(r.center(), 10.5, Stroke::new(1.5, t.text));
                    ui.painter().circle_filled(r.center(), 2.0, Color32::WHITE);
                }
                if resp.clicked() {
                    app.ui.mixer_mode = b.to_string();
                }
            }
        });
    });
    // targeted adjustment: pick the attribute, then drag on the photo. A wider row would widen the whole panel's
    // content, so the gaps narrow first (English keeps one row in the default panel), then a button that still doesn't
    // fit starts the next row, aligned after the target icon, as in the Edit header.
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 2 }).show(ui, |ui| {
        const GAP: f32 = 6.0;
        const MIN_GAP: f32 = 2.0;
        const ICON: f32 = 20.0;
        let items: &[(&str, &str)] =
            if crate::is_bw(d) { &[("lum", "B&W Mix")] } else { &[("hue", "Hue"), ("sat", "Saturation"), ("lum", "Luminance")] };
        let widths: Vec<f32> = items.iter().map(|(_, label)| crate::widgets::text_button_size(ui, label)).collect();
        let full = ui.available_width();
        let buttons: f32 = widths.iter().sum();
        let gaps = items.len() as f32;
        let gap = ((full - ICON - buttons) / gaps).clamp(MIN_GAP, GAP);
        // (half a point of slack: the gap that exactly fills the row must not wrap on rounding)
        let gap = if ICON + buttons + gaps * gap <= full + 0.5 { gap } else { GAP };
        let avail = full - ICON - gap;
        let mut rows: Vec<Vec<(&str, &str)>> = Vec::new();
        let mut row = Vec::new();
        let mut used = 0.0;
        for (&(k, label), &w) in items.iter().zip(&widths) {
            if !row.is_empty() && used + gap + w > avail + 0.5 {
                rows.push(std::mem::take(&mut row));
            }
            used = if row.is_empty() { w } else { used + gap + w };
            row.push((k, label));
        }
        rows.push(row);
        for (r, row) in rows.iter().enumerate() {
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = gap;
                if r == 0 {
                    let (icon, _) = ui.allocate_exact_size(vec2(ICON, ICON), Sense::hover());
                    paint(ui.painter(), icon, Icon::Target, t.icon);
                } else {
                    ui.add_space(ICON + gap);
                }
                for &(k, label) in row {
                    let tool = format!("tat:{k}");
                    let active = app.ui.tool == tool;
                    if text_button(ui, &format!("tatMixer-{k}"), crate::i18n::tr(label), active)
                        .on_hover_text(crate::i18n::tr("Targeted adjustment: drag up/down on the photo to adjust the colours there"))
                        .clicked()
                    {
                        set_tat(app, ui.ctx(), &tool, active);
                    }
                }
            });
        }
    });
    let b = bands[sel];
    if crate::is_bw(d) {
        ui.horizontal(|ui| {
            ui.add_space(24.0);
            if crate::widgets::text_button(ui, "bwAuto", crate::i18n::tr("Auto"), false)
                .on_hover_text(crate::i18n::tr("Set the mix from the photo's colours"))
                .clicked()
            {
                let _ = app.run("develop.autoBwMix", json!({}));
            }
        });
        control(app, ui, d, &format!("bw.{b}"), true);
    } else {
        for k in ["hue", "sat", "lum"] {
            control(app, ui, d, &format!("mixer.{b}.{k}"), true);
        }
    }
}

// ------------------------------------------------------------------------------ point color

/// Display colour of an OkLCh sample (lightness, chroma, hue in degrees).
fn oklch_color(l: f64, c: f64, h_deg: f64) -> Color32 {
    use lightcraft_color::perceptual::{lch_to_lab, oklab_to_2020};
    let lin = oklab_to_2020(lch_to_lab([l as f32, c as f32, (h_deg as f32).to_radians()]));
    let s = lightcraft_color::REC2020.to_space(&lightcraft_color::SRGB).apply_f32(lin);
    let e = s.map(|v| (lightcraft_color::transfer::linear_to_srgb(v.clamp(0.0, 1.0)) * 255.0).round() as u8);
    Color32::from_rgb(e[0], e[1], e[2])
}

/// Point Color: swatches of the samples (+ the eyedropper), the selected sample's shifts and range,
/// and "Visualize range".
pub(super) fn point_color(app: &mut LightcraftApp, ui: &mut egui::Ui, d: &DevelopSettings) {
    let t = Tokens::get(ui.ctx());
    let n = d.point_colors.len();
    if app.ui.point_color >= n && n > 0 {
        app.ui.point_color = n - 1;
    }
    let sel = app.ui.point_color;
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 8, bottom: 4 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 5.0;
            let active = app.ui.tool == "pointColor";
            let full = n >= lightcraft_develop::MAX_POINT_COLORS;
            if crate::widgets::icon_button(
                ui,
                "pointColorPicker",
                Icon::Picker,
                vec2(26.0, 26.0),
                active,
                !full,
                crate::i18n::tr("Sample a colour on the photo"),
            )
            .clicked()
            {
                app.ui.tool = if active { String::new() } else { "pointColor".into() };
            }
            for (i, p) in d.point_colors.iter().enumerate() {
                let (r, resp) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::click());
                crate::widgets::announce(&resp, format!("{} {}", crate::i18n::tr("Point Color"), i + 1));
                register(ui, format!("pointColor:{i}"), r);
                ui.painter().rect_filled(r.shrink(2.0), 3.0, oklch_color(p.lum, p.chroma, p.hue));
                if i == sel {
                    ui.painter().rect_stroke(r, 4.0, Stroke::new(1.5, t.text), egui::StrokeKind::Inside);
                }
                if resp.clicked() {
                    app.ui.point_color = i;
                }
            }
        });
    });
    if n == 0 {
        egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 2, bottom: 8 }).show(ui, |ui| {
            ui.label(egui::RichText::new(crate::i18n::tr("Pick a colour on the photo with the eyedropper.")).size(12.0).color(t.text_dim));
        });
        return;
    }
    for k in ["hueShift", "satShift", "lumShift", "variance"] {
        control(app, ui, d, &format!("pointColor.{sel}.{k}"), true);
    }
    sub_title(ui, crate::i18n::tr("Range"));
    for k in ["range", "hueRange", "satRange", "lumRange"] {
        control(app, ui, d, &format!("pointColor.{sel}.{k}"), true);
    }
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 4, bottom: 8 }).show(ui, |ui| {
        ui.horizontal(|ui| {
            let mut v = app.ui.point_color_visualize;
            if ui.checkbox(&mut v, crate::i18n::tr("Visualize range")).changed() {
                app.ui.point_color_visualize = v;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if text_button(ui, "pointColorDelete", crate::i18n::tr("Delete"), false).clicked() {
                    let _ = app.run("pointColor.delete", json!({"index": sel}));
                }
            });
        });
    });
}
