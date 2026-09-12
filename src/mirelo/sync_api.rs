use crossbeam_queue::ArrayQueue;
use serde::{Deserialize, Serialize};
use std::env;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

const PREFLIGHT_URL: &str = "https://api.mirelo.ai/v2/text-to-sfx/v1.6/preflight";
const SUBMIT_URL: &str = "https://api.mirelo.ai/v2/text-to-sfx/v1.6/sync";
const MAX_DOWNLOADED_AUDIO_FILE_BYTES: u64 = 50 * 1024 * 1024;
const WORKER_WAKE_INTERVAL: Duration = Duration::from_millis(100);

/// All failures that cross from the HTTP worker to the editor are rendered as
/// text. The worker must never panic just because the service rejected a
/// request or the editor has closed.
#[derive(Clone, Debug)]
pub struct ApiError(String);

impl ApiError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ApiError {}

impl From<ureq::Error> for ApiError {
    fn from(error: ureq::Error) -> Self {
        Self::new(error.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub struct Api {
    agent: ureq::Agent,
    token: String,
}

pub enum Request {
    Submit(Prompt),
    PreflightCheck(Prompt),
}

#[derive(Serialize)]
pub struct Prompt {
    prompt: String,
    duration_ms: u64,
    #[serde(rename = "loop", skip_serializing_if = "Option::is_none")]
    looping: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    num_samples: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_format: Option<OutputFormat>,
}

impl Prompt {
    pub fn new(prompt: String, duration_ms: u64) -> Self {
        Self {
            prompt,
            duration_ms,
            looping: None,
            num_samples: None,
            seed: None,
            output_format: Some(OutputFormat::Wav),
        }
    }
}

#[allow(dead_code)] // The first UI exposes WAV; the API supports these formats too.
#[derive(Default)]
pub enum OutputFormat {
    #[default]
    Wav,
    Mp3,
    Aac,
    Flac,
}

impl OutputFormat {
    fn as_str(&self) -> &str {
        match self {
            Self::Wav => "wav",
            Self::Mp3 => "mp3",
            Self::Aac => "aac",
            Self::Flac => "flac",
        }
    }
}

impl Serialize for OutputFormat {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Deserialize, Serialize)]
pub struct CostEstimate {
    credits: usize,
    estimated_ms: usize,
}

impl CostEstimate {
    pub fn credits(&self) -> usize {
        self.credits
    }

    pub fn estimated_ms(&self) -> usize {
        self.estimated_ms
    }
}

#[derive(Deserialize, Serialize)]
pub struct Files {
    result_urls: Vec<String>,
}

/// One downloaded audio file. The worker fills this off-thread; the audio
/// callback only receives its ownership and never reads from the network.
#[allow(dead_code)] // Consumed by the upcoming MIDI playback implementation.
pub(crate) struct InMemoryAudioFile {
    source_url: String,
    bytes: Vec<u8>,
}

#[allow(dead_code)] // Consumed by the upcoming MIDI playback implementation.
impl InMemoryAudioFile {
    pub(crate) fn source_url(&self) -> &str {
        &self.source_url
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// The complete result of one Mirelo generation. These are encoded audio
/// bytes for now; decoding into playback-ready sample buffers comes with MIDI
/// playback support.
#[allow(dead_code)] // Consumed by the upcoming MIDI playback implementation.
pub(crate) struct InMemoryAudioFiles {
    files: Vec<InMemoryAudioFile>,
    total_bytes: usize,
}

#[allow(dead_code)] // Consumed by the upcoming MIDI playback implementation.
impl InMemoryAudioFiles {
    pub(crate) fn files(&self) -> &[InMemoryAudioFile] {
        &self.files
    }

    pub(crate) fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

pub struct DownloadReport {
    file_count: usize,
    total_bytes: usize,
}

impl DownloadReport {
    pub fn file_count(&self) -> usize {
        self.file_count
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }
}

pub enum Response {
    PreflightCheck(Result<CostEstimate, ApiError>),
    Submit(Result<DownloadReport, ApiError>),
    WorkerStopped,
}

/// A per-plugin, single-flight bridge between the editor and one dedicated
/// HTTP thread. It is held in a `#[skip]` parameter field, which gives the
/// receiverless Truce editor factory access to it without involving DSP state.
pub struct HttpService {
    request_tx: OnceLock<SyncSender<Request>>,
    response_rx: Mutex<Option<Receiver<Response>>>,
    start_lock: Mutex<()>,
    busy: AtomicBool,
    ready_files: Arc<ArrayQueue<InMemoryAudioFiles>>,
    returned_files: Arc<ArrayQueue<InMemoryAudioFiles>>,
}

impl Default for HttpService {
    fn default() -> Self {
        Self {
            request_tx: OnceLock::new(),
            response_rx: Mutex::new(None),
            start_lock: Mutex::new(()),
            busy: AtomicBool::new(false),
            // One generation can be waiting for the audio callback while the
            // previous generation is returned for worker-thread destruction.
            ready_files: Arc::new(ArrayQueue::new(1)),
            returned_files: Arc::new(ArrayQueue::new(1)),
        }
    }
}

impl HttpService {
    /// Starts the dedicated worker once. This is called from plugin init, and
    /// is also safe as a lazy fallback if an unusual host opens the editor
    /// before it initializes processing.
    pub fn start(&self) -> Result<(), ApiError> {
        if self.request_tx.get().is_some() {
            return Ok(());
        }

        let _start_guard = self
            .start_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.request_tx.get().is_some() {
            return Ok(());
        }

        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (response_tx, response_rx) = mpsc::channel();
        let ready_files = Arc::clone(&self.ready_files);
        let returned_files = Arc::clone(&self.returned_files);
        thread::Builder::new()
            .name("mirelo-http".to_owned())
            .spawn(move || worker_loop(request_rx, response_tx, ready_files, returned_files))
            .map_err(|error| ApiError::new(format!("could not start HTTP worker: {error}")))?;

        let _ = self.request_tx.set(request_tx);
        *self
            .response_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(response_rx);
        Ok(())
    }

    pub fn is_busy(&self) -> bool {
        self.busy.load(Ordering::Acquire)
    }

    /// Enqueue one request without ever waiting for the HTTP worker.
    pub fn send(&self, request: Request) -> Result<(), ApiError> {
        self.start()?;
        if self
            .busy
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(ApiError::new("a Mirelo request is already in progress"));
        }

        let Some(sender) = self.request_tx.get() else {
            self.busy.store(false, Ordering::Release);
            return Err(ApiError::new("HTTP worker is unavailable"));
        };

        match sender.try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => {
                self.busy.store(false, Ordering::Release);
                Err(ApiError::new("HTTP worker is still busy"))
            }
            Err(TrySendError::Disconnected(_)) => {
                self.busy.store(false, Ordering::Release);
                Err(ApiError::new("HTTP worker stopped"))
            }
        }
    }

    /// Polls from the editor frame loop. This never blocks the GUI thread.
    pub fn poll(&self) -> Option<Response> {
        let receiver_guard = self
            .response_rx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let receiver = receiver_guard.as_ref()?;
        match receiver.try_recv() {
            Ok(response) => {
                self.busy.store(false, Ordering::Release);
                Some(response)
            }
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.busy.store(false, Ordering::Release);
                Some(Response::WorkerStopped)
            }
        }
    }

    /// Audio-thread side of the worker-to-audio handoff. `ArrayQueue::pop`
    /// uses no allocation or mutex.
    pub(crate) fn take_ready_files(&self) -> Option<InMemoryAudioFiles> {
        self.ready_files.pop()
    }

    /// Return an old audio buffer to the worker so its Vec allocation is
    /// released off the audio thread. The caller retains the value when the
    /// preallocated queue is temporarily full.
    pub(crate) fn return_files(&self, files: InMemoryAudioFiles) -> Result<(), InMemoryAudioFiles> {
        self.returned_files.push(files)
    }
}

impl Api {
    fn new() -> Result<Self, ApiError> {
        let token = env::var("MIRELO_API_TOKEN")
            .map_err(|_| ApiError::new("set MIRELO_API_TOKEN before sending a request"))?;
        if token.trim().is_empty() {
            return Err(ApiError::new("MIRELO_API_TOKEN is empty"));
        }

        let config = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .timeout_connect(Some(Duration::from_secs(10)))
            .build();
        Ok(Self {
            agent: config.into(),
            token,
        })
    }

    fn preflight_check(&self, prompt: Prompt) -> Result<CostEstimate, ApiError> {
        self.post(PREFLIGHT_URL, prompt)
    }

    fn submit(&self, prompt: Prompt) -> Result<Files, ApiError> {
        self.post(SUBMIT_URL, prompt)
    }

    fn submit_and_download(&self, prompt: Prompt) -> Result<InMemoryAudioFiles, ApiError> {
        let files = self.submit(prompt)?;
        let mut downloaded = Vec::with_capacity(files.result_urls.len());
        let mut total_bytes: usize = 0;

        for source_url in files.result_urls {
            let bytes = self
                .agent
                .get(&source_url)
                .call()?
                .body_mut()
                .with_config()
                .limit(MAX_DOWNLOADED_AUDIO_FILE_BYTES)
                .read_to_vec()?;
            total_bytes = total_bytes
                .checked_add(bytes.len())
                .ok_or_else(|| ApiError::new("downloaded audio is too large to hold in memory"))?;
            downloaded.push(InMemoryAudioFile { source_url, bytes });
        }

        Ok(InMemoryAudioFiles {
            files: downloaded,
            total_bytes,
        })
    }

    fn post<T: for<'de> Deserialize<'de>>(
        &self,
        url: &str,
        request: Prompt,
    ) -> Result<T, ApiError> {
        let body = serde_json::to_string(&request)?;
        let response = self
            .agent
            .post(url)
            .header("Authorization", format!("Bearer {}", self.token))
            .content_type("application/json")
            .send(body)?
            .body_mut()
            .read_to_string()?;
        Ok(serde_json::from_str(&response)?)
    }
}

fn worker_loop(
    request_rx: Receiver<Request>,
    response_tx: mpsc::Sender<Response>,
    ready_files: Arc<ArrayQueue<InMemoryAudioFiles>>,
    returned_files: Arc<ArrayQueue<InMemoryAudioFiles>>,
) {
    // The agent and its connection pool belong solely to this thread.
    let api = Api::new();
    loop {
        // Vec destruction is deliberately kept on this worker, not on the
        // real-time callback that previously owned a completed generation.
        while returned_files.pop().is_some() {}

        let request = match request_rx.recv_timeout(WORKER_WAKE_INTERVAL) {
            Ok(request) => request,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let response = match request {
            Request::Submit(prompt) => {
                let result = api
                    .as_ref()
                    .map_err(Clone::clone)
                    .and_then(|api| api.submit_and_download(prompt));
                Response::Submit(result.map(|files| {
                    let report = DownloadReport {
                        file_count: files.files.len(),
                        total_bytes: files.total_bytes,
                    };
                    // If the audio callback has not consumed a previous
                    // result, replace it here. This drop is worker-thread-only.
                    drop(ready_files.force_push(files));
                    report
                }))
            }
            Request::PreflightCheck(prompt) => Response::PreflightCheck(
                api.as_ref()
                    .map_err(Clone::clone)
                    .and_then(|api| api.preflight_check(prompt)),
            ),
        };
        if response_tx.send(response).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Prompt;

    #[test]
    fn prompt_serializes_duration_as_milliseconds() {
        let prompt = Prompt::new("thunder".to_owned(), 1_250);
        let value = serde_json::to_value(prompt).expect("serialize prompt");

        assert_eq!(value["duration_ms"], 1_250);
        assert_eq!(value["output_format"], "wav");
        assert!(value.get("loop").is_none());
    }
}
