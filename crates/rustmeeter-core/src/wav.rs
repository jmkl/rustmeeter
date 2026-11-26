//! A tiny WAV reader for `transcribe(file.wav)`.
//!
//! The Cactus speech model wants 16 kHz mono `i16`, while the files people hand to an IPC call are
//! ordinary WAVs: 44.1/48 kHz, mono or stereo, PCM or float. This parser handles the common cases
//! and resamples with linear interpolation. It is deliberately small — no dependencies, no
//! streaming — because it exists to feed one-shot transcription, not to be a general audio library.

use anyhow::{Context, Result, bail};
use std::path::Path;

pub const TARGET_RATE: u32 = 16_000;

/// Read a WAV file and return 16 kHz mono samples.
pub fn read_pcm16k(path: &Path) -> Result<Vec<i16>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    parse_wav(&bytes).with_context(|| format!("cannot decode {}", path.display()))
}

/// Parse an in-memory WAV (`RIFF....WAVE`). Exposed for tests.
pub fn parse_wav(bytes: &[u8]) -> Result<Vec<i16>> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }

    let mut format: Option<Fmt> = None;
    let mut data: Option<&[u8]> = None;

    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body_start = at + 8;
        // Chunks are padded to an even length; clamp a truncated final chunk instead of failing.
        let body_end = body_start.saturating_add(size).min(bytes.len());
        let body = &bytes[body_start..body_end];

        match id {
            b"fmt " => format = Some(Fmt::parse(body)?),
            b"data" => data = Some(body),
            _ => {}
        }
        at = body_start + size + (size & 1);
    }

    let format = format.context("WAV has no fmt chunk")?;
    let data = data.context("WAV has no data chunk")?;
    let mono = format.to_mono(data)?;
    Ok(resample(&mono, format.sample_rate, TARGET_RATE))
}

struct Fmt {
    format: u16,
    channels: u16,
    sample_rate: u32,
    bits: u16,
}

impl Fmt {
    fn parse(body: &[u8]) -> Result<Self> {
        if body.len() < 16 {
            bail!("fmt chunk is too short");
        }
        let mut format = u16::from_le_bytes(body[0..2].try_into().unwrap());
        let channels = u16::from_le_bytes(body[2..4].try_into().unwrap());
        let sample_rate = u32::from_le_bytes(body[4..8].try_into().unwrap());
        let bits = u16::from_le_bytes(body[14..16].try_into().unwrap());
        // WAVE_FORMAT_EXTENSIBLE: the real format is the first two bytes of the sub-format GUID.
        if format == 0xFFFE && body.len() >= 26 {
            format = u16::from_le_bytes(body[24..26].try_into().unwrap());
        }
        if channels == 0 {
            bail!("WAV has zero channels");
        }
        if sample_rate == 0 {
            bail!("WAV has a zero sample rate");
        }
        Ok(Self {
            format,
            channels,
            sample_rate,
            bits,
        })
    }

    /// Interleaved samples of all channels -> one mono `f32` channel in -1..1.
    fn to_mono(&self, data: &[u8]) -> Result<Vec<f32>> {
        let channels = self.channels as usize;
        let mut mono = Vec::new();
        let mut frame = Vec::with_capacity(channels);
        let mut at = 0;

        let sample = |at: &mut usize| -> Result<f32> {
            let value = match (self.format, self.bits) {
                (1, 8) => {
                    let v = *data.get(*at).context("truncated 8-bit sample")?;
                    *at += 1;
                    (v as f32 - 128.0) / 128.0
                }
                (1, 16) => {
                    let s = i16::from_le_bytes(
                        data.get(*at..*at + 2)
                            .context("truncated 16-bit sample")?
                            .try_into()
                            .unwrap(),
                    );
                    *at += 2;
                    s as f32 / 32768.0
                }
                (1, 24) => {
                    let b = data.get(*at..*at + 3).context("truncated 24-bit sample")?;
                    *at += 3;
                    // Sign-extend the 24-bit value into an i32.
                    let v = i32::from_le_bytes([b[0], b[1], b[2], 0]) << 8 >> 8;
                    v as f32 / 8_388_608.0
                }
                (1, 32) => {
                    let v = i32::from_le_bytes(
                        data.get(*at..*at + 4)
                            .context("truncated 32-bit sample")?
                            .try_into()
                            .unwrap(),
                    );
                    *at += 4;
                    v as f32 / 2_147_483_648.0
                }
                (3, 32) => {
                    let v = f32::from_le_bytes(
                        data.get(*at..*at + 4)
                            .context("truncated float sample")?
                            .try_into()
                            .unwrap(),
                    );
                    *at += 4;
                    v
                }
                (3, 64) => {
                    let v = f64::from_le_bytes(
                        data.get(*at..*at + 8)
                            .context("truncated double sample")?
                            .try_into()
                            .unwrap(),
                    );
                    *at += 8;
                    v as f32
                }
                (f, b) => bail!("unsupported WAV encoding (format {f}, {b} bits)"),
            };
            Ok(value.clamp(-1.0, 1.0))
        };

        while at + channels * bytes_per_sample(self.format, self.bits) <= data.len() {
            frame.clear();
            for _ in 0..channels {
                frame.push(sample(&mut at)?);
            }
            mono.push(frame.iter().sum::<f32>() / channels as f32);
        }
        Ok(mono)
    }
}

fn bytes_per_sample(format: u16, bits: u16) -> usize {
    match (format, bits) {
        (1, 8) => 1,
        (1, 16) => 2,
        (1, 24) => 3,
        (1, 32) => 4,
        (3, 32) => 4,
        (3, 64) => 8,
        (_, b) => (b as usize).div_ceil(8),
    }
}

/// Linear-interpolation resampler. Good enough for speech; the model itself is far more forgiving
/// than a music-grade SRC.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<i16> {
    if input.is_empty() {
        return vec![];
    }
    let to_i16 = |s: f32| (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
    if from == to {
        return input.iter().copied().map(to_i16).collect();
    }

    let ratio = from as f64 / to as f64;
    let out_len = ((input.len() as f64) / ratio).floor().max(1.0) as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src = i as f64 * ratio;
        let i0 = src.floor() as usize;
        let frac = (src - i0 as f64) as f32;
        let a = input.get(i0).copied().unwrap_or(0.0);
        let b = input.get(i0 + 1).copied().unwrap_or(a);
        out.push(to_i16(a + (b - a) * frac));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest valid WAV: 16-bit mono at 8 kHz with `samples`.
    fn wav16(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        let block_align = channels * 2;
        let mut out = vec![];
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36u32 + bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVE");
        out.extend_from_slice(b"fmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&sample_rate.to_le_bytes());
        out.extend_from_slice(&(sample_rate * block_align as u32).to_le_bytes());
        out.extend_from_slice(&block_align.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
        out
    }

    #[test]
    fn reads_16k_mono_unchanged() {
        let decoded = parse_wav(&wav16(16_000, 1, &[0, 16384, -32768])).unwrap();
        assert_eq!(decoded, vec![0, 16384, -32767]);
    }

    #[test]
    fn mixes_stereo_to_mono() {
        // Two frames: (+1, -1) and (0, 0).
        let decoded = parse_wav(&wav16(16_000, 2, &[32767, -32768, 0, 0])).unwrap();
        assert_eq!(decoded.len(), 2);
        assert_eq!(decoded[1], 0);
    }

    #[test]
    fn resamples_to_16k() {
        let decoded = parse_wav(&wav16(8_000, 1, &[0; 800])).unwrap();
        assert_eq!(decoded.len(), 1600);
    }

    #[test]
    fn rejects_non_wav() {
        assert!(parse_wav(b"not a wave file").is_err());
    }
}
