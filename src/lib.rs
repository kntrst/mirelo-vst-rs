pub mod mirelo;
use crate::mirelo::service::HttpService;

use std::sync::Arc;
use truce::prelude::*;

#[derive(Params)]
pub struct MireloVstRsParams {
    #[param(
        name = "Gain",
        range = "linear(-60, 6)",
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub gain: FloatParam,
    /// Editor/worker-only state. Truce does not persist or automate this
    /// field, but every editor for this plugin instance receives the same Arc.
    #[skip]
    pub http: Arc<HttpService>,
}

// The plugin struct is its own DSP state (`type DspState = Self`). The
// shell owns it and preserves it across a hot-reload, so a code-only
// reload keeps reverb tails and oscillator phase alive.
pub struct MireloVstRs {
    /// Latest encoded files available to the audio callback. MIDI playback
    /// will later decode/use these without involving the HTTP worker.
    audio_files: Option<mirelo::sync_api::InMemoryAudioFiles>,
    /// An older generation waiting to be returned to the worker for drop.
    /// This prevents a potentially large Vec deallocation in `process`.
    retiring_files: Option<mirelo::sync_api::InMemoryAudioFiles>,
}

impl Default for MireloVstRs {
    fn default() -> Self {
        Self {
            audio_files: None,
            retiring_files: None,
        }
    }
}

impl MireloVstRs {
    /// Receives a fully-downloaded result through bounded lock-free queues.
    /// Every value that cannot yet be returned stays in DSP state, so this
    /// path never drops a file allocation on the audio thread.
    fn receive_downloaded_files(&mut self, http: &HttpService) {
        self.return_retired_files(http);
        if self.retiring_files.is_some() {
            return;
        }

        if let Some(files) = http.take_ready_files() {
            self.retiring_files = self.audio_files.replace(files);
            self.return_retired_files(http);
        }
    }

    fn return_retired_files(&mut self, http: &HttpService) {
        let Some(files) = self.retiring_files.take() else {
            return;
        };
        if let Err(files) = http.return_files(files) {
            self.retiring_files = Some(files);
        }
    }
}

impl PluginLogic for MireloVstRs {
    type Params = MireloVstRsParams;
    type DspState = Self;

    fn init(params: &Self::Params, _context: &InitContext) -> Self::DspState {
        // Truce calls init off the audio thread. Starting the worker here keeps
        // agent setup and every later HTTP operation out of process().
        let _ = params.http.start();
        Self::default()
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        state.receive_downloaded_files(&params.http);
        for i in 0..buffer.num_samples() {
            let gain = db_to_linear(params.gain.read());
            for ch in 0..buffer.channels() {
                let (inp, out) = buffer.io(ch);
                out[i] = inp[i] * gain;
            }
        }
        ProcessStatus::Normal
    }

    fn editor(params: Arc<MireloVstRsParams>) -> Box<dyn Editor> {
        truce_egui::EguiEditor::with_ui(params, (560, 400), mirelo::ui::MireloUi::default())
            .into_editor()
    }
}

truce::plugin! {
    logic: MireloVstRs,
    params: MireloVstRsParams,
}

// Installs the real-time allocation checker under `--features rt-paranoid`
// (a no-op otherwise). Wrap a driver run in `assert_no_audio_alloc` to
// fail a test if `process` ever allocates. See the audio-testing guide.
truce::enable_rt_paranoid!();
