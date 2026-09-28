//! Mirelo v3 text-to-sfx client (blocking; worker thread only).
//! Flow mirrors mirelo-ai/reaper: submit -> poll -> download presigned URL.

use std::time::Duration;

use serde_json::{Value, json};
use ureq::Agent;

const BASE: &str = "https://api.mirelo.ai";
const MODEL: &str = "sfx-1.6";
const MAX_DOWNLOAD_BYTES: u64 = 100 * 1024 * 1024;

pub struct Client {
    agent: Agent,
    api_key: String,
}

/// A finished job's audio file.
pub struct AudioFile {
    pub url: String,
    pub ext: &'static str,
}

pub enum Poll {
    Running(f32),
    Done(AudioFile),
}

impl Client {
    pub fn new(api_key: String) -> Self {
        let agent: Agent = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(60)))
            .http_status_as_error(false)
            .build()
            .into();
        Self { agent, api_key }
    }

    pub fn submit(&self, prompt: &str, duration_ms: u32) -> Result<String, String> {
        let body = json!({
            "model": MODEL,
            "duration_ms": duration_ms,
            "num_variants": 1,
            "input": { "prompt": prompt },
            "output": {"format": "wav" }
        });
        let header_txt = format!("Bearer sk-{}", self.api_key);
        println!("{}", &header_txt);
        let resp = self
            .agent
            .post(format!("{BASE}/v3/text-to-sfx/generations"))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .content_type("application/json")
            .send_json(&body);
        let data = json_result(resp)?;
        data.get("id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .ok_or_else(|| "Mirelo returned no job id".to_owned())
    }

    pub fn poll(&self, job_id: &str) -> Result<Poll, String> {
        if !job_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err("invalid job id".to_owned());
        }
        let resp = self
            .agent
            .get(format!("{BASE}/v3/text-to-sfx/generations/{job_id}"))
            .header("Authorization", format!("Bearer sk-{}", self.api_key))
            .call();
        let data = json_result(resp)?;
        let status = data.get("status").and_then(Value::as_str).unwrap_or("");
        match status {
            "succeeded" | "partially_succeeded" => first_audio(&data)
                .map(Poll::Done)
                .ok_or_else(|| job_failure(&data).unwrap_or("job succeeded but returned no audio".into())),
            "failed" => Err(job_failure(&data).unwrap_or("generation failed".into())),
            "canceled" => Err(job_failure(&data).unwrap_or("the generation was canceled".into())),
            "expired" => Err(job_failure(&data).unwrap_or("the generation expired".into())),
            _ if data.get("completed_at").is_some_and(Value::is_string) => Err(job_failure(&data)
                .unwrap_or_else(|| format!("generation ended in unknown state '{status}'"))),
            _ => Ok(Poll::Running(
                data.get("progress").and_then(Value::as_f64).unwrap_or(0.0) as f32,
            )),
        }
    }

    pub fn download(&self, url: &str) -> Result<Vec<u8>, String> {
        if !url.starts_with("https://") {
            return Err("refusing non-https download URL".to_owned());
        }
        let mut resp = self
            .agent
            .get(url)
            .config()
            .timeout_global(Some(Duration::from_secs(300)))
            .build()
            .call()
            .map_err(|e| format!("download failed: {e}"))?;
        if !resp.status().is_success() {
            return Err(format!("download failed (HTTP {})", resp.status().as_u16()));
        }
        resp.body_mut()
            .with_config()
            .limit(MAX_DOWNLOAD_BYTES)
            .read_to_vec()
            .map_err(|e| format!("download failed: {e}"))
    }
}

fn json_result(resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Value, String> {
    let mut resp = resp.map_err(|e| format!("Couldn't reach Mirelo: {e}"))?;
    let status = resp.status().as_u16();
    let data: Option<Value> = resp.body_mut().read_json().ok();
    if (200..300).contains(&status) {
        return data.ok_or_else(|| "Mirelo returned an unreadable response".to_owned());
    }
    let msg = data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Err(match (status, msg) {
        (401 | 403, m) => format!("Authentication failed - check your API key{}", m.map(|m| format!(" ({m})")).unwrap_or_default()),
        (_, Some(m)) => m,
        (s, None) => format!("Mirelo request failed (HTTP {s})"),
    })
}

fn job_failure(data: &Value) -> Option<String> {
    data.get("errors")?
        .get(0)?
        .get("message")?
        .as_str()
        .map(str::to_owned)
}

/// get first audio file in json result
fn first_audio(data: &Value) -> Option<AudioFile> {
    let result = data.get("result")?;
    let outputs: Vec<&Value> = match result.get("outputs").and_then(Value::as_array) {
        Some(list) => list.iter().collect(),
        None => vec![result.get("output")?],
    };
    outputs
        .iter()
        .filter_map(|o| o.get("variants")?.as_array())
        .flatten()
        .filter(|v| v.get("status").and_then(Value::as_str) == Some("succeeded"))
        .find_map(|v| {
            let audio = v.get("files")?.get("audio")?;
            let url = audio.get("url")?.as_str()?.to_owned();
            let ext = format_ext(audio.get("format").and_then(Value::as_str));
            Some(AudioFile { url, ext })
        })
}

/// `mp3_320` -> `mp3`; restricted to a whitelist since it names a file on disk.
fn format_ext(format: Option<&str>) -> &'static str {
    match format.and_then(|f| f.split('_').next()) {
        Some("flac") => "flac",
        Some("mp3") => "mp3",
        Some("m4a") => "m4a",
        Some("ogg") => "ogg",
        _ => "wav",
    }
}
