//! 课时发布到 Cloudflare R2（对外只读、增量、幂等、只增不删）。
//!
//! 契约：[`docs/share-publish.md`](../docs/share-publish.md)
//! （§4 数据格式、§5 发布状态机、§7 配置与凭据）。
//!
//! 发布走 R2 的 S3 API（SigV4 自签名，无 `*-sys` 依赖），不经过公开域名，
//! 因此以后加 WAF 口令也不影响发布。R2 上的 `lesson.json` 是唯一真相源：
//! 本地只提供本次要发布的那一课，历史丢失不影响远端存量。

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use time::macros::format_description;
use time::OffsetDateTime;

use crate::config::read_secret;
use crate::lesson::list::{self, Listing};
use crate::manifest::Manifest;

/// 发布数据 `lesson.json` / `index.json` 的 schema 版本。
pub const PUBLISH_SCHEMA_VERSION: u32 = 1;

const EMPTY_SHA256: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const AUDIO_CONTENT_TYPE: &str = "audio/wav";
const MP3_CONTENT_TYPE: &str = "audio/mpeg";
const JSON_CONTENT_TYPE: &str = "application/json";
const CLIP_CACHE_CONTROL: &str = "public, max-age=31536000, immutable";
const MANIFEST_CACHE_CONTROL: &str = "no-cache";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

/// `config.toml` 的 `[publish]` 段（不含密钥；密钥只走环境变量）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PublishSection {
    /// 未显式开启时发布端点一律 503。
    pub enabled: Option<bool>,
    pub data_base: Option<String>,
    pub pages_base: Option<String>,
    pub bucket: Option<String>,
    pub pages_project: Option<String>,
    pub shell_dir: Option<PathBuf>,
}

/// 发布运行时配置：文件段 + 环境变量密钥。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublishSettings {
    pub enabled: bool,
    pub data_base: Option<String>,
    pub pages_base: Option<String>,
    pub bucket: Option<String>,
    pub pages_project: Option<String>,
    pub shell_dir: Option<PathBuf>,
    pub account_id: Option<String>,
    pub access_key_id: Option<String>,
    pub secret_access_key: Option<String>,
}

impl PublishSettings {
    /// 环境变量可覆盖 `config.toml`；密钥走 `read_secret`
    /// （`$CREDENTIALS_DIRECTORY` → 环境变量，同 D13b）。
    pub fn from_config(section: &PublishSection) -> Self {
        Self {
            enabled: section.enabled.unwrap_or(false),
            data_base: section
                .data_base
                .clone()
                .or_else(|| read_secret("R2_DATA_BASE")),
            pages_base: section.pages_base.clone(),
            bucket: section.bucket.clone().or_else(|| read_secret("R2_BUCKET")),
            pages_project: section.pages_project.clone(),
            shell_dir: section.shell_dir.clone(),
            account_id: read_secret("R2_ACCOUNT_ID"),
            access_key_id: read_secret("R2_ACCESS_KEY_ID"),
            secret_access_key: read_secret("R2_SECRET_ACCESS_KEY"),
        }
    }

    /// 按契约 §7.1/§7.2：`enabled` + `data_base` + `bucket` + R2 凭证齐全。
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if !self.enabled {
            missing.push("enabled");
        }
        if self.data_base.is_none() {
            missing.push("data_base");
        }
        if self.bucket.is_none() {
            missing.push("bucket");
        }
        if self.account_id.is_none() {
            missing.push("R2_ACCOUNT_ID");
        }
        if self.access_key_id.is_none() {
            missing.push("R2_ACCESS_KEY_ID");
        }
        if self.secret_access_key.is_none() {
            missing.push("R2_SECRET_ACCESS_KEY");
        }
        missing
    }

    pub fn is_configured(&self) -> bool {
        self.missing().is_empty()
    }

    pub fn unconfigured_error() -> String {
        "未配置发布目标（config.toml [publish]）".to_string()
    }

    /// 家长分享链接：`pages_base/?l=<lesson_id>`；未配置 `pages_base` 时为 None。
    pub fn share_url(&self, lesson_id: &str) -> Option<String> {
        self.pages_base
            .as_ref()
            .map(|base| format!("{}/?l={}", base.trim_end_matches('/'), lesson_id))
    }
}

// ---------------------------------------------------------------------------
// 对象存储
// ---------------------------------------------------------------------------

#[async_trait::async_trait]
pub trait ObjectStore: Send + Sync {
    /// 404 返回 None；其余非 200 报错。
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>>;
    async fn head(&self, key: &str) -> Result<bool>;
    async fn put(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
        cache_control: &str,
    ) -> Result<()>;
}

/// 内存实现：单测用（发布流程 / index 合并 / 幂等）。
#[derive(Default)]
pub struct MemoryStore {
    objects: Mutex<BTreeMap<String, StoredObject>>,
    put_count: std::sync::atomic::AtomicUsize,
}

#[derive(Debug, Clone)]
pub struct StoredObject {
    pub body: Vec<u8>,
    pub content_type: String,
    pub cache_control: String,
}

impl MemoryStore {
    pub fn put_count(&self) -> usize {
        self.put_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn object_count(&self) -> usize {
        self.objects.lock().unwrap().len()
    }

    pub fn has(&self, key: &str) -> bool {
        self.objects.lock().unwrap().contains_key(key)
    }

    pub fn object(&self, key: &str) -> Option<StoredObject> {
        self.objects.lock().unwrap().get(key).cloned()
    }
}

#[async_trait::async_trait]
impl ObjectStore for MemoryStore {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        Ok(self
            .objects
            .lock()
            .unwrap()
            .get(key)
            .map(|obj| obj.body.clone()))
    }

    async fn head(&self, key: &str) -> Result<bool> {
        Ok(self.has(key))
    }

    async fn put(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
        cache_control: &str,
    ) -> Result<()> {
        self.put_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.objects.lock().unwrap().insert(
            key.to_string(),
            StoredObject {
                body: body.to_vec(),
                content_type: content_type.to_string(),
                cache_control: cache_control.to_string(),
            },
        );
        Ok(())
    }
}

/// AWS SigV4 签名的 R2 S3 客户端。
///
/// 为什么自己签：R2 只需要 GET/HEAD/PUT + 固定头部，SigV4 用 `sha2` + `hmac`
/// 两个纯 Rust crate 即可，避免引入 S3 SDK（依赖面大、部分非纯 Rust）。
pub struct S3Store {
    client: reqwest::Client,
    endpoint: String,
    host: String,
    bucket: String,
    access_key_id: String,
    secret_access_key: String,
    /// 测试注入固定时间；生产为 None（当前 UTC）。
    fixed_time: Option<OffsetDateTime>,
}

impl S3Store {
    pub fn new(settings: &PublishSettings) -> Result<Self> {
        let account_id = settings
            .account_id
            .as_deref()
            .context("缺少 R2_ACCOUNT_ID")?;
        let access_key_id = settings
            .access_key_id
            .as_deref()
            .context("缺少 R2_ACCESS_KEY_ID")?;
        let secret_access_key = settings
            .secret_access_key
            .as_deref()
            .context("缺少 R2_SECRET_ACCESS_KEY")?;
        let bucket = settings.bucket.as_deref().context("缺少 bucket")?;

        let endpoint = format!("https://{account_id}.r2.cloudflarestorage.com");
        let host = endpoint
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            client: reqwest::Client::new(),
            endpoint,
            host,
            bucket: bucket.to_string(),
            access_key_id: access_key_id.to_string(),
            secret_access_key: secret_access_key.to_string(),
            fixed_time: None,
        })
    }

    fn amz_date(&self) -> Result<(String, String)> {
        let now = self.fixed_time.unwrap_or_else(OffsetDateTime::now_utc);
        let datetime_fmt = format_description!("[year][month][day]T[hour][minute][second]Z");
        let date_fmt = format_description!("[year][month][day]");
        let datetime = now.format(&datetime_fmt).context("生成 x-amz-date 失败")?;
        let datestamp = now.format(&date_fmt).context("生成签名日期失败")?;
        Ok((datetime, datestamp))
    }

    /// AWS 风格 URI 编码：不编码 `/`，其余按 RFC3986。
    fn uri_encode(value: &str) -> String {
        uri_encode(value)
    }

    fn authorization(
        &self,
        method: &str,
        canonical_uri: &str,
        query: &str,
        payload_hash: &str,
        amz_date: &str,
    ) -> Result<String> {
        let datestamp = &amz_date[..8];
        let canonical_request = canonical_request(
            method,
            canonical_uri,
            query,
            &self.host,
            payload_hash,
            amz_date,
        );
        let scope = format!("{datestamp}/auto/s3/aws4_request");
        let string_to_sign = format!(
            "AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}",
            hex_sha256(canonical_request.as_bytes()),
        );

        let mut key =
            Hmac::<Sha256>::new_from_slice(format!("AWS4{}", self.secret_access_key).as_bytes())?;
        key.update(datestamp.as_bytes());
        let mut key = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes())?;
        key.update(b"auto");
        let mut key = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes())?;
        key.update(b"s3");
        let mut key = Hmac::<Sha256>::new_from_slice(&key.finalize().into_bytes())?;
        key.update(b"aws4_request");
        let signing_key = key.finalize().into_bytes();
        let mut mac = Hmac::<Sha256>::new_from_slice(&signing_key)?;
        mac.update(string_to_sign.as_bytes());
        Ok(format!(
            "AWS4-HMAC-SHA256 Credential={ak}/{scope}, SignedHeaders={signed_headers}, Signature={signature}",
            ak = self.access_key_id,
            scope = scope,
            signed_headers = "host;x-amz-content-sha256;x-amz-date",
            signature = hex::encode(mac.finalize().into_bytes()),
        ))
    }

    fn request(
        &self,
        method: reqwest::Method,
        key: &str,
        query: &str,
        body: Vec<u8>,
        content_type: Option<&str>,
        cache_control: Option<&str>,
    ) -> Result<reqwest::RequestBuilder> {
        let payload_hash = if body.is_empty() {
            EMPTY_SHA256.to_string()
        } else {
            hex_sha256(&body)
        };
        let (amz_date, _) = self.amz_date()?;
        let canonical_uri = format!("/{}/{}", self.bucket, Self::uri_encode(key));
        let authorization = self.authorization(
            method.as_str(),
            &canonical_uri,
            query,
            &payload_hash,
            &amz_date,
        )?;

        let mut url = format!("{}{}", self.endpoint, canonical_uri);
        if !query.is_empty() {
            url.push('?');
            url.push_str(query);
        }

        let mut builder = self
            .client
            .request(method, &url)
            .header("x-amz-date", &amz_date)
            .header("x-amz-content-sha256", &payload_hash)
            .header("authorization", authorization);
        if let Some(content_type) = content_type {
            builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        if let Some(cache_control) = cache_control {
            builder = builder.header("cache-control", cache_control);
        }
        if !body.is_empty() {
            builder = builder.body(body);
        }
        Ok(builder)
    }

    fn status_error(status: u16, body: &str) -> anyhow::Error {
        anyhow::anyhow!("R2 返回 {status}：{}", first_line(body))
    }
}

#[async_trait::async_trait]
impl ObjectStore for S3Store {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let res = self
            .request(reqwest::Method::GET, key, "", Vec::new(), None, None)?
            .send()
            .await
            .context("请求 R2 失败")?;
        match res.status().as_u16() {
            200 => Ok(Some(res.bytes().await?.to_vec())),
            404 => Ok(None),
            403 => bail!("R2 拒绝访问：检查 R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY"),
            status => bail!("{}", Self::status_error(status, &res.text().await?)),
        }
    }

    async fn head(&self, key: &str) -> Result<bool> {
        let res = self
            .request(reqwest::Method::HEAD, key, "", Vec::new(), None, None)?
            .send()
            .await
            .context("请求 R2 失败")?;
        match res.status().as_u16() {
            200 => Ok(true),
            404 => Ok(false),
            403 => bail!("R2 拒绝访问：检查 R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY"),
            status => bail!("{}", Self::status_error(status, "")),
        }
    }

    async fn put(
        &self,
        key: &str,
        body: &[u8],
        content_type: &str,
        cache_control: &str,
    ) -> Result<()> {
        let res = self
            .request(
                reqwest::Method::PUT,
                key,
                "",
                body.to_vec(),
                Some(content_type),
                Some(cache_control),
            )?
            .send()
            .await
            .context("请求 R2 失败")?;
        match res.status().as_u16() {
            200 => Ok(()),
            403 => bail!("R2 拒绝访问：检查 R2_ACCESS_KEY_ID / R2_SECRET_ACCESS_KEY"),
            status => bail!("{}", Self::status_error(status, &res.text().await?)),
        }
    }
}

// ---------------------------------------------------------------------------
// Pages 壳（B 方案：静态资源 + R2 代理 Function）
// ---------------------------------------------------------------------------

/// `publish --shell` 的产物信息。
#[derive(Debug, Clone)]
pub struct ShellOutcome {
    pub dir: PathBuf,
    pub files: usize,
    pub data_base: String,
    pub bucket: String,
}

const PAGES_FUNCTION: &str = r#"// 同源代理 R2：/data/* -> 桶内 lessons/*
// 由 `dictation publish --shell` 生成；binding 见根目录 wrangler.jsonc。
const TYPES = {
  json: 'application/json; charset=utf-8',
  wav: 'audio/wav',
  mp3: 'audio/mpeg',
};

export async function onRequest({ request, env, params }) {
  if (request.method !== 'GET' && request.method !== 'HEAD') {
    return new Response('method not allowed', { status: 405 });
  }
  // /data/lessons/index.json -> 桶内 lessons/index.json
  const key = (params.path || []).join('/');
  if (!key.startsWith('lessons/') || key.includes('..')) {
    return new Response('not found', { status: 404 });
  }

  const object = await env.BUCKET.get(key);
  if (!object) {
    return new Response('not found', { status: 404 });
  }

  const ext = key.split('.').pop();
  const headers = {
    'content-type': TYPES[ext] || 'application/octet-stream',
    'cache-control': key.includes('/clips/')
      ? 'public, max-age=31536000, immutable'
      : 'no-cache',
    'x-content-type-options': 'nosniff',
  };
  if (request.method === 'HEAD') {
    return new Response(null, { headers });
  }
  return new Response(object.body, { headers });
}
"#;

const PAGES_HEADERS: &str = "/*\n  Referrer-Policy: no-referrer\n\n/\n  Cache-Control: no-cache\n\n/index.html\n  Cache-Control: no-cache\n\n/app.js\n  Cache-Control: no-cache\n\n/config.js\n  Cache-Control: no-cache\n\n/style.css\n  Cache-Control: no-cache\n\n/manifest.webmanifest\n  Cache-Control: no-cache\n";

fn copy_tree(src: &Path, dst: &Path) -> Result<usize> {
    fs::create_dir_all(dst)?;
    let mut copied = 0;
    for entry in fs::read_dir(src).with_context(|| format!("读取目录失败: {}", src.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        if path.is_dir() {
            copied += copy_tree(&path, &target)?;
        } else {
            fs::copy(&path, &target).with_context(|| {
                format!("复制文件失败: {} -> {}", path.display(), target.display())
            })?;
            copied += 1;
        }
    }
    Ok(copied)
}

fn today_utc() -> String {
    let date = OffsetDateTime::now_utc().date();
    format!(
        "{:04}-{:02}-{:02}",
        date.year(),
        u8::from(date.month()),
        date.day()
    )
}

/// 生成 Cloudflare Pages 工程（B 方案：私有桶 + 同源 Function）。
///
/// 产物：
/// ```text
/// <out_dir>/
/// ├── wrangler.jsonc          name / pages_build_output_dir / R2 binding
/// ├── functions/data/[[path]].js
/// └── public/                 静态壳（web/ + config.js + _headers）
/// ```
/// 只清理 `public/` 与 `functions/`（本命令自己的产物），不动目录里的其他文件。
pub fn write_shell(
    settings: &PublishSettings,
    web_dir: &Path,
    out_dir: &Path,
    dry_run: bool,
) -> Result<ShellOutcome> {
    let bucket = settings
        .bucket
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("缺少 [publish].bucket；B 方案的 Pages Function 需要绑定桶名")?;
    let data_base = settings
        .data_base
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            settings
                .pages_base
                .as_ref()
                .map(|base| format!("{}/data", base.trim_end_matches('/')))
        })
        .context("缺少 [publish].data_base 或 pages_base")?;
    let project = settings
        .pages_project
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("dictation");

    if !web_dir.is_dir() {
        bail!("前端目录不存在: {}", web_dir.display());
    }

    let public_dir = out_dir.join("public");
    let functions_dir = out_dir.join("functions/data");
    let function_file = functions_dir.join("[[path]].js");
    let wrangler_file = out_dir.join("wrangler.jsonc");

    let config_js = format!(
        "// 由 dictation publish --shell 生成：静态壳 + 同源 R2 代理\nwindow.DICTATION_CONFIG = {{ mode: 'static', dataBase: '{}' }};\n",
        data_base.trim_end_matches('/')
    );
    let wrangler = format!(
        r#"{{
  "$schema": "https://unpkg.com/wrangler/config-schema.json",
  "name": "{project}",
  "pages_build_output_dir": "./public",
  "compatibility_date": "{date}",
  "r2_buckets": [
    {{ "binding": "BUCKET", "bucket_name": "{bucket}" }}
  ]
}}
"#,
        project = project,
        date = today_utc(),
        bucket = bucket,
    );

    if dry_run {
        return Ok(ShellOutcome {
            dir: out_dir.to_path_buf(),
            files: 0,
            data_base,
            bucket: bucket.to_string(),
        });
    }

    // 只重建自己的产物目录，避免旧文件残留（例如已删除的前端资源）
    for dir in [&public_dir, out_dir.join("functions").as_path()] {
        if dir.exists() {
            fs::remove_dir_all(dir)
                .with_context(|| format!("清理旧产物失败: {}", dir.display()))?;
        }
    }

    let mut files = copy_tree(web_dir, &public_dir)?;
    // web/config.js 是 server 模式默认值，必须覆盖
    fs::write(public_dir.join("config.js"), config_js).context("写入 public/config.js 失败")?;
    files += 1;
    fs::write(public_dir.join("_headers"), PAGES_HEADERS).context("写入 public/_headers 失败")?;
    files += 1;

    fs::create_dir_all(&functions_dir)?;
    fs::write(&function_file, PAGES_FUNCTION).context("写入 Pages Function 失败")?;
    files += 1;
    fs::write(&wrangler_file, wrangler).context("写入 wrangler.jsonc 失败")?;
    files += 1;

    Ok(ShellOutcome {
        dir: out_dir.to_path_buf(),
        files,
        data_base,
        bucket: bucket.to_string(),
    })
}

// ---------------------------------------------------------------------------
// 数据构造
// ---------------------------------------------------------------------------

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

/// AWS 风格 URI 编码：不编码 `/`，其余按 RFC3986。
fn uri_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// SigV4 canonical request（可测）；签名头固定 `host;x-amz-content-sha256;x-amz-date`。
fn canonical_request(
    method: &str,
    canonical_uri: &str,
    query: &str,
    host: &str,
    payload_hash: &str,
    amz_date: &str,
) -> String {
    format!(
        "{method}\n{uri}\n{query}\nhost:{host}\nx-amz-content-sha256:{payload}\nx-amz-date:{date}\n\nhost;x-amz-content-sha256;x-amz-date\n{payload}",
        uri = canonical_uri,
        payload = payload_hash,
        date = amz_date,
    )
}

/// 键排序、无空白的规范化 JSON（serde_json::Map 默认 BTreeMap）。
fn canonical_json(value: &Value) -> String {
    // Value 序列化不会失败；expect 仅用于绕过 Result 签名。
    serde_json::to_string(value).expect("Value 序列化不会失败")
}

pub fn content_hash_of(payload: &Value) -> String {
    let mut input = payload.clone();
    if let Value::Object(map) = &mut input {
        map.remove("content_hash");
    }
    format!("sha256:{}", hex_sha256(canonical_json(&input).as_bytes()))
}

#[derive(Debug)]
pub struct LessonPlan {
    /// `lesson.json` 完整载荷（含 `content_hash`）。
    pub payload: Value,
    pub content_hash: String,
    /// (远端 key, 本地文件)。
    pub objects: Vec<(String, PathBuf, &'static str)>,
}

/// 由 manifest 构造发布载荷与对象清单；缺失的本地片段直接报错（§5.1 第 2 步）。
pub fn build_plan(
    listing: &Listing,
    manifest: &Manifest,
    lesson_dir: &Path,
    with_full: bool,
) -> Result<LessonPlan> {
    let mut clip_paths: Vec<String> = Vec::new();
    let push_clip = |paths: &mut Vec<String>, clip: &crate::manifest::ManifestClip| {
        if !paths.contains(&clip.path) {
            paths.push(clip.path.clone());
        }
    };
    for item in &manifest.items {
        for clip in &item.clips {
            push_clip(&mut clip_paths, clip);
        }
    }
    if let Some(intro) = &manifest.intro {
        push_clip(&mut clip_paths, intro);
    }
    if let Some(outro) = &manifest.outro {
        push_clip(&mut clip_paths, outro);
    }

    let mut objects: Vec<(String, PathBuf, &'static str)> = Vec::new();
    for path in &clip_paths {
        let file = lesson_dir.join(path);
        if !file.is_file() {
            bail!(
                "片段缺失：{} 不存在；先运行 dictation gen 重新生成",
                file.display()
            );
        }
        objects.push((
            format!("lessons/{}/{}", listing.id, path),
            file,
            AUDIO_CONTENT_TYPE,
        ));
    }

    let full_track = if with_full {
        manifest.full_track.as_deref()
    } else {
        None
    }
    .filter(|name| *name == "full.mp3");
    if let Some(name) = full_track {
        let file = lesson_dir.join(name);
        if file.is_file() {
            objects.push((
                format!("lessons/{}/{}", listing.id, name),
                file,
                MP3_CONTENT_TYPE,
            ));
        }
    }

    let items: Vec<Value> = manifest
        .items
        .iter()
        .map(|item| {
            json!({
                "index": item.index,
                "text": item.text,
                "lang": item.lang,
                "clips": item.clips,
                "repeat": item.repeat,
                "repeat_gap_ms": item.repeat_gap_ms,
                "gap_ms": item.gap_ms,
                "review": false,
            })
        })
        .collect();

    let mut payload = json!({
        "schema_version": PUBLISH_SCHEMA_VERSION,
        "lesson_id": listing.id,
        "kind": "lesson",
        "title": manifest.title,
        "subject": manifest.subject().as_str(),
        "grade": manifest.grade,
        "term": manifest.term,
        "defaults": manifest.defaults,
        "intro": manifest.intro,
        "intro_text": manifest.intro_text,
        "outro": manifest.outro,
        "outro_text": manifest.outro_text,
        "items": items,
        "total_ms": manifest.total_ms,
    });
    if let Some(name) = full_track {
        if let Value::Object(map) = &mut payload {
            map.insert("full_track".to_string(), json!(name));
        }
    }
    let content_hash = content_hash_of(&payload);
    if let Value::Object(map) = &mut payload {
        map.insert("content_hash".to_string(), json!(content_hash));
    }

    Ok(LessonPlan {
        payload,
        content_hash,
        objects,
    })
}

/// 读本地 manifest.json（失败即视为不可发布）。
pub fn read_manifest(lesson_dir: &Path) -> Result<Manifest> {
    let path = lesson_dir.join("manifest.json");
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("读取课时清单失败: {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("课时清单解析失败: {}", path.display()))
}

fn resolve_listing<'a>(listings: &'a [Listing], target: &str) -> Result<&'a Listing> {
    let target = target.trim();
    if let Some(found) = listings.iter().find(|l| l.dir == target) {
        return Ok(found);
    }
    if let Some(found) = listings.iter().find(|l| l.id == target) {
        return Ok(found);
    }
    bail!("课时不存在：{target}")
}

/// `lessons/index.json`：GET 到的旧值 merge 本课条目（§5.1 第 7 步）。
/// 未知条目原样保留——本地只剩一课也不丢其他课。
pub fn merge_index(existing: Option<&Value>, entry: &Value, updated_at: &str) -> Value {
    let mut lessons = existing
        .and_then(|index| index.get("lessons"))
        .and_then(|lessons| lessons.as_array())
        .cloned()
        .unwrap_or_default();
    let id = entry
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    lessons.retain(|item| item.get("id").and_then(Value::as_str) != Some(id.as_str()));
    lessons.push(entry.clone());
    fn sort_key(item: &Value) -> (&str, &str) {
        (
            item.get("title").and_then(Value::as_str).unwrap_or(""),
            item.get("id").and_then(Value::as_str).unwrap_or(""),
        )
    }
    lessons.sort_by(|a, b| sort_key(a).cmp(&sort_key(b)));
    json!({
        "schema_version": PUBLISH_SCHEMA_VERSION,
        "updated_at": updated_at,
        "lessons": lessons,
    })
}

// ---------------------------------------------------------------------------
// 发布状态机
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// 本地输入不完整（清单解析失败、片段缺失）。
    Local,
    /// 片段 / 远端读取阶段。
    Clips,
    /// `lesson.json` 提交点。
    Commit,
    /// `index.json` 合并。
    Index,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::Local => "local",
            Stage::Clips => "clips",
            Stage::Commit => "commit",
            Stage::Index => "index",
        }
    }
}

impl fmt::Display for Stage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 带阶段标记的发布错误：HTTP/CLI 透出 `stage/uploaded/committed`。
#[derive(Debug, thiserror::Error)]
#[error("{stage}: {message}")]
pub struct PublishError {
    pub stage: Stage,
    pub message: String,
    pub uploaded: usize,
    pub committed: bool,
}

impl PublishError {
    fn new(stage: Stage, message: String, uploaded: usize, committed: bool) -> Self {
        Self {
            stage,
            message,
            uploaded,
            committed,
        }
    }
}

fn wrap(stage: Stage, uploaded: usize, committed: bool) -> impl Fn(anyhow::Error) -> PublishError {
    move |err| PublishError::new(stage, err.to_string(), uploaded, committed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PublishOptions {
    pub force: bool,
    pub with_full: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStatus {
    Published,
    Skipped,
}

impl PublishStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            PublishStatus::Published => "published",
            PublishStatus::Skipped => "skipped",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PublishOutcome {
    pub lesson_id: String,
    pub title: String,
    pub status: PublishStatus,
    pub dry_run: bool,
    pub uploaded: usize,
    pub skipped: usize,
    pub index_updated: bool,
    pub url: Option<String>,
}

impl PublishOutcome {
    pub fn to_json(&self) -> Value {
        let mut out = json!({
            "lesson_id": self.lesson_id,
            "title": self.title,
            "status": self.status.as_str(),
            "dry_run": self.dry_run,
            "uploaded": self.uploaded,
            "skipped": self.skipped,
            "index_updated": self.index_updated,
        });
        if let Some(url) = &self.url {
            if let Value::Object(map) = &mut out {
                map.insert("url".to_string(), json!(url));
            }
        }
        out
    }
}

fn index_entry(listing: &Listing, content_hash: &str, total_ms: u32, published_at: &str) -> Value {
    json!({
        "id": listing.id,
        "title": listing.title,
        "subject": listing.subject,
        "grade": listing.grade,
        "term": listing.term,
        "item_count": listing.item_count,
        "total_ms": total_ms,
        "path": format!("lessons/{}/lesson.json", listing.id),
        "content_hash": content_hash,
        "published_at": published_at,
    })
}

/// 发布一课（§5.1 状态机）。`now` 是 RFC3339 时间戳，供 index 元数据使用；
/// 传 None 则取当前 UTC。
pub async fn publish_lesson(
    store: &dyn ObjectStore,
    settings: &PublishSettings,
    lessons_dir: &Path,
    target: &str,
    options: PublishOptions,
    now: Option<String>,
) -> std::result::Result<PublishOutcome, PublishError> {
    let now = match now {
        Some(now) => now,
        None => rfc3339_now(),
    };

    let listings = list::scan(lessons_dir).map_err(|err| {
        PublishError::new(Stage::Local, format!("扫描课时目录失败: {err}"), 0, false)
    })?;
    let listing = resolve_listing(&listings, target)
        .map_err(|err| PublishError::new(Stage::Local, err.to_string(), 0, false))?;
    let lesson_dir = lessons_dir.join(&listing.dir);
    let manifest = read_manifest(&lesson_dir)
        .map_err(|err| PublishError::new(Stage::Local, err.to_string(), 0, false))?;
    let plan = build_plan(listing, &manifest, &lesson_dir, options.with_full)
        .map_err(|err| PublishError::new(Stage::Local, err.to_string(), 0, false))?;

    let uploaded_at_err = |err| wrap(Stage::Clips, 0, false)(err);
    let lesson_key = format!("lessons/{}/lesson.json", listing.id);
    let remote = store
        .get(&lesson_key)
        .await
        .map_err(uploaded_at_err)?
        .and_then(|body| serde_json::from_str::<Value>(&String::from_utf8_lossy(&body)).ok());

    let unchanged = remote
        .as_ref()
        .and_then(|payload| payload.get("content_hash"))
        .and_then(Value::as_str)
        .is_some_and(|hash| hash == plan.content_hash);
    if unchanged && !options.force {
        return Ok(PublishOutcome {
            lesson_id: listing.id.clone(),
            title: listing.title.clone(),
            status: PublishStatus::Skipped,
            dry_run: options.dry_run,
            uploaded: 0,
            skipped: plan.objects.len(),
            index_updated: false,
            url: settings.share_url(&listing.id),
        });
    }

    let mut uploaded = 0_usize;
    let mut skipped = 0_usize;
    for (key, file, content_type) in &plan.objects {
        let body = std::fs::read(file)
            .with_context(|| format!("读取片段失败: {}", file.display()))
            .map_err(|err| wrap(Stage::Clips, uploaded, false)(err))?;
        let exists = store
            .head(key)
            .await
            .map_err(wrap(Stage::Clips, uploaded, false))?;
        if exists && !options.force {
            skipped += 1;
            continue;
        }
        if options.dry_run {
            uploaded += 1; // dry-run：统计“将要上传”的对象数
            continue;
        }
        store
            .put(key, &body, content_type, CLIP_CACHE_CONTROL)
            .await
            .map_err(wrap(Stage::Clips, uploaded, false))?;
        uploaded += 1;
    }

    if options.dry_run {
        return Ok(PublishOutcome {
            lesson_id: listing.id.clone(),
            title: listing.title.clone(),
            status: PublishStatus::Published,
            dry_run: true,
            uploaded,
            skipped,
            index_updated: false,
            url: settings.share_url(&listing.id),
        });
    }

    let payload_bytes = serde_json::to_vec_pretty(&plan.payload)
        .context("序列化 lesson.json 失败")
        .map_err(wrap(Stage::Commit, uploaded, false))?;
    store
        .put(
            &lesson_key,
            &payload_bytes,
            JSON_CONTENT_TYPE,
            MANIFEST_CACHE_CONTROL,
        )
        .await
        .map_err(wrap(Stage::Commit, uploaded, false))?;

    let index_key = "lessons/index.json";
    let index = store
        .get(index_key)
        .await
        .map_err(wrap(Stage::Index, uploaded, true))?
        .and_then(|body| serde_json::from_str::<Value>(&String::from_utf8_lossy(&body)).ok());
    let entry = index_entry(listing, &plan.content_hash, manifest.total_ms, &now);
    let merged = merge_index(index.as_ref(), &entry, &now);
    let index_bytes = serde_json::to_vec_pretty(&merged)
        .context("序列化 index.json 失败")
        .map_err(wrap(Stage::Index, uploaded, true))?;
    store
        .put(
            index_key,
            &index_bytes,
            JSON_CONTENT_TYPE,
            MANIFEST_CACHE_CONTROL,
        )
        .await
        .map_err(wrap(Stage::Index, uploaded, true))?;

    Ok(PublishOutcome {
        lesson_id: listing.id.clone(),
        title: listing.title.clone(),
        status: PublishStatus::Published,
        dry_run: false,
        uploaded,
        skipped,
        index_updated: true,
        url: settings.share_url(&listing.id),
    })
}

fn rfc3339_now() -> String {
    OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| String::new())
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or("")
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_listing() -> Listing {
        Listing {
            id: "049edc3427d6".to_string(),
            dir: "第5课".to_string(),
            title: "第5课 词语听写".to_string(),
            subject: "chinese".to_string(),
            grade: Some(3),
            term: None,
            item_count: 2,
            total_ms: 9000,
        }
    }

    fn sample_manifest() -> Manifest {
        serde_json::from_str(
            r#"{
                "version": 1,
                "title": "第5课 词语听写",
                "subject": "chinese",
                "grade": 3,
                "review_cap": 8,
                "generated_at": "2026-11-19T10:00:00+08:00",
                "defaults": {"gap_ms": 6000, "repeat": 2, "repeat_gap_ms": 1200,
                             "lead_in_ms": 3000, "intro_gap_ms": 1500},
                "intro": {"path": "clips/intro-a.wav", "duration_ms": 300},
                "intro_text": "开始",
                "items": [
                    {"index": 1, "text": "苹果", "lang": "zh",
                     "clips": [{"path": "clips/w0.wav", "duration_ms": 300}],
                     "repeat": 1, "repeat_gap_ms": 100, "gap_ms": 400},
                    {"index": 2, "text": "香蕉", "lang": "zh",
                     "clips": [{"path": "clips/w1.wav", "duration_ms": 300}],
                     "repeat": 1, "repeat_gap_ms": 100, "gap_ms": 400}
                ],
                "total_ms": 5000
            }"#,
        )
        .unwrap()
    }

    fn sample_dir() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let clips = dir.path().join("clips");
        fs::create_dir_all(&clips).unwrap();
        for name in ["w0.wav", "w1.wav", "intro-a.wav"] {
            fs::write(clips.join(name), b"RIFFfake").unwrap();
        }
        dir
    }

    fn sample_settings() -> PublishSettings {
        PublishSettings {
            enabled: true,
            data_base: Some("https://audio.example.com".to_string()),
            pages_base: Some("https://dictation.example.com".to_string()),
            bucket: Some("dictation".to_string()),
            account_id: Some("acct".to_string()),
            access_key_id: Some("AKID".to_string()),
            secret_access_key: Some("SECRET".to_string()),
            ..PublishSettings::default()
        }
    }

    // ---- SigV4 已知答案（用 Python 独立实现对照生成） ----

    #[test]
    fn sigv4_get_known_answer() {
        let store = S3Store {
            client: reqwest::Client::new(),
            endpoint: "https://acct.r2.cloudflarestorage.com".to_string(),
            host: "test.example.com".to_string(),
            bucket: "bucket".to_string(),
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            fixed_time: Some(OffsetDateTime::from_unix_timestamp(1_795_046_400).unwrap()),
        };
        let (amz_date, _) = store.amz_date().unwrap();
        assert_eq!(amz_date, "20261119T000000Z");
        let auth = store
            .authorization(
                "GET",
                "/bucket/lessons/abc/lesson.json",
                "",
                EMPTY_SHA256,
                &amz_date,
            )
            .unwrap();
        assert!(auth.ends_with(
            "Signature=415fe10da3acdc1e17daa1f1d1bb37885cbb167b3a012d2921903649cda5ec84"
        ));
        assert!(auth.contains("Credential=AKIDEXAMPLE/20261119/auto/s3/aws4_request"));
    }

    #[test]
    fn sigv4_put_known_answer() {
        let store = S3Store {
            client: reqwest::Client::new(),
            endpoint: "https://acct.r2.cloudflarestorage.com".to_string(),
            host: "test.example.com".to_string(),
            bucket: "bucket".to_string(),
            access_key_id: "AKIDEXAMPLE".to_string(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".to_string(),
            fixed_time: Some(OffsetDateTime::from_unix_timestamp(1_795_046_400).unwrap()),
        };
        let body = br#"{"a":1}"#;
        let payload_hash = hex_sha256(body);
        assert_eq!(
            payload_hash,
            "015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862"
        );
        let (amz_date, _) = store.amz_date().unwrap();
        let auth = store
            .authorization(
                "PUT",
                "/bucket/lessons/abc/lesson.json",
                "",
                &payload_hash,
                &amz_date,
            )
            .unwrap();
        assert!(auth.ends_with(
            "Signature=9641fab453110e765b3514f33a33914a48229493f8ff75bc2205fdcefe1e9c10"
        ));
    }

    #[test]
    fn uri_encode_keeps_slashes_and_escapes_spaces() {
        assert_eq!(
            S3Store::uri_encode("lessons/ab/clips/01-152a8adf.wav"),
            "lessons/ab/clips/01-152a8adf.wav"
        );
        assert_eq!(S3Store::uri_encode("a b"), "a%20b");
    }

    // ---- 载荷与哈希 ----

    #[test]
    fn payload_matches_contract_shape() {
        let listing = sample_listing();
        let dir = sample_dir();
        let manifest = sample_manifest();
        let plan = build_plan(&listing, &manifest, dir.path(), false).unwrap();

        assert_eq!(plan.payload["schema_version"], 1);
        assert_eq!(plan.payload["lesson_id"], listing.id);
        assert_eq!(plan.payload["subject"], "chinese");
        assert_eq!(plan.payload["grade"], 3);
        assert!(plan.payload["term"].is_null());
        assert_eq!(plan.payload["items"][0]["review"], false);
        assert!(plan.payload.get("full_track").is_none());
        assert_eq!(plan.objects.len(), 3); // 2 clips + intro
        assert_eq!(plan.content_hash, plan.payload["content_hash"]);
    }

    #[test]
    fn missing_clip_is_a_local_error() {
        let listing = sample_listing();
        let dir = tempfile::tempdir().unwrap();
        let manifest = sample_manifest();
        let err = build_plan(&listing, &manifest, dir.path(), false).unwrap_err();
        assert!(err.to_string().contains("片段缺失"));
    }

    // ---- index 合并 ----

    #[test]
    fn merge_keeps_unknown_entries_and_sorts_by_title() {
        let existing = json!({
            "schema_version": 1,
            "updated_at": "2026-11-01T00:00:00Z",
            "lessons": [
                {"id": "zzz", "title": "乙课", "path": "lessons/zzz/lesson.json"},
                {"id": "aaa", "title": "第5课", "path": "lessons/aaa/lesson.json",
                 "content_hash": "sha256:old"}
            ]
        });
        let entry = json!({
            "id": "aaa", "title": "第5课", "path": "lessons/aaa/lesson.json",
            "content_hash": "sha256:new"
        });
        let merged = merge_index(Some(&existing), &entry, "2026-11-19T00:00:00Z");
        let lessons = merged["lessons"].as_array().unwrap();
        assert_eq!(lessons.len(), 2); // 未知条目 zzz 保留，aaa 被替换
        assert_eq!(merged["updated_at"], "2026-11-19T00:00:00Z");
        assert_eq!(lessons[0]["id"], "zzz", "按 title 排序：乙课 < 第5课");
        assert_eq!(lessons[1]["content_hash"], "sha256:new", "同 id 条目被替换");
    }

    // ---- 发布流程 ----

    async fn fixture_setup() -> (tempfile::TempDir, Listing) {
        let dir = sample_dir();
        let listing = sample_listing();
        (dir, listing)
    }

    #[tokio::test]
    async fn first_publish_uploads_then_second_is_skipped() {
        let (_dir, listing) = fixture_setup().await;
        let store = MemoryStore::default();
        let settings = sample_settings();
        let lessons_dir = tempfile::tempdir().unwrap();
        // 把样例课时放到 lessons/<dir>/
        let src = lessons_dir.path().join(&listing.dir);
        fs::create_dir_all(src.join("clips")).unwrap();
        for name in ["w0.wav", "w1.wav", "intro-a.wav"] {
            fs::write(src.join("clips").join(name), b"RIFFfake").unwrap();
        }
        fs::write(
            src.join("manifest.json"),
            serde_json::to_string(&sample_manifest()).unwrap(),
        )
        .unwrap();

        let first = publish_lesson(
            &store,
            &settings,
            lessons_dir.path(),
            &listing.dir,
            PublishOptions::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(first.status, PublishStatus::Published);
        assert_eq!(first.uploaded, 3);
        assert_eq!(first.skipped, 0);
        assert!(first.index_updated);
        assert_eq!(
            first.url.as_deref(),
            Some("https://dictation.example.com/?l=049edc3427d6")
        );
        assert_eq!(store.object_count(), 3 + 1 + 1); // clips + lesson.json + index

        // 缓存语义（契约 §11.3）：clips immutable，lesson.json / index no-cache
        let clip = store.object("lessons/049edc3427d6/clips/w0.wav").unwrap();
        assert_eq!(clip.content_type, "audio/wav");
        assert_eq!(clip.cache_control, "public, max-age=31536000, immutable");
        let lesson_json = store.object("lessons/049edc3427d6/lesson.json").unwrap();
        assert_eq!(lesson_json.content_type, "application/json");
        assert_eq!(lesson_json.cache_control, "no-cache");
        let index = store.object("lessons/index.json").unwrap();
        assert_eq!(index.cache_control, "no-cache");

        let second = publish_lesson(
            &store,
            &settings,
            lessons_dir.path(),
            &listing.dir,
            PublishOptions::default(),
            None,
        )
        .await
        .unwrap();
        assert_eq!(second.status, PublishStatus::Skipped);
        assert_eq!(second.uploaded, 0);
        assert!(!second.index_updated);

        // force：全部重传
        let forced = publish_lesson(
            &store,
            &settings,
            lessons_dir.path(),
            &listing.dir,
            PublishOptions {
                force: true,
                ..PublishOptions::default()
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(forced.status, PublishStatus::Published);
        assert_eq!(forced.uploaded, 3);
    }

    #[tokio::test]
    async fn index_merge_preserves_remote_only_lessons() {
        let (_dir, listing) = fixture_setup().await;
        let store = MemoryStore::default();
        let settings = sample_settings();
        let lessons_dir = tempfile::tempdir().unwrap();
        let src = lessons_dir.path().join(&listing.dir);
        fs::create_dir_all(src.join("clips")).unwrap();
        for name in ["w0.wav", "w1.wav", "intro-a.wav"] {
            fs::write(src.join("clips").join(name), b"x").unwrap();
        }
        fs::write(
            src.join("manifest.json"),
            serde_json::to_string(&sample_manifest()).unwrap(),
        )
        .unwrap();

        // 远端已有一门本地不存在的历史课
        let old_index = json!({
            "schema_version": 1,
            "updated_at": "2026-11-01T00:00:00Z",
            "lessons": [{"id": "old9999", "title": "旧课", "path": "lessons/old9999/lesson.json"}]
        });
        store
            .put(
                "lessons/index.json",
                serde_json::to_vec_pretty(&old_index).unwrap().as_slice(),
                JSON_CONTENT_TYPE,
                MANIFEST_CACHE_CONTROL,
            )
            .await
            .unwrap();

        publish_lesson(
            &store,
            &settings,
            lessons_dir.path(),
            &listing.dir,
            PublishOptions::default(),
            None,
        )
        .await
        .unwrap();

        let index: Value =
            serde_json::from_slice(&store.get("lessons/index.json").await.unwrap().unwrap())
                .unwrap();
        let lessons = index["lessons"].as_array().unwrap();
        assert_eq!(lessons.len(), 2, "本地缺失的历史课不被丢弃");
        assert_eq!(lessons[0]["id"], "old9999");
    }

    #[tokio::test]
    async fn missing_local_clip_errors_without_writing() {
        let (_dir, listing) = fixture_setup().await;
        let store = MemoryStore::default();
        let settings = sample_settings();
        let lessons_dir = tempfile::tempdir().unwrap();
        let src = lessons_dir.path().join(&listing.dir);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            src.join("manifest.json"),
            serde_json::to_string(&sample_manifest()).unwrap(),
        )
        .unwrap();

        let err = publish_lesson(
            &store,
            &settings,
            lessons_dir.path(),
            &listing.dir,
            PublishOptions::default(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.stage, Stage::Local);
        assert_eq!(err.uploaded, 0);
        assert!(!err.committed);
        assert_eq!(store.object_count(), 0, "不发布残缺课");
    }

    #[tokio::test]
    async fn dry_run_uploads_nothing() {
        let (_dir, listing) = fixture_setup().await;
        let store = MemoryStore::default();
        let settings = sample_settings();
        let lessons_dir = tempfile::tempdir().unwrap();
        let src = lessons_dir.path().join(&listing.dir);
        fs::create_dir_all(src.join("clips")).unwrap();
        for name in ["w0.wav", "w1.wav", "intro-a.wav"] {
            fs::write(src.join("clips").join(name), b"x").unwrap();
        }
        fs::write(
            src.join("manifest.json"),
            serde_json::to_string(&sample_manifest()).unwrap(),
        )
        .unwrap();

        let outcome = publish_lesson(
            &store,
            &settings,
            lessons_dir.path(),
            &listing.dir,
            PublishOptions {
                dry_run: true,
                ..PublishOptions::default()
            },
            None,
        )
        .await
        .unwrap();
        assert!(outcome.dry_run);
        assert_eq!(outcome.uploaded, 3, "统计“将要上传”的对象数");
        assert_eq!(store.object_count(), 0);
        assert!(!outcome.index_updated);
    }

    #[tokio::test]
    async fn unknown_target_is_local_404ish_error() {
        let store = MemoryStore::default();
        let settings = sample_settings();
        let lessons_dir = tempfile::tempdir().unwrap();
        let err = publish_lesson(
            &store,
            &settings,
            lessons_dir.path(),
            "nope",
            PublishOptions::default(),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(err.stage, Stage::Local);
        assert!(err.message.contains("课时不存在"));
    }

    #[test]
    fn settings_missing_reports_all() {
        let settings = PublishSettings::default();
        let missing = settings.missing();
        assert!(missing.contains(&"enabled"));
        assert!(missing.contains(&"data_base"));
        assert!(missing.contains(&"R2_SECRET_ACCESS_KEY"));
        assert!(!settings.is_configured());
    }

    #[test]
    fn outcome_json_omits_url_when_pages_base_absent() {
        let outcome = PublishOutcome {
            lesson_id: "abc".to_string(),
            title: "第5课".to_string(),
            status: PublishStatus::Published,
            dry_run: false,
            uploaded: 3,
            skipped: 0,
            index_updated: true,
            url: None,
        };
        let json = outcome.to_json();
        assert!(json.get("url").is_none());
        assert_eq!(json["uploaded"], 3);
        assert_eq!(json["status"], "published");
    }

    // ---- Pages 壳生成（B 方案） ----

    fn shell_web() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("index.html"), "<!doctype html>").unwrap();
        fs::write(dir.path().join("app.js"), "// app").unwrap();
        fs::write(
            dir.path().join("config.js"),
            "window.DICTATION_CONFIG = { mode: 'server' };\n",
        )
        .unwrap();
        dir
    }

    #[test]
    fn shell_generates_pages_project_with_binding() {
        let web = shell_web();
        let out = tempfile::tempdir().unwrap();
        let mut settings = sample_settings();
        settings.bucket = Some("dictation".to_string());
        settings.pages_base = Some("https://dictation.example.com".to_string());
        settings.data_base = None;
        settings.pages_project = Some("dictation".to_string());

        let outcome = write_shell(&settings, web.path(), out.path(), false).unwrap();
        assert_eq!(outcome.data_base, "https://dictation.example.com/data");
        assert_eq!(outcome.bucket, "dictation");

        let config = fs::read_to_string(out.path().join("public/config.js")).unwrap();
        assert!(config.contains("mode: 'static'"));
        assert!(config.contains("https://dictation.example.com/data"));

        let wrangler = fs::read_to_string(out.path().join("wrangler.jsonc")).unwrap();
        assert!(wrangler.contains("\"binding\": \"BUCKET\""));
        assert!(wrangler.contains("\"bucket_name\": \"dictation\""));
        assert!(wrangler.contains("\"pages_build_output_dir\": \"./public\""));

        let function = fs::read_to_string(out.path().join("functions/data/[[path]].js")).unwrap();
        assert!(function.contains("env.BUCKET.get(key)"));
        assert!(function.contains("key.startsWith('lessons/')"));
        assert!(function.contains("immutable"));

        assert!(out.path().join("public/_headers").is_file());
        assert!(out.path().join("public/index.html").is_file());
    }

    #[test]
    fn shell_dry_run_writes_nothing() {
        let web = shell_web();
        let out = tempfile::tempdir().unwrap();
        let settings = sample_settings();
        let outcome = write_shell(&settings, web.path(), out.path(), true).unwrap();
        assert_eq!(outcome.files, 0);
        assert!(!out.path().join("public").exists());
        assert!(!out.path().join("wrangler.jsonc").exists());
    }

    #[test]
    fn shell_rebuild_removes_stale_assets() {
        let web = shell_web();
        let out = tempfile::tempdir().unwrap();
        let settings = sample_settings();
        write_shell(&settings, web.path(), out.path(), false).unwrap();
        fs::write(out.path().join("public/removed.js"), "old").unwrap();

        write_shell(&settings, web.path(), out.path(), false).unwrap();
        assert!(
            !out.path().join("public/removed.js").exists(),
            "重新生成时清理旧产物"
        );
    }

    #[test]
    fn shell_requires_bucket() {
        let web = shell_web();
        let out = tempfile::tempdir().unwrap();
        let mut settings = sample_settings();
        settings.bucket = None;
        let err = write_shell(&settings, web.path(), out.path(), false).unwrap_err();
        assert!(err.to_string().contains("bucket"));
    }
}
