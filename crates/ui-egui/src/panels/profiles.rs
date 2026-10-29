//! The profile browser (the profile menu's "Browse…", or the grid button next to it): every
//! profile as a live thumbnail of the current photo, grouped (Favorites first), with a favourite
//! star, the applied profile's amount, a temporary loupe preview while hovering and apply on click.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use lightcraft_catalog::PhotoId;
use lightcraft_develop::DevelopSettings;
use lightcraft_engine::presets::{PROFILES, profile_groups};
use serde_json::json;

use super::presets::cover_uv;
use crate::icons::{Icon, paint};
use crate::state::RightPanel;
use crate::theme::Tokens;
use crate::widgets::{divider, icon_button, one_line, register};
use crate::{HoverPreview, LightcraftApp};

/// Long edge of the variant thumbnails (px).
const THUMB_EDGE: usize = 256;
/// Thumbnail columns.
const COLUMNS: usize = 2;

pub fn show(app: &mut LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    let t = Tokens::get(ui.ctx());
    let d = (*app.session.develop_of(id).unwrap_or_default()).clone();
    // header: back + title
    let (hr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
    let mut back = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(hr.min + vec2(12.0, 9.0), vec2(28.0, 28.0))));
    if icon_button(&mut back, "profilesBack", Icon::ChevronLeft, vec2(28.0, 28.0), false, true, "Back to Edit").clicked() {
        app.ui.right = RightPanel::Edit;
    }
    ui.painter().text(pos2(hr.left() + 46.0, hr.center().y + 2.0), Align2::LEFT_CENTER, crate::i18n::tr("Profiles"), t.semibold(15.0), t.text);
    divider(ui);
    // the applied profile and its amount
    let (cr, _) = ui.allocate_exact_size(vec2(ui.available_width(), 34.0), Sense::hover());
    let name = app.session.profile_info(&d.profile.id).map(|(name, _)| name).unwrap_or("Color");
    ui.painter().text(pos2(cr.left() + 24.0, cr.center().y), Align2::LEFT_CENTER, crate::i18n::tr("Profile"), t.font(13.0), t.text_dim);
    // a long name is cut short (with …) at the panel's edge
    let g = one_line(ui.painter(), crate::i18n::profile_label(&d.profile.id, name), t.font(14.0), t.text_label, cr.width() - 84.0 - 24.0);
    ui.painter().galley(pos2(cr.left() + 84.0, cr.center().y - g.size().y / 2.0), g, t.text_label);
    egui::Frame::NONE.inner_margin(egui::Margin { left: 24, right: 22, top: 0, bottom: 10 }).show(ui, |ui| {
        colour_source(app, ui, id);
    });
    if d.profile.id != "lc.color" {
        super::edit::control(app, ui, &d, "profile.amount", true);
        ui.add_space(6.0);
    }
    divider(ui);
    // (title, cell tag, imported LUT folder, profiles)
    let mut sections: Vec<(&str, &str, bool, Vec<(String, String, String)>)> = Vec::new();
    let favs: Vec<(String, String, String)> = app
        .session
        .profile_favorites
        .iter()
        .filter_map(|f| app.session.profile_info(f).map(|(name, group)| (f.clone(), name.to_string(), group.to_string())))
        .collect();
    if !favs.is_empty() {
        sections.push(("Favorites", "fav", false, favs));
    }
    for g in profile_groups() {
        sections.push((
            g,
            "",
            false,
            PROFILES.iter().filter(|p| p.group == g).map(|p| (p.id.to_string(), p.name.to_string(), p.group.to_string())).collect(),
        ));
    }
    // imported LUT profile groups
    let mut lut_groups: Vec<String> = app.session.lut_profiles.iter().map(|p| p.group.clone()).collect();
    lut_groups.sort_unstable();
    lut_groups.dedup();
    for g in &lut_groups {
        let profiles = app.session.lut_profiles.iter().filter(|p| &p.group == g).map(|p| (p.id.clone(), p.name.clone(), p.group.clone())).collect();
        sections.push((g.as_str(), "", true, profiles));
    }
    for (title, tag, imported, list) in sections {
        // an imported folder named like a built-in group still opens and closes on its own
        let open_id = egui::Id::unique(("profile-group", imported, title));
        let open: bool = ui.data(|m| m.get_temp(open_id)).unwrap_or(true);
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::click());
        crate::widgets::announce(&resp, title.to_string());
        register(ui, if imported { format!("profileGroup:lut:{title}") } else { format!("profileGroup:{title}") }, r);
        paint(
            ui.painter(),
            Rect::from_center_size(pos2(r.left() + 22.0, r.center().y), vec2(16.0, 16.0)),
            if open { Icon::ChevronDown } else { Icon::ChevronRight },
            t.text_label,
        );
        ui.painter().text(
            pos2(r.left() + 36.0, r.center().y),
            Align2::LEFT_CENTER,
            crate::i18n::builtin_label(title, !imported),
            t.semibold(13.0),
            t.text_label,
        );
        ui.painter().text(pos2(r.right() - 22.0, r.center().y), Align2::RIGHT_CENTER, list.len().to_string(), t.font(12.0), t.text_dim);
        if resp.clicked() {
            ui.data_mut(|m| m.insert_temp(open_id, !open));
        }
        if !open {
            continue;
        }
        let gap = 8.0;
        let cw = ((ui.available_width() - 44.0 - gap * (COLUMNS as f32 - 1.0)) / COLUMNS as f32).max(40.0);
        let th = (cw * 0.75).round();
        for row in list.chunks(COLUMNS) {
            let (rr, _) = ui.allocate_exact_size(vec2(ui.available_width(), th + 30.0), Sense::hover());
            for (i, p) in row.iter().enumerate() {
                let cell = Rect::from_min_size(pos2(rr.left() + 22.0 + i as f32 * (cw + gap), rr.top()), vec2(cw, th + 24.0));
                cell_ui(app, ui, &d, id, tag, p, cell, th);
            }
        }
        ui.add_space(6.0);
    }
    ui.add_space(30.0);
    let last_hover_id = egui::Id::unique("last-profile-hover");
    let pointer_in_profiles = ui.ctx().input(|i| i.pointer.hover_pos()).is_some_and(|pos| ui.max_rect().contains(pos));
    if pointer_in_profiles {
        if app.hover_preview.is_none()
            && let Some((saved_id, saved_profile_id)) = ui.data(|m| m.get_temp::<(PhotoId, String)>(last_hover_id))
            && saved_id == id
            && let Some((p_name, _)) = app.session.profile_info(&saved_profile_id)
        {
            let s = with_profile_id(&d, &saved_profile_id);
            app.hover_preview = Some(HoverPreview {
                label: crate::i18n::tr_format!("Profile: {}", crate::i18n::profile_label(&saved_profile_id, p_name)),
                settings: s,
            });
        }
    } else {
        ui.data_mut(|m| m.remove::<(PhotoId, String)>(last_hover_id));
    }
}

/// `d` with profile `profile_id` (at 100 % unless it is the applied one).
fn with_profile_id(d: &DevelopSettings, profile_id: &str) -> DevelopSettings {
    let mut s = d.clone();
    if s.profile.id != profile_id {
        s.profile.id = profile_id.to_string();
        s.profile.amount = 100.0;
    }
    s
}

#[allow(clippy::too_many_arguments)]
fn cell_ui(
    app: &mut LightcraftApp,
    ui: &mut egui::Ui,
    d: &DevelopSettings,
    id: PhotoId,
    tag: &str,
    p: &(String, String, String),
    cell: Rect,
    th: f32,
) {
    let (p_id, p_name, p_group) = (p.0.as_str(), p.1.as_str(), p.2.as_str());
    let t = Tokens::get(ui.ctx());
    let key = if tag.is_empty() { format!("profileCell:{p_id}") } else { format!("profileCell:{tag}:{p_id}") };
    let resp = ui.interact(cell, egui::Id::unique(&key), Sense::click());
    crate::widgets::announce(&resp, p_name.to_string());
    register(ui, key, cell);
    let thumb = Rect::from_min_size(cell.min, vec2(cell.width(), th));
    let s = with_profile_id(d, p_id);
    let painter = ui.painter();
    painter.rect_filled(thumb, 3.0, t.canvas);
    if let Some(job) = app.session.variant_job(id, &s, THUMB_EDGE)
        && let Some(tex) = app.renderer.variant(job)
    {
        painter.image(tex.tex.id(), thumb, cover_uv(thumb, tex.size), Color32::WHITE);
    }
    let applied = d.profile.id == p_id;
    if applied {
        painter.rect_stroke(thumb.expand(1.0), 4.0, Stroke::new(2.0, t.pick), egui::StrokeKind::Outside);
    } else if resp.hovered() {
        painter.rect_stroke(thumb, 3.0, Stroke::new(1.0, t.text_dim), egui::StrokeKind::Outside);
    }
    let name_color = if applied { t.text } else { t.text_label };
    // cut short (with …) to the cell, so it never runs into its neighbour; the full name on hover
    let g = one_line(painter, crate::i18n::profile_label(p_id, p_name), t.font(12.0), name_color, cell.width() - 2.0);
    painter.galley(pos2(cell.left() + 2.0, thumb.bottom() + 12.0 - g.size().y / 2.0), g, name_color);
    // favourite star (always shown when set, on hover otherwise)
    let fav = app.session.profile_favorites.iter().any(|f| f == p_id);
    let star = Rect::from_center_size(pos2(thumb.right() - 13.0, thumb.top() + 13.0), vec2(20.0, 20.0));
    register(ui, format!("profileStar:{}{}", if tag.is_empty() { "" } else { "fav:" }, p_id), star);
    let over_star = resp.hover_pos().is_some_and(|q| star.contains(q));
    if fav || resp.hovered() {
        painter.circle_filled(star.center(), 10.0, Color32::from_black_alpha(110));
        crate::rating::paint_star(painter, star.shrink(4.0), fav, if fav || over_star { t.star } else { Color32::WHITE });
    }
    if resp.hovered() && !applied && !over_star {
        app.hover_preview =
            Some(HoverPreview { label: crate::i18n::tr_format!("Profile: {}", crate::i18n::profile_label(p_id, p_name)), settings: s.clone() });
        ui.data_mut(|m| m.insert_temp(egui::Id::unique("last-profile-hover"), (id, p_id.to_string())));
    } else if resp.hovered() && applied {
        // resting on the applied profile shows it (no preview), not the last other profile hovered
        ui.data_mut(|m| m.remove::<(PhotoId, String)>(egui::Id::unique("last-profile-hover")));
    }
    let resp = resp.on_hover_text(format!(
        "{} ({})",
        crate::i18n::profile_label(p_id, p_name),
        crate::i18n::builtin_label(p_group, lightcraft_engine::presets::profile(p_id).is_some())
    ));
    if resp.clicked() {
        if resp.interact_pointer_pos().is_some_and(|q| star.contains(q)) {
            let _ = app.run("profile.favorite", json!({"id": p_id}));
        } else if !applied {
            let _ = app.run("develop.profile", json!({"id": p_id, "amount": 100}));
            ui.data_mut(|m| m.remove::<(PhotoId, String)>(egui::Id::unique("last-profile-hover")));
        }
    }
}

/// Read from the selected photo's decoded source, never from the previously displayed preview.
pub(super) fn colour_source(app: &LightcraftApp, ui: &mut egui::Ui, id: PhotoId) {
    if !app.session.catalog.photo(id).is_some_and(|p| p.develops_raw()) {
        return;
    }
    let info = app.session.source_info(id);
    let text = colour_source_text(info.colour_source.as_ref());
    let t = Tokens::get(ui.ctx());
    let response = ui.add(egui::Label::new(egui::RichText::new(text).font(t.font(12.0)).color(t.text_dim)).wrap());
    register(ui, "label:cameraColourSource", response.rect);
}

fn colour_source_text(source: Option<&lightcraft_pipeline::ColourSource>) -> String {
    use lightcraft_pipeline::ColourBasis;
    let Some(source) = source else { return crate::i18n::tr("Camera colour: not available until the original is decoded").to_string() };
    let mut label = match &source.basis {
        ColourBasis::FileMatrices => crate::i18n::tr("Camera colour: raw's own matrices").to_string(),
        ColourBasis::CameraProfile { model, local } => {
            if *local {
                crate::i18n::tr("Camera colour: {model} profile (local)").replace("{model}", model)
            } else {
                crate::i18n::tr("Camera colour: {model} profile (built in)").replace("{model}", model)
            }
        }
        ColourBasis::CameraJpeg => crate::i18n::tr("Camera colour: fitted to the camera JPEG").to_string(),
        ColourBasis::PartialCameraJpeg => crate::i18n::tr("Camera colour: partial camera JPEG fit").to_string(),
        ColourBasis::Spectral => crate::i18n::tr("Camera colour: spectral sensitivities (no accepted colour fit)").to_string(),
        ColourBasis::Neutral => crate::i18n::tr("Camera colour: neutral (no accepted colour fit)").to_string(),
    };
    if source.monochrome_tone {
        label.push_str(" · ");
        label.push_str(crate::i18n::tr("Tone from black-and-white camera JPEG"));
    }
    if source.dark_tone {
        label.push_str(" · ");
        label.push_str(crate::i18n::tr("Tone from near-black camera JPEG"));
    }
    if source.dro_tone_removed {
        label.push_str(" · ");
        label.push_str(crate::i18n::tr("DRO tone removed"));
    }
    label
}

#[cfg(test)]
mod tests {
    use super::*;
    use lightcraft_catalog::{MediaKind, Op, Photo, Source};
    use lightcraft_engine::media::SourceLevel;
    use lightcraft_pipeline::{ColourBasis, ColourSource, SourceInfo};
    use std::{sync::Arc, time::Duration};

    #[test]
    fn colour_readout_follows_selection_in_both_panels() {
        let sources = [
            (ColourBasis::FileMatrices, "raw's own matrices", "eigene Matrizen"),
            (ColourBasis::CameraProfile { model: "Test Camera".into(), local: true }, "profile (local)", "(lokal)"),
            (ColourBasis::CameraProfile { model: "Test Camera".into(), local: false }, "profile (built in)", "(integriert)"),
            (ColourBasis::CameraJpeg, "fitted to the camera JPEG", "an das Kamera-JPEG angepasst"),
            (ColourBasis::PartialCameraJpeg, "partial camera JPEG fit", "teilweise an das Kamera-JPEG angepasst"),
            (ColourBasis::Spectral, "spectral sensitivities", "spektrale Empfindlichkeiten"),
            (ColourBasis::Neutral, "neutral (no accepted colour fit)", "neutral (keine akzeptierte Farbanpassung)"),
        ];
        let mut session = lightcraft_engine::Session::new();
        let mut ids = Vec::new();
        for i in 0..=sources.len() {
            let raw = i < sources.len();
            let name = format!("synthetic-{i}.{}", if raw { "ARW" } else { "jpg" });
            let id = session.catalog.alloc_photo_id();
            let mut photo =
                Photo::new(id, Source::File { path: name.clone() }, &name, if raw { "ARW" } else { "JPEG" }, 32, 32, "2026-10-23T00:00:00");
            photo.kind = if raw { MediaKind::Raw } else { MediaKind::Image };
            session.catalog.apply(Op::AddPhoto { photo: Box::new(photo) }).unwrap();
            ids.push(id);
        }
        let bases: Vec<_> = sources.iter().map(|(basis, _, _)| basis.clone()).collect();
        let loader: lightcraft_engine::media::FileLoader = Arc::new(move |path, _| {
            let index = path.strip_prefix("synthetic-").and_then(|p| p.split('.').next()).and_then(|p| p.parse::<usize>().ok()).unwrap();
            Ok((
                lightcraft_raster::Rgb32f::new(32, 32),
                SourceInfo {
                    raw: true,
                    colour_source: bases.get(index).map(|basis| ColourSource {
                        basis: basis.clone(),
                        monochrome_tone: true,
                        dro_tone_removed: true,
                        dark_tone: true,
                    }),
                    ..Default::default()
                },
            ))
        });
        let app = LightcraftApp::new(session, crate::Services { png: None, ..Default::default() });
        let mut h = crate::headless::Headless::new(app, [1400.0, 1400.0], 1.0);
        let timeout = Duration::from_secs(30);
        for language in ["en", "de"] {
            for panel in ["edit", "profiles"] {
                h.app.session.media.clear_sources();
                for (i, id) in ids.iter().enumerate() {
                    h.request("engine.execute", json!({"command":"library.select", "params":{"ids":[id.0], "active":id.0}}), timeout);
                    h.request("ui.set", json!({"view":"detail", "right":panel, "language":language}), timeout);
                    h.step();
                    let text = h.painted_text().join("\n");
                    assert_eq!(h.app.widgets.iter().any(|(name, _)| name == "label:cameraColourSource"), i < sources.len(), "{panel}: {i}");
                    if i < sources.len() {
                        assert!(text.contains(if language == "de" { "nach dem Dekodieren" } else { "not available until" }), "{text}");
                    }
                    if i < sources.len() {
                        h.app.session.media.file_loader = Some(loader.clone());
                        h.app.session.source_now(*id, SourceLevel::Preview).unwrap();
                        h.app.session.media.file_loader = None;
                        h.step();
                        let text = h.painted_text().join("\n");
                        assert!(text.contains(if language == "de" { sources[i].2 } else { sources[i].1 }), "{panel} {language} {i}: {text}");
                        for (en, de) in [
                            ("Tone from black-and-white camera JPEG", "Tonwerte aus dem Schwarzweiß-Kamera-JPEG"),
                            ("Tone from near-black camera JPEG", "Tonwerte aus dem nahezu schwarzen Kamera-JPEG"),
                            ("DRO tone removed", "DRO-Tonwertanhebung entfernt"),
                        ] {
                            assert!(text.contains(if language == "de" { de } else { en }), "{text}");
                        }
                        assert_eq!(text.contains("Test Camera"), i == 1 || i == 2, "{text}");
                    } else {
                        assert!(!text.contains(if language == "de" { "Kamerafarbe:" } else { "Camera colour:" }), "{text}");
                    }
                }
            }
        }
    }
}
