use super::geometry::CanvasMap;
use super::masks::{pin, pins_live};
use super::{Color32, DevelopSettings, Gesture, LightcraftApp, Point, Pos2, Rect, Stroke, ToolOverlay, json, register, vec2};

// ------------------------------------------------------------------------ remove

/// The Remove tool on the photo: every spot's outline and pin (the selected one with its source),
/// click a pin to select its spot, drag a target or source to move it, paint elsewhere to add one.
/// The Tool Overlay mode (Shift+H) says which spots show; a hidden spot can't be grabbed, so a
/// click there adds a new one.
pub(super) fn remove_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    if ui.data(|data| data.get_temp::<bool>(egui::Id::unique("ai-remove-brush-mode"))).unwrap_or(true) {
        ai_selection_overlay(app, ui, resp, map);
        return;
    }
    let long = (map.rect.width().max(map.rect.height())) as f64;
    let active = app.session.active_spot.filter(|i| *i < d.spots.len());
    let (mode, live) = (app.ui.tool_overlay, pins_live(app, resp, map));
    // Auto also hides them while a new spot is painted
    let painting = mode == ToolOverlay::Auto && matches!(app.gesture, Some(Gesture::Spot { .. }));
    let p = ui.painter_at(app.canvas_rect.unwrap_or(map.rect));
    let mut pins = p.clone();
    if painting {
        pins.set_invisible();
    }
    // (spot, is source, screen centre, radius) of everything that can be grabbed
    let mut grips: Vec<(usize, bool, Pos2, f32)> = Vec::new();
    for (i, s) in d.spots.iter().enumerate() {
        let r = (s.size * long) as f32;
        let sel = Some(i) == active;
        if !mode.shows(sel, live) {
            continue;
        }
        let p = &pins;
        let col = Color32::from_white_alpha(if sel { 230 } else { 150 });
        for q in &s.points {
            p.circle_stroke(map.screen(*q), r, Stroke::new(if sel { 1.5 } else { 1.0 }, col));
        }
        let Some(&t) = s.points.first() else { continue };
        let tq = map.screen(t);
        if let Some(o) = s.source_offset.filter(|_| sel) {
            let sq = map.screen(Point::new(t.x + o.x, t.y + o.y));
            for q in &s.points {
                p.circle_stroke(map.screen(Point::new(q.x + o.x, q.y + o.y)), r, Stroke::new(1.0, Color32::from_white_alpha(170)));
            }
            let dir = (tq - sq).normalized();
            p.arrow(sq + dir * r, (tq - sq) - dir * (2.0 * r).min((tq - sq).length()), Stroke::new(1.0, Color32::from_white_alpha(200)));
            register(ui, format!("spotSource:{i}"), Rect::from_center_size(sq, vec2(14.0, 14.0)));
            grips.push((i, true, sq, r));
        }
        pin(p, tq, sel);
        register(ui, format!("spotPin:{i}"), Rect::from_center_size(tq, vec2(14.0, 14.0)));
        grips.push((i, false, tq, r));
    }
    if spot_keys(app, ui, map, d, &grips) {
        return;
    }
    let r = (app.ui.remove_size as f64 * long) as f32;
    // a target or source under `q`: the selected spot's first, then the nearest pin
    let hit = |q: Pos2| {
        grips
            .iter()
            .filter(|g| g.2.distance(q) < g.3.max(8.0))
            .min_by(|a, b| {
                (Some(a.0) != active, a.2.distance(q)).partial_cmp(&(Some(b.0) != active, b.2.distance(q))).unwrap_or(std::cmp::Ordering::Equal)
            })
            .copied()
    };
    let hover = resp.hover_pos().and_then(hit);
    if let Some(h) = resp.hover_pos() {
        if hover.is_some() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
        } else {
            ui.ctx().set_cursor_icon(egui::CursorIcon::None);
            p.circle_stroke(h, r, Stroke::new(1.0, Color32::WHITE));
            p.circle_stroke(h, r * (1.0 - app.ui.remove_feather / 100.0).max(0.05), Stroke::new(1.0, Color32::from_white_alpha(110)));
        }
    }
    let press = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos());
    if resp.drag_started()
        && let Some((spot, source, ..)) = press.and_then(hit)
    {
        if Some(spot) != active {
            let _ = app.run("spot.select", json!({"index": spot}));
        }
        let _ = app.run("develop.beginInteraction", json!({"label": "Edit Spot"}));
        app.gesture = Some(Gesture::SpotMove { spot, source });
    }
    if let Some(Gesture::SpotMove { spot, source }) = app.gesture.clone() {
        if resp.dragged()
            && let Some(q) = resp.interact_pointer_pos()
        {
            let (n, n0) = (map.norm(q), map.norm(q - resp.drag_delta()));
            let dn = [n.x - n0.x, n.y - n0.y];
            if dn != [0.0, 0.0] {
                let _ = app.run("spot.update", json!({"index": spot, if source { "moveSource" } else { "move" }: dn}));
            }
        }
        if resp.drag_stopped() {
            app.gesture = None;
            let _ = app.run("develop.endInteraction", json!({}));
        }
        return;
    }
    if resp.clicked()
        && let Some((spot, ..)) = press.and_then(hit)
    {
        let _ = app.run("spot.select", json!({"index": spot}));
        return;
    }
    if let Some(q) = resp.interact_pointer_pos()
        && (resp.drag_started() || resp.dragged() || resp.clicked())
    {
        let n = map.norm(q);
        match &mut app.gesture {
            Some(Gesture::Spot { points }) => points.push(n),
            _ => app.gesture = Some(Gesture::Spot { points: vec![n] }),
        }
    }
    if let Some(Gesture::Spot { points }) = &app.gesture {
        for q in points {
            p.circle_filled(map.screen(*q), r, Color32::from_white_alpha(60));
        }
    }
    if (resp.drag_stopped() || resp.clicked())
        && let Some(Gesture::Spot { points }) = app.gesture.take()
    {
        let mode = match app.ui.tool.as_str() {
            "heal" => "heal",
            "clone" => "clone",
            "fill" => "fill",
            _ => "remove",
        };
        let pts: Vec<[f64; 2]> = points.iter().map(|q| [q.x, q.y]).collect();
        let _ = app.run(
            "spot.add",
            json!({"mode": mode, "points": pts, "size": app.ui.remove_size, "feather": app.ui.remove_feather, "opacity": app.ui.remove_opacity}),
        );
    }
}

/// Paint a private AI mask; classic healing spots and their source handles are untouched.
fn ai_selection_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap) {
    let radius = app.ui.remove_size * map.rect.width().max(map.rect.height());
    let p = ui.painter_at(app.canvas_rect.unwrap_or(map.rect));
    if let Some(q) = resp.hover_pos().filter(|p| map.rect.contains(*p)) {
        ui.ctx().set_cursor_icon(egui::CursorIcon::None);
        p.circle_stroke(q, radius, Stroke::new(1.0, Color32::from_rgb(35, 220, 240)));
    }
    if let Some(q) = resp.interact_pointer_pos()
        && (resp.drag_started() || resp.dragged() || resp.clicked())
        && map.rect.contains(q)
    {
        if !matches!(app.gesture, Some(Gesture::Spot { .. })) {
            let _ = app.run("aiRemove.cancel", json!({}));
            let start = ui.input(|i| i.pointer.press_origin()).unwrap_or(q);
            app.gesture = Some(Gesture::Spot { points: vec![map.norm(start)] });
        }
        let point = map.norm(q);
        if point.x.is_finite()
            && point.y.is_finite()
            && (0.0..=1.0).contains(&point.x)
            && (0.0..=1.0).contains(&point.y)
            && let Some(Gesture::Spot { points }) = &mut app.gesture
            && points.len() < 4096
            && points.last() != Some(&point)
        {
            points.push(point);
        }
    }
    let selection = app.session.ai_remove.selection.as_ref();
    let painted = match &app.gesture {
        Some(Gesture::Spot { points }) => Some((points.as_slice(), radius)),
        _ if app.session.ai_remove.draft.is_none() => {
            selection.map(|s| (s.points.as_slice(), s.size as f32 * map.rect.width().max(map.rect.height())))
        }
        _ => None,
    };
    if let Some((points, radius)) = painted {
        draw_ai_selection(ui, &p, map, points, radius);
    }
    if (resp.drag_stopped() || resp.clicked())
        && let Some(Gesture::Spot { points }) = app.gesture.take()
    {
        let points: Vec<[f64; 2]> = points
            .iter()
            .filter(|p| p.x.is_finite() && p.y.is_finite() && (0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y))
            .map(|p| [p.x, p.y])
            .collect();
        if let Err(e) = app.run(
            "aiRemove.select",
            json!({"points":points,"size":app.ui.remove_size,"feather":app.ui.remove_feather,"opacity":app.ui.remove_opacity}),
        ) {
            app.toast(ui.ctx(), e);
        }
    }
}

/// A union mask in one texture: overlapping capsules never accumulate opacity.
fn draw_ai_selection(ui: &egui::Ui, painter: &egui::Painter, map: &CanvasMap, points: &[Point], radius: f32) {
    use std::hash::{Hash, Hasher};
    let points: Vec<Pos2> = points.iter().map(|p| map.screen(*p)).filter(|p| p.x.is_finite() && p.y.is_finite()).collect();
    if points.is_empty() || !radius.is_finite() || radius <= 0.0 {
        return;
    }
    let mut bounds = Rect::NOTHING;
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    radius.to_bits().hash(&mut hash);
    for point in &points {
        bounds.extend_with(*point);
        point.x.to_bits().hash(&mut hash);
        point.y.to_bits().hash(&mut hash);
    }
    bounds = bounds.expand(radius + 1.0).intersect(painter.clip_rect());
    if !bounds.is_positive() {
        return;
    }
    for v in [bounds.min.x, bounds.min.y, bounds.max.x, bounds.max.y] {
        v.to_bits().hash(&mut hash);
    }
    let hash = hash.finish();
    let key = egui::Id::unique("ai-remove-mask-texture");
    let cached = ui.data(|data| data.get_temp::<(u64, egui::TextureHandle)>(key));
    let texture = match cached {
        Some((old, texture)) if old == hash => texture,
        _ => {
            // Bound overlay raster work even for a supplied zigzag mask with 4096 long segments.
            let work: f32 = points
                .iter()
                .zip(points.iter().skip(1).chain(points.last()))
                .map(|(a, b)| ((a.x - b.x).abs() + 2.0 * radius) * ((a.y - b.y).abs() + 2.0 * radius))
                .sum();
            let scale = (1024.0 / bounds.width().max(bounds.height())).min(1.0).min((8_000_000.0 / work.max(1.0)).sqrt());
            let width = (bounds.width() * scale).ceil().max(1.0) as usize;
            let height = (bounds.height() * scale).ceil().max(1.0) as usize;
            let mut mask = vec![false; width * height];
            for (a, b) in points.iter().zip(points.iter().skip(1).chain(points.last())) {
                let rect = Rect::from_two_pos(*a, *b).expand(radius).intersect(bounds);
                let x0 = ((rect.left() - bounds.left()) * scale).floor().max(0.0) as usize;
                let y0 = ((rect.top() - bounds.top()) * scale).floor().max(0.0) as usize;
                let x1 = (((rect.right() - bounds.left()) * scale).ceil().max(0.0) as usize).min(width);
                let y1 = (((rect.bottom() - bounds.top()) * scale).ceil().max(0.0) as usize).min(height);
                let delta = *b - *a;
                for y in y0..y1 {
                    for x in x0..x1 {
                        let q = bounds.min + vec2((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
                        let t = if delta.length_sq() > 0.0 { ((q - *a).dot(delta) / delta.length_sq()).clamp(0.0, 1.0) } else { 0.0 };
                        if q.distance_sq(*a + delta * t) <= radius * radius
                            && let Some(pixel) = mask.get_mut(y * width + x)
                        {
                            *pixel = true;
                        }
                    }
                }
            }
            let pixels = mask
                .into_iter()
                .map(|inside| if inside { Color32::from_rgba_unmultiplied(20, 210, 235, 100) } else { Color32::TRANSPARENT })
                .collect();
            let texture = ui.ctx().load_texture("ai-remove-selection", egui::ColorImage::new([width, height], pixels), egui::TextureOptions::LINEAR);
            ui.data_mut(|data| data.insert_temp(key, (hash, texture.clone())));
            texture
        }
    };
    painter.image(texture.id(), bounds, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
}

/// Keyboard stand-ins for the Remove tool: each spot's target and source (an arrow press moves
/// it a step, one undo step; Return selects the spot), and a placement point that the arrows move
/// and Return turns into a new spot of the brush's size there. True when a spot was added.
fn spot_keys(app: &mut LightcraftApp, ui: &mut egui::Ui, map: &CanvasMap, d: &DevelopSettings, grips: &[(usize, bool, Pos2, f32)]) -> bool {
    for &(spot, source, q, _) in grips {
        let at = map.norm(q);
        let name =
            format!("{} {}{}", crate::i18n::tr("Spot"), spot + 1, if source { format!(" {}", crate::i18n::tr("source")) } else { String::new() });
        let size = d.spots.get(spot).map(|s| s.size).unwrap_or_default();
        let value = format!("{}, {} {:.0}%", crate::canvas_keys::position_text(at.x, at.y), crate::i18n::tr("size"), size * 100.0);
        let keys = crate::canvas_keys::handle(ui, &format!("spot:{spot}:{}", if source { "source" } else { "target" }), &name, value, q);
        if (keys.activated || keys.delta.is_some()) && app.session.active_spot != Some(spot) {
            let _ = app.run("spot.select", json!({"index": spot}));
        }
        if let Some((dx, dy)) = keys.delta {
            let _ = app.run("develop.beginInteraction", json!({"label": "Edit Spot"}));
            let _ = app.run("spot.update", json!({"index": spot, if source { "moveSource" } else { "move" }: [dx, dy]}));
            let _ = app.run("develop.endInteraction", json!({}));
        }
    }
    // where a new spot goes: the photo's centre at first, then wherever the arrows took it
    let key = egui::Id::unique("lc-spot-placement");
    let place: (f64, f64) = ui.data(|m| m.get_temp(key)).unwrap_or((0.5, 0.5));
    let q = map.screen(Point::new(place.0, place.1));
    let keys = crate::canvas_keys::handle(
        ui,
        "spot:new",
        crate::i18n::tr("New spot (Return adds it here)"),
        crate::canvas_keys::position_text(place.0, place.1),
        q,
    );
    if let Some((dx, dy)) = keys.delta {
        let moved = ((place.0 + dx).clamp(0.0, 1.0), (place.1 + dy).clamp(0.0, 1.0));
        ui.data_mut(|m| m.insert_temp(key, moved));
    }
    if crate::canvas_keys::has_focus(ui.ctx()) && ui.memory(|m| m.has_focus(crate::canvas_keys::id("spot:new"))) {
        let r = (app.ui.remove_size as f64 * (map.rect.width().max(map.rect.height())) as f64) as f32;
        ui.painter().circle_stroke(q, r, Stroke::new(1.0, Color32::WHITE));
    }
    if keys.activated {
        let mode = match app.ui.tool.as_str() {
            "heal" => "heal",
            "clone" => "clone",
            "fill" => "fill",
            _ => "remove",
        };
        let _ = app.run(
            "spot.add",
            json!({"mode": mode, "points": [[place.0, place.1]], "size": app.ui.remove_size, "feather": app.ui.remove_feather, "opacity": app.ui.remove_opacity}),
        );
        return true;
    }
    false
}

// ------------------------------------------------------------------------ red eye

/// Red Eye tool: drag an ellipse over an eye (a new correction), click one to select it.
pub(super) fn eye_overlay(app: &mut LightcraftApp, ui: &mut egui::Ui, resp: &egui::Response, map: &CanvasMap, d: &DevelopSettings) {
    // screen points per long-edge unit
    let o = map.screen(Point::new(0.0, 0.0));
    let (sx, sy) = (map.screen(Point::new(1.0, 0.0)).distance(o), map.screen(Point::new(0.0, 1.0)).distance(o));
    let per_long = sx.max(sy);
    let p = ui.painter();
    for (i, e) in d.red_eye.iter().enumerate() {
        let c = map.screen(e.center);
        let r = vec2(e.rx as f32 * per_long, e.ry as f32 * per_long);
        let sel = i == app.ui.eye;
        let col = if sel { Color32::WHITE } else { Color32::from_white_alpha(150) };
        p.add(egui::Shape::ellipse_stroke(c, r, Stroke::new(if sel { 1.5 } else { 1.0 }, col)));
        p.line_segment([c - vec2(4.0, 0.0), c + vec2(4.0, 0.0)], Stroke::new(1.0, col));
        p.line_segment([c - vec2(0.0, 4.0), c + vec2(0.0, 4.0)], Stroke::new(1.0, col));
    }
    if resp.hover_pos().is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Crosshair);
    }
    if resp.drag_started()
        && let Some(q) = ui.input(|i| i.pointer.press_origin()).or(resp.interact_pointer_pos())
    {
        // from where the button went down (a drag is recognized only after some movement)
        app.gesture = Some(Gesture::Eye { a: map.norm(q) });
    }
    if let Some(Gesture::Eye { a }) = app.gesture
        && let Some(q) = resp.interact_pointer_pos()
    {
        let (a, b) = (map.screen(a), q);
        p.add(egui::Shape::ellipse_stroke(a.lerp(b, 0.5), ((b - a) / 2.0).abs(), Stroke::new(1.0, Color32::WHITE)));
    }
    if resp.drag_stopped()
        && let Some(Gesture::Eye { a }) = app.gesture.take()
        && let Some(q) = resp.interact_pointer_pos()
    {
        let (sa, sb) = (map.screen(a), q);
        let half = ((sb - sa) / 2.0).abs() / per_long;
        if half.x > 0.002 && half.y > 0.002 {
            let c = map.norm(sa.lerp(sb, 0.5));
            let r = app.run("redeye.add", json!({"center": [c.x, c.y], "rx": half.x, "ry": half.y, "pet": app.ui.eye_pet}));
            if let Ok(v) = r {
                app.ui.eye = v["index"].as_u64().unwrap_or(0) as usize;
            }
        }
    } else if resp.clicked()
        && let Some(q) = resp.interact_pointer_pos()
        && let Some(i) = d.red_eye.iter().position(|e| {
            let c = map.screen(e.center);
            let (dx, dy) = ((q.x - c.x) / (e.rx as f32 * per_long).max(4.0), (q.y - c.y) / (e.ry as f32 * per_long).max(4.0));
            dx * dx + dy * dy <= 1.0
        })
    {
        app.ui.eye = i;
    }
}
