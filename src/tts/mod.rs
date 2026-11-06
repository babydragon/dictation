pub mod doubao;
pub mod mock;
pub mod qwen;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::Config;
use crate::model::{Lang, ProviderKind};

/// One synthesis request. `text` is already the `speak` text.
#[derive(Debug, Clone)]
pub struct TtsRequest {
    pub text: String,
    pub voice: String,
    pub lang: Lang,
    /// 0.5..=2.0. Providers that do not support native rate should ignore this
    /// and let the caller apply time-stretching.
    pub rate: f32,
}

/// Raw provider response.
#[derive(Debug, Clone)]
pub struct AudioData {
    pub bytes: Vec<u8>,
    pub format: AudioFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Wav,
    PcmS16le { sample_rate: u32 },
    Mp3,
}

#[async_trait]
pub trait TtsProvider: Send + Sync {
    fn id(&self) -> &'static str;

    /// Whether `rate` is honored by the provider itself. If false, the render
    /// pipeline time-stretches the output.
    fn supports_native_rate(&self) -> bool;

    async fn synthesize(&self, req: &TtsRequest) -> Result<AudioData>;
}

pub fn build_provider(kind: ProviderKind, cfg: &Config) -> Result<Arc<dyn TtsProvider>> {
    let provider: Arc<dyn TtsProvider> = match kind {
        ProviderKind::Doubao => Arc::new(doubao::DoubaoProvider::new(cfg.doubao.clone())?),
        ProviderKind::Qwen => Arc::new(qwen::QwenProvider::new(cfg.qwen.clone())?),
        ProviderKind::Mock => Arc::new(mock::MockProvider::new()),
    };
    Ok(provider)
}
