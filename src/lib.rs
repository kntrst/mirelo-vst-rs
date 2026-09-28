mod config;
mod decode;
mod editor;
mod mirelo;
mod player;
mod shared;
mod worker;

use std::sync::{Arc, Mutex};

use truce::prelude::*;

use player::Voice;
use shared::{Command, Inner, AudioClip, Shared, SharedString};

const GRAVEYARD_SIZE: usize = 16;
pub const DEFAULT_DURATION_MS: u32 = 1000;

#[derive(Params)]
pub struct MireloVstRsParams {
    #[param(
        name = "Gain",
        range = "linear(-60, 6)",
        unit = "dB",
        smooth = "exp(5)"
    )]
    pub gain: FloatParam,

    #[persist]
    pub prompt: Mutex<String>,
    #[persist]
    pub duration_ms: Mutex<u32>,
    #[persist]
    pub last_file: SharedString,

    #[skip]
    pub shared: Shared,
}

impl MireloVstRsParams {
    fn start_worker(&self) {
        self.shared.ensure_worker(&self.last_file);
    }
}

/// owned by audio thread
#[derive(Default)]
pub struct Dsp {
    inner: Option<Arc<Inner>>,
    graveyard: Option<rtrb::Producer<Arc<AudioClip>>>,
    current: Option<Arc<AudioClip>>,
    voice: Voice,
    host_rate: f64,
}

impl Dsp {
    fn sync_sample(&mut self) {
        let Some(inner) = &self.inner else { return };
        let latest = inner.sample.load();
        let same = match (&*latest, &self.current) {
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        let new = (*latest).clone();
        drop(latest);
        if let Some(old) = std::mem::replace(&mut self.current, new) {
            let retired = match self.graveyard.as_mut() {
                Some(g) => g.push(old).err().map(|rtrb::PushError::Full(o)| o),
                None => Some(old),
            };
            if let Some(o) = retired {
                std::mem::forget(o);
            }
        }
        self.voice.stop();
    }
}

pub struct MireloVstRs;

impl PluginLogic for MireloVstRs {
    type Params = MireloVstRsParams;
    type DspState = Dsp;

    fn bus_layouts() -> Vec<BusLayout> {
        vec![BusLayout::new().with_output("Main", ChannelConfig::Stereo)]
    }

    fn init(params: &Self::Params, _cx: &InitContext) -> Self::DspState {
        params.start_worker();
        let (producer, consumer) = rtrb::RingBuffer::new(GRAVEYARD_SIZE);
        let inner = params.shared.inner.clone();
        if let Ok(mut g) = inner.graveyard.lock() {
            *g = Some(consumer);
        }
        params.shared.send(Command::Reload);
        Dsp {
            inner: Some(inner),
            graveyard: Some(producer),
            host_rate: 48_000.0,
            ..Default::default()
        }
    }

    fn reset(state: &mut Self::DspState, _params: &Self::Params, config: &AudioConfig) {
        state.host_rate = config.sample_rate;
        state.voice.stop();
    }

    fn state_changed(_state: &mut Self::DspState, params: &Self::Params) {
        params.shared.send(Command::Reload);
    }

    fn process(
        state: &mut Self::DspState,
        params: &Self::Params,
        buffer: &mut AudioBuffer,
        events: &EventList,
        _context: &mut ProcessContext,
    ) -> ProcessStatus {
        state.sync_sample();

        let n = buffer.num_samples();
        let n_out = buffer.num_output_channels();
        for ch in 0..n_out {
            buffer.output(ch)[..n].fill(0.0);
        }

        let step = state
            .current
            .as_ref()
            .map_or(1.0, |s| s.sample_rate / state.host_rate.max(1.0));
        let mut gain = || db_to_linear(params.gain.read());
        let mut cursor = 0usize;
        let Dsp { current, voice, .. } = state;

        let mut render_to = |voice: &mut Voice, end: usize, buffer: &mut AudioBuffer| {
            let end = end.min(n);
            if end > cursor {
                voice.render(
                    current.as_ref(),
                    step,
                    n_out,
                    cursor,
                    end - cursor,
                    &mut gain,
                    &mut |ch, i, v| buffer.output(ch)[i] += v,
                );
                cursor = end;
            }
        };

        // any note-on (any channel / note) retriggers
        for event in events.iter() {
            let velocity = match event.body {
                EventBody::NoteOn { velocity, .. } if velocity > 0 => f32::from(velocity) / 127.0,
                EventBody::NoteOn2 { velocity, .. } if velocity > 0 => {
                    f32::from(velocity) / 65535.0
                }
                _ => continue,
            };
            render_to(voice, event.sample_offset as usize, buffer);
            voice.trigger(velocity);
        }
        render_to(voice, n, buffer);
        ProcessStatus::Normal
    }

    fn editor(params: Arc<MireloVstRsParams>) -> Box<dyn Editor> {
        editor::create(params)
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
