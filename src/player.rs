//! Realtime-safe one-shot sample voice. No allocation, locking or I/O.

use std::sync::Arc;

use crate::shared::AudioClip;

#[derive(Default)]
pub struct Voice {
    pos: f64,
    playing: bool,
    gain: f32,
}

impl Voice {
    pub fn trigger(&mut self, velocity: f32) {
        self.pos = 0.0;
        self.playing = true;
        self.gain = velocity;
    }

    pub fn stop(&mut self) {
        self.playing = false;
    }

    #[allow(clippy::too_many_arguments)]
    pub fn render(
        &mut self,
        sample: Option<&Arc<AudioClip>>,
        step: f64,
        n_out: usize,
        start: usize,
        frames: usize,
        gain: &mut impl FnMut() -> f32,
        write: &mut impl FnMut(usize, usize, f32),
    ) {
        let Some(s) = sample else {
            return;
        };
        let len = s.len();
        let n_src = s.channels.len();
        for i in start..start + frames {
            let g = gain();
            if !self.playing {
                continue;
            }
            let idx = self.pos as usize;
            if idx + 1 >= len {
                self.playing = false;
                continue;
            }
            let frac = (self.pos - idx as f64) as f32;
            for ch in 0..n_out {
                let src = &s.channels[ch.min(n_src - 1)];
                let v = src[idx] + (src[idx + 1] - src[idx]) * frac;
                write(ch, i, v * self.gain * g);
            }
            self.pos += step;
        }
    }
}
