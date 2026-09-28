use std::path::Path;

use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::{CODEC_TYPE_NULL, DecoderOptions};
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use crate::shared::AudioClip;

const MAX_CHANNELS: usize = 2;

pub fn decode_file(path: &Path) -> Result<AudioClip, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("open failed: {e}"))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("unsupported audio: {e}"))?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or("no audio track")?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.ok_or("unknown sample rate")?;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("unsupported codec: {e}"))?;

    let mut channels: Vec<Vec<f32>> = Vec::new();
    let mut buf: Option<SampleBuffer<f32>> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => return Err(format!("read failed: {e}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(Error::DecodeError(_)) => continue,
            Err(e) => return Err(format!("decode failed: {e}")),
        };
        let spec = *decoded.spec();
        let n_ch = spec.channels.count().max(1);
        if channels.is_empty() {
            channels = vec![Vec::new(); n_ch.min(MAX_CHANNELS)];
        }
        let sb = match &mut buf {
            Some(b) if b.capacity() >= decoded.capacity() * n_ch => b,
            _ => buf.insert(SampleBuffer::new(decoded.capacity() as u64, spec)),
        };
        sb.copy_interleaved_ref(decoded);
        for frame in sb.samples().chunks_exact(n_ch) {
            for (ch, out) in channels.iter_mut().enumerate() {
                out.push(frame[ch]);
            }
        }
    }
    if channels.first().is_none_or(Vec::is_empty) {
        return Err("file contains no audio".into());
    }
    Ok(AudioClip {
        channels,
        sample_rate: f64::from(sample_rate),
    })
}
