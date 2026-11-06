use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::model::{Lang, ProviderKind};

pub const DEFAULT_QWEN_ENDPOINT: &str =
    "https://dashscope.aliyuncs.com/api/v1/services/aigc/multimodal-generation/generation";
pub const DEFAULT_DOUBAO_ENDPOINT: &str =
    "https://openspeech.bytedance.com/api/v3/tts/unidirectional";
pub const DEFAULT_DOUBAO_RESOURCE_ID: &str = "seed-tts-2.0";
pub const DEFAULT_DOUBAO_VOICE_ZH: &str = "zh_female_vv_uranus_bigtts";
pub const DEFAULT_DOUBAO_VOICE_EN: &str = "en_female_stokie_uranus_bigtts";
pub const DEFAULT_QWEN_MODEL: &str = "qwen3-tts-flash";
pub const DEFAULT_QWEN_VOICE_ZH: &str = "Cherry";
pub const DEFAULT_QWEN_VOICE_EN: &str = "Aiden";

#[derive(Debug, Clone)]
pub struct QwenConfig {
    pub api_key: Option<String>,
    pub endpoint: String,
    pub model: String,
    pub voice_zh: String,
    pub voice_en: String,
}

#[derive(Debug, Clone)]
pub struct DoubaoConfig {
    /// V3 uses a single API key (`X-Api-Key`); no app_id/token pair.
    pub api_key: Option<String>,
    pub endpoint: String,
    /// `seed-tts-2.0` for system voices, `seed-icl-2.0` for cloned voices.
    pub resource_id: String,
    /// Optional model override, mainly for cloned voices.
    pub model: Option<String>,
    pub voice_zh: Option<String>,
    pub voice_en: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub default_provider: ProviderKind,
    pub cache_dir: PathBuf,
    pub qwen: QwenConfig,
    pub doubao: DoubaoConfig,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let default_provider = std::env::var("DICTATION_PROVIDER")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.parse())
            .transpose()?
            .unwrap_or(ProviderKind::Qwen);

        let cache_dir = std::env::var("DICTATION_CACHE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("cache"));

        Ok(Self {
            default_provider,
            cache_dir,
            qwen: QwenConfig {
                api_key: env_nonempty("DASHSCOPE_API_KEY"),
                endpoint: env_or("QWEN_TTS_ENDPOINT", DEFAULT_QWEN_ENDPOINT),
                model: env_or("QWEN_TTS_MODEL", DEFAULT_QWEN_MODEL),
                voice_zh: env_or("QWEN_VOICE_ZH", DEFAULT_QWEN_VOICE_ZH),
                voice_en: env_or("QWEN_VOICE_EN", DEFAULT_QWEN_VOICE_EN),
            },
            doubao: DoubaoConfig {
                api_key: env_nonempty("DOUBAO_API_KEY"),
                endpoint: env_or("DOUBAO_TTS_ENDPOINT", DEFAULT_DOUBAO_ENDPOINT),
                resource_id: env_or("DOUBAO_RESOURCE_ID", DEFAULT_DOUBAO_RESOURCE_ID),
                model: env_nonempty("DOUBAO_MODEL"),
                voice_zh: Some(env_or("DOUBAO_VOICE_ZH", DEFAULT_DOUBAO_VOICE_ZH)),
                voice_en: Some(env_or("DOUBAO_VOICE_EN", DEFAULT_DOUBAO_VOICE_EN)),
            },
        })
    }

    /// Pick the best voice for a language from the lesson overrides, then env defaults.
    pub fn voice_for(
        &self,
        provider: ProviderKind,
        lang: Lang,
        lesson_zh: Option<&str>,
        lesson_en: Option<&str>,
        lesson_mixed: Option<&str>,
    ) -> Result<String> {
        let explicit = match lang {
            Lang::Zh => lesson_zh,
            Lang::En => lesson_en,
            Lang::Mixed => lesson_mixed.or(lesson_zh),
        };
        if let Some(v) = explicit.filter(|s| !s.trim().is_empty()) {
            return Ok(v.to_string());
        }

        let (zh, en) = match provider {
            ProviderKind::Qwen => (self.qwen.voice_zh.as_str(), self.qwen.voice_en.as_str()),
            ProviderKind::Doubao => (
                self.doubao.voice_zh.as_deref().unwrap_or(""),
                self.doubao.voice_en.as_deref().unwrap_or(""),
            ),
            ProviderKind::Mock => ("mock", "mock"),
        };

        let chosen = match lang {
            Lang::Zh => zh,
            Lang::En => en,
            Lang::Mixed => {
                if zh.is_empty() {
                    en
                } else {
                    zh
                }
            }
        };

        if chosen.trim().is_empty() {
            anyhow::bail!(
                "缺少音色配置：provider={provider:?}, lang={lang:?}。\
                 请在 lesson.toml 或环境变量中设置（豆包需 DOUBAO_VOICE_ZH / DOUBAO_VOICE_EN）"
            );
        }
        Ok(chosen.to_string())
    }
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn env_or(key: &str, default: &str) -> String {
    env_nonempty(key).unwrap_or_else(|| default.to_string())
}

/// Read a required secret with a friendly error.
pub fn require<'a>(value: Option<&'a String>, key: &str) -> Result<&'a str> {
    value
        .map(String::as_str)
        .filter(|s| !s.trim().is_empty())
        .with_context(|| format!("缺少环境变量 {key}；可设置后重试，或使用 --provider mock"))
}
