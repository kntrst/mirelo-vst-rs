pub mod mirelo;

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
    pub http: Arc<mirelo::sync_api::HttpService>,
}

// The plugin struct is its own DSP state (`type DspState = Self`). The
// shell owns it and preserves it across a hot-reload, so a code-only
// reload keeps reverb tails and oscillator phase alive.
#[derive(Default)]
pub struct MireloVstRs {
    // Per-instance DSP state - filters, delay lines, phase counters.
    // Fields need `Default`. Add them as your DSP grows.
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
        _state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        _events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
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
