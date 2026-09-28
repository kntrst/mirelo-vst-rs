use std::sync::Arc;

use truce::prelude::*;
use truce_egui::EguiEditor;
use truce_egui::widgets::param_knob;

use crate::shared::Command;
use crate::{DEFAULT_DURATION_MS, MireloVstRsParams, MireloVstRsParamsParamId as P};

const MIN_DURATION_MS: u32 = 1000;
const MAX_DURATION_MS: u32 = 10_000;

struct Ui {
    key_input: Option<String>,
    key_note: Option<String>,
}

pub fn create(params: Arc<MireloVstRsParams>) -> Box<dyn Editor> {
    params.start_worker();
    let mut state = Ui {
        key_input: None,
        key_note: None,
    };

    EguiEditor::new(params, (700, 420), move |ui, cx| draw(ui, cx, &mut state))
        .with_visuals(truce_egui::theme::dark())
        .resizable(true)
        .into_editor()
}

fn draw(ui: &mut egui::Ui, cx: &PluginContext<MireloVstRsParams>, st: &mut Ui) {
    let p: &MireloVstRsParams = cx;
    let status = p
        .shared
        .inner
        .status
        .lock()
        .map(|s| s.clone())
        .unwrap_or_default();

    ui.heading("Mirelo Sampler");
    ui.add_space(6.0);

    ui.label("Prompt");
    if let Ok(mut prompt) = p.prompt.lock() {
        ui.add(
            egui::TextEdit::multiline(&mut *prompt)
                .hint_text("e.g. glass shattering on a stone floor")
                .desired_rows(4)
                .desired_width(f32::INFINITY),
        );
    }

    let duration_ms = {
        let mut d = p.duration_ms.lock().map(|d| *d).unwrap_or(0);
        if d == 0 {
            d = DEFAULT_DURATION_MS;
        }
        let mut secs = d as f32 / 1000.0;
        ui.horizontal(|ui| {
            ui.label("Duration (s)");
            ui.add(egui::Slider::new(
                &mut secs,
                MIN_DURATION_MS as f32 / 1000.0..=MAX_DURATION_MS as f32 / 1000.0,
            ));
        });
        let d = (secs * 1000.0).round() as u32;
        if let Ok(mut g) = p.duration_ms.lock() {
            *g = d;
        }
        d
    };

    ui.add_space(6.0);
    ui.horizontal(|ui| {
        let prompt = p
            .prompt
            .lock()
            .map(|s| s.trim().to_owned())
            .unwrap_or_default();
        let enabled = !status.busy && !prompt.is_empty();
        if ui
            .add_enabled(enabled, egui::Button::new("Generate"))
            .clicked()
        {
            let api_key = p
                .shared
                .api_key
                .lock()
                .map(|k| k.clone())
                .unwrap_or_default();
            p.shared.inner.set_status(|s| {
                s.busy = true;
                s.error = None;
                s.message = "Queued...".into();
                s.progress = 0.0;
            });
            if !p.shared.send(Command::Generate {
                prompt,
                duration_ms,
                api_key,
            }) {
                p.shared.inner.set_status(|s| {
                    s.busy = false;
                    s.error = Some("Worker not running".into());
                });
            }
        }
        if status.busy {
            ui.spinner();
        }
        ui.label(&status.message);
    });
    if status.busy {
        ui.add(egui::ProgressBar::new(status.progress));
    }
    if let Some(err) = &status.error {
        ui.colored_label(egui::Color32::from_rgb(230, 90, 90), err);
    }

    ui.add_space(10.0);
    ui.separator();
    ui.horizontal(|ui| {
        param_knob(ui, cx, P::Gain, "Gain");
        ui.vertical(|ui| {
            ui.label("API key (stored in %APPDATA%\\MireloVst, not in the project)");
            let key = st.key_input.get_or_insert_with(|| {
                p.shared
                    .api_key
                    .lock()
                    .map(|k| k.clone())
                    .unwrap_or_default()
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(key)
                        .password(true)
                        .desired_width(260.0),
                );
                if ui.button("Save").clicked() {
                    let trimmed = key.trim().to_owned();
                    st.key_note = Some(match crate::config::save_api_key(&trimmed) {
                        Ok(()) => "Saved".into(),
                        Err(e) => format!("Save failed: {e}"),
                    });
                    if let Ok(mut k) = p.shared.api_key.lock() {
                        *k = trimmed;
                    }
                }
            });
            if let Some(note) = &st.key_note {
                ui.small(note);
            }
        });
    });

    if status.busy {
        ui.ctx().request_repaint();
    }
}
