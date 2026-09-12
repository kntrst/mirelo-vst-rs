use crossbeam_queue::ArrayQueue;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use std::time::Duration;

const WORKER_WAKE_INTERVAL: Duration = Duration::from_millis(100);

use crate::mirelo::sync_api::{Api, DownloadReport};
use crate::mirelo::{
    error::ApiError,
    sync_api::{InMemoryAudioFiles, Request, Response},
};

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

pub fn worker_loop(
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
