use std::fs;
use std::io::{Cursor, Read};
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context, Result};

/// Canonical internal sample rate.
pub const SAMPLE_RATE: u32 = 24_000;

/// VAD frame length in milliseconds.
const FRAME_MS: u32 = 10;
/// Leading silence kept before the first voiced frame.
const LEAD_PADDING_MS: u32 = 20;
/// Trailing silence kept after the last voiced frame.
const TAIL_PADDING_MS: u32 = 60;
/// Minimum clip length.
const MIN_CLIP_MS: u32 = 100;
/// Fade length at both ends.
const FADE_MS: u32 = 5;
/// VAD energy threshold.
const VAD_THRESHOLD_DBFS: f64 = -45.0;
/// Loudness target for the active clip (RMS, including the short padding).
///
/// Peak normalization alone made Chinese voices (~0.15 RMS) and English voices
/// (~0.095 RMS) audibly different; RMS balancing fixes the perceived level.
const TARGET_RMS_DBFS: f64 = -19.0;
/// Clips already this quiet are treated as (near) silence and left alone.
const SILENCE_FLOOR_DBFS: f64 = -50.0;
/// Absolute ceiling after gain, so loud material cannot clip.
const PEAK_CEILING_DBFS: f64 = -1.5;

/// Read a mono WAV file into i16 samples. Multi-channel input is downmixed.
pub fn read_wav_mono(path: &Path) -> Result<Vec<i16>> {
    let file =
        fs::File::open(path).with_context(|| format!("打开 WAV 失败: {}", path.display()))?;
    let reader = hound::WavReader::new(std::io::BufReader::new(file))
        .with_context(|| format!("解析 WAV 失败: {}", path.display()))?;
    let (samples, _) = decode_reader(reader)?;
    Ok(samples)
}

/// Write mono i16 samples as a 16-bit PCM WAV.
pub fn write_wav_mono(path: &Path, samples: &[i16], sample_rate: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
    }
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec)
        .with_context(|| format!("创建 WAV 失败: {}", path.display()))?;
    for &sample in samples {
        writer.write_sample(sample)?;
    }
    writer
        .finalize()
        .with_context(|| format!("写入 WAV 失败: {}", path.display()))?;
    Ok(())
}

/// Decode WAV bytes into mono samples plus the source sample rate.
pub fn decode_wav(bytes: &[u8]) -> Result<(Vec<i16>, u32)> {
    let reader = hound::WavReader::new(Cursor::new(bytes)).context("解析 WAV 数据失败")?;
    decode_reader(reader)
}

fn decode_reader<R: Read>(mut reader: hound::WavReader<R>) -> Result<(Vec<i16>, u32)> {
    let spec = reader.spec();
    let channels = usize::from(spec.channels.max(1));
    let bits = spec.bits_per_sample;
    if bits == 0 || bits > 32 {
        bail!("不支持的 WAV 位深: {bits}");
    }

    let values: Vec<f64> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .map(|sample| sample.map(f64::from))
            .collect::<std::result::Result<Vec<_>, _>>()
            .context("读取 WAV 浮点采样失败")?,
        hound::SampleFormat::Int => {
            let scale = 2f64.powi(i32::from(bits) - 1);
            reader
                .samples::<i32>()
                .map(|sample| sample.map(|v| f64::from(v) / scale))
                .collect::<std::result::Result<Vec<_>, _>>()
                .context("读取 WAV 整数采样失败")?
        }
    };

    let mut samples = Vec::with_capacity(values.len() / channels + 1);
    for frame in values.chunks(channels) {
        if frame.is_empty() {
            continue;
        }
        let mixed = frame.iter().sum::<f64>() / frame.len() as f64;
        samples.push(normalized_to_i16(mixed));
    }
    Ok((samples, spec.sample_rate))
}

fn normalized_to_i16(value: f64) -> i16 {
    (value * 32_768.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}

/// Linear resampling. Good enough for 16k -> 24k speech upsampling.
pub fn resample_linear(samples: &[i16], from: u32, to: u32) -> Vec<i16> {
    if samples.is_empty() || from == 0 || to == 0 || from == to {
        return samples.to_vec();
    }
    let out_len =
        ((samples.len() as u64 * u64::from(to) + u64::from(from) / 2) / u64::from(from)) as usize;
    let out_len = out_len.max(1);
    let step = f64::from(from) / f64::from(to);
    let last = samples.len() - 1;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let position = i as f64 * step;
        let left = position.floor() as usize;
        let frac = position - left as f64;
        let right = (left + 1).min(last);
        let value =
            f64::from(samples[left.min(last)]) * (1.0 - frac) + f64::from(samples[right]) * frac;
        out.push(
            value
                .round()
                .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16,
        );
    }
    out
}

/// DC removal, VAD trim, fades, and RMS loudness normalization.
pub fn process_clip(mut samples: Vec<i16>) -> Result<Vec<i16>> {
    if samples.is_empty() {
        return Ok(samples);
    }

    // 1. Remove DC offset.
    let mean = samples.iter().map(|&s| f64::from(s)).sum::<f64>() / samples.len() as f64;
    for sample in &mut samples {
        let value = f64::from(*sample) - mean;
        *sample = value
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    }

    // 2. Energy VAD on 10 ms frames.
    let frame_len = samples_per_ms(FRAME_MS) as usize;
    let threshold = 10f64.powf(VAD_THRESHOLD_DBFS / 20.0);
    let mut first_voiced: Option<usize> = None;
    let mut last_voiced: Option<usize> = None;
    for (frame_index, frame) in samples.chunks(frame_len).enumerate() {
        if frame.is_empty() {
            continue;
        }
        let rms = (frame
            .iter()
            .map(|&s| {
                let value = f64::from(s) / 32_768.0;
                value * value
            })
            .sum::<f64>()
            / frame.len() as f64)
            .sqrt();
        if rms >= threshold {
            first_voiced.get_or_insert(frame_index);
            last_voiced = Some(frame_index);
        }
    }

    let (Some(first), Some(last)) = (first_voiced, last_voiced) else {
        // 完全静音：返回 100ms 静音，调用方不会 panic。
        return Ok(vec![0; samples_per_ms(MIN_CLIP_MS) as usize]);
    };

    let start = (first * frame_len).saturating_sub(samples_per_ms(LEAD_PADDING_MS) as usize);
    let end =
        (((last + 1) * frame_len) + samples_per_ms(TAIL_PADDING_MS) as usize).min(samples.len());
    let mut trimmed = samples[start..end].to_vec();

    let min_len = samples_per_ms(MIN_CLIP_MS) as usize;
    if trimmed.len() < min_len {
        trimmed.resize(min_len, 0);
    }

    apply_fade(&mut trimmed);
    normalize_rms(&mut trimmed);
    Ok(trimmed)
}

fn apply_fade(samples: &mut [i16]) {
    let fade = (samples_per_ms(FADE_MS) as usize).min(samples.len() / 2);
    if fade == 0 {
        return;
    }
    for i in 0..fade {
        let gain = 0.5 - 0.5 * (std::f32::consts::PI * i as f32 / fade as f32).cos();
        let head = (f64::from(samples[i]) * f64::from(gain)).round();
        samples[i] = head.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        let tail_index = samples.len() - 1 - i;
        let tail = (f64::from(samples[tail_index]) * f64::from(gain)).round();
        samples[tail_index] = tail.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    }
}

/// Balance the whole clip (padding included) to `TARGET_RMS_DBFS`, then make
/// sure the resulting peak stays below `PEAK_CEILING_DBFS`.
fn normalize_rms(samples: &mut [i16]) {
    let rms = normalized_rms(samples);
    if rms <= 0.0 {
        return;
    }
    let rms_db = 20.0 * rms.log10();
    if rms_db < SILENCE_FLOOR_DBFS {
        return;
    }

    let target = 10f64.powf(TARGET_RMS_DBFS / 20.0);
    let mut gain = target / rms;

    let peak = samples
        .iter()
        .map(|s| f64::from(i32::from(*s).abs()) / 32_768.0)
        .fold(0.0_f64, f64::max);
    if peak > 0.0 {
        let ceiling = 10f64.powf(PEAK_CEILING_DBFS / 20.0);
        if peak * gain > ceiling {
            gain = ceiling / peak;
        }
    }

    for sample in samples.iter_mut() {
        let value = f64::from(*sample) * gain;
        *sample = value
            .round()
            .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
    }
}

fn normalized_rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    (samples
        .iter()
        .map(|&s| {
            let value = f64::from(s) / 32_768.0;
            value * value
        })
        .sum::<f64>()
        / samples.len() as f64)
        .sqrt()
}

/// Exactly `ms` milliseconds of silence at the canonical sample rate.
pub fn silence_ms(ms: u32) -> Vec<i16> {
    vec![0; samples_per_ms(ms) as usize]
}

pub fn concat(parts: &[&[i16]]) -> Vec<i16> {
    let total: usize = parts.iter().map(|part| part.len()).sum();
    let mut out = Vec::with_capacity(total);
    for part in parts {
        out.extend_from_slice(part);
    }
    out
}

pub fn duration_ms(samples: &[i16]) -> u32 {
    let rounded =
        (samples.len() as u64 * 1_000 + u64::from(SAMPLE_RATE) / 2) / u64::from(SAMPLE_RATE);
    rounded.min(u64::from(u32::MAX)) as u32
}

/// Pitch-preserving time stretch. Uses ffmpeg `atempo` when available.
pub fn time_stretch(samples: &[i16], rate: f32) -> Result<Vec<i16>> {
    if samples.is_empty() || (rate - 1.0).abs() < 0.01 {
        return Ok(samples.to_vec());
    }
    if !(0.5..=2.0).contains(&rate) {
        bail!("语速 {rate} 超出 atempo 支持范围 0.5–2.0");
    }

    let temp_dir = std::env::temp_dir();
    let id = uuid::Uuid::new_v4();
    let input = temp_dir.join(format!("dictation-stretch-{id}-in.wav"));
    let output = temp_dir.join(format!("dictation-stretch-{id}-out.wav"));

    let result = (|| -> Result<Vec<i16>> {
        write_wav_mono(&input, samples, SAMPLE_RATE)?;
        let args = vec![
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-y".to_string(),
            "-i".to_string(),
            input.to_string_lossy().into_owned(),
            "-filter:a".to_string(),
            format!("atempo={rate:.4}"),
            "-ar".to_string(),
            SAMPLE_RATE.to_string(),
            "-ac".to_string(),
            "1".to_string(),
            "-c:a".to_string(),
            "pcm_s16le".to_string(),
            output.to_string_lossy().into_owned(),
        ];
        let command = Command::new("ffmpeg").args(&args).output();
        match command {
            Ok(out) if out.status.success() => read_wav_mono(&output),
            Ok(out) => bail!(
                "ffmpeg 时长伸缩失败: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                bail!("未找到 ffmpeg：无法对语速做不变调时长伸缩")
            }
            Err(err) => Err(err).context("调用 ffmpeg 失败"),
        }
    })();

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
    result
}

/// Encode a WAV file to MP3 with ffmpeg. Returns false when ffmpeg is missing.
pub fn encode_mp3(wav_path: &Path, mp3_path: &Path) -> Result<bool> {
    let args = vec![
        "-hide_banner".to_string(),
        "-loglevel".to_string(),
        "error".to_string(),
        "-y".to_string(),
        "-i".to_string(),
        wav_path.to_string_lossy().into_owned(),
        "-codec:a".to_string(),
        "libmp3lame".to_string(),
        "-b:a".to_string(),
        "96k".to_string(),
        mp3_path.to_string_lossy().into_owned(),
    ];
    match Command::new("ffmpeg").args(&args).output() {
        Ok(out) if out.status.success() => Ok(true),
        Ok(out) => {
            tracing::warn!(
                "ffmpeg 编码 MP3 失败，保留 WAV: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
            Ok(false)
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!("未找到 ffmpeg，跳过 MP3 导出，保留 WAV");
            Ok(false)
        }
        Err(err) => Err(err).context("调用 ffmpeg 失败"),
    }
}

fn samples_per_ms(ms: u32) -> u32 {
    ms.saturating_mul(SAMPLE_RATE) / 1_000
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(freq: f64, ms: u32, amplitude: f64) -> Vec<i16> {
        sine_at(freq, ms, amplitude, SAMPLE_RATE)
    }

    fn sine_at(freq: f64, ms: u32, amplitude: f64, sample_rate: u32) -> Vec<i16> {
        let count = u64::from(ms) * u64::from(sample_rate) / 1_000;
        (0..count as usize)
            .map(|i| {
                let t = i as f64 / f64::from(sample_rate);
                (amplitude * (2.0 * std::f64::consts::PI * freq * t).sin() * 32_767.0).round()
                    as i16
            })
            .collect()
    }

    fn peak(samples: &[i16]) -> i32 {
        samples
            .iter()
            .map(|s| i32::from(*s).abs())
            .max()
            .unwrap_or(0)
    }

    fn rms_db(samples: &[i16]) -> f64 {
        20.0 * normalized_rms(samples).log10()
    }

    /// -1.5 dBFS expressed as an absolute i16 peak, rounded up for tolerance.
    const PEAK_CEILING: i32 = 27_574;

    #[test]
    fn silence_is_exact() {
        assert_eq!(silence_ms(1_000).len(), 24_000);
        assert_eq!(silence_ms(1).len(), 24);
        assert_eq!(silence_ms(0).len(), 0);
        assert!(silence_ms(500).iter().all(|&s| s == 0));
    }

    #[test]
    fn concat_and_duration() {
        let a = vec![0i16; 24_000];
        let b = vec![0i16; 12_000];
        let joined = concat(&[&a, &b, &[]]);
        assert_eq!(joined.len(), 36_000);
        assert_eq!(duration_ms(&joined), 1_500);
        assert_eq!(duration_ms(&[]), 0);
    }

    #[test]
    fn resample_16k_to_24k() {
        let samples = sine_at(440.0, 100, 0.5, 16_000);
        let up = resample_linear(&samples, 16_000, 24_000);
        assert_eq!(samples.len(), 1_600);
        assert_eq!(up.len(), 2_400);
        assert_eq!(resample_linear(&samples, 24_000, 24_000), samples);
        assert_eq!(resample_linear(&[], 16_000, 24_000), Vec::<i16>::new());
        assert_eq!(resample_linear(&samples, 0, 24_000), samples);
    }

    #[test]
    fn resample_then_duration_is_stable() {
        let samples = vec![1i16; 44_100];
        let out = resample_linear(&samples, 44_100, 24_000);
        assert_eq!(out.len(), 24_000);
    }

    #[test]
    fn process_clip_trims_padding_and_balances_rms() {
        let mut input = silence_ms(100);
        input.extend(sine(440.0, 200, 0.25));
        input.extend(silence_ms(100));
        let original_len = input.len();

        let processed = process_clip(input).unwrap();
        assert!(processed.len() < original_len, "should trim silence");
        // 200ms voiced + 20ms lead + 60ms tail ≈ 280ms.
        assert!(
            (5_000..=7_500).contains(&processed.len()),
            "unexpected length {}",
            processed.len()
        );
        // Whole-clip RMS lands on the -19 dBFS target, peak stays below -1.5 dBFS.
        let rms = rms_db(&processed);
        assert!((rms - (-19.0)).abs() < 0.5, "unexpected RMS {rms} dBFS");
        assert!(
            peak(&processed) <= PEAK_CEILING,
            "peak {}",
            peak(&processed)
        );
        // Fades: first and last sample are (nearly) silent.
        assert!(processed[0].abs() < 50);
        assert!(processed[processed.len() - 1].abs() < 50);
    }

    #[test]
    fn rms_normalization_balances_quiet_and_loud_speech() {
        let quiet = process_clip(sine(440.0, 200, 0.1)).unwrap();
        let loud = process_clip(sine(440.0, 200, 0.3)).unwrap();

        let quiet_db = rms_db(&quiet);
        let loud_db = rms_db(&loud);
        assert!(
            (quiet_db - loud_db).abs() < 1.0,
            "quiet {quiet_db} dBFS vs loud {loud_db} dBFS 差超过 1 dB"
        );
        for (name, samples) in [("quiet", &quiet), ("loud", &loud)] {
            let db = rms_db(samples);
            assert!(
                (db - (-19.0)).abs() < 0.5,
                "{name} RMS {db} dBFS 未到 -19 dBFS"
            );
            assert!(
                peak(samples) <= PEAK_CEILING,
                "{name} peak {} 超过 -1.5 dBFS",
                peak(samples)
            );
        }
    }

    #[test]
    fn rms_gain_respects_peak_ceiling() {
        // A very peaky signal: hitting the RMS target would clip, so the gain
        // must be reduced until the peak is at the -1.5 dBFS ceiling.
        let mut input = silence_ms(50);
        input.push(32_767);
        input.extend(silence_ms(50));
        let processed = process_clip(input).unwrap();

        assert!(
            peak(&processed) <= PEAK_CEILING,
            "peak {} 超过 ceiling",
            peak(&processed)
        );
        assert!(
            rms_db(&processed) < -19.0,
            "被峰值限制后 RMS 应低于目标: {}",
            rms_db(&processed)
        );
    }

    #[test]
    fn near_silence_skips_gain() {
        // 10ms blip in heavy padding: voiced enough for VAD, but whole-clip RMS
        // stays below the -50 dBFS floor, so it must not be boosted.
        let mut input = silence_ms(500);
        input.extend(sine(440.0, 10, 0.01));
        input.extend(silence_ms(500));
        let processed = process_clip(input).unwrap();
        assert!(
            rms_db(&processed) < -50.0,
            "unexpected RMS {}",
            rms_db(&processed)
        );
        assert!(
            peak(&processed) <= 330,
            "近静音被放大了: peak {}",
            peak(&processed)
        );
    }

    #[test]
    fn process_clip_handles_total_silence() {
        let out = process_clip(silence_ms(500)).unwrap();
        assert_eq!(out.len(), samples_per_ms(MIN_CLIP_MS) as usize);
        assert!(out.iter().all(|&s| s == 0));

        let empty = process_clip(Vec::new()).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn process_clip_shortens_min_length() {
        let out = process_clip(sine(440.0, 30, 0.4)).unwrap();
        assert_eq!(out.len(), samples_per_ms(MIN_CLIP_MS) as usize);
    }

    #[test]
    fn wav_roundtrip_downmixes_stereo() {
        let dir = std::env::temp_dir().join(format!("dictation-audio-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stereo.wav");

        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..100 {
            writer.write_sample(10_000i16).unwrap();
            writer.write_sample(-10_000i16).unwrap();
        }
        writer.finalize().unwrap();

        let samples = read_wav_mono(&path).unwrap();
        assert_eq!(samples.len(), 100);
        assert!(samples.iter().all(|&s| s == 0));

        let bytes = fs::read(&path).unwrap();
        let (decoded, rate) = decode_wav(&bytes).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(decoded, samples);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_wav_is_readable() {
        let dir = std::env::temp_dir().join(format!("dictation-audio-{}", uuid::Uuid::new_v4()));
        let path = dir.join("nested/mono.wav");
        write_wav_mono(&path, &[0, 1, -1, 32_767, -32_768], SAMPLE_RATE).unwrap();
        let read = read_wav_mono(&path).unwrap();
        assert_eq!(read, vec![0, 1, -1, 32_767, -32_768]);
        assert_eq!(duration_ms(&read), 0);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn time_stretch_noop_near_one() {
        let samples = sine(440.0, 100, 0.4);
        assert_eq!(time_stretch(&samples, 1.0).unwrap(), samples);
        assert_eq!(time_stretch(&samples, 1.005).unwrap(), samples);
    }
}
