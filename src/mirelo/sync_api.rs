#![allow(unused)] // TODO

use serde::{Deserialize, Serialize};

use std::sync::mpsc::{Receiver, Sender};
use std::{thread, time::Duration};

const TOKEN: &str = "<API-TOKEN>";

pub struct Api(ureq::Agent);

pub enum RequestType {
    Submit,
    PreflightCheck,
}
pub struct Request {
    request_type: RequestType,
    prompt: Prompt,
}

#[derive(Serialize)]
pub struct Prompt {
    prompt: String,
    duration_ms: Duration,
    #[serde(rename = "loop")]
    looping: Option<bool>,
    num_samples: Option<usize>,
    seed: Option<i32>,
    output_format: Option<OutputFormat>,
}

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
            OutputFormat::Wav => "wav",
            OutputFormat::Mp3 => "mp3",
            OutputFormat::Aac => "aac",
            OutputFormat::Flac => "flac",
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

#[derive(Deserialize, Serialize)]
pub struct Files {
    result_urls: Vec<String>,
}

pub enum Response {
    PreflightCheck(CostEstimate),
    Submit(Files),
}

impl Api {
    fn new() -> Self {
        Api(ureq::agent())
    }
    fn inner(&self) -> &ureq::Agent {
        &self.0
    }

    pub fn preflight_check(&self, r: Prompt) -> Result<CostEstimate, ureq::Error> {
        let r_string = serde_json::to_string(&r).unwrap();
        let response = self
            .inner()
            .post("https://api.mirelo.ai/v2/text-to-sfx/v1.6/preflight")
            .header("Authorization", format!("Bearer sk-{}", TOKEN))
            .content_type("application/json")
            .send(r_string)?
            .body_mut()
            .read_to_string()?;
        let estimate = serde_json::from_str(&response).unwrap();
        Ok(estimate)
    }
    pub fn submit(&self, r: Prompt) -> Result<Files, ureq::Error> {
        let r_string = serde_json::to_string(&r).unwrap();
        let response = self
            .inner()
            .post("https://api.mirelo.ai/v2/text-to-sfx/v1.6/sync")
            .header("Authorization", format!("Bearer sk-{}", TOKEN))
            .content_type("application/json")
            .send(r_string)?
            .body_mut()
            .read_to_string()?;
        let file_urls = serde_json::from_str(&response).unwrap();
        Ok(file_urls)
    }
}

pub fn spawn() -> (Sender<Request>, Receiver<Response>, thread::JoinHandle<()>) {
    let (tx, rx): (Sender<Request>, Receiver<Request>) = std::sync::mpsc::channel();
    let (tx_files, rx_files): (Sender<Response>, Receiver<Response>) = std::sync::mpsc::channel();
    let handler = thread::spawn(move || {
        let client = Api::new();
        loop {
            if let Ok(msg) = rx.recv() {
                let response = match msg.request_type {
                    RequestType::Submit => {
                        let result = client.submit(msg.prompt).unwrap();
                        Response::Submit(result)
                    }
                    RequestType::PreflightCheck => {
                        let result = client.preflight_check(msg.prompt).unwrap();
                        Response::PreflightCheck(result)
                    }
                };
                tx_files.send(response).unwrap();
            }
        }
    });
    (tx, rx_files, handler)
}
