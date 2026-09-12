use serde::{Deserialize, Serialize};
use std::env;
use std::time::Duration;

use crate::mirelo::error::ApiError;

const PREFLIGHT_URL: &str = "https://api.mirelo.ai/v2/text-to-sfx/v1.6/preflight";
const SUBMIT_URL: &str = "https://api.mirelo.ai/v2/text-to-sfx/v1.6/sync";
const MAX_DOWNLOADED_AUDIO_FILE_BYTES: u64 = 50 * 1024 * 1024;

pub struct Api {
    agent: ureq::Agent,
    token: String,
}

impl Api {
    pub fn new() -> Result<Self, ApiError> {
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

    pub fn preflight_check(&self, prompt: Prompt) -> Result<CostEstimate, ApiError> {
        self.post(PREFLIGHT_URL, prompt)
    }

    pub fn submit(&self, prompt: Prompt) -> Result<Files, ApiError> {
        self.post(SUBMIT_URL, prompt)
    }

    pub fn submit_and_download(&self, prompt: Prompt) -> Result<InMemoryAudioFiles, ApiError> {
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

    pub fn post<T: for<'de> Deserialize<'de>>(
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
pub struct InMemoryAudioFiles {
    pub(crate) files: Vec<InMemoryAudioFile>,
    pub(crate) total_bytes: usize,
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
    pub(crate) file_count: usize,
    pub(crate) total_bytes: usize,
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
