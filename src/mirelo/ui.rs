use super::sync_api::{Prompt, Request, Response};
use crate::{MireloVstRsParams, MireloVstRsParamsParamId as P};
use std::time::Duration;
use truce::prelude::PluginContext;
use truce_egui::EditorUi;
use truce_egui::widgets::param_knob;

pub(crate) struct MireloUi {
    prompt: String,
    duration_ms: u64,
    status: String,
}

impl Default for MireloUi {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            duration_ms: 1_000,
            status: "Ready".to_owned(),
        }
    }
}

impl EditorUi<MireloVstRsParams> for MireloUi {
    fn ui(&mut self, ui: &mut egui::Ui, state: &PluginContext<MireloVstRsParams>) {
        if let Some(response) = state.http.poll() {
            self.status = response_status(response);
        }

        let busy = state.http.is_busy();
        ui.heading("Mirelo");
        ui.label("Describe the sound effect to generate.");
        ui.add_enabled(
            !busy,
            egui::TextEdit::multiline(&mut self.prompt)
                .desired_rows(5)
                .hint_text("A distant thunder clap in a large cave"),
        );

        ui.horizontal(|ui| {
            ui.label("Duration (ms)");
            ui.add_enabled(
                !busy,
                egui::DragValue::new(&mut self.duration_ms).range(100..=30_000),
            );
        });

        ui.horizontal(|ui| {
            let can_send = !busy && !self.prompt.trim().is_empty();
            if ui
                .add_enabled(can_send, egui::Button::new("Estimate cost"))
                .clicked()
            {
                self.send(state, Request::PreflightCheck);
            }
            if ui
                .add_enabled(can_send, egui::Button::new("Generate"))
                .clicked()
            {
                self.send(state, Request::Submit);
            }
            if busy {
                ui.spinner();
                ui.label("Contacting Mirelo…");
                // Keep polling while waiting without blocking this frame.
                ui.ctx().request_repaint_after(Duration::from_millis(16));
            }
        });

        ui.separator();
        ui.label(&self.status);
        ui.separator();
        param_knob(ui, state, P::Gain, "Gain");
    }
}

impl MireloUi {
    fn send(
        &mut self,
        state: &PluginContext<MireloVstRsParams>,
        request: impl FnOnce(Prompt) -> Request,
    ) {
        let prompt = Prompt::new(self.prompt.trim().to_owned(), self.duration_ms);
        self.status = match state.http.send(request(prompt)) {
            Ok(()) => "Request sent…".to_owned(),
            Err(error) => format!("Request not sent: {error}"),
        };
    }
}

fn response_status(response: Response) -> String {
    match response {
        Response::PreflightCheck(Ok(estimate)) => format!(
            "Estimated cost: {} credits for {} ms",
            estimate.credits(),
            estimate.estimated_ms()
        ),
        Response::Submit(Ok(report)) => format!(
            "Downloaded {} file(s) into audio memory ({} bytes).",
            report.file_count(),
            report.total_bytes()
        ),
        Response::PreflightCheck(Err(error)) | Response::Submit(Err(error)) => {
            format!("Request failed: {error}")
        }
        Response::WorkerStopped => "The HTTP worker stopped unexpectedly.".to_owned(),
    }
}
