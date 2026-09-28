use std::sync::{Arc, Mutex, OnceLock};

use arc_swap::ArcSwapOption;
use crossbeam_channel::{Sender, TrySendError};
use truce::core::custom_state::{PersistField, StateCursor};

pub struct AudioClip {
    pub channels: Vec<Vec<f32>>,
    pub sample_rate: f64,
}

impl AudioClip {
    pub fn len(&self) -> usize {
        self.channels.first().map_or(0, Vec::len)
    }
}

pub enum Command {
    Generate {
        prompt: String,
        duration_ms: u32,
        api_key: String,
    },
    Reload,
}

#[derive(Clone, Default)]
pub struct Status {
    pub busy: bool,
    pub message: String,
    pub error: Option<String>,
    pub progress: f32,
}

#[derive(Default)]
pub struct Inner {
    pub sample: ArcSwapOption<AudioClip>,
    pub status: Mutex<Status>,
    pub graveyard: Mutex<Option<rtrb::Consumer<Arc<AudioClip>>>>,
}

impl Inner {
    pub fn set_status(&self, f: impl FnOnce(&mut Status)) {
        if let Ok(mut s) = self.status.lock() {
            f(&mut s);
        }
    }

    /// Free samples
    pub fn drain_graveyard(&self) {
        if let Ok(mut g) = self.graveyard.lock()
            && let Some(c) = g.as_mut()
        {
            while c.pop().is_ok() {}
        }
    }
}

#[derive(Default)]
pub struct Shared {
    pub inner: Arc<Inner>,
    pub api_key: Mutex<String>,
    cmd: OnceLock<Sender<Command>>,
}

impl Shared {
    /// Spawn the worker thread. Don't call from audio thread
    pub fn ensure_worker(&self, last_file: &SharedString) {
        self.cmd.get_or_init(|| {
            if let Ok(mut k) = self.api_key.lock() {
                *k = crate::config::load_api_key();
            }
            crate::worker::spawn(self.inner.clone(), last_file.clone())
        });
    }

    pub fn send(&self, cmd: Command) -> bool {
        match self.cmd.get() {
            Some(tx) => !matches!(tx.try_send(cmd), Err(TrySendError::Disconnected(_))),
            None => false,
        }
    }
}

#[derive(Default, Clone)]
pub struct SharedString(pub Arc<Mutex<String>>);

impl SharedString {
    pub fn get(&self) -> String {
        self.0.lock().map(|s| s.clone()).unwrap_or_default()
    }
    pub fn set(&self, v: String) {
        if let Ok(mut s) = self.0.lock() {
            *s = v;
        }
    }
}

impl PersistField for SharedString {
    fn persist_write(&self, buf: &mut Vec<u8>) {
        self.0.persist_write(buf);
    }
    fn persist_read(&self, cursor: &mut StateCursor) {
        self.0.persist_read(cursor);
    }
}
