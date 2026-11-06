use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::config::{require, QwenConfig};
use crate::model::Lang;
use crate::tts::{AudioData, AudioFormat, TtsProvider, TtsRequest};

/// DashScope rejects texts longer than 600 characters.
const MAX_TEXT_CHARS: usize = 600;
const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RETRIES: u32 = 1;
const RETRY_BASE_DELAY_MS: u64 = 500;
const HTTP_REQUEST_ID_HEADER: &str = "x-dashscope-request-id";

/// 阿里云百炼 DashScope `qwen3-tts-flash`。
///
/// 该模型不支持原生语速参数，`rate` 由 render 层做不变调时长伸缩
/// （AGENTS.md 6.4）；这里只负责按正常语速合成。
#[derive(Debug)]
pub struct QwenProvider {
    cfg: QwenConfig,
    api_key: String,
    client: reqwest::Client,
}

impl QwenProvider {
    pub fn new(cfg: QwenConfig) -> Result<Self> {
        let api_key = require(cfg.api_key.as_ref(), "DASHSCOPE_API_KEY")?.to_string();
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("创建 DashScope TTS HTTP 客户端失败")?;
        Ok(Self {
            cfg,
            api_key,
            client,
        })
    }

    fn build_body(&self, req: &TtsRequest) -> Value {
        json!({
            "model": self.cfg.model,
            "input": {
                "text": req.text,
                "voice": req.voice,
                "language_type": language_type(req.lang),
            },
        })
    }

    async fn post_once(&self, body: &Value) -> Result<RawResponse> {
        let resp = self
            .client
            .post(&self.cfg.endpoint)
            .header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", self.api_key),
            )
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .context("发送 DashScope TTS 请求失败")?;

        let status = resp.status();
        let request_id = resp
            .headers()
            .get(HTTP_REQUEST_ID_HEADER)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned);
        let bytes = resp
            .bytes()
            .await
            .context("读取 DashScope TTS 响应失败")?
            .to_vec();
        Ok(RawResponse {
            status,
            request_id,
            body: bytes,
        })
    }

    /// POST with one retry on network errors or 5xx (exponential backoff).
    async fn post_with_retry(&self, body: &Value) -> Result<RawResponse> {
        let mut attempt = 0u32;
        loop {
            match self.post_once(body).await {
                Ok(raw) if raw.status.is_server_error() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::warn!(
                        status = raw.status.as_u16(),
                        attempt,
                        "DashScope TTS 服务端错误，稍后重试"
                    );
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Ok(raw) => return Ok(raw),
                Err(err) if attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::warn!(error = %err, attempt, "DashScope TTS 网络错误，稍后重试");
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(err) => {
                    return Err(
                        err.context(format!("DashScope TTS 请求失败（已重试 {attempt} 次）"))
                    )
                }
            }
        }
    }

    /// Download the generated audio. The signed URL is public, so no auth header.
    async fn download(&self, url: &str) -> Result<Vec<u8>> {
        let mut attempt = 0u32;
        loop {
            match self.download_once(url).await {
                Ok((status, _)) if status.is_server_error() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::warn!(
                        status = status.as_u16(),
                        attempt,
                        "下载 DashScope 音频失败，稍后重试"
                    );
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Ok((status, bytes)) => {
                    if !status.is_success() {
                        bail!("下载 DashScope 音频失败: HTTP {}", status.as_u16());
                    }
                    if bytes.is_empty() {
                        bail!("下载 DashScope 音频失败: 内容为空");
                    }
                    return Ok(bytes);
                }
                Err(err) if attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::warn!(error = %err, attempt, "下载 DashScope 音频网络错误，稍后重试");
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(err) => {
                    return Err(
                        err.context(format!("下载 DashScope 音频失败（已重试 {attempt} 次）"))
                    )
                }
            }
        }
    }

    async fn download_once(&self, url: &str) -> Result<(reqwest::StatusCode, Vec<u8>)> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .context("请求 DashScope 音频 URL 失败")?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .context("读取 DashScope 音频内容失败")?
            .to_vec();
        Ok((status, bytes))
    }
}

#[async_trait]
impl TtsProvider for QwenProvider {
    fn id(&self) -> &'static str {
        "qwen"
    }

    fn supports_native_rate(&self) -> bool {
        false
    }

    async fn synthesize(&self, req: &TtsRequest) -> Result<AudioData> {
        if req.text.trim().is_empty() {
            bail!("DashScope TTS 拒绝空文本");
        }
        let chars = req.text.chars().count();
        if chars > MAX_TEXT_CHARS {
            bail!(
                "DashScope TTS 单次文本上限 {MAX_TEXT_CHARS} 字符，当前 {chars} 字符；请拆句后重试"
            );
        }

        let raw = self.post_with_retry(&self.build_body(req)).await?;
        let resp: Value = serde_json::from_slice(&raw.body).with_context(|| {
            format!(
                "解析 DashScope TTS 响应失败: request_id={} HTTP {} body={}",
                raw.request_id.as_deref().unwrap_or("-"),
                raw.status.as_u16(),
                snippet(&raw.body)
            )
        })?;

        let request_id = resp
            .get("request_id")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| raw.request_id.clone())
            .unwrap_or_else(|| "-".to_string());

        let audio = resp
            .get("output")
            .and_then(|output| output.get("audio"))
            .filter(|audio| audio.is_object());

        let Some(audio) = audio else {
            bail!(
                "DashScope TTS 未返回音频: request_id={request_id} code={} message={}",
                json_text(resp.get("code")),
                json_text(resp.get("message"))
            );
        };

        match extract_audio_source(audio) {
            Some(AudioSource::Url(url)) => {
                let bytes = self.download(&url).await?;
                if bytes.is_empty() {
                    bail!("DashScope TTS 音频下载为空: request_id={request_id}");
                }
                let format = sniff_format(&bytes);
                Ok(AudioData { bytes, format })
            }
            Some(AudioSource::Data(data)) => {
                audio_from_base64(&data).with_context(|| format!("request_id={request_id}"))
            }
            None => bail!(
                "DashScope TTS 响应里既没有 audio.url 也没有 audio.data: request_id={request_id}"
            ),
        }
    }
}

#[derive(Debug)]
struct RawResponse {
    status: reqwest::StatusCode,
    request_id: Option<String>,
    body: Vec<u8>,
}

/// Where the synthesized audio lives in the DashScope response.
#[derive(Debug, PartialEq, Eq)]
enum AudioSource {
    Url(String),
    Data(String),
}

/// Prefer the signed URL; fall back to inline base64 data.
fn extract_audio_source(audio: &Value) -> Option<AudioSource> {
    if let Some(url) = audio
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        return Some(AudioSource::Url(url.to_string()));
    }
    audio
        .get("data")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|data| !data.is_empty())
        .map(|data| AudioSource::Data(data.to_string()))
}

fn audio_from_base64(data: &str) -> Result<AudioData> {
    let bytes = BASE64
        .decode(data.as_bytes())
        .context("DashScope TTS 返回的 audio.data 不是合法 base64")?;
    if bytes.is_empty() {
        bail!("DashScope TTS 返回的 audio.data 解码后为空");
    }
    let format = sniff_format(&bytes);
    Ok(AudioData { bytes, format })
}

/// DashScope returns wav by default, but we trust the container magic bytes.
fn sniff_format(bytes: &[u8]) -> AudioFormat {
    if bytes.starts_with(b"RIFF") {
        AudioFormat::Wav
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() >= 2 && bytes[0] == 0xFF && (bytes[1] & 0xE0) == 0xE0)
    {
        // MPEG frame sync: 11 set bits. Covers 0xFF 0xFB (MP3, no CRC).
        AudioFormat::Mp3
    } else {
        AudioFormat::Wav
    }
}

fn language_type(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "Chinese",
        Lang::En => "English",
        // Mixed lessons use the bilingual Chinese voice.
        Lang::Mixed => "Chinese",
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(RETRY_BASE_DELAY_MS << (attempt.saturating_sub(1)))
}

fn snippet(bytes: &[u8]) -> String {
    const LIMIT: usize = 200;
    let text = String::from_utf8_lossy(bytes);
    let text = text.trim();
    if text.chars().count() <= LIMIT {
        text.to_string()
    } else {
        format!("{}…", text.chars().take(LIMIT).collect::<String>())
    }
}

/// Render a JSON scalar for error messages, tolerating missing/wrong types.
fn json_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => "-".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_QWEN_ENDPOINT;

    fn config(api_key: Option<&str>) -> QwenConfig {
        QwenConfig {
            api_key: api_key.map(str::to_string),
            endpoint: DEFAULT_QWEN_ENDPOINT.to_string(),
            model: "qwen3-tts-flash".to_string(),
            voice_zh: "Cherry".to_string(),
            voice_en: "Ethan".to_string(),
        }
    }

    fn provider() -> QwenProvider {
        QwenProvider {
            cfg: config(Some("test-key")),
            api_key: "test-key".to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn request(text: &str, lang: Lang) -> TtsRequest {
        TtsRequest {
            text: text.to_string(),
            voice: "Cherry".to_string(),
            lang,
            rate: 1.0,
        }
    }

    #[test]
    fn new_requires_api_key() {
        let err = QwenProvider::new(config(None)).unwrap_err().to_string();
        assert!(err.contains("DASHSCOPE_API_KEY"), "unexpected error: {err}");
    }

    #[test]
    fn build_body_maps_input_fields() {
        let body = provider().build_body(&request("摇晃", Lang::Zh));
        assert_eq!(body["model"], "qwen3-tts-flash");
        assert_eq!(body["input"]["text"], "摇晃");
        assert_eq!(body["input"]["voice"], "Cherry");
        assert_eq!(body["input"]["language_type"], "Chinese");
    }

    #[test]
    fn language_type_mapping() {
        assert_eq!(language_type(Lang::Zh), "Chinese");
        assert_eq!(language_type(Lang::En), "English");
        assert_eq!(language_type(Lang::Mixed), "Chinese");
    }

    #[test]
    fn sniff_format_detects_containers() {
        assert_eq!(
            sniff_format(b"RIFF\x24\x00\x00\x00WAVEfmt "),
            AudioFormat::Wav
        );
        assert_eq!(
            sniff_format(b"ID3\x04\x00\x00\x00\x00\x00\x00"),
            AudioFormat::Mp3
        );
        assert_eq!(sniff_format(&[0xFF, 0xFB, 0x90, 0x00]), AudioFormat::Mp3);
        // Unknown bytes are treated as the DashScope default (wav).
        assert_eq!(sniff_format(b"\x89PNG\r\n\x1a\n"), AudioFormat::Wav);
        assert_eq!(sniff_format(b""), AudioFormat::Wav);
        assert_eq!(sniff_format(&[0xFF]), AudioFormat::Wav);
    }

    #[test]
    fn extract_prefers_url_then_data() {
        let both = json!({
            "url": "https://example.com/a.wav",
            "data": "UklGRg==",
        });
        assert_eq!(
            extract_audio_source(&both),
            Some(AudioSource::Url("https://example.com/a.wav".to_string()))
        );

        let data_only = json!({ "url": "", "data": "UklGRg==" });
        assert_eq!(
            extract_audio_source(&data_only),
            Some(AudioSource::Data("UklGRg==".to_string()))
        );

        assert_eq!(extract_audio_source(&json!({})), None);
        assert_eq!(extract_audio_source(&json!({ "url": "   " })), None);
    }

    #[test]
    fn decodes_inline_base64_audio() {
        // base64("RIFF....WAVE") -> wav container detected from magic bytes.
        let data = BASE64.encode(b"RIFF\x24\x00\x00\x00WAVEfmt ");
        let audio = audio_from_base64(&data).unwrap();
        assert_eq!(audio.format, AudioFormat::Wav);
        assert!(audio.bytes.starts_with(b"RIFF"));

        assert!(audio_from_base64("not base64!!!").is_err());
        assert!(audio_from_base64("").is_err());
    }

    #[tokio::test]
    async fn rejects_text_above_600_chars() {
        let req = request(&"汉".repeat(601), Lang::Zh);
        let err = provider().synthesize(&req).await.unwrap_err().to_string();
        assert!(err.contains("600"), "unexpected error: {err}");
    }

    #[test]
    fn backoff_grows() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_millis(1000));
    }
}
