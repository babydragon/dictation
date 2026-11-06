//! P1 HTTP service: lesson list, sessions, review scheduling and media.

use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::cache::Cache;
use crate::config::Config;
use crate::db::{self, CheckError, Db, NewItem, SessionRecord};
use crate::lesson::import as lesson_import;
use crate::manifest::{Manifest, ManifestClip, ManifestDefaults};
use crate::model::ProviderKind;
use crate::render::{self, RenderOptions};
use crate::tts;

const REVIEW_LEAD_IN_MS: u32 = 3_000;
const REVIEW_GAP_MS: u32 = 6_000;
const REVIEW_REPEAT: u8 = 2;
const REVIEW_REPEAT_GAP_MS: u32 = 1_200;

/// How many review words may be mixed into a lesson session.
const MIX_REVIEW_LIMIT: u32 = 5;

#[derive(Debug, Clone)]
pub struct ServeOptions {
    pub lessons_dir: PathBuf,
    pub web_dir: PathBuf,
    pub data_path: PathBuf,
    pub cache_dir: PathBuf,
    pub host: String,
    pub port: u16,
}

pub async fn serve(opts: ServeOptions) -> Result<()> {
    let lessons = {
        let db = Db::open(&opts.data_path)
            .with_context(|| format!("打开数据库失败: {}", opts.data_path.display()))?;
        db.scan_lessons(&opts.lessons_dir)
            .map(|l| l.len())
            .unwrap_or(0)
    };
    let app = build_router(opts.clone()).await?;

    let addr = format!("{}:{}", opts.host.trim(), opts.port);
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("监听 {addr} 失败（端口被占用或地址不可用）"))?;
    let local = listener
        .local_addr()
        .map(|addr| addr.to_string())
        .unwrap_or(addr);

    info!("听写服务已启动：http://{local}（{lessons} 个课时）");
    println!("听写服务已启动：http://{local}");
    println!("  课时目录：{}", opts.lessons_dir.display());
    println!("  前端目录：{}", opts.web_dir.display());
    println!("  数据库：{}", opts.data_path.display());
    println!("  按 Ctrl+C 停止。");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("HTTP 服务异常退出")?;
    info!("服务已停止");
    Ok(())
}

/// Build the database, in-memory job registry and router without binding a
/// socket. Used by [`serve`] and by the HTTP tests.
pub async fn build_router(opts: ServeOptions) -> Result<Router> {
    let db = Db::open(&opts.data_path)
        .with_context(|| format!("打开数据库失败: {}", opts.data_path.display()))?;

    match db.scan_lessons(&opts.lessons_dir) {
        Ok(lessons) => info!(
            "扫描到 {} 个课时：{}",
            lessons.len(),
            opts.lessons_dir.display()
        ),
        Err(err) => warn!("扫描课时目录失败（{}）：{err}", opts.lessons_dir.display()),
    }
    if !opts.web_dir.is_dir() {
        warn!(
            "前端目录不存在：{}（API 仍可用，静态页面会返回 500）",
            opts.web_dir.display()
        );
    }

    let tts = server_tts_config(&opts);
    let api_token = std::env::var("DICTATION_API_TOKEN")
        .ok()
        .map(|token| token.trim().to_string())
        .filter(|token| !token.is_empty())
        .map(Arc::from);

    let cache_dir = tts
        .as_ref()
        .map(|config| config.cache_dir.clone())
        .unwrap_or_else(|| opts.cache_dir.clone());
    let state = AppState {
        db: Arc::new(db),
        lessons_dir: opts.lessons_dir,
        web_dir: opts.web_dir,
        cache_dir,
        tts,
        jobs: Arc::new(Mutex::new(JobRegistry::default())),
        gen_permits: Arc::new(Semaphore::new(1)),
        api_token,
    };
    Ok(router(state))
}

/// TTS config for the serve process. Missing keys are not fatal: generation
/// simply returns 503 until the operator installs `/etc/dictation/env`.
fn server_tts_config(opts: &ServeOptions) -> Option<Arc<Config>> {
    let mut config = match Config::from_env() {
        Ok(config) => config,
        Err(err) => {
            warn!("读取 TTS 配置失败，生成端点将返回 503：{err:#}");
            return None;
        }
    };
    let cache_from_env = std::env::var("DICTATION_CACHE_DIR")
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);
    if !cache_from_env {
        config.cache_dir = opts.cache_dir.clone();
    }
    if missing_key(&config, config.default_provider) {
        warn!(
            "未配置 {:?} 的 TTS 密钥（/etc/dictation/env），生成端点将返回 503",
            config.default_provider
        );
        return None;
    }
    Some(Arc::new(config))
}

fn missing_key(config: &Config, kind: ProviderKind) -> bool {
    match kind {
        ProviderKind::Mock => false,
        ProviderKind::Qwen => config.qwen.api_key.is_none(),
        ProviderKind::Doubao => config.doubao.api_key.is_none(),
    }
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/lessons", get(list_lessons).post(import_lesson))
        .route("/api/lessons/{id}/generate", post(generate_lesson))
        .route("/api/lessons/{id}/generate/cancel", post(cancel_generation))
        .route("/api/jobs/{id}", get(get_job))
        .route("/api/review/summary", get(review_summary))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/review", post(create_review_session))
        .route("/api/sessions/{id}/check", post(check_session))
        .route("/media/{lesson_id}/{*path}", get(media))
        .fallback(get(static_file))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[derive(Clone)]
struct AppState {
    db: Arc<Db>,
    lessons_dir: PathBuf,
    web_dir: PathBuf,
    cache_dir: PathBuf,
    tts: Option<Arc<Config>>,
    jobs: Arc<Mutex<JobRegistry>>,
    gen_permits: Arc<Semaphore>,
    api_token: Option<Arc<str>>,
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

struct ApiError {
    status: StatusCode,
    message: String,
    /// Extra top-level fields merged into the `{"error": ...}` body.
    extra: Option<Value>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            extra: None,
        }
    }

    fn with_extra(mut self, extra: Value) -> Self {
        self.extra = Some(extra);
        self
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new(StatusCode::NOT_FOUND, message)
    }

    fn conflict(message: impl Into<String>) -> Self {
        Self::new(StatusCode::CONFLICT, message)
    }

    fn internal(err: anyhow::Error) -> Self {
        tracing::error!("服务端错误：{err:#}");
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("服务端错误：{err}"),
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({ "error": self.message });
        if let Some(extra) = self.extra {
            if let (Some(target), Value::Object(fields)) = (body.as_object_mut(), extra) {
                for (key, value) in fields {
                    target.insert(key, value);
                }
            }
        }
        (self.status, Json(body)).into_response()
    }
}

impl From<CheckError> for ApiError {
    fn from(err: CheckError) -> Self {
        match err {
            CheckError::NotFound => ApiError::not_found(err.to_string()),
            CheckError::AlreadyFinished => ApiError::conflict(err.to_string()),
            CheckError::BadRequest(message) => ApiError::bad_request(message),
            CheckError::Db(err) => ApiError::internal(err),
        }
    }
}

async fn run_db<T, F>(db: Arc<Db>, f: F) -> Result<T, ApiError>
where
    F: FnOnce(&Db) -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(move || f(&db)).await {
        Ok(result) => result,
        Err(err) => Err(ApiError::internal(anyhow::anyhow!(
            "后台数据库任务失败：{err}"
        ))),
    }
}

// ---------------------------------------------------------------------------
// Auth
// ---------------------------------------------------------------------------

const NO_TTS_MESSAGE: &str = "未配置 TTS 密钥（/etc/dictation/env）";
const UNAUTHORIZED_MESSAGE: &str = "未授权";

/// Length-aware constant-time byte comparison (no external dependency).
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        diff |= left ^ right;
    }
    diff == 0
}

fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    let Some(expected) = state.api_token.as_deref() else {
        return Ok(());
    };
    let provided = headers
        .get("x-dictation-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if ct_eq(provided.as_bytes(), expected.as_bytes()) {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            UNAUTHORIZED_MESSAGE,
        ))
    }
}

// ---------------------------------------------------------------------------
// Generation jobs (in-memory only)
// ---------------------------------------------------------------------------

const MAX_QUEUED_JOBS: usize = 4;
const MAX_JOBS: usize = 100;
const JOB_RETENTION: Duration = Duration::from_secs(3_600);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum JobStatus {
    Queued,
    Running,
    Done,
    Error,
    Cancelled,
}

impl JobStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Done => "done",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct JobResult {
    total_ms: u32,
    tts_calls: usize,
    cache_hits: usize,
    clips: usize,
}

#[derive(Debug, Clone)]
struct Job {
    id: String,
    lesson: String,
    status: JobStatus,
    queued_at: String,
    started_at: Option<String>,
    finished_at: Option<String>,
    result: Option<JobResult>,
    error: Option<String>,
    /// When the job reached a terminal state; used for retention.
    terminal_at: Option<Instant>,
}

impl Job {
    fn queued(id: String, lesson: String) -> Self {
        Self {
            id,
            lesson,
            status: JobStatus::Queued,
            queued_at: now_rfc3339(),
            started_at: None,
            finished_at: None,
            result: None,
            error: None,
            terminal_at: None,
        }
    }

    fn mark_running(&mut self) {
        self.status = JobStatus::Running;
        self.started_at = Some(now_rfc3339());
    }

    fn mark_done(&mut self, result: JobResult) {
        self.status = JobStatus::Done;
        self.finished_at = Some(now_rfc3339());
        self.result = Some(result);
        self.terminal_at = Some(Instant::now());
    }

    fn mark_error(&mut self, message: String) {
        self.status = JobStatus::Error;
        self.finished_at = Some(now_rfc3339());
        self.error = Some(message);
        self.terminal_at = Some(Instant::now());
    }

    fn mark_cancelled(&mut self) {
        self.status = JobStatus::Cancelled;
        self.finished_at = Some(now_rfc3339());
        self.terminal_at = Some(Instant::now());
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.status,
            JobStatus::Done | JobStatus::Error | JobStatus::Cancelled
        )
    }

    fn view(&self) -> Value {
        json!({
            "job_id": self.id,
            "status": self.status.as_str(),
            "lesson": self.lesson,
            "queued_at": self.queued_at,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "result": self.result,
            "error": self.error,
        })
    }
}

#[derive(Debug, Default)]
struct JobRegistry {
    jobs: HashMap<String, Job>,
}

impl JobRegistry {
    fn insert(&mut self, job: Job) {
        self.jobs.insert(job.id.clone(), job);
        self.prune();
    }

    fn get(&self, id: &str) -> Option<&Job> {
        self.jobs.get(id)
    }

    fn get_mut(&mut self, id: &str) -> Option<&mut Job> {
        self.jobs.get_mut(id)
    }

    /// Job id for a lesson that is still queued or running.
    fn active_for(&self, lesson: &str) -> Option<String> {
        self.jobs
            .values()
            .find(|job| {
                job.lesson == lesson && matches!(job.status, JobStatus::Queued | JobStatus::Running)
            })
            .map(|job| job.id.clone())
    }

    fn queued_count(&self) -> usize {
        self.jobs
            .values()
            .filter(|job| job.status == JobStatus::Queued)
            .count()
    }

    /// Drop terminal jobs older than one hour, then keep at most 100 entries.
    fn prune(&mut self) {
        let now = Instant::now();
        self.jobs.retain(|_, job| match job.terminal_at {
            Some(at) => now.saturating_duration_since(at) < JOB_RETENTION,
            None => true,
        });
        while self.jobs.len() > MAX_JOBS {
            let oldest = self
                .jobs
                .values()
                .filter(|job| job.is_terminal())
                .min_by_key(|job| job.terminal_at)
                .map(|job| job.id.clone());
            match oldest {
                Some(id) => {
                    self.jobs.remove(&id);
                }
                None => break,
            }
        }
    }
}

fn now_rfc3339() -> String {
    let offset = time::UtcOffset::from_hms(8, 0, 0).unwrap_or(time::UtcOffset::UTC);
    let now = time::OffsetDateTime::now_utc().to_offset(offset);
    now.format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| now.to_string())
}

// ---------------------------------------------------------------------------
// S1 handlers: import + generate
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
struct ImportRequestBody {
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    text: String,
    #[serde(default)]
    toml: Option<String>,
    #[serde(default)]
    dir: Option<String>,
    #[serde(default)]
    force: bool,
}

async fn import_lesson(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, ApiError> {
    // Auth first: the body is only inspected after the token check so an
    // invalid/missing token always wins over a bad payload.
    authorize(&state, &headers)?;
    if body.iter().all(u8::is_ascii_whitespace) {
        return Err(ApiError::bad_request("请求体无效：请求体为空"));
    }
    let body: ImportRequestBody = serde_json::from_slice(&body)
        .map_err(|err| ApiError::bad_request(format!("请求体无效：{err}")))?;

    let lessons_dir = state.lessons_dir.clone();
    let request = lesson_import::ImportRequest {
        title: body.title,
        text: body.text,
        toml: body.toml,
        dir: body.dir,
        force: body.force,
    };
    let outcome = tokio::task::spawn_blocking(move || lesson_import::import(&lessons_dir, request))
        .await
        .map_err(|err| ApiError::internal(anyhow::anyhow!("导入任务失败：{err}")))?;

    match outcome {
        Ok(outcome) => {
            let status = if outcome.created {
                StatusCode::CREATED
            } else {
                StatusCode::OK
            };
            Ok((
                status,
                Json(json!({
                    "created": outcome.created,
                    "dir": outcome.dir_name,
                    "title": outcome.title,
                    "item_count": outcome.item_count,
                })),
            )
                .into_response())
        }
        Err(err) => {
            if let Some(exists) = err.downcast_ref::<lesson_import::LessonExists>() {
                Err(ApiError::new(StatusCode::CONFLICT, "课时已存在")
                    .with_extra(json!({ "dir": exists.dir_name })))
            } else {
                Err(ApiError::bad_request(format!("{err:#}")))
            }
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct GenerateRequestBody {
    #[serde(default)]
    force: bool,
    #[serde(default)]
    full_track: bool,
    #[serde(default)]
    provider: Option<String>,
}

async fn generate_lesson(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
    body: Bytes,
) -> Result<Response, ApiError> {
    authorize(&state, &headers)?;
    let body: GenerateRequestBody = if body.iter().all(u8::is_ascii_whitespace) {
        GenerateRequestBody::default()
    } else {
        serde_json::from_slice(&body)
            .map_err(|err| ApiError::bad_request(format!("请求体无效：{err}")))?
    };

    let config = state
        .tts
        .as_ref()
        .ok_or_else(|| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, NO_TTS_MESSAGE))?;
    let kind = match body.provider.as_deref().map(str::trim) {
        Some(raw) if !raw.is_empty() => raw
            .parse::<ProviderKind>()
            .map_err(|_| ApiError::bad_request(format!("未知 provider：{raw}")))?,
        _ => config.default_provider,
    };
    if missing_key(config, kind) {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            NO_TTS_MESSAGE,
        ));
    }
    let provider = tts::build_provider(kind, config)
        .map_err(|_| ApiError::new(StatusCode::SERVICE_UNAVAILABLE, NO_TTS_MESSAGE))?;

    let (dir_name, dir_path) = resolve_lesson(&state, &id).await?;

    if !body.force && manifest_is_fresh(&dir_path) {
        return Ok(Json(json!({
            "job_id": null,
            "status": "done",
            "skipped": true,
            "lesson": dir_name,
        }))
        .into_response());
    }

    let job_id = uuid::Uuid::new_v4().simple().to_string();
    {
        let mut jobs = state.jobs.lock().unwrap();
        if let Some(existing) = jobs.active_for(&dir_name) {
            let status = jobs
                .get(&existing)
                .map(|job| job.status.as_str())
                .unwrap_or("queued");
            return Ok((
                StatusCode::ACCEPTED,
                Json(json!({
                    "job_id": existing,
                    "status": status,
                    "lesson": dir_name,
                })),
            )
                .into_response());
        }
        if jobs.queued_count() >= MAX_QUEUED_JOBS {
            return Err(ApiError::new(StatusCode::TOO_MANY_REQUESTS, "生成队列已满"));
        }
        jobs.insert(Job::queued(job_id.clone(), dir_name.clone()));
    }

    let task_state = state.clone();
    let task_job_id = job_id.clone();
    let task_lesson = dir_name.clone();
    let task_dir = dir_path.clone();
    let full_track = body.full_track;
    tokio::spawn(async move {
        run_generation(
            task_state,
            task_job_id,
            task_lesson,
            task_dir,
            provider,
            full_track,
        )
        .await;
    });

    Ok((
        StatusCode::ACCEPTED,
        Json(json!({
            "job_id": job_id,
            "status": "queued",
            "lesson": dir_name,
        })),
    )
        .into_response())
}

async fn get_job(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let mut jobs = state.jobs.lock().unwrap();
    jobs.prune();
    let job = jobs
        .get(&id)
        .ok_or_else(|| ApiError::not_found("任务不存在或已过期"))?;
    Ok(Json(job.view()))
}

async fn cancel_generation(
    State(state): State<AppState>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    authorize(&state, &headers)?;
    let (dir_name, _) = resolve_lesson(&state, &id).await?;

    let mut jobs = state.jobs.lock().unwrap();
    let job_id = jobs
        .active_for(&dir_name)
        .ok_or_else(|| ApiError::not_found("没有可取消的任务"))?;
    let job = jobs
        .get_mut(&job_id)
        .ok_or_else(|| ApiError::not_found("没有可取消的任务"))?;
    match job.status {
        JobStatus::Queued => {
            job.mark_cancelled();
            Ok(Json(json!({ "status": "cancelled" })))
        }
        JobStatus::Running => Err(ApiError::conflict("任务已在执行，无法取消")),
        _ => Err(ApiError::not_found("没有可取消的任务")),
    }
}

/// Resolve `{id}` to a lesson directory: exact directory name first, then a
/// 12-hex `lessons.id` from the database.
async fn resolve_lesson(state: &AppState, raw: &str) -> Result<(String, PathBuf), ApiError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(ApiError::not_found("课时不存在"));
    }
    if let Ok(candidate) = crate::app_config::resolve_lesson_dir(&state.lessons_dir, raw) {
        if candidate.join("lesson.txt").is_file() {
            let name = candidate
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| raw.to_string());
            return Ok((name, candidate));
        }
    }

    let lookup = raw.to_string();
    let lessons_dir = state.lessons_dir.clone();
    let found = run_db(state.db.clone(), move |db| {
        // Refresh the DB in case the manifest was written after startup.
        let _ = db.scan_lessons(&lessons_dir);
        db.lesson_by_id(&lookup).map_err(ApiError::internal)
    })
    .await?;
    let Some(entry) = found else {
        return Err(ApiError::not_found("课时不存在"));
    };
    let path = PathBuf::from(&entry.dir);
    if !path.starts_with(&state.lessons_dir) {
        return Err(ApiError::not_found("课时不存在"));
    }
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| ApiError::not_found("课时不存在"))?;
    Ok((name, path))
}

/// True when `manifest.json` is at least as new as `lesson.txt`/`lesson.toml`.
fn manifest_is_fresh(lesson_dir: &Path) -> bool {
    let manifest = lesson_dir.join("manifest.json");
    let Ok(manifest_meta) = std::fs::metadata(&manifest) else {
        return false;
    };
    let Ok(manifest_time) = manifest_meta.modified() else {
        return false;
    };
    for name in ["lesson.txt", "lesson.toml"] {
        let path = lesson_dir.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if let Ok(time) = meta.modified() {
            if time > manifest_time {
                return false;
            }
        }
    }
    true
}

async fn run_generation(
    state: AppState,
    job_id: String,
    lesson_name: String,
    lesson_dir: PathBuf,
    provider: Arc<dyn tts::TtsProvider>,
    full_track: bool,
) {
    let permit = match state.gen_permits.clone().acquire_owned().await {
        Ok(permit) => permit,
        Err(_) => return,
    };

    {
        let mut jobs = state.jobs.lock().unwrap();
        match jobs.get_mut(&job_id) {
            Some(job) if job.status == JobStatus::Queued => job.mark_running(),
            // Cancelled before it got a slot.
            _ => return,
        }
    }

    let cache_dir = state.cache_dir.clone();
    let Some(config) = state.tts.clone() else {
        let mut jobs = state.jobs.lock().unwrap();
        if let Some(job) = jobs.get_mut(&job_id) {
            job.mark_error(NO_TTS_MESSAGE.to_string());
        }
        drop(permit);
        return;
    };

    let result = generate(
        &config,
        &cache_dir,
        &lesson_dir,
        provider.as_ref(),
        full_track,
    )
    .await;

    let mut jobs = state.jobs.lock().unwrap();
    if let Some(job) = jobs.get_mut(&job_id) {
        match result {
            Ok(result) => {
                let clips = count_wav_clips(&lesson_dir.join("clips"));
                job.mark_done(JobResult {
                    total_ms: result.total_ms,
                    tts_calls: result.tts_calls,
                    cache_hits: result.cache_hits,
                    clips,
                });
            }
            Err(err) => {
                warn!("生成失败（{lesson_name}）：{err:#}");
                job.mark_error(format!("{err:#}"));
            }
        }
    }
    drop(permit);
}

async fn generate(
    config: &Config,
    cache_dir: &Path,
    lesson_dir: &Path,
    provider: &dyn tts::TtsProvider,
    full_track: bool,
) -> Result<render::RenderResult> {
    let path = lesson_dir.join("lesson.txt");
    let lesson = tokio::task::spawn_blocking(move || crate::lesson::parse_file(&path))
        .await
        .context("解析词表任务失败")??;

    let cache = Cache::new(cache_dir);
    let options = RenderOptions {
        out_dir: lesson_dir.to_path_buf(),
        full_track,
    };
    render::render(&lesson, provider, &cache, config, &options).await
}

fn count_wav_clips(dir: &Path) -> usize {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok())
                .filter(|entry| {
                    entry
                        .path()
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
                })
                .count()
        })
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Session manifest payload
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct SessionManifest {
    session_id: String,
    kind: String,
    title: String,
    base: String,
    defaults: ManifestDefaults,
    intro: Option<ManifestClip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    intro_text: Option<String>,
    outro: Option<ManifestClip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    outro_text: Option<String>,
    items: Vec<SessionItem>,
    total_ms: u32,
}

#[derive(Debug, Serialize)]
struct SessionItem {
    index: usize,
    text: String,
    lang: String,
    base: String,
    clips: Vec<ManifestClip>,
    repeat: u8,
    repeat_gap_ms: u32,
    gap_ms: u32,
    review: bool,
}

fn item_json(index: usize, item: &NewItem) -> SessionItem {
    SessionItem {
        index,
        text: item.text.clone(),
        lang: item.lang.clone(),
        base: media_base(&item.clip_lesson),
        clips: vec![ManifestClip {
            path: item.clip_path.clone(),
            duration_ms: item.duration_ms,
        }],
        repeat: item.repeat.max(1),
        repeat_gap_ms: item.repeat_gap_ms,
        gap_ms: item.gap_ms,
        review: item.is_review,
    }
}

fn media_base(lesson_id: &str) -> String {
    format!("/media/{lesson_id}")
}

fn review_total_ms(items: &[NewItem]) -> u32 {
    let mut total = REVIEW_LEAD_IN_MS;
    for item in items {
        let repeat = u32::from(item.repeat.max(1));
        total +=
            item.duration_ms * repeat + item.repeat_gap_ms * repeat.saturating_sub(1) + item.gap_ms;
    }
    total
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

async fn list_lessons(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let dir = state.lessons_dir.clone();
    let lessons = run_db(state.db.clone(), move |db| {
        db.scan_lessons(&dir).map_err(ApiError::internal)
    })
    .await?;
    let lessons: Vec<Value> = lessons
        .iter()
        .map(|lesson| {
            json!({
                "id": lesson.id,
                "title": lesson.title,
                "item_count": lesson.item_count,
                "total_ms": lesson.total_ms,
            })
        })
        .collect();
    Ok(Json(json!({ "lessons": lessons })))
}

async fn review_summary(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let summary = run_db(state.db.clone(), move |db| {
        db.review_summary().map_err(ApiError::internal)
    })
    .await?;
    Ok(Json(json!({
        "due_count": summary.due_count,
        "wrong_count": summary.wrong_count,
        "review_count": summary.review_count,
    })))
}

#[derive(Debug, Deserialize)]
struct CreateSessionRequest {
    lesson_id: String,
}

async fn create_session(
    State(state): State<AppState>,
    payload: Result<Json<CreateSessionRequest>, JsonRejection>,
) -> Result<Json<SessionManifest>, ApiError> {
    let Json(body) = payload.map_err(json_rejection)?;
    if body.lesson_id.trim().is_empty() {
        return Err(ApiError::bad_request("lesson_id 不能为空"));
    }
    let lesson_id = body.lesson_id.trim().to_string();

    let db = state.db.clone();
    let lessons_dir = state.lessons_dir.clone();
    let lookup_id = lesson_id.clone();
    let lesson = run_db(db.clone(), move |db| {
        let found = db
            .lesson_by_id(&lookup_id)
            .map_err(ApiError::internal)?
            .or_else(|| {
                db.scan_lessons(&lessons_dir)
                    .ok()
                    .and_then(|_| db.lesson_by_id(&lookup_id).ok().flatten())
            });
        found.ok_or_else(|| ApiError::not_found("课时不存在，请先运行 dictation gen"))
    })
    .await?;

    if !Path::new(&lesson.dir).join("manifest.json").is_file() {
        return Err(ApiError::not_found(
            "课时文件不存在，请重新运行 dictation gen",
        ));
    }
    let manifest = read_manifest(Path::new(&lesson.dir))
        .map_err(|err| ApiError::internal(err.context("读取 manifest 失败")))?;
    let mut items = lesson_items(&lesson.id, &manifest);
    if items.is_empty() {
        return Err(ApiError::bad_request("课时没有可播放的片段"));
    }

    // Append up to 5 review words after the lesson items; their audio still
    // lives in the original lesson, so each item carries its own `base`.
    let exclude_texts: Vec<String> = items.iter().map(|item| item.text.clone()).collect();
    let mixed = run_db(db.clone(), move |db| {
        db.review_items_for_mix(MIX_REVIEW_LIMIT, &exclude_texts)
            .map_err(ApiError::internal)
    })
    .await?;
    items.extend(mixed);

    let session_lesson_id = lesson.id.clone();
    let session_title = lesson.title.clone();
    let record = run_db(db, move |db| {
        db.create_lesson_session(&session_lesson_id, &session_title, &items)
            .map_err(ApiError::internal)
    })
    .await?;

    Ok(Json(lesson_manifest(
        &record,
        &manifest,
        &lesson.id,
        &lesson.title,
    )))
}

fn lesson_items(lesson_id: &str, manifest: &Manifest) -> Vec<NewItem> {
    manifest
        .items
        .iter()
        .filter_map(|item| {
            let clip = item.clips.first()?;
            Some(NewItem {
                text: item.text.clone(),
                lang: db::lang_str(item.lang).to_string(),
                clip_lesson: lesson_id.to_string(),
                clip_path: clip.path.clone(),
                duration_ms: clip.duration_ms,
                gap_ms: item.gap_ms,
                repeat: item.repeat.max(1),
                repeat_gap_ms: item.repeat_gap_ms,
                is_review: false,
            })
        })
        .collect()
}

fn lesson_manifest(
    record: &SessionRecord,
    manifest: &Manifest,
    lesson_id: &str,
    title: &str,
) -> SessionManifest {
    SessionManifest {
        session_id: record.id.clone(),
        kind: record.kind.clone(),
        title: title.to_string(),
        base: media_base(lesson_id),
        defaults: manifest.defaults.clone(),
        intro: manifest.intro.clone(),
        intro_text: manifest.intro_text.clone(),
        outro: manifest.outro.clone(),
        outro_text: manifest.outro_text.clone(),
        items: record
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| item_json(index + 1, item))
            .collect(),
        total_ms: session_total_ms(&record.items, manifest),
    }
}

/// Playable length of a session: lead-in + intro + intro gap + every item
/// (including mixed review words) + outro.
fn session_total_ms(items: &[NewItem], manifest: &Manifest) -> u32 {
    let defaults = &manifest.defaults;
    let mut total = u64::from(defaults.lead_in_ms);
    if let Some(intro) = &manifest.intro {
        total += u64::from(intro.duration_ms) + u64::from(defaults.intro_gap_ms);
    }
    for item in items {
        let repeat = u64::from(item.repeat.max(1));
        total += u64::from(item.duration_ms) * repeat
            + u64::from(item.repeat_gap_ms) * repeat.saturating_sub(1)
            + u64::from(item.gap_ms);
    }
    if let Some(outro) = &manifest.outro {
        total += u64::from(outro.duration_ms);
    }
    total.min(u64::from(u32::MAX)) as u32
}

async fn create_review_session(
    State(state): State<AppState>,
) -> Result<Json<SessionManifest>, ApiError> {
    let db = state.db.clone();
    let record = run_db(db, move |db| {
        db.create_review_session().map_err(ApiError::internal)
    })
    .await?;
    let Some(record) = record else {
        return Err(ApiError::conflict("没有到期错词"));
    };

    let total_ms = review_total_ms(&record.items);
    Ok(Json(SessionManifest {
        session_id: record.id,
        kind: record.kind,
        title: record.title,
        base: String::new(),
        defaults: ManifestDefaults {
            gap_ms: REVIEW_GAP_MS,
            repeat: REVIEW_REPEAT,
            repeat_gap_ms: REVIEW_REPEAT_GAP_MS,
            lead_in_ms: REVIEW_LEAD_IN_MS,
            intro_gap_ms: 0,
        },
        intro: None,
        intro_text: None,
        outro: None,
        outro_text: None,
        items: record
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| item_json(index + 1, item))
            .collect(),
        total_ms,
    }))
}

#[derive(Debug, Deserialize)]
struct CheckRequest {
    results: Vec<CheckResult>,
}

#[derive(Debug, Deserialize)]
struct CheckResult {
    position: usize,
    correct: bool,
}

async fn check_session(
    State(state): State<AppState>,
    UrlPath(session_id): UrlPath<String>,
    payload: Result<Json<CheckRequest>, JsonRejection>,
) -> Result<Json<Value>, ApiError> {
    let Json(body) = payload.map_err(json_rejection)?;
    if body.results.is_empty() {
        return Err(ApiError::bad_request("results 不能为空"));
    }
    let results: Vec<(usize, bool)> = body
        .results
        .iter()
        .map(|result| (result.position, result.correct))
        .collect();

    let outcome = run_db(state.db.clone(), move |db| {
        db.check_session(&session_id, &results)
            .map_err(ApiError::from)
    })
    .await?;

    Ok(Json(json!({
        "correct": outcome.correct,
        "wrong": outcome.wrong,
        "due_count": outcome.due_count,
    })))
}

async fn media(
    State(state): State<AppState>,
    UrlPath((lesson_id, rel)): UrlPath<(String, String)>,
) -> Result<Response, ApiError> {
    let lookup = lesson_id.clone();
    let lesson = run_db(state.db.clone(), move |db| {
        db.lesson_by_id(&lookup).map_err(ApiError::internal)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("课时不存在"))?;

    let path = resolve_media_path(Path::new(&lesson.dir), &rel)
        .ok_or_else(|| ApiError::not_found("文件不存在"))?;

    let bytes = tokio::fs::read(&path)
        .await
        .map_err(|err| ApiError::not_found(format!("读取音频失败：{err}")))?;

    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type(&path))],
        bytes,
    )
        .into_response())
}

/// Only `clips/...` inside `lesson_dir`, canonicalized, no traversal.
fn resolve_media_path(lesson_dir: &Path, rel: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute() || rel_path.components().next().is_none() {
        return None;
    }
    match rel_path.components().next() {
        Some(Component::Normal(first)) if first == "clips" => {}
        _ => return None,
    }
    let joined = safe_join(lesson_dir, rel_path)?;
    let root = std::fs::canonicalize(lesson_dir).ok()?;
    let clips = root.join("clips");
    let candidate = std::fs::canonicalize(&joined).ok()?;
    if !candidate.starts_with(&clips) || !candidate.is_file() {
        return None;
    }
    Some(candidate)
}

/// Join `rel` onto `root`, rejecting absolute paths and `..`.
fn safe_join(root: &Path, rel: &Path) -> Option<PathBuf> {
    if rel.is_absolute() {
        return None;
    }
    let mut out = root.to_path_buf();
    let mut depth = 0usize;
    for component in rel.components() {
        match component {
            Component::Normal(part) => {
                out.push(part);
                depth += 1;
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if depth == 0 {
        return None;
    }
    Some(out)
}

async fn static_file(State(state): State<AppState>, uri: Uri) -> Result<Response, ApiError> {
    if !state.web_dir.is_dir() {
        return Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("前端目录不存在：{}", state.web_dir.display()),
        ));
    }

    let raw = uri.path().trim_start_matches('/');
    let rel = if raw.is_empty() { "index.html" } else { raw };
    let joined = safe_join(&state.web_dir, Path::new(rel))
        .ok_or_else(|| ApiError::not_found("页面不存在"))?;

    let root =
        std::fs::canonicalize(&state.web_dir).map_err(|err| ApiError::internal(err.into()))?;
    let candidate = match std::fs::canonicalize(&joined) {
        Ok(candidate) if candidate.starts_with(&root) && candidate.is_file() => candidate,
        _ => return Err(ApiError::not_found("页面不存在")),
    };

    let bytes = tokio::fs::read(&candidate)
        .await
        .map_err(|err| ApiError::not_found(format!("读取文件失败：{err}")))?;
    Ok((
        StatusCode::OK,
        [(header::CONTENT_TYPE, content_type(&candidate))],
        bytes,
    )
        .into_response())
}

fn json_rejection(rejection: JsonRejection) -> ApiError {
    ApiError::bad_request(format!("请求体无效：{rejection}"))
}

fn content_type(path: &Path) -> &'static str {
    let ext = path
        .extension()
        .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wav" => "audio/wav",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "webmanifest" => "application/manifest+json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn read_manifest(lesson_dir: &Path) -> Result<Manifest> {
    let path = lesson_dir.join("manifest.json");
    let raw =
        std::fs::read_to_string(&path).with_context(|| format!("读取失败: {}", path.display()))?;
    serde_json::from_str(&raw).with_context(|| format!("解析失败: {}", path.display()))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
    info!("收到退出信号，正在关闭服务");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_lesson() -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let dir = temp.path().join("第5课");
        fs::create_dir_all(dir.join("clips")).unwrap();
        fs::write(dir.join("clips/01-abcd.wav"), b"RIFF").unwrap();
        fs::write(dir.join("secret.txt"), b"nope").unwrap();
        (temp, dir)
    }

    #[test]
    fn media_path_allows_only_clips() {
        let (_temp, dir) = temp_lesson();
        let resolved = resolve_media_path(&dir, "clips/01-abcd.wav").unwrap();
        assert!(resolved.ends_with("clips/01-abcd.wav"));
        assert!(resolve_media_path(&dir, "secret.txt").is_none());
        assert!(resolve_media_path(&dir, "clips/../secret.txt").is_none());
        assert!(resolve_media_path(&dir, "../第5课/clips/01-abcd.wav").is_none());
        assert!(resolve_media_path(&dir, "/etc/passwd").is_none());
        assert!(resolve_media_path(&dir, "clips/missing.wav").is_none());
        assert!(resolve_media_path(&dir, "clips/").is_none());
    }

    #[test]
    fn static_paths_reject_traversal() {
        let (_temp, dir) = temp_lesson();
        assert!(safe_join(&dir, Path::new("index.html"))
            .unwrap()
            .ends_with("index.html"));
        assert!(safe_join(&dir, Path::new("../etc/passwd")).is_none());
        assert!(safe_join(&dir, Path::new("/etc/passwd")).is_none());
        assert!(safe_join(&dir, Path::new("")).is_none());
    }

    #[test]
    fn content_types_by_extension() {
        assert_eq!(content_type(Path::new("a.wav")), "audio/wav");
        assert_eq!(content_type(Path::new("a.MP3")), "audio/mpeg");
        assert_eq!(
            content_type(Path::new("index.html")),
            "text/html; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("app.js")),
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            content_type(Path::new("manifest.webmanifest")),
            "application/manifest+json"
        );
    }

    #[test]
    fn media_base_is_stable() {
        assert_eq!(media_base("a1b2c3d4e5f6"), "/media/a1b2c3d4e5f6");
    }

    fn test_manifest() -> Manifest {
        Manifest {
            version: 1,
            title: "第5课".to_string(),
            generated_at: "2026-10-20T12:00:00+08:00".to_string(),
            defaults: ManifestDefaults {
                gap_ms: 6_000,
                repeat: 2,
                repeat_gap_ms: 1_200,
                lead_in_ms: 3_000,
                intro_gap_ms: 1_500,
            },
            intro: Some(ManifestClip {
                path: "clips/intro.wav".to_string(),
                duration_ms: 2_000,
            }),
            intro_text: Some("开始".to_string()),
            outro: Some(ManifestClip {
                path: "clips/outro.wav".to_string(),
                duration_ms: 1_000,
            }),
            outro_text: Some("结束".to_string()),
            items: Vec::new(),
            full_track: None,
            // Deliberately wrong: the response must recompute, not echo this.
            total_ms: 1,
        }
    }

    fn test_item(text: &str, is_review: bool, clip_lesson: &str) -> NewItem {
        NewItem {
            text: text.to_string(),
            lang: "zh".to_string(),
            clip_lesson: clip_lesson.to_string(),
            clip_path: format!("clips/{text}.wav"),
            duration_ms: 1_000,
            gap_ms: 6_000,
            repeat: 2,
            repeat_gap_ms: 1_200,
            is_review,
        }
    }

    #[test]
    fn total_ms_covers_intro_outro_and_review_items() {
        let manifest = test_manifest();
        let lesson_only = vec![test_item("本节1", false, "lesson000001")];
        let with_review = vec![
            test_item("本节1", false, "lesson000001"),
            test_item("旧词1", true, "oldlesson001"),
        ];

        // 3000 lead-in + 2000 intro + 1500 intro gap
        // + (1000*2 + 1200 + 6000) per item + 1000 outro.
        assert_eq!(session_total_ms(&lesson_only, &manifest), 16_700);
        assert_eq!(session_total_ms(&with_review, &manifest), 25_900);
        assert!(session_total_ms(&with_review, &manifest) > manifest.total_ms);
    }

    // ------------------------------------------------------------------
    // P2 / S1: import + background generation over HTTP
    // ------------------------------------------------------------------

    use axum::body::Body;
    use axum::http::{Method, Request};
    use http_body_util::BodyExt;
    use std::time::Duration;
    use tower::ServiceExt;

    use crate::config::{DoubaoConfig, QwenConfig};
    use crate::model::ProviderKind;

    fn test_opts(temp: &tempfile::TempDir) -> ServeOptions {
        let root = temp.path();
        ServeOptions {
            lessons_dir: root.join("lessons"),
            web_dir: root.join("web"),
            data_path: root.join("data/dictation.db"),
            cache_dir: root.join("cache"),
            host: "127.0.0.1".to_string(),
            port: 0,
        }
    }

    fn test_state(opts: &ServeOptions, tts: Option<Arc<Config>>, token: Option<&str>) -> AppState {
        fs::create_dir_all(&opts.lessons_dir).unwrap();
        let db = Db::open(&opts.data_path).unwrap();
        AppState {
            db: Arc::new(db),
            lessons_dir: opts.lessons_dir.clone(),
            web_dir: opts.web_dir.clone(),
            cache_dir: opts.cache_dir.clone(),
            tts,
            jobs: Arc::new(Mutex::new(JobRegistry::default())),
            gen_permits: Arc::new(Semaphore::new(1)),
            api_token: token.map(Arc::from),
        }
    }

    fn mock_config(cache_dir: &Path) -> Arc<Config> {
        Arc::new(Config {
            default_provider: ProviderKind::Mock,
            cache_dir: cache_dir.to_path_buf(),
            qwen: QwenConfig {
                api_key: None,
                endpoint: String::new(),
                model: String::new(),
                voice_zh: String::new(),
                voice_en: String::new(),
            },
            doubao: DoubaoConfig {
                api_key: None,
                endpoint: String::new(),
                resource_id: String::new(),
                model: None,
                voice_zh: None,
                voice_en: None,
            },
        })
    }

    fn json_req(method: Method, uri: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn get_req(uri: &str) -> Request<Body> {
        Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(Body::empty())
            .unwrap()
    }

    async fn body_json(response: Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn post(app: &Router, uri: &str, body: Value) -> Response {
        app.clone()
            .oneshot(json_req(Method::POST, uri, body))
            .await
            .unwrap()
    }

    async fn wait_job(app: &Router, job_id: &str) -> Value {
        for _ in 0..400 {
            let response = app
                .clone()
                .oneshot(get_req(&format!("/api/jobs/{job_id}")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = body_json(response).await;
            match body["status"].as_str() {
                Some("done" | "error" | "cancelled") => return body,
                _ => tokio::time::sleep(Duration::from_millis(25)).await,
            }
        }
        panic!("job {job_id} 未在限定时间内结束");
    }

    fn import_payload() -> Value {
        json!({
            "title": "第5课",
            "dir": "第5课",
            "text": "摇晃\n粗壮\n毕竟\n",
        })
    }

    #[tokio::test]
    async fn import_endpoint_returns_relative_dir_and_handles_conflicts() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        let response = post(&app, "/api/lessons", import_payload()).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let body = body_json(response).await;
        assert_eq!(body["created"], true);
        assert_eq!(body["dir"], "第5课");
        assert_eq!(body["title"], "第5课");
        assert_eq!(body["item_count"], 3);
        let rendered = body.to_string();
        assert!(
            !rendered.contains(&temp.path().display().to_string()),
            "响应不能包含主机绝对路径: {rendered}"
        );
        assert!(opts.lessons_dir.join("第5课/lesson.txt").is_file());

        let response = post(&app, "/api/lessons", import_payload()).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let body = body_json(response).await;
        assert_eq!(body["error"], "课时已存在");
        assert_eq!(body["dir"], "第5课");

        let mut payload = import_payload();
        payload["force"] = json!(true);
        let response = post(&app, "/api/lessons", payload).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["created"], false);
    }

    #[tokio::test]
    async fn import_endpoint_rejects_invalid_payloads() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        let cases = [
            json!({"title": "", "text": "摇晃\n"}),
            json!({"title": "第5课", "text": "  \n\n"}),
            json!({"title": "第5课", "text": format!("{}\n", "很".repeat(201))}),
        ];
        for payload in cases {
            let response = post(&app, "/api/lessons", payload.clone()).await;
            assert_eq!(
                response.status(),
                StatusCode::BAD_REQUEST,
                "payload: {payload}"
            );
            let body = body_json(response).await;
            assert!(
                body["error"].as_str().is_some_and(|err| !err.is_empty()),
                "错误响应必须有 error: {body}"
            );
        }
        assert!(!opts.lessons_dir.join("第5课").exists());
    }

    fn raw_req(method: Method, uri: &str, content_type: Option<&str>, body: &str) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        builder.body(Body::from(body.to_string())).unwrap()
    }

    #[tokio::test]
    async fn generate_accepts_empty_body_and_ignores_content_type() {
        // Empty body with a JSON content-type, `{}` sent as form encoding, and
        // no body/content-type at all must all mean "defaults".
        for (content_type, body) in [
            (Some("application/json"), ""),
            (Some("application/x-www-form-urlencoded"), "{}"),
            (None, ""),
        ] {
            let temp = tempfile::tempdir().unwrap();
            let opts = test_opts(&temp);
            let tts = mock_config(&opts.cache_dir);
            let app = router(test_state(&opts, Some(tts), None));

            let response = post(&app, "/api/lessons", import_payload()).await;
            assert_eq!(response.status(), StatusCode::CREATED);
            let _ = body_json(response).await;

            let response = app
                .clone()
                .oneshot(raw_req(
                    Method::POST,
                    "/api/lessons/第5课/generate",
                    content_type,
                    body,
                ))
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::ACCEPTED,
                "content-type={content_type:?} body={body:?}"
            );
            assert_eq!(body_json(response).await["status"], "queued");
        }
    }

    #[tokio::test]
    async fn generate_rejects_invalid_json_with_json_error() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let tts = mock_config(&opts.cache_dir);
        let app = router(test_state(&opts, Some(tts), None));

        let response = app
            .clone()
            .oneshot(raw_req(
                Method::POST,
                "/api/lessons/第5课/generate",
                Some("application/json"),
                "{bad",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("请求体无效：")),
            "必须是 JSON 错误体: {body}"
        );
    }

    #[tokio::test]
    async fn auth_wins_over_invalid_body() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, Some("s3cret")));

        let response = app
            .clone()
            .oneshot(raw_req(
                Method::POST,
                "/api/lessons/第5课/generate",
                Some("application/json"),
                "{bad",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await["error"], "未授权");

        // A wrong Content-Type without a token is still an auth failure.
        let response = app
            .clone()
            .oneshot(raw_req(
                Method::POST,
                "/api/lessons",
                Some("application/x-www-form-urlencoded"),
                &import_payload().to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(body_json(response).await["error"], "未授权");
    }

    #[tokio::test]
    async fn import_requires_a_json_body() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        let response = app
            .clone()
            .oneshot(raw_req(
                Method::POST,
                "/api/lessons",
                Some("application/json"),
                "",
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = body_json(response).await;
        assert!(
            body["error"]
                .as_str()
                .is_some_and(|error| error.starts_with("请求体无效：")),
            "必须是 JSON 错误体: {body}"
        );

        // Valid JSON is accepted even with a wrong Content-Type.
        let response = app
            .clone()
            .oneshot(raw_req(
                Method::POST,
                "/api/lessons",
                Some("text/plain"),
                &import_payload().to_string(),
            ))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CREATED);
    }

    #[tokio::test]
    async fn generate_runs_in_background_and_skips_when_up_to_date() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let tts = mock_config(&opts.cache_dir);
        let app = router(test_state(&opts, Some(tts), None));

        let response = post(&app, "/api/lessons", import_payload()).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let _ = body_json(response).await;

        let response = post(
            &app,
            "/api/lessons/第5课/generate",
            json!({"provider": "mock"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = body_json(response).await;
        assert_eq!(body["status"], "queued");
        assert_eq!(body["lesson"], "第5课");
        let job_id = body["job_id"].as_str().unwrap().to_string();

        let job = wait_job(&app, &job_id).await;
        assert_eq!(job["status"], "done", "job: {job}");
        let result = &job["result"];
        assert!(result["total_ms"].as_u64().unwrap() > 0);
        assert!(result["tts_calls"].as_u64().unwrap() > 0);
        assert!(result["clips"].as_u64().unwrap() > 0);
        assert!(job["error"].is_null());

        let lesson_dir = opts.lessons_dir.join("第5课");
        assert!(lesson_dir.join("manifest.json").is_file());
        let clips = fs::read_dir(lesson_dir.join("clips"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "wav"))
            .count();
        assert!(clips > 0);

        let response = post(
            &app,
            "/api/lessons/第5课/generate",
            json!({"provider": "mock"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["status"], "done");
        assert_eq!(body["skipped"], true);
        assert!(body["job_id"].is_null());
    }

    #[test]
    fn job_registry_dedupes_active_jobs_per_lesson() {
        let mut registry = JobRegistry::default();
        let first = Job::queued("job-1".to_string(), "第5课".to_string());
        registry.insert(first);
        assert_eq!(
            registry.active_for("第5课"),
            Some("job-1".to_string()),
            "排队中的同课任务复用 job_id"
        );
        assert_eq!(registry.active_for("第6课"), None);

        registry.get_mut("job-1").unwrap().mark_running();
        assert_eq!(
            registry.active_for("第5课"),
            Some("job-1".to_string()),
            "运行中的同课任务同样复用"
        );

        registry.get_mut("job-1").unwrap().mark_done(JobResult {
            total_ms: 1,
            tts_calls: 1,
            cache_hits: 0,
            clips: 1,
        });
        assert_eq!(registry.active_for("第5课"), None, "完成后可重新生成");
    }

    #[tokio::test]
    async fn generate_without_tts_config_returns_503() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        let response = post(&app, "/api/lessons", import_payload()).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let _ = body_json(response).await;

        let response = post(
            &app,
            "/api/lessons/第5课/generate",
            json!({"provider": "mock"}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_json(response).await;
        assert!(body["error"].as_str().unwrap().contains("未配置 TTS 密钥"));
    }

    #[tokio::test]
    async fn token_protects_write_routes_only() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, Some("s3cret")));

        let no_header = app
            .clone()
            .oneshot(json_req(Method::POST, "/api/lessons", import_payload()))
            .await
            .unwrap();
        assert_eq!(no_header.status(), StatusCode::UNAUTHORIZED);

        let wrong = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/lessons")
                    .header("content-type", "application/json")
                    .header("X-Dictation-Token", "nope")
                    .body(Body::from(import_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

        let ok = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/lessons")
                    .header("content-type", "application/json")
                    .header("X-Dictation-Token", "s3cret")
                    .body(Body::from(import_payload().to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ok.status(), StatusCode::CREATED);

        let list = app.clone().oneshot(get_req("/api/lessons")).await.unwrap();
        assert_eq!(list.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn cancel_only_affects_queued_jobs() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());
        fs::create_dir_all(opts.lessons_dir.join("第5课")).unwrap();
        fs::write(opts.lessons_dir.join("第5课/lesson.txt"), "# 第5课\n摇晃\n").unwrap();
        fs::create_dir_all(opts.lessons_dir.join("第6课")).unwrap();
        fs::write(opts.lessons_dir.join("第6课/lesson.txt"), "# 第6课\n粗壮\n").unwrap();

        state
            .jobs
            .lock()
            .unwrap()
            .insert(Job::queued("queued-1".to_string(), "第5课".to_string()));
        let response = post(&app, "/api/lessons/第5课/generate/cancel", json!({})).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await["status"], "cancelled");

        state
            .jobs
            .lock()
            .unwrap()
            .insert(Job::queued("running-1".to_string(), "第6课".to_string()));
        state
            .jobs
            .lock()
            .unwrap()
            .get_mut("running-1")
            .unwrap()
            .mark_running();
        let response = post(&app, "/api/lessons/第6课/generate/cancel", json!({})).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(response).await["error"], "任务已在执行，无法取消");

        let response = post(&app, "/api/lessons/不存在/generate/cancel", json!({})).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn http_import_and_cli_import_write_identical_files() {
        let http_temp = tempfile::tempdir().unwrap();
        let cli_temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&http_temp);
        let app = router(test_state(&opts, None, None));

        let payload = json!({
            "title": "第5课 词语听写",
            "dir": "第5课",
            "text": "摇晃\n粗壮\n",
            "toml": "rate = 0.9\ngap = \"2s\"\n",
        });
        let response = post(&app, "/api/lessons", payload).await;
        assert_eq!(response.status(), StatusCode::CREATED);
        let _ = body_json(response).await;

        let outcome = crate::lesson::import::import(
            &cli_temp.path().join("lessons"),
            crate::lesson::import::ImportRequest {
                title: Some("第5课 词语听写".to_string()),
                text: "摇晃\n粗壮\n".to_string(),
                toml: Some("rate = 0.9\ngap = \"2s\"\n".to_string()),
                dir: Some("第5课".to_string()),
                force: false,
            },
        )
        .unwrap();
        assert!(outcome.created);

        let http_dir = opts.lessons_dir.join("第5课");
        assert_eq!(
            fs::read(http_dir.join("lesson.txt")).unwrap(),
            fs::read(outcome.dir_path.join("lesson.txt")).unwrap()
        );
        assert_eq!(
            fs::read(http_dir.join("lesson.toml")).unwrap(),
            fs::read(outcome.dir_path.join("lesson.toml")).unwrap()
        );
    }

    #[test]
    fn lesson_manifest_marks_mixed_review_items() {
        let manifest = test_manifest();
        let record = SessionRecord {
            id: "session-1".to_string(),
            kind: "lesson".to_string(),
            title: "第5课".to_string(),
            items: vec![
                test_item("本节1", false, "lesson000001"),
                test_item("旧词1", true, "oldlesson001"),
            ],
        };

        let payload = lesson_manifest(&record, &manifest, "lesson000001", "第5课");
        assert_eq!(payload.kind, "lesson");
        assert_eq!(payload.items.len(), 2);
        assert_eq!(payload.items[0].index, 1);
        assert!(!payload.items[0].review);
        assert_eq!(payload.items[0].base, "/media/lesson000001");
        assert_eq!(payload.items[1].index, 2);
        assert!(payload.items[1].review, "混入的词标记为复习");
        assert_eq!(
            payload.items[1].base, "/media/oldlesson001",
            "复习词的音频仍指向原课"
        );
        assert_eq!(payload.items[1].repeat_gap_ms, 1_200);
        assert_eq!(payload.defaults.lead_in_ms, 3_000);
        assert_eq!(payload.total_ms, 25_900);
    }
}
