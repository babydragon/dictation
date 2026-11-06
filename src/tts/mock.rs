use std::f32::consts::TAU;

use anyhow::Result;
use async_trait::async_trait;

use crate::tts::{AudioData, AudioFormat, TtsProvider, TtsRequest};

/// Per-Han-character duration at rate 1.0.
const HAN_CHAR_MS: u32 = 260;
/// Per-Latin-character duration at rate 1.0.
const LATIN_CHAR_MS: u32 = 90;
/// Never synthesize less than this, even for a single short character.
const MIN_SPEECH_MS: u32 = 400;
/// Leading/trailing silence, so the VAD trim stage has something to cut.
const PAD_MS: u32 = 150;
const FADE_MS: u32 = 5;
/// ~0.35 full scale.
const AMPLITUDE: f32 = 0.35;
const FREQ_MIN_HZ: u32 = 400;
const FREQ_RANGE_HZ: u32 = 401;

/// Deterministic offline provider used by tests and `--provider mock`.
///
/// Emits 24 kHz mono s16le PCM: a sine tone whose frequency is derived from the
/// first character, wrapped in silence. The same request always yields the same
/// bytes, so tests can assert on sample counts and gap positions.
pub struct MockProvider {
    pub sample_rate: u32,
}

impl MockProvider {
    pub fn new() -> Self {
        Self {
            sample_rate: 24_000,
        }
    }

    pub fn with_sample_rate(sample_rate: u32) -> Self {
        Self { sample_rate }
    }
}

impl Default for MockProvider {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TtsProvider for MockProvider {
    fn id(&self) -> &'static str {
        "mock"
    }

    fn supports_native_rate(&self) -> bool {
        true
    }

    async fn synthesize(&self, req: &TtsRequest) -> Result<AudioData> {
        let rate = req.rate.clamp(0.5, 2.0);
        let speech_ms = ((base_duration_ms(&req.text) as f32 / rate).round() as u32).max(1);
        let speech_samples = ms_to_samples(speech_ms, self.sample_rate).max(1);
        let pad_samples = ms_to_samples(PAD_MS, self.sample_rate);

        let total = speech_samples + pad_samples * 2;
        let mut samples = vec![0i16; total];

        let fade_samples = (ms_to_samples(FADE_MS, self.sample_rate)).min(speech_samples / 2);
        let freq = frequency_for(&req.text);
        let step = TAU * freq / self.sample_rate as f32;
        let scale = AMPLITUDE * i16::MAX as f32;
        let mut phase = 0.0f32;

        for (i, slot) in samples[pad_samples..pad_samples + speech_samples]
            .iter_mut()
            .enumerate()
        {
            let gain = fade_gain(i, speech_samples, fade_samples);
            *slot = (gain * scale * phase.sin()).round() as i16;
            phase += step;
            if phase >= TAU {
                phase -= TAU;
            }
        }

        Ok(AudioData {
            bytes: pcm_s16le_bytes(&samples),
            format: AudioFormat::PcmS16le {
                sample_rate: self.sample_rate,
            },
        })
    }
}

/// Rough spoken length before rate scaling.
fn base_duration_ms(text: &str) -> u32 {
    let total: u32 = text
        .chars()
        .map(|c| {
            if is_han(c) {
                HAN_CHAR_MS
            } else {
                LATIN_CHAR_MS
            }
        })
        .sum();
    total.max(MIN_SPEECH_MS)
}

fn is_han(c: char) -> bool {
    matches!(
        c as u32,
        0x3400..=0x4DBF        // CJK Extension A
        | 0x4E00..=0x9FFF       // CJK Unified Ideographs
        | 0xF900..=0xFAFF       // CJK Compatibility Ideographs
        | 0x20000..=0x2FA1F     // CJK Extension B+
    )
}

/// Stable per-text frequency in `400..=800` Hz, from an FNV-1a hash of the first
/// character. Empty text falls back to a fixed middle pitch.
fn frequency_for(text: &str) -> f32 {
    let seed = text.chars().next().map_or(0x4E2D, |c| c as u32); // 0x4E2D = '中'
    let mut hash: u32 = 0x811C_9DC5;
    for byte in seed.to_le_bytes() {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    (FREQ_MIN_HZ + hash % FREQ_RANGE_HZ) as f32
}

/// Raised-cosine fade in/out, so the clip has no click at either edge.
fn fade_gain(i: usize, len: usize, fade: usize) -> f32 {
    if fade == 0 {
        return 1.0;
    }
    if i < fade {
        let x = (i as f32 + 0.5) / fade as f32;
        0.5 - 0.5 * (std::f32::consts::PI * x).cos()
    } else if i + fade >= len {
        let x = ((len - i) as f32 / fade as f32).min(1.0);
        0.5 - 0.5 * (std::f32::consts::PI * x).cos()
    } else {
        1.0
    }
}

fn ms_to_samples(ms: u32, sample_rate: u32) -> usize {
    (u64::from(sample_rate) * u64::from(ms) / 1000) as usize
}

fn pcm_s16le_bytes(samples: &[i16]) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * 2);
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Lang;

    fn request(text: &str, rate: f32) -> TtsRequest {
        TtsRequest {
            text: text.to_string(),
            voice: "mock".to_string(),
            lang: Lang::Zh,
            rate,
        }
    }

    fn samples_of(audio: &AudioData) -> Vec<i16> {
        audio
            .bytes
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]))
            .collect()
    }

    #[tokio::test]
    async fn slower_rate_is_longer() {
        let provider = MockProvider::new();
        let normal = provider.synthesize(&request("摇晃", 1.0)).await.unwrap();
        let slow = provider.synthesize(&request("摇晃", 0.8)).await.unwrap();
        let fast = provider.synthesize(&request("摇晃", 1.5)).await.unwrap();
        assert!(slow.bytes.len() > normal.bytes.len());
        assert!(normal.bytes.len() > fast.bytes.len());
    }

    #[tokio::test]
    async fn length_matches_format_and_pads_with_silence() {
        let provider = MockProvider::new();
        let audio = provider.synthesize(&request("摇晃", 1.0)).await.unwrap();

        assert_eq!(
            audio.format,
            AudioFormat::PcmS16le {
                sample_rate: 24_000
            }
        );
        assert_eq!(audio.bytes.len() % 2, 0);

        let samples = samples_of(&audio);
        assert_eq!(samples.len() * 2, audio.bytes.len());

        // 2 Han chars * 260ms = 520ms of tone, plus 150ms silence on each side.
        let expected = ms_to_samples(520 + PAD_MS * 2, 24_000);
        assert_eq!(samples.len(), expected);

        let pad = ms_to_samples(PAD_MS, 24_000);
        assert!(samples[..pad].iter().all(|s| *s == 0), "缺少前导静音");
        assert!(
            samples[samples.len() - pad..].iter().all(|s| *s == 0),
            "缺少尾部静音"
        );
        assert!(
            samples[pad..samples.len() - pad].iter().any(|s| *s != 0),
            "中间没有音频"
        );

        let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
        assert!(peak > 9_000, "振幅过小: {peak}");
        assert!(
            peak <= (AMPLITUDE * i16::MAX as f32) as u16,
            "振幅过大: {peak}"
        );
    }

    #[tokio::test]
    async fn short_text_uses_minimum_duration() {
        let provider = MockProvider::new();
        let audio = provider.synthesize(&request("a", 1.0)).await.unwrap();
        let samples = samples_of(&audio);
        assert_eq!(
            samples.len(),
            ms_to_samples(MIN_SPEECH_MS + PAD_MS * 2, 24_000)
        );
    }

    #[tokio::test]
    async fn output_is_byte_identical_for_same_request() {
        let provider = MockProvider::new();
        let a = provider.synthesize(&request("apple", 0.9)).await.unwrap();
        let b = provider.synthesize(&request("apple", 0.9)).await.unwrap();
        assert_eq!(a.bytes, b.bytes);

        let other = provider.synthesize(&request("摇晃", 0.9)).await.unwrap();
        assert_ne!(a.bytes.len(), other.bytes.len());
    }

    #[test]
    fn frequency_stays_in_range_and_varies() {
        let mut seen = std::collections::HashSet::new();
        for text in ["摇晃", "粗壮", "毕竟", "apple", "library", "z"] {
            let freq = frequency_for(text);
            assert!((400.0..=800.0).contains(&freq), "{text}: {freq}");
            seen.insert(freq as u32);
        }
        assert!(seen.len() > 1, "不同文本应有不同音高");
    }

    #[test]
    fn fade_edges_are_quiet() {
        assert!(fade_gain(0, 1000, 120) < 0.01);
        assert!(fade_gain(999, 1000, 120) < 0.01);
        assert!((fade_gain(500, 1000, 120) - 1.0).abs() < f32::EPSILON);
        assert_eq!(fade_gain(0, 1000, 0), 1.0);
    }
}
