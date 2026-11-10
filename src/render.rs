use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

use crate::audio;
use crate::cache::{Cache, CacheMeta, PIPELINE_VERSION};
use crate::config::Config;
use crate::manifest::{self, Manifest, ManifestClip, ManifestDefaults, ManifestItem};
use crate::model::{Lang, Lesson, ProviderKind};
use crate::plan;
use crate::tts::{AudioData, AudioFormat, TtsProvider, TtsRequest};

#[derive(Debug, Clone)]
pub struct RenderOptions {
    pub out_dir: PathBuf,
    pub full_track: bool,
}

#[derive(Debug, Clone)]
pub struct RenderResult {
    pub manifest_path: PathBuf,
    pub full_track: Option<PathBuf>,
    pub total_ms: u32,
    pub tts_calls: usize,
    pub cache_hits: usize,
}

/// A clip that has been written to `out_dir/clips`, plus its samples so the
/// full track can be assembled without re-reading from disk.
struct Materialized {
    clip: ManifestClip,
    samples: Vec<i16>,
}

#[derive(Default)]
struct Counters {
    tts_calls: usize,
    cache_hits: usize,
}

/// Synthesize all missing clips, assemble the full track, and write manifest.json.
pub async fn render(
    lesson: &Lesson,
    provider: &dyn TtsProvider,
    cache: &Cache,
    config: &Config,
    opts: &RenderOptions,
) -> Result<RenderResult> {
    // 自定义 provider（例如测试内的本地 mock）可能用任意 id；音色默认值按 mock 处理，
    // 显式写在 lesson.toml / CLI 里的音色仍然优先生效。
    let kind: ProviderKind = provider.id().parse().unwrap_or_else(|_| {
        tracing::debug!(
            "未知 provider id `{}`，按 mock 解析音色默认值",
            provider.id()
        );
        ProviderKind::Mock
    });

    let clips_dir = opts.out_dir.join("clips");
    fs::create_dir_all(&clips_dir)
        .with_context(|| format!("创建输出目录失败: {}", clips_dir.display()))?;

    let mut counters = Counters::default();
    let mut written: HashMap<String, ManifestClip> = HashMap::new();
    let mut measured_clips: HashMap<usize, u32> = HashMap::new();
    let mut measured_announces: HashMap<usize, u32> = HashMap::new();
    let mut items: Vec<ManifestItem> = Vec::with_capacity(lesson.items.len());
    let mut timeline: Vec<Vec<i16>> = vec![audio::silence_ms(lesson.config.lead_in_ms)];

    // 开场白：lead_in 之后、第一个词之前。
    let intro_text = lesson.intro_text().map(str::to_string);
    let mut intro_ms = 0u32;
    let intro = if let Some(text) = intro_text.as_deref() {
        let lang = lesson.intro_lang();
        let voice = config.voice_for(
            kind,
            lang,
            lesson.config.voice_zh.as_deref(),
            lesson.config.voice_en.as_deref(),
            lesson.config.voice_mixed.as_deref(),
        )?;
        let (key, samples) = ensure_clip(
            provider,
            cache,
            &voice,
            lesson.config.rate,
            text,
            lang,
            &mut counters,
        )
        .await
        .context("生成开场白失败")?;
        let intro = materialize(&opts.out_dir, &mut written, &key, &samples, "intro")?;
        intro_ms = intro.clip.duration_ms;
        timeline.push(intro.samples.clone());
        timeline.push(audio::silence_ms(lesson.config.intro_gap_ms));
        Some(intro)
    } else {
        None
    };

    for item in &lesson.items {
        let rate = lesson.rate_for(item);
        let voice = config.voice_for(
            kind,
            item.lang,
            lesson.config.voice_zh.as_deref(),
            lesson.config.voice_en.as_deref(),
            lesson.config.voice_mixed.as_deref(),
        )?;
        let (key, samples) = ensure_clip(
            provider,
            cache,
            &voice,
            rate,
            item.tts_text(),
            item.lang,
            &mut counters,
        )
        .await
        .with_context(|| format!("处理第 {} 条「{}」失败", item.index, item.text))?;
        let clip = materialize(
            &opts.out_dir,
            &mut written,
            &key,
            &samples,
            &format!("{:02}", item.index),
        )?;
        measured_clips.insert(item.index, clip.clip.duration_ms);

        let announce = if lesson.config.announce_index {
            let announce_text = format!("第{}个", chinese_index(item.index));
            let announce_voice = config.voice_for(
                kind,
                Lang::Zh,
                lesson.config.voice_zh.as_deref(),
                lesson.config.voice_en.as_deref(),
                lesson.config.voice_mixed.as_deref(),
            )?;
            let (announce_key, announce_samples) = ensure_clip(
                provider,
                cache,
                &announce_voice,
                lesson.config.rate,
                &announce_text,
                Lang::Zh,
                &mut counters,
            )
            .await
            .with_context(|| format!("生成题号播报失败（第 {} 条）", item.index))?;
            let announce = materialize(
                &opts.out_dir,
                &mut written,
                &announce_key,
                &announce_samples,
                &format!("idx-{:02}", item.index),
            )?;
            measured_announces.insert(item.index, announce.clip.duration_ms);
            Some(announce)
        } else {
            None
        };

        if let Some(announce) = &announce {
            timeline.push(announce.samples.clone());
        }
        let repeat = lesson.repeat_for(item);
        for round in 0..repeat {
            if round > 0 {
                timeline.push(audio::silence_ms(lesson.config.repeat_gap_ms));
            }
            timeline.push(clip.samples.clone());
        }
        timeline.push(audio::silence_ms(lesson.gap_for(item)));

        items.push(ManifestItem {
            index: item.index,
            text: item.text.clone(),
            lang: item.lang,
            announce: announce.map(|a| a.clip),
            clips: vec![clip.clip],
            repeat,
            repeat_gap_ms: lesson.config.repeat_gap_ms,
            gap_ms: lesson.gap_for(item),
        });
    }

    // 结束语：最后一个词条的 gap 之后，不再额外插入静音。
    let outro_text = lesson.outro_text().map(str::to_string);
    let mut outro_ms = 0u32;
    let outro = if let Some(text) = outro_text.as_deref() {
        let lang = lesson.outro_lang();
        let voice = config.voice_for(
            kind,
            lang,
            lesson.config.voice_zh.as_deref(),
            lesson.config.voice_en.as_deref(),
            lesson.config.voice_mixed.as_deref(),
        )?;
        let (key, samples) = ensure_clip(
            provider,
            cache,
            &voice,
            lesson.config.rate,
            text,
            lang,
            &mut counters,
        )
        .await
        .context("生成结束语失败")?;
        let outro = materialize(&opts.out_dir, &mut written, &key, &samples, "outro")?;
        outro_ms = outro.clip.duration_ms;
        timeline.push(outro.samples.clone());
        Some(outro)
    } else {
        None
    };

    let total_ms = plan::build(
        lesson,
        &measured_clips,
        &measured_announces,
        intro_ms,
        outro_ms,
    )
    .total_ms;

    let full_samples = audio::concat(&timeline.iter().map(Vec::as_slice).collect::<Vec<_>>());
    let full_wav = opts.out_dir.join("full.wav");
    audio::write_wav_mono(&full_wav, &full_samples, audio::SAMPLE_RATE)?;

    let full_track = if opts.full_track {
        let mp3_path = opts.out_dir.join("full.mp3");
        if audio::encode_mp3(&full_wav, &mp3_path)? {
            Some(mp3_path)
        } else {
            Some(full_wav.clone())
        }
    } else {
        Some(full_wav.clone())
    };

    let manifest = Manifest {
        version: manifest::MANIFEST_VERSION,
        title: lesson.title.clone(),
        subject: Some(lesson.config.subject),
        grade: lesson.config.grade,
        term: lesson.config.term,
        review_cap: lesson.config.review_cap,
        generated_at: now_rfc3339(),
        defaults: ManifestDefaults {
            gap_ms: lesson.config.gap_ms,
            repeat: lesson.config.repeat,
            repeat_gap_ms: lesson.config.repeat_gap_ms,
            lead_in_ms: lesson.config.lead_in_ms,
            intro_gap_ms: lesson.config.intro_gap_ms,
        },
        intro: intro.map(|intro| intro.clip),
        intro_text,
        outro: outro.map(|outro| outro.clip),
        outro_text,
        items,
        full_track: full_track.as_ref().map(|path| {
            path.strip_prefix(&opts.out_dir)
                .unwrap_or(path)
                .to_string_lossy()
                .replace('\\', "/")
        }),
        total_ms,
    };
    let manifest_path = opts.out_dir.join("manifest.json");
    manifest::write(&manifest_path, &manifest)?;

    Ok(RenderResult {
        manifest_path,
        full_track,
        total_ms,
        tts_calls: counters.tts_calls,
        cache_hits: counters.cache_hits,
    })
}

/// Return a cached clip or synthesize, normalize, and cache a new one.
async fn ensure_clip(
    provider: &dyn TtsProvider,
    cache: &Cache,
    voice: &str,
    rate: f32,
    text: &str,
    lang: Lang,
    counters: &mut Counters,
) -> Result<(String, Vec<i16>)> {
    let key = Cache::key(provider.id(), voice, rate, text);
    if let Some(cached) = cache.get(&key)? {
        counters.cache_hits += 1;
        return Ok((key, cached.samples));
    }

    tracing::info!("TTS 合成: 「{text}」 voice={voice} rate={rate}");
    let request = TtsRequest {
        text: text.to_string(),
        voice: voice.to_string(),
        lang,
        rate,
    };
    let data = provider.synthesize(&request).await?;
    let (samples, sample_rate) = decode_audio(&data)?;
    let mut samples = if sample_rate == audio::SAMPLE_RATE {
        samples
    } else {
        audio::resample_linear(&samples, sample_rate, audio::SAMPLE_RATE)
    };

    if !provider.supports_native_rate() && (rate - 1.0).abs() >= 0.01 {
        samples = audio::time_stretch(&samples, rate)?;
    }

    let samples = audio::process_clip(samples)?;
    let meta = CacheMeta {
        duration_ms: audio::duration_ms(&samples),
        sample_rate: audio::SAMPLE_RATE,
        provider: provider.id().to_string(),
        voice: voice.to_string(),
        rate,
        text: text.to_string(),
        pipeline_version: PIPELINE_VERSION,
    };
    cache.put(&key, &samples, &meta)?;
    counters.tts_calls += 1;
    Ok((key, samples))
}

fn materialize(
    out_dir: &Path,
    written: &mut HashMap<String, ManifestClip>,
    key: &str,
    samples: &[i16],
    prefix: &str,
) -> Result<Materialized> {
    if let Some(existing) = written.get(key) {
        return Ok(Materialized {
            clip: existing.clone(),
            samples: samples.to_vec(),
        });
    }

    let hash8 = &key[..key.len().min(8)];
    let relative = format!("clips/{prefix}-{hash8}.wav");
    audio::write_wav_mono(&out_dir.join(&relative), samples, audio::SAMPLE_RATE)?;
    let clip = ManifestClip {
        path: relative,
        duration_ms: audio::duration_ms(samples),
    };
    written.insert(key.to_string(), clip.clone());
    Ok(Materialized {
        clip,
        samples: samples.to_vec(),
    })
}

fn decode_audio(data: &AudioData) -> Result<(Vec<i16>, u32)> {
    match data.format {
        AudioFormat::Wav => audio::decode_wav(&data.bytes),
        AudioFormat::PcmS16le { sample_rate } => {
            let samples = data
                .bytes
                .chunks_exact(2)
                .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
                .collect();
            Ok((samples, sample_rate))
        }
        AudioFormat::Mp3 => decode_mp3(&data.bytes),
    }
}

fn decode_mp3(bytes: &[u8]) -> Result<(Vec<i16>, u32)> {
    let temp_dir = std::env::temp_dir();
    let id = uuid::Uuid::new_v4();
    let input = temp_dir.join(format!("dictation-decode-{id}.mp3"));
    let output = temp_dir.join(format!("dictation-decode-{id}.wav"));

    let result = (|| -> Result<(Vec<i16>, u32)> {
        fs::write(&input, bytes)
            .with_context(|| format!("写临时 MP3 失败: {}", input.display()))?;
        let args = vec![
            "-hide_banner".to_string(),
            "-loglevel".to_string(),
            "error".to_string(),
            "-y".to_string(),
            "-i".to_string(),
            input.to_string_lossy().into_owned(),
            "-ac".to_string(),
            "1".to_string(),
            "-ar".to_string(),
            audio::SAMPLE_RATE.to_string(),
            "-c:a".to_string(),
            "pcm_s16le".to_string(),
            output.to_string_lossy().into_owned(),
        ];
        let command = std::process::Command::new("ffmpeg").args(&args).output();
        match command {
            Ok(out) if out.status.success() => {
                Ok((audio::read_wav_mono(&output)?, audio::SAMPLE_RATE))
            }
            Ok(out) => Err(anyhow!(
                "ffmpeg 解码 MP3 失败: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            )),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                Err(anyhow!("未找到 ffmpeg：无法解码 MP3 音频"))
            }
            Err(err) => Err(err).context("调用 ffmpeg 失败"),
        }
    })();

    let _ = fs::remove_file(&input);
    let _ = fs::remove_file(&output);
    result
}

/// 1 -> 一, 10 -> 十, 21 -> 二十一. Used for the spoken item index so TTS
/// never has to guess how to read an Arabic numeral.
fn chinese_index(n: usize) -> String {
    const DIGITS: [&str; 10] = ["零", "一", "二", "三", "四", "五", "六", "七", "八", "九"];
    match n {
        0..=9 => DIGITS[n].to_string(),
        10..=19 => {
            if n == 10 {
                "十".to_string()
            } else {
                format!("十{}", DIGITS[n % 10])
            }
        }
        20..=99 => {
            if n % 10 == 0 {
                format!("{}十", DIGITS[n / 10])
            } else {
                format!("{}十{}", DIGITS[n / 10], DIGITS[n % 10])
            }
        }
        _ => n.to_string(),
    }
}

fn now_rfc3339() -> String {
    let offset = time::UtcOffset::from_hms(8, 0, 0).unwrap_or(time::UtcOffset::UTC);
    let now = time::OffsetDateTime::now_utc().to_offset(offset);
    now.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| now.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use crate::cache::Cache;
    use crate::lesson::parse_str;

    /// Local mock provider: a tone whose length depends on the text length.
    /// Deliberately independent from `tts::mock` so the two can evolve apart.
    struct TestProvider {
        calls: AtomicUsize,
        sample_rate: u32,
    }

    impl TestProvider {
        fn new(sample_rate: u32) -> Self {
            Self {
                calls: AtomicUsize::new(0),
                sample_rate,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl TtsProvider for TestProvider {
        fn id(&self) -> &'static str {
            "mock"
        }

        fn supports_native_rate(&self) -> bool {
            true
        }

        async fn synthesize(&self, req: &TtsRequest) -> Result<AudioData> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let sample_rate = self.sample_rate;
            let tone_ms = 150 + 50 * req.text.chars().count().max(1) as u32;
            let mut samples: Vec<i16> = vec![0; (sample_rate / 20) as usize]; // 50ms leading silence
            let tone_samples = sample_rate as usize * tone_ms as usize / 1_000;
            for i in 0..tone_samples {
                let t = i as f64 / f64::from(sample_rate);
                let value = 0.4 * (2.0 * std::f64::consts::PI * 440.0 * t).sin();
                samples.push((value * 32_767.0).round() as i16);
            }
            samples.resize(samples.len() + (sample_rate / 20) as usize, 0); // tail

            let spec = hound::WavSpec {
                channels: 1,
                sample_rate,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            };
            let mut bytes = Vec::new();
            {
                let cursor = Cursor::new(&mut bytes);
                let mut writer = hound::WavWriter::new(cursor, spec)?;
                for sample in &samples {
                    writer.write_sample(*sample)?;
                }
                writer.finalize()?;
            }
            Ok(AudioData {
                bytes,
                format: AudioFormat::Wav,
            })
        }
    }

    fn test_lesson(gap_ms: u32) -> Lesson {
        let mut lesson = parse_str(
            "摇晃\n粗壮\n",
            Path::new("/tmp/dictation-render-test.txt"),
            None,
        )
        .unwrap();
        lesson.config.lead_in_ms = 200;
        lesson.config.gap_ms = gap_ms;
        lesson.config.repeat = 1;
        lesson.config.repeat_gap_ms = 0;
        lesson.config.announce_index = false;
        lesson.config.intro = Some(String::new()); // 开场白单独测
        lesson.config.outro = Some(String::new()); // 结束语单独测
        lesson.config.rate = 1.0;
        lesson
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("dictation-render-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config() -> Config {
        let mut config = Config {
            default_provider: ProviderKind::Mock,
            cache_dir: PathBuf::from("cache"),
            qwen: crate::config::QwenConfig {
                api_key: None,
                endpoint: String::new(),
                model: String::new(),
                voice_zh: String::new(),
                voice_en: String::new(),
            },
            doubao: crate::config::DoubaoConfig {
                api_key: None,
                endpoint: String::new(),
                resource_id: "seed-tts-2.0".to_string(),
                model: None,
                voice_zh: None,
                voice_en: None,
            },
        };
        config.cache_dir = PathBuf::from("cache");
        config
    }

    fn full_ms_matches(total_ms: u32, samples: &[i16]) -> bool {
        audio::duration_ms(samples).abs_diff(total_ms) <= 2
    }

    fn frame_rms(samples: &[i16]) -> f64 {
        if samples.is_empty() {
            return 0.0;
        }
        (samples
            .iter()
            .map(|&s| {
                let v = f64::from(s) / 32_768.0;
                v * v
            })
            .sum::<f64>()
            / samples.len() as f64)
            .sqrt()
    }

    /// Contiguous 10ms frames whose RMS is above -60 dBFS.
    fn voiced_regions(samples: &[i16]) -> Vec<(usize, usize)> {
        const FRAME: usize = 240;
        let mut regions = Vec::new();
        let mut start: Option<usize> = None;
        for (index, frame) in samples.chunks(FRAME).enumerate() {
            let voiced = frame_rms(frame) > 0.001;
            match (voiced, start) {
                (true, None) => start = Some(index * FRAME),
                (false, Some(begin)) => {
                    regions.push((begin, index * FRAME));
                    start = None;
                }
                _ => {}
            }
        }
        if let Some(begin) = start {
            regions.push((begin, samples.len()));
        }
        regions
    }

    #[test]
    fn chinese_index_covers_lesson_range() {
        assert_eq!(chinese_index(1), "一");
        assert_eq!(chinese_index(9), "九");
        assert_eq!(chinese_index(10), "十");
        assert_eq!(chinese_index(11), "十一");
        assert_eq!(chinese_index(20), "二十");
        assert_eq!(chinese_index(21), "二十一");
        assert_eq!(chinese_index(99), "九十九");
    }

    #[tokio::test]
    async fn renders_manifest_and_precise_gap() {
        let dir = temp_dir("gap");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(16_000); // exercise 16k -> 24k resampling
        let lesson = test_lesson(1_000);

        let result = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.tts_calls, 2);
        assert_eq!(result.cache_hits, 0);
        assert!(result.manifest_path.exists());
        assert!(out_dir.join("full.wav").exists());

        let raw = fs::read_to_string(&result.manifest_path).unwrap();
        let manifest: Manifest = serde_json::from_str(&raw).unwrap();
        assert_eq!(manifest.version, 1);
        assert_eq!(manifest.title, lesson.title);
        assert_eq!(manifest.items.len(), 2);
        assert_eq!(manifest.items[0].lang, Lang::Zh);
        assert_eq!(manifest.items[0].gap_ms, 1_000);
        assert_eq!(manifest.defaults.lead_in_ms, 200);
        assert_eq!(manifest.full_track.as_deref(), Some("full.wav"));
        assert!(manifest.intro.is_none());
        assert!(manifest.intro_text.is_none());
        assert!(manifest.outro.is_none());
        assert!(manifest.outro_text.is_none());
        for item in &manifest.items {
            let path = out_dir.join(&item.clips[0].path);
            assert!(path.exists(), "缺少片段: {}", path.display());
            assert!(item.clips[0].duration_ms > 0);
        }

        let full = audio::read_wav_mono(&out_dir.join("full.wav")).unwrap();
        let full_ms = audio::duration_ms(&full);
        assert!(
            full_ms.abs_diff(result.total_ms) <= 2,
            "manifest 总时长 {}ms 与 full.wav {}ms 不一致",
            result.total_ms,
            full_ms
        );

        // Find the silence run between the two voiced regions. The clip pipeline
        // deliberately keeps 20ms lead / 60ms tail padding, so the audible zero
        // run is gap + 80ms.
        let regions = voiced_regions(&full);
        assert_eq!(regions.len(), 2, "应有两条独立的朗读区间: {regions:?}");
        let silence_samples = regions[1].0 - regions[0].1;
        let silence_ms = silence_samples as f64 * 1_000.0 / 24_000.0;
        assert!(
            (silence_ms - 1_080.0).abs() < 20.0,
            "词间静音 {silence_ms}ms 与预期 1080ms（gap 1000ms + 80ms 裁剪留白）误差超过 20ms"
        );

        // The inserted gap itself is exactly `gap_ms` of zero samples.
        let clip1 = manifest.items[0].clips[0].duration_ms;
        let gap_start = (200 + clip1) as usize * 24;
        let gap_length = 1_000 * 24;
        assert!(
            full[gap_start..gap_start + gap_length]
                .iter()
                .all(|&s| s == 0),
            "gap 区域应全部是零采样"
        );

        // The gap ends where the next clip begins (accounting for rounding).
        let next_clip_start = gap_start + gap_length;
        assert!(
            next_clip_start.abs_diff(regions[1].0) < 24 * 35,
            "第二条应紧接 gap 开始"
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn intro_is_spoken_first_spaced_and_cached() {
        use crate::model::DEFAULT_INTRO_ZH;

        let dir = temp_dir("intro");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        let mut lesson = test_lesson(1_000);
        lesson.config.intro = None; // 恢复默认自动开场白
        lesson.config.intro_gap_ms = 300;

        let result = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.tts_calls, 3, "开场白 + 两个词");
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&result.manifest_path).unwrap()).unwrap();
        let intro = manifest.intro.as_ref().expect("manifest 应有 intro");
        assert!(out_dir.join(&intro.path).exists());
        assert!(intro.path.starts_with("clips/intro-"));
        assert!(intro.duration_ms > 0);
        assert_eq!(manifest.intro_text.as_deref(), Some(DEFAULT_INTRO_ZH));
        assert_eq!(manifest.defaults.intro_gap_ms, 300);
        assert!(manifest.items.iter().all(|item| item.announce.is_none()));

        // 总时长 = lead_in + intro + intro_gap + Σ items。
        let items_total: u32 = manifest
            .items
            .iter()
            .map(|item| {
                let repeat = u32::from(item.repeat.max(1));
                item.clips[0].duration_ms * repeat + item.repeat_gap_ms * (repeat - 1) + item.gap_ms
            })
            .sum();
        assert_eq!(result.total_ms, 200 + intro.duration_ms + 300 + items_total);

        let full = audio::read_wav_mono(&out_dir.join("full.wav")).unwrap();
        assert!(full_ms_matches(result.total_ms, &full));
        let regions = voiced_regions(&full);
        assert_eq!(regions.len(), 3, "开场白 + 两个词: {regions:?}");

        // 开场白紧跟在 lead_in 之后。
        let intro_offset_ms = regions[0].0 as f64 / 24.0;
        assert!(
            (intro_offset_ms - 200.0).abs() < 30.0,
            "开场白应从 ~200ms 开始，实际 {intro_offset_ms}ms"
        );
        // 开场白与第一词之间：intro_gap + 80ms 裁剪留白。
        let gap_ms = (regions[1].0 - regions[0].1) as f64 / 24.0;
        assert!(
            (gap_ms - 380.0).abs() < 20.0,
            "开场白后静音 {gap_ms}ms，预期 380ms"
        );

        // 第二次渲染全部命中缓存，包括开场白。
        let second = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(second.tts_calls, 0);
        assert_eq!(second.cache_hits, 3);
        assert_eq!(provider.calls(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn outro_follows_last_gap_and_cached() {
        use crate::model::DEFAULT_OUTRO_ZH;

        let dir = temp_dir("outro");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        let mut lesson = test_lesson(1_000);
        lesson.config.outro = None; // 恢复默认自动结束语

        let result = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.tts_calls, 3, "两个词 + 结束语");
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&result.manifest_path).unwrap()).unwrap();
        let outro = manifest.outro.as_ref().expect("manifest 应有 outro");
        assert!(out_dir.join(&outro.path).exists());
        assert!(outro.path.starts_with("clips/outro-"));
        assert!(outro.duration_ms > 0);
        assert_eq!(manifest.outro_text.as_deref(), Some(DEFAULT_OUTRO_ZH));
        assert!(manifest.intro.is_none());

        // 总时长 = lead_in + Σ items + outro（无 intro/intro_gap）。
        let items_total: u32 = manifest
            .items
            .iter()
            .map(|item| {
                let repeat = u32::from(item.repeat.max(1));
                item.clips[0].duration_ms * repeat + item.repeat_gap_ms * (repeat - 1) + item.gap_ms
            })
            .sum();
        assert_eq!(result.total_ms, 200 + items_total + outro.duration_ms);

        let full = audio::read_wav_mono(&out_dir.join("full.wav")).unwrap();
        assert!(full_ms_matches(result.total_ms, &full));
        let regions = voiced_regions(&full);
        assert_eq!(regions.len(), 3, "两个词 + 结束语: {regions:?}");

        // 最后一个词之后的 gap 结束才播结束语。
        let last_gap_ms = (regions[2].0 - regions[1].1) as f64 / 24.0;
        assert!(
            (last_gap_ms - 1_080.0).abs() < 20.0,
            "最后一个词后的静音 {last_gap_ms}ms，预期 1080ms（gap 1000 + 80 留白）"
        );
        // 结束语就是最后一段音频，后面只剩 60ms 尾音留白。
        let tail_ms = (full.len() - regions[2].1) as f64 / 24.0;
        assert!(
            tail_ms <= 80.0,
            "结束语后不应再有额外静音，实际 {tail_ms}ms"
        );

        // 第二次渲染全部命中缓存，包括结束语。
        let second = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(second.tts_calls, 0);
        assert_eq!(second.cache_hits, 3);
        assert_eq!(provider.calls(), 3);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn outro_disabled_adds_nothing() {
        let dir = temp_dir("outro-off");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        // test_lesson 默认关闭结束语。
        let lesson = test_lesson(1_000);

        let result = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&result.manifest_path).unwrap()).unwrap();
        assert!(manifest.outro.is_none());
        assert!(manifest.outro_text.is_none());
        let wavs: Vec<_> = fs::read_dir(out_dir.join("clips"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
            .collect();
        assert_eq!(wavs.len(), 2, "关闭结束语后不应写 outro 片段");
        assert!(
            wavs.iter().all(|path| {
                !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("outro-"))
            }),
            "不应存在 outro 片段: {wavs:?}"
        );

        let items_total: u32 = manifest
            .items
            .iter()
            .map(|item| item.clips[0].duration_ms * u32::from(item.repeat.max(1)) + item.gap_ms)
            .sum();
        assert_eq!(result.total_ms, 200 + items_total);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn second_render_hits_cache() {
        let dir = temp_dir("cache");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        let lesson = test_lesson(500);

        let first = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(first.tts_calls, 2);

        let second = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(second.tts_calls, 0, "第二次渲染不应再调用 TTS");
        assert_eq!(second.cache_hits, 2);
        assert_eq!(provider.calls(), 2);
        assert_eq!(first.total_ms, second.total_ms);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn announce_clips_are_written() {
        let dir = temp_dir("announce");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        let mut lesson = test_lesson(300);
        lesson.config.announce_index = true;

        let result = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        assert_eq!(result.tts_calls, 4, "两条词 + 两条题号");
        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&result.manifest_path).unwrap()).unwrap();
        for item in &manifest.items {
            let announce = item.announce.as_ref().expect("应生成题号播报");
            assert!(out_dir.join(&announce.path).exists());
            assert!(announce.duration_ms > 0);
        }

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn deduplicates_shared_clips_on_disk() {
        let dir = temp_dir("dedupe");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        let mut lesson = test_lesson(300);
        // Same word twice is rejected by the parser, but different display text
        // with the same speak text must reuse one file.
        lesson.items[1].speak = Some("摇晃".to_string());

        render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        let wavs: Vec<_> = fs::read_dir(out_dir.join("clips"))
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|ext| ext == "wav"))
            .collect();
        assert_eq!(wavs.len(), 1, "相同缓存键只应写一个片段文件");

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn manifest_carries_review_cap() {
        let dir = temp_dir("review-cap");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        let mut lesson = test_lesson(1_000);
        lesson.config.review_cap = 2;

        let result = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&result.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.review_cap, 2);

        let _ = fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn manifest_carries_subject_grade_and_term() {
        use crate::model::Subject;

        let dir = temp_dir("meta");
        let out_dir = dir.join("out");
        let cache = Cache::new(dir.join("cache"));
        let provider = TestProvider::new(24_000);
        let mut lesson = test_lesson(1_000);
        lesson.config.subject = Subject::English;
        lesson.config.grade = Some(3);
        lesson.config.term = Some(2);

        let result = render(
            &lesson,
            &provider,
            &cache,
            &config(),
            &RenderOptions {
                out_dir: out_dir.clone(),
                full_track: false,
            },
        )
        .await
        .unwrap();

        let manifest: Manifest =
            serde_json::from_str(&fs::read_to_string(&result.manifest_path).unwrap()).unwrap();
        assert_eq!(manifest.subject(), Subject::English);
        assert_eq!(manifest.grade, Some(3));
        assert_eq!(manifest.term, Some(2));

        let _ = fs::remove_dir_all(&dir);
    }
}
