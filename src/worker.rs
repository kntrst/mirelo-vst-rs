//! Background worker: Mirelo HTTP, file cache, decoding. Never touches the
//! audio thread except by publishing into `Inner::sample`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TryRecvError};

use crate::config;
use crate::decode::decode_file;
use crate::mirelo::{Client, Poll};
use crate::shared::{Command, Inner, SharedString};

const POLL_INTERVAL: Duration = Duration::from_secs(2);
const POLL_TIMEOUT: Duration = Duration::from_secs(600);
const IDLE_TICK: Duration = Duration::from_millis(250);

pub fn spawn(inner: Arc<Inner>, last_file: SharedString) -> Sender<Command> {
    let (tx, rx) = crossbeam_channel::bounded(16);
    let spawned = std::thread::Builder::new()
        .name("mirelo-worker".into())
        .spawn(move || Worker { inner, last_file, rx, loaded: None }.run());
    if let Err(e) = spawned {
        eprintln!("mirelo: failed to start worker thread: {e}");
    }
    tx
}

enum Outcome {
    Done,
    /// A newer command arrived while a job was running.
    Superseded(Command),
    Shutdown,
}

struct Worker {
    inner: Arc<Inner>,
    last_file: SharedString,
    rx: Receiver<Command>,
    loaded: Option<PathBuf>,
}

impl Worker {
    fn run(mut self) {
        let mut pending: Option<Command> = None;
        loop {
            self.inner.drain_graveyard();
            let cmd = match pending.take() {
                Some(c) => c,
                None => match self.rx.recv_timeout(IDLE_TICK) {
                    Ok(c) => c,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
            };
            let outcome = match cmd {
                Command::Generate { prompt, duration_ms, api_key } => {
                    self.generate(&prompt, duration_ms, api_key)
                }
                Command::Reload => {
                    self.reload();
                    Outcome::Done
                }
            };
            match outcome {
                Outcome::Done => {}
                Outcome::Superseded(c) => pending = Some(c),
                Outcome::Shutdown => break,
            }
        }
        self.inner.sample.store(None);
    }

    fn fail(&self, msg: String) {
        self.inner.set_status(|s| {
            s.busy = false;
            s.message.clear();
            s.error = Some(msg);
        });
    }

    fn progress(&self, message: &str, progress: f32) {
        self.inner.set_status(|s| {
            s.busy = true;
            s.error = None;
            s.message = message.to_owned();
            s.progress = progress;
        });
    }

    /// Sleep for `d`, returning early if a command arrives.
    fn wait(&self, d: Duration) -> Option<Outcome> {
        let deadline = Instant::now() + d;
        loop {
            self.inner.drain_graveyard();
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            match self.rx.recv_timeout(left.min(IDLE_TICK)) {
                Ok(c) => return Some(Outcome::Superseded(c)),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return Some(Outcome::Shutdown),
            }
        }
    }

    fn check_cmd(&self) -> Option<Outcome> {
        match self.rx.try_recv() {
            Ok(c) => Some(Outcome::Superseded(c)),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => Some(Outcome::Shutdown),
        }
    }

    fn generate(&mut self, prompt: &str, duration_ms: u32, api_key: String) -> Outcome {
        if api_key.trim().is_empty() {
            self.fail("No API key set".into());
            return Outcome::Done;
        }
        let client = Client::new(api_key.trim().to_owned());

        self.progress("Submitting...", 0.0);
        let job_id = match client.submit(prompt, duration_ms) {
            Ok(id) => id,
            Err(e) => {
                self.fail(e);
                return Outcome::Done;
            }
        };

        let started = Instant::now();
        let file = loop {
            if let Some(o) = self.wait(POLL_INTERVAL) {
                return o;
            }
            match client.poll(&job_id) {
                Ok(Poll::Done(f)) => break f,
                Ok(Poll::Running(p)) => self.progress("Generating...", p.clamp(0.0, 1.0) * 0.9),
                Err(e) => {
                    self.fail(e);
                    return Outcome::Done;
                }
            }
            if started.elapsed() > POLL_TIMEOUT {
                self.fail("Generation timed out".into());
                return Outcome::Done;
            }
        };

        self.progress("Downloading...", 0.9);
        let bytes = match client.download(&file.url) {
            Ok(b) => b,
            Err(e) => {
                self.fail(e);
                return Outcome::Done;
            }
        };
        if let Some(o) = self.check_cmd() {
            return o;
        }

        let path = match save(&bytes, &job_id, file.ext) {
            Ok(p) => p,
            Err(e) => {
                self.fail(format!("Couldn't save file: {e}"));
                return Outcome::Done;
            }
        };
        self.progress("Decoding...", 0.95);
        if self.load(&path) {
            self.last_file.set(path.to_string_lossy().into_owned());
        }
        Outcome::Done
    }

    fn reload(&mut self) {
        let path = PathBuf::from(self.last_file.get());
        if path.as_os_str().is_empty() || self.loaded.as_deref() == Some(path.as_path()) {
            return;
        }
        // The path comes from project state: only read from our own cache dir.
        if !in_samples_dir(&path) {
            self.fail("Saved sample is outside the Mirelo cache folder; ignoring".into());
            return;
        }
        self.progress("Loading saved sample...", 0.95);
        self.load(&path);
    }

    fn load(&mut self, path: &Path) -> bool {
        match decode_file(path) {
            Ok(sample) => {
                self.inner.sample.store(Some(Arc::new(sample)));
                self.loaded = Some(path.to_path_buf());
                self.inner.set_status(|s| {
                    s.busy = false;
                    s.error = None;
                    s.progress = 1.0;
                    s.message = format!(
                        "Ready: {}",
                        path.file_name().map(|n| n.to_string_lossy()).unwrap_or_default()
                    );
                });
                true
            }
            Err(e) => {
                self.fail(format!("Couldn't decode audio: {e}"));
                false
            }
        }
    }
}

fn save(bytes: &[u8], job_id: &str, ext: &str) -> std::io::Result<PathBuf> {
    let dir = config::samples_dir().ok_or_else(|| std::io::Error::other("no data dir"))?;
    std::fs::create_dir_all(&dir)?;
    let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let id: String = job_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(40)
        .collect();
    let path = dir.join(format!("mirelo_{ts}_{id}.{ext}"));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

fn in_samples_dir(path: &Path) -> bool {
    let (Some(dir), Ok(p)) = (config::samples_dir(), path.canonicalize()) else {
        return false;
    };
    dir.canonicalize().is_ok_and(|d| p.starts_with(d))
}
