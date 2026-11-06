use std::fmt;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::config::{require, DoubaoConfig};
use crate::tts::{AudioData, AudioFormat, TtsProvider, TtsRequest};

/// V3 `unidirectional` streaming endpoint. PCM is requested at the same rate the
/// rest of the pipeline uses, so no resampling is needed.
const SAMPLE_RATE: u32 = 24_000;
/// A single lesson item is capped at 200 chars upstream; 2000 is plenty of room.
const MAX_TEXT_CHARS: usize = 2000;
const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_RETRIES: u32 = 1;
const RETRY_BASE_DELAY_MS: u64 = 500;

const HEADER_API_KEY: &str = "X-Api-Key";
const HEADER_RESOURCE_ID: &str = "X-Api-Resource-Id";
const HEADER_REQUEST_ID: &str = "X-Api-Request-Id";
const LOG_ID_HEADERS: [&str; 2] = ["X-Tt-Logid", "X-Api-Logid"];

/// Status code of an audio chunk.
const CODE_AUDIO: i64 = 0;
/// Status code of the terminal chunk (success, no audio payload).
const CODE_FINISHED: i64 = 20_000_000;

/// 火山引擎豆包语音合成 V3（`/api/v3/tts/unidirectional`）。
///
/// 一次请求合成一个词/句；停顿由本地拼接负责（AGENTS.md D1）。V3 把音频切成
/// 若干 JSON 块（chunked body），这里一次性读完 body 再按块解析、拼回 PCM。
pub struct DoubaoProvider {
    cfg: DoubaoConfig,
    api_key: String,
    client: reqwest::Client,
}

impl fmt::Debug for DoubaoProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Never print the API key, even in panic messages or `--verbose` logs.
        f.debug_struct("DoubaoProvider")
            .field("endpoint", &self.cfg.endpoint)
            .field("resource_id", &self.cfg.resource_id)
            .field("model", &self.cfg.model)
            .field("api_key", &"<redacted>")
            .finish()
    }
}

impl DoubaoProvider {
    pub fn new(cfg: DoubaoConfig) -> Result<Self> {
        let api_key = require(cfg.api_key.as_ref(), "DOUBAO_API_KEY")?.to_string();
        let client = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .context("创建豆包 TTS HTTP 客户端失败")?;
        Ok(Self {
            cfg,
            api_key,
            client,
        })
    }

    /// V3 request body. `model` is only sent for cloned voices; system voices
    /// must not carry it.
    fn build_body(&self, req: &TtsRequest) -> Value {
        let mut params = json!({
            "text": req.text,
            "speaker": req.voice,
            "audio_params": {
                "format": "pcm",
                "sample_rate": SAMPLE_RATE,
                "speech_rate": speech_rate(req.rate),
            },
        });
        if let Some(model) = self.cfg.model.as_deref().filter(|m| !m.trim().is_empty()) {
            params["model"] = Value::String(model.to_string());
        }
        json!({ "req_params": params })
    }

    async fn post_once(&self, body: &Value) -> Result<RawResponse> {
        let request_id = Uuid::new_v4().to_string();
        let resp = self
            .client
            .post(&self.cfg.endpoint)
            .header(HEADER_API_KEY, self.api_key.as_str())
            .header(HEADER_RESOURCE_ID, self.cfg.resource_id.as_str())
            .header(HEADER_REQUEST_ID, request_id.as_str())
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(body)
            .send()
            .await
            .context("发送豆包 TTS 请求失败")?;

        let status = resp.status();
        let log_id = LOG_ID_HEADERS
            .iter()
            .find_map(|name| header_value(resp.headers(), name));
        // The service echoes our request id back; prefer its copy for support
        // tickets, but keep the one we sent when the header is absent.
        let request_id = header_value(resp.headers(), HEADER_REQUEST_ID).unwrap_or(request_id);
        if let Some(id) = log_id.as_deref() {
            tracing::debug!(logid = id, request_id, "豆包 TTS 响应头");
        }

        let bytes = resp
            .bytes()
            .await
            .context("读取豆包 TTS 响应失败")?
            .to_vec();
        Ok(RawResponse {
            status,
            request_id,
            log_id,
            body: bytes,
        })
    }

    /// POST with one retry on network errors or 5xx (exponential backoff).
    /// Business/4xx errors are never retried.
    async fn post_with_retry(&self, body: &Value) -> Result<RawResponse> {
        let mut attempt = 0u32;
        loop {
            match self.post_once(body).await {
                Ok(raw) if raw.status.is_server_error() && attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::warn!(
                        status = raw.status.as_u16(),
                        request_id = %raw.request_id,
                        attempt,
                        "豆包 TTS 服务端错误，稍后重试"
                    );
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Ok(raw) => return Ok(raw),
                Err(err) if attempt < MAX_RETRIES => {
                    attempt += 1;
                    tracing::warn!(error = %err, attempt, "豆包 TTS 网络错误，稍后重试");
                    tokio::time::sleep(backoff(attempt)).await;
                }
                Err(err) => {
                    return Err(err.context(format!("豆包 TTS 请求失败（已重试 {attempt} 次）")))
                }
            }
        }
    }
}

#[async_trait]
impl TtsProvider for DoubaoProvider {
    fn id(&self) -> &'static str {
        "doubao"
    }

    fn supports_native_rate(&self) -> bool {
        true
    }

    async fn synthesize(&self, req: &TtsRequest) -> Result<AudioData> {
        if req.text.trim().is_empty() {
            bail!("豆包 TTS 拒绝空文本");
        }
        let chars = req.text.chars().count();
        if chars > MAX_TEXT_CHARS {
            bail!(
                "豆包 TTS 单次文本上限 {MAX_TEXT_CHARS} 字符，当前 {chars} 字符；\
                 请拆句后重试"
            );
        }

        let raw = self.post_with_retry(&self.build_body(req)).await?;
        let pcm = audio_from_response(&raw)?;
        Ok(AudioData {
            bytes: pcm,
            format: AudioFormat::PcmS16le {
                sample_rate: SAMPLE_RATE,
            },
        })
    }
}

/// Whole response body plus the metadata we want in every error message.
#[derive(Debug)]
struct RawResponse {
    status: reqwest::StatusCode,
    /// The `X-Api-Request-Id` we sent; the service echoes it back on errors.
    request_id: String,
    log_id: Option<String>,
    body: Vec<u8>,
}

impl RawResponse {
    fn describe(&self) -> String {
        format!(
            "HTTP {} request_id={} logid={}",
            self.status.as_u16(),
            self.request_id,
            self.log_id.as_deref().unwrap_or("-")
        )
    }
}

/// One decoded V3 response chunk.
#[derive(Debug, Default, PartialEq, Eq)]
struct Chunk {
    code: Option<i64>,
    message: Option<String>,
    data: Option<String>,
}

/// Parse the chunked body into audio bytes.
fn audio_from_response(raw: &RawResponse) -> Result<Vec<u8>> {
    let chunks = parse_chunks(&raw.body)
        .with_context(|| format!("解析豆包 TTS 响应失败: {}", raw.describe()))?;
    check_codes(&chunks, raw)?;

    let mut pcm = decode_audio_chunks(&chunks)
        .with_context(|| format!("豆包 TTS 音频解码失败: {}", raw.describe()))?;
    if pcm.len() % 2 == 1 {
        tracing::warn!(len = pcm.len(), "豆包 TTS 返回奇数长度 PCM，丢弃尾字节");
        pcm.truncate(pcm.len() - 1);
    }
    if pcm.is_empty() {
        bail!("豆包 TTS 返回空音频: {}", raw.describe());
    }
    Ok(pcm)
}

/// Split the body into chunks.
///
/// V3 answers with a chunked body of concatenated JSON objects (sometimes
/// newline separated, sometimes with an SSE-style `data:` prefix). Try the
/// streaming deserializer first, then fall back to line-by-line parsing.
fn parse_chunks(bytes: &[u8]) -> Result<Vec<Chunk>> {
    if let Some(chunks) = parse_json_stream(bytes) {
        return Ok(chunks);
    }
    let chunks = parse_json_lines(bytes);
    if chunks.is_empty() {
        bail!("响应既不是 JSON 流也不是 NDJSON: {}", snippet(bytes));
    }
    Ok(chunks)
}

fn parse_json_stream(bytes: &[u8]) -> Option<Vec<Chunk>> {
    let mut chunks = Vec::new();
    for value in serde_json::Deserializer::from_slice(bytes).into_iter::<Value>() {
        match value {
            Ok(value) => chunks.push(chunk_from_value(&value)),
            // Trailing non-JSON (SSE prefixes, [DONE], partial writes) means the
            // stream parser is not the right tool for this body.
            Err(_) => return None,
        }
    }
    if chunks.is_empty() {
        None
    } else {
        Some(chunks)
    }
}

fn parse_json_lines(bytes: &[u8]) -> Vec<Chunk> {
    let text = String::from_utf8_lossy(bytes);
    text.lines()
        .filter_map(|line| {
            let payload = strip_sse_prefix(line);
            if payload.is_empty() {
                return None;
            }
            serde_json::from_str::<Value>(payload)
                .ok()
                .map(|value| chunk_from_value(&value))
        })
        .collect()
}

/// Drop SSE field prefixes (`data: {...}`) and comment lines.
fn strip_sse_prefix(line: &str) -> &str {
    let line = line.trim();
    if let Some(rest) = line.strip_prefix("data:") {
        rest.trim()
    } else if line.starts_with("event:") || line.starts_with("id:") || line.starts_with(':') {
        ""
    } else {
        line
    }
}

fn chunk_from_value(value: &Value) -> Chunk {
    Chunk {
        code: value.get("code").and_then(code_of),
        message: value
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string),
        data: value
            .get("data")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

/// `code` is a number in the docs, but tolerate the occasional string.
fn code_of(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
}

/// Business errors arrive with HTTP 200 and a non-zero `code`.
fn check_codes(chunks: &[Chunk], raw: &RawResponse) -> Result<()> {
    for chunk in chunks {
        let Some(code) = chunk.code else { continue };
        if code == CODE_AUDIO || code == CODE_FINISHED {
            continue;
        }
        bail!(
            "豆包 TTS 失败: code={code} message={} {}",
            chunk
                .message
                .as_deref()
                .filter(|m| !m.trim().is_empty())
                .unwrap_or("<无>"),
            raw.describe()
        );
    }
    if !raw.status.is_success() {
        bail!(
            "豆包 TTS HTTP 错误: {} body={}",
            raw.describe(),
            snippet(&raw.body)
        );
    }
    Ok(())
}

/// Decode the audio payloads.
///
/// V3 returns each `data` block as an **independently padded** base64 string.
/// Concatenating the encoded strings first and decoding once can silently
/// produce shifted/corrupted PCM when a chunk ends with `=` padding (observed
/// with short words such as 「毕竟」), so decode chunk by chunk first and only
/// fall back to joining when a chunk is a partial base64 quantum (the other
/// possible server behavior).
fn decode_audio_chunks(chunks: &[Chunk]) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    let mut saw_data = false;
    let mut per_chunk_ok = true;

    for chunk in chunks {
        let Some(data) = chunk.data.as_deref() else {
            continue;
        };
        let data = strip_whitespace(data);
        if data.is_empty() {
            continue;
        }
        saw_data = true;
        match BASE64.decode(data.as_bytes()) {
            Ok(part) => out.extend_from_slice(&part),
            Err(_) => {
                per_chunk_ok = false;
                break;
            }
        }
    }

    if !saw_data {
        bail!("豆包 TTS 返回空音频（所有 data 块为空）");
    }
    if per_chunk_ok {
        return Ok(out);
    }

    // Fallback: the service split one base64 string across chunks, so no single
    // chunk is independently decodable.
    let joined: String = chunks
        .iter()
        .filter_map(|chunk| chunk.data.as_deref())
        .map(strip_whitespace)
        .collect();
    BASE64
        .decode(joined.as_bytes())
        .with_context(|| format!("音频块不是合法 base64: {}", snippet(joined.as_bytes())))
}

fn strip_whitespace(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Map 0.5..=2.0 to the V3 `speech_rate` range: 0 = normal, 100 = 2.0x,
/// -50 = 0.5x.
fn speech_rate(rate: f32) -> i32 {
    ((rate - 1.0) * 100.0).round().clamp(-50.0, 100.0) as i32
}

fn header_value(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_DOUBAO_ENDPOINT;
    use crate::model::Lang;

    fn config(model: Option<&str>) -> DoubaoConfig {
        DoubaoConfig {
            api_key: Some("test-key".to_string()),
            endpoint: DEFAULT_DOUBAO_ENDPOINT.to_string(),
            resource_id: "seed-tts-2.0".to_string(),
            model: model.map(str::to_string),
            voice_zh: None,
            voice_en: None,
        }
    }

    fn provider_with_model(model: Option<&str>) -> DoubaoProvider {
        DoubaoProvider {
            cfg: config(model),
            api_key: "test-key".to_string(),
            client: reqwest::Client::new(),
        }
    }

    fn provider() -> DoubaoProvider {
        provider_with_model(None)
    }

    fn request(text: &str, rate: f32) -> TtsRequest {
        TtsRequest {
            text: text.to_string(),
            voice: "zh_female_vv_uranus_bigtts".to_string(),
            lang: Lang::Zh,
            rate,
        }
    }

    /// Build a fake chunked response body around the given raw chunks.
    fn raw_response(body: impl Into<Vec<u8>>) -> RawResponse {
        RawResponse {
            status: reqwest::StatusCode::OK,
            request_id: "test-request-id".to_string(),
            log_id: Some("test-logid".to_string()),
            body: body.into(),
        }
    }

    fn audio_chunk(data: &str) -> String {
        json!({ "code": 0, "message": "OK", "data": data }).to_string()
    }

    const FINISH_CHUNK: &str = r#"{"code":20000000,"message":"OK","data":""}"#;

    fn pcm_bytes(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 3 % 251) as u8).collect()
    }

    #[test]
    fn new_requires_api_key() {
        let mut cfg = config(None);
        cfg.api_key = None;
        let err = DoubaoProvider::new(cfg).unwrap_err().to_string();
        assert!(err.contains("DOUBAO_API_KEY"), "unexpected error: {err}");
    }

    #[test]
    fn debug_redacts_api_key() {
        let text = format!("{:?}", provider());
        assert!(text.contains("<redacted>"), "unexpected debug: {text}");
        assert!(!text.contains("test-key"), "api key leaked: {text}");
    }

    #[test]
    fn build_body_maps_request_fields() {
        let body = provider().build_body(&request("摇晃", 1.0));
        let params = &body["req_params"];

        assert_eq!(params["text"], "摇晃");
        assert_eq!(params["speaker"], "zh_female_vv_uranus_bigtts");
        assert_eq!(params["audio_params"]["format"], "pcm");
        assert_eq!(params["audio_params"]["sample_rate"], 24_000);
        assert_eq!(params["audio_params"]["speech_rate"], 0);
        assert!(
            params.get("model").is_none(),
            "系统音色不应携带 model: {params}"
        );
    }

    #[test]
    fn build_body_includes_model_only_when_configured() {
        let body =
            provider_with_model(Some("seed-tts-2.0-standard")).build_body(&request("摇晃", 1.0));
        assert_eq!(body["req_params"]["model"], "seed-tts-2.0-standard");

        let blank = provider_with_model(Some("  ")).build_body(&request("摇晃", 1.0));
        assert!(blank["req_params"].get("model").is_none());
    }

    #[test]
    fn speech_rate_maps_speed_range() {
        assert_eq!(speech_rate(0.5), -50);
        assert_eq!(speech_rate(1.0), 0);
        assert_eq!(speech_rate(2.0), 100);
        assert_eq!(speech_rate(1.5), 50);
        assert_eq!(speech_rate(0.9), -10);
        assert_eq!(speech_rate(0.3), -50, "低于 0.5 应夹到 -50");
        assert_eq!(speech_rate(3.0), 100, "高于 2.0 应夹到 100");
    }

    #[tokio::test]
    async fn rejects_text_over_2000_chars() {
        let req = request(&"汉".repeat(2001), 1.0);
        let err = provider().synthesize(&req).await.unwrap_err().to_string();
        assert!(err.contains("2000"), "unexpected error: {err}");
    }

    #[tokio::test]
    async fn rejects_empty_text() {
        let err = provider()
            .synthesize(&request("   ", 1.0))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("空文本"), "unexpected error: {err}");
    }

    #[test]
    fn decodes_two_audio_chunks_plus_finish() {
        let pcm = pcm_bytes(16);
        let encoded = BASE64.encode(&pcm);
        let (head, tail) = encoded.split_at(encoded.len() / 2);
        let body = format!("{}{}{}", audio_chunk(head), audio_chunk(tail), FINISH_CHUNK);

        let decoded = audio_from_response(&raw_response(body)).unwrap();
        assert_eq!(decoded, pcm);
    }

    #[test]
    fn decodes_independently_padded_chunks_without_shift() {
        // Both chunks are valid on their own; joining the base64 strings first
        // would put `=` padding in the middle and silently add a byte.
        let first = vec![0u8; 4]; // "AAAAAA"
        let second = vec![9u8; 2]; // "CQk="
        let body = format!(
            "{}{}{}",
            audio_chunk(&BASE64.encode(&first)),
            audio_chunk(&BASE64.encode(&second)),
            FINISH_CHUNK
        );

        let decoded = audio_from_response(&raw_response(body)).unwrap();
        let expected: Vec<u8> = first.into_iter().chain(second).collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn decodes_base64_split_across_chunks() {
        let pcm = pcm_bytes(64);
        let encoded = BASE64.encode(&pcm);
        // Split inside a base64 quantum so neither half is independently valid.
        let (head, tail) = encoded.split_at(21);
        let body = format!(
            "{}\n{}\n{}",
            audio_chunk(head),
            audio_chunk(tail),
            FINISH_CHUNK
        );

        let decoded = audio_from_response(&raw_response(body)).unwrap();
        assert_eq!(decoded, pcm);
    }

    #[test]
    fn decodes_single_json_response() {
        let pcm = pcm_bytes(32);
        let body = audio_chunk(&BASE64.encode(&pcm));
        let decoded = audio_from_response(&raw_response(body)).unwrap();
        assert_eq!(decoded, pcm);
    }

    #[test]
    fn decodes_ndjson_with_data_prefix() {
        let pcm = pcm_bytes(24);
        let encoded = BASE64.encode(&pcm);
        let (head, tail) = encoded.split_at(encoded.len() / 2);
        let body = format!(
            "data: {}\n\ndata: {}\n\n: keep-alive\n\ndata: [DONE]\n",
            audio_chunk(head),
            audio_chunk(tail)
        );

        let decoded = audio_from_response(&raw_response(body)).unwrap();
        assert_eq!(decoded, pcm);
    }

    #[test]
    fn base64_with_inner_newlines_is_accepted() {
        let pcm = pcm_bytes(30);
        let encoded = BASE64.encode(&pcm);
        let (head, tail) = encoded.split_at(encoded.len() / 2);
        let data = format!("{head}\n  {tail}\n");
        let body = format!("{}{}", audio_chunk(&data), FINISH_CHUNK);

        let decoded = audio_from_response(&raw_response(body)).unwrap();
        assert_eq!(decoded, pcm);
    }

    #[test]
    fn reports_non_zero_code() {
        let body = r#"{"code":40000001,"message":"invalid api key","data":""}"#;
        let err = audio_from_response(&raw_response(body))
            .unwrap_err()
            .to_string();
        assert!(err.contains("40000001"), "unexpected error: {err}");
        assert!(err.contains("invalid api key"), "unexpected error: {err}");
        assert!(err.contains("test-request-id"), "缺少 request_id: {err}");
        assert!(err.contains("test-logid"), "缺少 logid: {err}");
    }

    #[test]
    fn error_code_wins_over_success_http_status() {
        let body = format!(
            "{}{}",
            audio_chunk(&BASE64.encode(pcm_bytes(8))),
            r#"{"code":45000001,"message":"quota exceeded","data":""}"#
        );
        let err = audio_from_response(&raw_response(body))
            .unwrap_err()
            .to_string();
        assert!(err.contains("45000001"), "unexpected error: {err}");
        assert!(err.contains("quota exceeded"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_empty_audio() {
        let body = format!("{}{}", audio_chunk(""), FINISH_CHUNK);
        let err = audio_from_response(&raw_response(body)).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("空音频"), "unexpected error: {err}");
        assert!(err.contains("所有 data 块为空"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_garbage_body() {
        let err = audio_from_response(&raw_response("<html>502</html>")).unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("NDJSON"), "unexpected error: {err}");
    }

    #[test]
    fn rejects_http_error_without_code() {
        let mut raw = raw_response(r#"{"message":"unauthorized"}"#);
        raw.status = reqwest::StatusCode::UNAUTHORIZED;
        let err = audio_from_response(&raw).unwrap_err().to_string();
        assert!(err.contains("HTTP 401"), "unexpected error: {err}");
        assert!(err.contains("unauthorized"), "unexpected error: {err}");
    }

    #[test]
    fn drops_odd_trailing_byte() {
        let odd = vec![1u8, 2, 3];
        let body = format!("{}{}", audio_chunk(&BASE64.encode(&odd)), FINISH_CHUNK);
        let decoded = audio_from_response(&raw_response(body)).unwrap();
        assert_eq!(decoded, vec![1, 2]);
    }

    #[test]
    fn backoff_grows() {
        assert_eq!(backoff(1), Duration::from_millis(500));
        assert_eq!(backoff(2), Duration::from_millis(1000));
    }
}
