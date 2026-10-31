//! Management and display of separately installed AI model packages.
use serde_json::json;

pub(super) fn ai_model_management(app: &mut crate::LightcraftApp, ui: &mut egui::Ui, status: &serde_json::Value) {
    let open_key = egui::Id::unique("ai-models-window-open");
    let mut open = ui.data(|d| d.get_temp::<bool>(open_key)).unwrap_or(false);
    let response = ui.button(crate::i18n::tr("Manage models…"));
    crate::widgets::register(ui, "aiModelsManage", response.rect);
    if response.clicked() {
        open = true;
    }
    egui::Window::new(crate::i18n::tr("AI models"))
        .id(egui::Id::unique("ai-models-window"))
        .open(&mut open)
        .default_width(520.0)
        .resizable(true)
        .show(ui.ctx(), |ui| {
            egui::ScrollArea::vertical().max_height(650.0).show(ui, |ui| {
                let store = &status["store"];
                let running = status["running"] == true || app.session.ai_remove.busy();
                ui.add(egui::Label::new(crate::i18n::tr("Models and compatible runtimes can be imported as separate packages.")).wrap());
                ui.label(store["root"].as_str().unwrap_or(crate::i18n::tr("AI model folder unavailable")));
                ui.add_enabled_ui(!running, |ui| {
                    for (label, widget, command) in [
                        ("Choose existing AI store…", "aiModelsChooseRoot", "aiModels.setRoot"),
                        ("Import AI package…", "aiModelsImport", "aiModels.import"),
                    ] {
                        let response = ui.add_enabled(app.services.pick_folder.is_some(), egui::Button::new(crate::i18n::tr(label)));
                        crate::widgets::register(ui, widget, response.rect);
                        if response.clicked()
                            && let Some(path) = app.services.pick_folder.as_mut().and_then(|f| f())
                            && let Err(e) = app.run(command, json!({"path":path}))
                        {
                            app.toast(ui.ctx(), e);
                        }
                    }
                    if ui.button(crate::i18n::tr("Refresh")).clicked() {
                        let _ = app.run("aiModels.refresh", json!({}));
                    }
                    for model in store["models"].as_array().into_iter().flatten() {
                        let id = model["id"].as_str().unwrap_or_default();
                        ui.separator();
                        ui.add(
                            egui::Label::new(format!(
                                "{} · {:.1} GiB",
                                ai_model_label(model),
                                model["bytes"].as_u64().unwrap_or(0) as f64 / 1073741824.0
                            ))
                            .wrap(),
                        )
                        .on_hover_text(ai_model_details(model));
                        ui.label(crate::i18n::tr(model["status"].as_str().unwrap_or_default()));
                        runtime_selection(app, ui, model);
                        ui.push_id(("ai-model-details", id), |ui| {
                            ui.collapsing(crate::i18n::tr("Model details"), |ui| {
                                ui.add(
                                    egui::Label::new(crate::i18n::tr_format!(
                                        "Model revision: {}",
                                        model["model_version"].as_str().unwrap_or_default()
                                    ))
                                    .wrap(),
                                );
                                if let Some(source) = model["source"].as_str().filter(|s| !s.is_empty()) {
                                    ui.add(egui::Label::new(crate::i18n::tr_format!("Source: {}", source)).wrap());
                                }
                                if let Some(revision) = model["source_revision"].as_str().filter(|s| !s.is_empty()) {
                                    ui.add(egui::Label::new(crate::i18n::tr_format!("Source revision: {}", revision)).wrap());
                                }
                                let notices: Vec<&str> =
                                    model["licence_files"].as_array().into_iter().flatten().filter_map(serde_json::Value::as_str).collect();
                                if !notices.is_empty() {
                                    ui.label(crate::i18n::tr("Included licence and notice files:"));
                                    for path in notices {
                                        ui.add(egui::Label::new(path).wrap());
                                    }
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            if ui.button(crate::i18n::tr("Verify")).clicked() {
                                let _ = app.run("aiModels.verify", json!({"id":id}));
                            }
                            if ui.button(crate::i18n::tr("Remove model")).clicked() {
                                ui.data_mut(|d| d.insert_temp(egui::Id::unique(("ai-model-remove", id)), true));
                            }
                        });
                        let confirm_id = egui::Id::unique(("ai-model-remove", id));
                        if ui.data(|d| d.get_temp::<bool>(confirm_id)).unwrap_or(false) {
                            ui.add(egui::Label::new(crate::i18n::tr("Remove this model from disk? Existing photo edits remain available.")).wrap());
                            ui.horizontal(|ui| {
                                if ui.button(crate::i18n::tr("Confirm removal")).clicked() {
                                    let _ = app.run("aiModels.remove", json!({"id":id}));
                                    ui.data_mut(|d| d.remove::<bool>(confirm_id));
                                }
                                if ui.button(crate::i18n::tr("Cancel")).clicked() {
                                    ui.data_mut(|d| d.remove::<bool>(confirm_id));
                                }
                            });
                        }
                    }
                });
                if let Some(message) = status["message"].as_str().filter(|s| !s.is_empty()) {
                    ui.label(message);
                }
            });
        });
    ui.data_mut(|d| d.insert_temp(open_key, open));
}

fn runtime_selection(app: &mut crate::LightcraftApp, ui: &mut egui::Ui, model: &serde_json::Value) {
    let id = model["id"].as_str().unwrap_or_default();
    let selected = model["selected_runtime_id"].as_str().or_else(|| model["runtime_id"].as_str()).unwrap_or_default();
    let choices = model["runtime_choices"].as_array();
    let current = choices.and_then(|choices| choices.iter().find(|choice| choice["id"] == selected));
    let selected_label = current.map(runtime_label).unwrap_or_else(|| selected.to_owned());
    ui.label(crate::i18n::tr("Runtime"));
    let response = crate::widgets::combo_box(("ai-runtime", id)).selected_text(selected_label).show_ui(ui, |ui| {
        for choice in choices.into_iter().flatten() {
            let runtime_id = choice["id"].as_str().unwrap_or_default();
            let response = ui.selectable_label(runtime_id == selected, runtime_label(choice));
            crate::widgets::register(ui, format!("aiModelsRuntimeOption-{id}-{runtime_id}"), response.rect);
            if response.clicked()
                && runtime_id != selected
                && let Err(error) = app.run("aiModels.setRuntime", json!({"modelId":id,"runtimeId":runtime_id}))
            {
                app.toast(ui.ctx(), error);
            }
        }
    });
    crate::widgets::name_combo(&response.response, crate::i18n::tr("Runtime"));
    crate::widgets::register(ui, format!("aiModelsRuntime-{id}"), response.response.rect);
    if model["runtime_id"].as_str().is_some_and(|original| original != selected)
        && ui.button(crate::i18n::tr("Use original runtime")).clicked()
        && let Err(error) = app.run("aiModels.setRuntime", json!({"modelId":id,"runtimeId":null}))
    {
        app.toast(ui.ctx(), error);
    }
    if let Some(runtime) = current {
        if runtime["execution"] == "native-protocol-v1" {
            ui.add(egui::Label::new(crate::i18n::tr("ObjectClear native runtime: S-Lab License 1.0, non-commercial use.")).wrap());
        }
        ui.push_id(("ai-runtime-details", id), |ui| {
            ui.collapsing(crate::i18n::tr("Runtime details"), |ui| {
                ui.add(egui::Label::new(selected).wrap());
                if let Some(source) = runtime["source"].as_str().filter(|s| !s.is_empty()) {
                    ui.add(egui::Label::new(crate::i18n::tr_format!("Source: {}", source)).wrap());
                }
                for path in runtime["licence_files"].as_array().into_iter().flatten().filter_map(serde_json::Value::as_str) {
                    ui.add(egui::Label::new(path).wrap());
                }
            });
        });
    }
}

fn runtime_label(runtime: &serde_json::Value) -> String {
    match runtime["execution"].as_str() {
        Some("native-protocol-v1") => crate::i18n::tr("Native runtime (Rust)").to_owned(),
        Some("python-protocol-v1") => crate::i18n::tr("Reference runtime (Python)").to_owned(),
        _ => runtime["name"].as_str().or_else(|| runtime["id"].as_str()).unwrap_or_default().to_owned(),
    }
}

pub(super) fn ai_model_label(model: &serde_json::Value) -> String {
    let name = match model["backend"].as_str() {
        Some("objectclear") => "ObjectClear",
        Some("lama") => "LaMa",
        Some("rorem") => "RORem",
        _ => "AI",
    };
    format!("{} · {}", name, model["model_version"].as_str().unwrap_or_default().chars().take(8).collect::<String>())
}
pub(super) fn ai_model_details(model: &serde_json::Value) -> String {
    format!(
        "{}\n{}\n{}",
        model["name"].as_str().unwrap_or_default(),
        model["model_version"].as_str().unwrap_or_default(),
        model["id"].as_str().unwrap_or_default()
    )
}
