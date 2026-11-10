//! HTTP service: lesson list, sessions, review scheduling and media.

use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::body::Bytes;
use axum::extract::rejection::{JsonRejection, QueryRejection};
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{header, HeaderMap, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::cache::Cache;
use crate::config::Config;
use crate::db::{self, CheckError, CustomSessionError, Db, NewItem, SessionRecord, SessionResult};
use crate::lesson::import as lesson_import;
use crate::manifest::{Manifest, ManifestClip, ManifestDefaults};
use crate::model::{ProviderKind, Subject};
use crate::render::{self, RenderOptions};
use crate::result_view;
use crate::tts;

const REVIEW_LEAD_IN_MS: u32 = 3_000;
const REVIEW_GAP_MS: u32 = 6_000;
const REVIEW_REPEAT: u8 = 2;
const REVIEW_REPEAT_GAP_MS: u32 = 1_200;

/// Review words mixed into a lesson session: at most 25% of the new words,
/// never below 3, capped by `manifest.review_cap`, and at most 150s of audio.
const REVIEW_MIX_RATIO: f64 = 0.25;
const REVIEW_MIN_MIX: u32 = 3;
const REVIEW_BUDGET_MS: u64 = 150_000;
const REVIEW_DEFAULT_LIMIT: u32 = 20;
const REVIEW_MAX_LIMIT: u32 = 40;
const QUEUE_DEFAULT_LIMIT: u32 = 100;
const QUEUE_MAX_LIMIT: u32 = 200;
const CUSTOM_MAX_ITEMS: usize = 60;

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
        .route("/api/results", get(list_results))
        .route("/api/results/latest", get(latest_result))
        .route("/api/results/latest/wrong", get(latest_result_wrong))
        .route("/api/results/{id}", get(result_detail))
        .route("/api/results/{id}/wrong", get(result_wrong))
        .route("/api/sessions", post(create_session))
        .route("/api/sessions/review", post(create_review_session))
        .route("/api/sessions/custom", post(custom_session))
        .route("/api/sessions/{id}", get(get_session))
        .route("/api/sessions/{id}/check", post(check_session))
        .route("/api/review/queue", get(review_queue))
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

impl From<CustomSessionError> for ApiError {
    fn from(err: CustomSessionError) -> Self {
        match err {
            CustomSessionError::Unknown(words) => {
                ApiError::bad_request("未知词").with_extra(json!({ "unknown": words }))
            }
            CustomSessionError::Unplayable(words) => {
                ApiError::bad_request("缺少音频").with_extra(json!({ "unplayable": words }))
            }
            CustomSessionError::Db(err) => ApiError::internal(err),
        }
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
    #[serde(default)]
    subject: String,
    #[serde(default)]
    grade: Option<u8>,
    #[serde(default)]
    term: Option<u8>,
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
        subject: body.subject,
        grade: body.grade,
        term: body.term,
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
                    "subject": outcome.subject,
                    "grade": outcome.grade,
                    "term": outcome.term,
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
    subject: String,
    grade: Option<u8>,
    term: Option<u8>,
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
        .map(|lesson| crate::lesson::list::json(&lesson.listing()))
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
        "subjects": summary.subjects.iter().map(|subject| json!({
            "subject": subject.subject,
            "due_count": subject.due_count,
            "wrong_count": subject.wrong_count,
            "review_count": subject.review_count,
            "oldest_overdue_days": subject.oldest_overdue_days,
        })).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// Result query (read-only, same JSON as `dictation result ... --json`)
// ---------------------------------------------------------------------------

const RESULTS_DEFAULT_LIMIT: u32 = 20;
const RESULTS_MAX_LIMIT: u32 = 100;
const RESULTS_LIMIT_MESSAGE: &str = "limit 必须是 1–100 的整数";

#[derive(Debug, Default, Deserialize)]
struct ResultsQuery {
    limit: Option<String>,
    all: Option<String>,
}

fn parse_results_limit(raw: Option<&str>) -> Result<u32, ApiError> {
    let Some(raw) = raw else {
        return Ok(RESULTS_DEFAULT_LIMIT);
    };
    let limit: u32 = raw
        .trim()
        .parse()
        .map_err(|_| ApiError::bad_request(RESULTS_LIMIT_MESSAGE))?;
    if !(1..=RESULTS_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request(RESULTS_LIMIT_MESSAGE));
    }
    Ok(limit)
}

fn parse_results_all(raw: Option<&str>) -> Result<bool, ApiError> {
    match raw.map(str::trim) {
        None => Ok(false),
        Some("true" | "1") => Ok(true),
        Some("false" | "0") => Ok(false),
        Some(_) => Err(ApiError::bad_request("all 只能是 true/false/1/0")),
    }
}

async fn list_results(
    State(state): State<AppState>,
    Query(query): Query<ResultsQuery>,
) -> Result<Json<Value>, ApiError> {
    let limit = parse_results_limit(query.limit.as_deref())?;
    let include_unfinished = parse_results_all(query.all.as_deref())?;
    let sessions = run_db(state.db.clone(), move |db| {
        db.list_sessions(limit, include_unfinished)
            .map_err(ApiError::internal)
    })
    .await?;
    Ok(Json(json!({
        "sessions": sessions
            .iter()
            .map(result_view::session_json)
            .collect::<Vec<_>>(),
    })))
}

enum ResultRef {
    Id(String),
    Latest,
}

async fn load_result(db: Arc<Db>, reference: ResultRef) -> Result<SessionResult, ApiError> {
    run_db(db, move |db| match reference {
        ResultRef::Id(id) => {
            let id = id.trim();
            if id.is_empty() {
                return Err(ApiError::bad_request("session id 不能为空"));
            }
            db.session_result(id)
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::not_found("session 不存在"))
        }
        ResultRef::Latest => {
            let latest = db
                .latest_finished_session()
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::not_found("还没有已提交检查的听写记录"))?;
            db.session_result(&latest.id)
                .map_err(ApiError::internal)?
                .ok_or_else(|| ApiError::not_found("session 不存在"))
        }
    })
    .await
}

fn detail_json(result: &SessionResult) -> Value {
    json!({
        "session": result_view::session_json(&result.summary),
        "items": result.items.iter().map(result_view::item_json).collect::<Vec<_>>(),
        "wrong_words": result.wrong_words.iter().map(result_view::wrong_json).collect::<Vec<_>>(),
    })
}

fn wrong_view_json(result: &SessionResult) -> Result<Value, ApiError> {
    if result.summary.finished_at.is_none() {
        return Err(ApiError::conflict("本次听写还没提交检查"));
    }
    Ok(json!({
        "session": result_view::session_json(&result.summary),
        "wrong_words": result.wrong_words.iter().map(result_view::wrong_json).collect::<Vec<_>>(),
    }))
}

async fn result_detail(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let result = load_result(state.db.clone(), ResultRef::Id(id)).await?;
    Ok(Json(detail_json(&result)))
}

async fn latest_result(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let result = load_result(state.db.clone(), ResultRef::Latest).await?;
    Ok(Json(detail_json(&result)))
}

async fn result_wrong(
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    let result = load_result(state.db.clone(), ResultRef::Id(id)).await?;
    Ok(Json(wrong_view_json(&result)?))
}

async fn latest_result_wrong(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let result = load_result(state.db.clone(), ResultRef::Latest).await?;
    Ok(Json(wrong_view_json(&result)?))
}

/// Number of due words to mix into a lesson session.
fn review_mix_limit(new_count: usize, cap: u8) -> u32 {
    if cap == 0 {
        return 0;
    }
    let proportional = (new_count as f64 * REVIEW_MIX_RATIO).ceil() as u32;
    proportional.max(REVIEW_MIN_MIX).min(u32::from(cap))
}

/// Playable length of one item, matching the player timeline.
fn item_audio_ms(item: &NewItem) -> u64 {
    let repeat = u64::from(item.repeat.max(1));
    u64::from(item.duration_ms) * repeat
        + u64::from(item.repeat_gap_ms) * repeat.saturating_sub(1)
        + u64::from(item.gap_ms)
}

/// Keep items in priority order until the review block reaches `budget_ms`.
/// The first item is always kept so a review is never silently empty.
fn take_within_budget(items: Vec<NewItem>, budget_ms: u64) -> Vec<NewItem> {
    let mut total = 0u64;
    let mut kept = Vec::new();
    for item in items {
        let cost = item_audio_ms(&item);
        if !kept.is_empty() && total + cost > budget_ms {
            break;
        }
        total += cost;
        kept.push(item);
        if total >= budget_ms {
            break;
        }
    }
    kept
}

/// Deterministic shuffle keyed by the session seed: stable within one session
/// (so resume works) but different across sessions, which breaks positional
/// memorisation of review words.
fn shuffle_review_items(items: &mut [NewItem], seed: &str) {
    items.sort_by_cached_key(|item| {
        let mut hasher = Sha256::new();
        hasher.update(seed.as_bytes());
        hasher.update(b"\0");
        hasher.update(item.text.as_bytes());
        hasher.finalize().to_vec()
    });
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
    // The manifest is the source of truth for subject/grade/term; the DB row
    // may be stale if the lesson was re-generated without a rescan.
    let mut lesson = lesson;
    lesson.subject = manifest.subject().as_str().to_string();
    lesson.grade = manifest.grade;
    lesson.term = manifest.term;
    let mut items = lesson_items(&lesson.id, &manifest);
    if items.is_empty() {
        return Err(ApiError::bad_request("课时没有可播放的片段"));
    }

    // Append review words after the lesson items; their audio still lives in
    // the original lesson, so each item carries its own `base`.
    let new_count = items.len();
    let limit = review_mix_limit(new_count, manifest.review_cap);
    let exclude_texts: Vec<String> = items.iter().map(|item| item.text.clone()).collect();
    let mix_subject = lesson.subject.clone();
    let mixed = if limit == 0 {
        Vec::new()
    } else {
        run_db(db.clone(), move |db| {
            db.review_items_for_mix(&mix_subject, limit, &exclude_texts)
                .map_err(ApiError::internal)
        })
        .await?
    };
    let mut mixed = take_within_budget(mixed, REVIEW_BUDGET_MS);
    let seed = uuid::Uuid::new_v4().simple().to_string();
    shuffle_review_items(&mut mixed, &seed);
    items.extend(mixed);

    let lesson_for_db = lesson.clone();
    let record = run_db(db, move |db| {
        db.create_lesson_session(&lesson_for_db, &items)
            .map_err(ApiError::internal)
    })
    .await?;

    Ok(Json(lesson_manifest(&record, &manifest, &lesson)))
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
    lesson: &crate::db::LessonEntry,
) -> SessionManifest {
    SessionManifest {
        session_id: record.id.clone(),
        kind: record.kind.clone(),
        title: lesson.title.clone(),
        subject: lesson.subject.clone(),
        grade: lesson.grade,
        term: lesson.term,
        base: media_base(&lesson.id),
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

#[derive(Debug, Deserialize)]
struct ReviewSessionRequest {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    grade: Option<u8>,
    #[serde(default)]
    term: Option<u8>,
}

async fn create_review_session(
    State(state): State<AppState>,
    payload: Result<Json<ReviewSessionRequest>, JsonRejection>,
) -> Result<Json<SessionManifest>, ApiError> {
    let Json(body) = payload.map_err(json_rejection)?;
    let subject = Subject::parse(&body.subject)
        .ok_or_else(|| ApiError::bad_request("subject 必填（chinese|english）"))?
        .as_str()
        .to_string();
    let limit = body.limit.unwrap_or(REVIEW_DEFAULT_LIMIT);
    if !(1..=REVIEW_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request("limit 必须是 1–40 的整数"));
    }
    if let Some(grade) = body.grade {
        if !(1..=6).contains(&grade) {
            return Err(ApiError::bad_request("grade 必须在 1–6 之间"));
        }
    }
    if let Some(term) = body.term {
        if !(1..=2).contains(&term) {
            return Err(ApiError::bad_request("term 必须是 1（上）或 2（下）"));
        }
    }
    let db = state.db.clone();
    let lookup = subject.clone();
    let record = run_db(db, move |db| {
        db.create_review_session_filtered(&lookup, limit, body.grade, body.term)
            .map_err(ApiError::internal)
    })
    .await?;
    let Some(record) = record else {
        return Err(ApiError::conflict("没有到期错词"));
    };

    Ok(Json(review_manifest(
        &record, subject, body.grade, body.term,
    )))
}

fn review_manifest(
    record: &SessionRecord,
    subject: String,
    grade: Option<u8>,
    term: Option<u8>,
) -> SessionManifest {
    SessionManifest {
        session_id: record.id.clone(),
        kind: record.kind.clone(),
        title: record.title.clone(),
        subject,
        grade,
        term,
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
        total_ms: review_total_ms(&record.items),
    }
}

#[derive(Debug, Deserialize)]
struct ReviewQueueQuery {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    limit: Option<u32>,
    #[serde(default)]
    grade: Option<u8>,
    #[serde(default)]
    term: Option<u8>,
}

fn query_rejection(rejection: QueryRejection) -> ApiError {
    ApiError::bad_request(format!("查询参数无效：{rejection}"))
}

fn validate_grade_term(grade: Option<u8>, term: Option<u8>) -> Result<(), ApiError> {
    if let Some(grade) = grade {
        if !(1..=6).contains(&grade) {
            return Err(ApiError::bad_request("grade 必须在 1–6 之间"));
        }
    }
    if let Some(term) = term {
        if !(1..=2).contains(&term) {
            return Err(ApiError::bad_request("term 必须是 1（上）或 2（下）"));
        }
    }
    Ok(())
}

async fn review_queue(
    State(state): State<AppState>,
    query: Result<Query<ReviewQueueQuery>, QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    let Query(query) = query.map_err(query_rejection)?;
    let subject = Subject::parse(&query.subject)
        .ok_or_else(|| ApiError::bad_request("subject 必填（chinese|english）"))?
        .as_str()
        .to_string();
    let limit = query.limit.unwrap_or(QUEUE_DEFAULT_LIMIT);
    if !(1..=QUEUE_MAX_LIMIT).contains(&limit) {
        return Err(ApiError::bad_request("limit 必须是 1–200 的整数"));
    }
    validate_grade_term(query.grade, query.term)?;

    let db = state.db.clone();
    let lookup = subject.clone();
    let queue = run_db(db, move |db| {
        db.review_queue(&lookup, limit, query.grade, query.term)
            .map_err(ApiError::internal)
    })
    .await?;

    let words: Vec<Value> = queue
        .words
        .iter()
        .map(|word| {
            json!({
                "text": word.text,
                "lang": word.lang,
                "last_grade": word.last_grade,
                "lapses": word.lapses,
                "due_at": result_view::format_unix(word.due_at),
                "interval_days": word.interval_days,
                "last_review_at": result_view::format_unix(word.last_review_at),
                "overdue_days": word.overdue_days,
            })
        })
        .collect();
    Ok(Json(json!({
        "subject": subject,
        "total": queue.total,
        "words": words,
    })))
}

#[derive(Debug, Deserialize)]
struct CustomSessionRequest {
    #[serde(default)]
    subject: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    grade: Option<u8>,
    #[serde(default)]
    term: Option<u8>,
    #[serde(default)]
    items: Vec<String>,
}

async fn custom_session(
    State(state): State<AppState>,
    payload: Result<Json<CustomSessionRequest>, JsonRejection>,
) -> Result<Json<SessionManifest>, ApiError> {
    let Json(body) = payload.map_err(json_rejection)?;
    let subject = Subject::parse(&body.subject)
        .ok_or_else(|| ApiError::bad_request("subject 必填（chinese|english）"))?
        .as_str()
        .to_string();
    let title = body.title.trim().to_string();
    if title.is_empty() {
        return Err(ApiError::bad_request("title 不能为空"));
    }
    if body.items.is_empty() {
        return Err(ApiError::bad_request("items 不能为空"));
    }
    if body.items.len() > CUSTOM_MAX_ITEMS {
        return Err(ApiError::bad_request(format!(
            "复习词过多（最多 {CUSTOM_MAX_ITEMS} 个，本次 {} 个）",
            body.items.len()
        ))
        .with_extra(json!({ "limit": CUSTOM_MAX_ITEMS, "got": body.items.len() })));
    }
    validate_grade_term(body.grade, body.term)?;

    let mut seen = HashSet::new();
    let mut duplicated = Vec::new();
    let mut texts = Vec::new();
    for raw in &body.items {
        let text = raw.trim().to_string();
        if text.is_empty() {
            return Err(ApiError::bad_request("items 不能包含空字符串"));
        }
        if !seen.insert(text.clone()) {
            duplicated.push(text);
        } else {
            texts.push(text);
        }
    }
    if !duplicated.is_empty() {
        return Err(
            ApiError::bad_request("重复的词").with_extra(json!({ "duplicated": duplicated }))
        );
    }

    let db = state.db.clone();
    let subject_for_db = subject.clone();
    let title_for_db = title.clone();
    let grade = body.grade;
    let term = body.term;
    let record = run_db(db, move |db| {
        db.create_custom_session(&subject_for_db, &title_for_db, grade, term, &texts)
            .map_err(ApiError::from)
    })
    .await?;

    Ok(Json(review_manifest(&record, subject, grade, term)))
}

async fn get_session(
    State(state): State<AppState>,
    UrlPath(session_id): UrlPath<String>,
) -> Result<Json<SessionManifest>, ApiError> {
    let stored = run_db(state.db.clone(), move |db| {
        db.stored_session(&session_id).map_err(ApiError::internal)
    })
    .await?
    .ok_or_else(|| ApiError::not_found("session 不存在"))?;

    if stored.record.kind == "lesson" {
        let lesson_id = stored
            .lesson_id
            .clone()
            .ok_or_else(|| ApiError::not_found("session 不存在"))?;
        let db = state.db.clone();
        let lessons_dir = state.lessons_dir.clone();
        let lesson = run_db(db, move |db| {
            let found = db
                .lesson_by_id(&lesson_id)
                .map_err(ApiError::internal)?
                .or_else(|| {
                    db.scan_lessons(&lessons_dir)
                        .ok()
                        .and_then(|_| db.lesson_by_id(&lesson_id).ok().flatten())
                });
            found.ok_or_else(|| ApiError::not_found("课时不存在"))
        })
        .await?;
        if !Path::new(&lesson.dir).join("manifest.json").is_file() {
            return Err(ApiError::not_found("课时文件不存在"));
        }
        let manifest = read_manifest(Path::new(&lesson.dir))
            .map_err(|err| ApiError::internal(err.context("读取 manifest 失败")))?;
        Ok(Json(lesson_manifest(&stored.record, &manifest, &lesson)))
    } else {
        Ok(Json(review_manifest(
            &stored.record,
            stored.subject,
            stored.grade,
            stored.term,
        )))
    }
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
            subject: None,
            grade: None,
            term: None,
            review_cap: 8,
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
    // Import + background generation over HTTP
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
            "subject": "chinese",
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
            json!({"title": "", "subject": "chinese", "text": "摇晃\n"}),
            json!({"title": "第5课", "subject": "chinese", "text": "  \n\n"}),
            json!({"title": "第5课", "subject": "chinese", "text": format!("{}\n", "很".repeat(201))}),
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
            "subject": "chinese",
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
                subject: "chinese".to_string(),
                grade: None,
                term: None,
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

        let lesson = crate::db::LessonEntry {
            id: "lesson000001".to_string(),
            dir_name: "第5课".to_string(),
            dir: "/tmp/第5课".to_string(),
            title: "第5课".to_string(),
            subject: "chinese".to_string(),
            grade: None,
            term: None,
            item_count: 2,
            total_ms: 0,
        };
        let payload = lesson_manifest(&record, &manifest, &lesson);
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

    // ------------------------------------------------------------------
    // Review assembly policy
    // ------------------------------------------------------------------

    #[test]
    fn review_mix_limit_scales_with_new_words_and_cap() {
        assert_eq!(review_mix_limit(3, 8), 3, "小课时保底 3");
        assert_eq!(review_mix_limit(10, 8), 3);
        assert_eq!(review_mix_limit(20, 8), 5);
        assert_eq!(review_mix_limit(30, 8), 8);
        assert_eq!(review_mix_limit(40, 8), 8, "cap 封顶");
        assert_eq!(review_mix_limit(40, 3), 3);
        assert_eq!(review_mix_limit(20, 0), 0, "cap=0 关闭混入");
    }

    fn bulky_item(text: &str, duration_ms: u32) -> NewItem {
        let mut item = test_item(text, true, "lesson000001");
        item.duration_ms = duration_ms;
        item.gap_ms = 6_000;
        item.repeat = 2;
        item.repeat_gap_ms = 1_200;
        item
    }

    #[test]
    fn review_budget_keeps_at_least_one_and_limits_total() {
        // 10s clip × 2 + 1.2s + 6s = 27.2s per word.
        let items = vec![
            bulky_item("a", 10_000),
            bulky_item("b", 10_000),
            bulky_item("c", 10_000),
        ];
        let kept = take_within_budget(items, 60_000);
        assert_eq!(kept.len(), 2);

        // A single oversized word is still included, otherwise review could
        // silently produce an empty session.
        let kept = take_within_budget(vec![bulky_item("huge", 100_000)], 60_000);
        assert_eq!(kept.len(), 1);
    }

    #[test]
    fn review_shuffle_is_stable_per_seed() {
        let items = || {
            vec![
                test_item("词1", true, "l"),
                test_item("词2", true, "l"),
                test_item("词3", true, "l"),
                test_item("词4", true, "l"),
                test_item("词5", true, "l"),
            ]
        };
        let mut first = items();
        let mut second = items();
        shuffle_review_items(&mut first, "seed-1");
        shuffle_review_items(&mut second, "seed-1");
        let texts = |items: &[NewItem]| items.iter().map(|i| i.text.clone()).collect::<Vec<_>>();
        assert_eq!(texts(&first), texts(&second), "同一次 session 顺序必须稳定");
        let mut sorted = texts(&first);
        sorted.sort();
        assert_eq!(sorted, vec!["词1", "词2", "词3", "词4", "词5"]);
    }

    // ------------------------------------------------------------------
    // CLI/HTTP parity: lesson listing
    // ------------------------------------------------------------------

    fn write_lesson_manifest(root: &Path, dir: &str, title: &str, items: usize, total_ms: u32) {
        write_lesson_manifest_meta(root, dir, title, "chinese", None, None, items, total_ms);
    }

    fn set_manifest_review_cap(dir: &Path, cap: u8) {
        let path = dir.join("manifest.json");
        let mut value: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        value["review_cap"] = json!(cap);
        fs::write(path, value.to_string()).unwrap();
    }

    #[allow(clippy::too_many_arguments)]
    fn write_lesson_manifest_meta(
        root: &Path,
        dir: &str,
        title: &str,
        subject: &str,
        grade: Option<u8>,
        term: Option<u8>,
        items: usize,
        total_ms: u32,
    ) {
        let path = root.join(dir);
        fs::create_dir_all(&path).unwrap();
        let manifest = Manifest {
            version: 1,
            title: title.to_string(),
            subject: crate::model::Subject::parse(subject),
            grade,
            term,
            review_cap: 8,
            generated_at: "2026-11-09T12:00:00+08:00".to_string(),
            defaults: ManifestDefaults {
                gap_ms: 6_000,
                repeat: 2,
                repeat_gap_ms: 1_200,
                lead_in_ms: 3_000,
                intro_gap_ms: 1_500,
            },
            intro: None,
            intro_text: None,
            outro: None,
            outro_text: None,
            items: (1..=items)
                .map(|index| crate::manifest::ManifestItem {
                    index,
                    text: format!("词{index}"),
                    lang: crate::model::Lang::Zh,
                    announce: None,
                    clips: vec![ManifestClip {
                        path: format!("clips/{index:02}-abcd.wav"),
                        duration_ms: 1_000,
                    }],
                    repeat: 2,
                    repeat_gap_ms: 1_200,
                    gap_ms: 6_000,
                })
                .collect(),
            full_track: None,
            total_ms,
        };
        crate::manifest::write(&path.join("manifest.json"), &manifest).unwrap();
    }

    #[tokio::test]
    async fn lessons_endpoint_is_the_shared_listing() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        fs::create_dir_all(&opts.lessons_dir).unwrap();
        write_lesson_manifest(&opts.lessons_dir, "第6课", "第6课", 2, 20_000);
        write_lesson_manifest(&opts.lessons_dir, "第5课", "第5课 词语听写", 3, 30_000);
        // Same title, different dir: tie-break by dir.
        write_lesson_manifest(&opts.lessons_dir, "第5课副本", "第5课 词语听写", 1, 10_000);
        let app = router(test_state(&opts, None, None));

        let body = body_json(app.clone().oneshot(get_req("/api/lessons")).await.unwrap()).await;

        let shared = crate::lesson::list::scan(&opts.lessons_dir).unwrap();
        let expected: Vec<Value> = shared.iter().map(crate::lesson::list::json).collect();
        assert_eq!(
            body,
            json!({ "lessons": expected }),
            "HTTP 必须与 lesson::list 完全一致"
        );

        let lessons = body["lessons"].as_array().unwrap();
        let dirs: Vec<&str> = lessons
            .iter()
            .map(|lesson| lesson["dir"].as_str().unwrap())
            .collect();
        assert_eq!(dirs, ["第5课", "第5课副本", "第6课"]);
        assert_eq!(lessons[0]["id"], db::lesson_id("第5课"));
        assert!(lessons[0].get("dir_path").is_none());
        assert!(
            !body
                .to_string()
                .contains(&temp.path().display().to_string()),
            "响应不能包含主机绝对路径: {body}"
        );
    }

    #[tokio::test]
    async fn lessons_endpoint_is_empty_without_lessons() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        let body = body_json(app.clone().oneshot(get_req("/api/lessons")).await.unwrap()).await;
        assert_eq!(body, json!({ "lessons": [] }));
    }

    // ------------------------------------------------------------------
    // Result query over HTTP
    // ------------------------------------------------------------------

    fn seed_session(
        state: &AppState,
        items: &[NewItem],
        results: Option<&[(usize, bool)]>,
    ) -> String {
        seed_subject_session(state, "chinese", items, results)
    }

    fn seed_subject_session(
        state: &AppState,
        subject: &str,
        items: &[NewItem],
        results: Option<&[(usize, bool)]>,
    ) -> String {
        let lesson = crate::db::LessonEntry {
            id: "lesson000001".to_string(),
            dir_name: "第5课".to_string(),
            dir: "/tmp/第5课".to_string(),
            title: "第5课".to_string(),
            subject: subject.to_string(),
            grade: None,
            term: None,
            item_count: items.len(),
            total_ms: 0,
        };
        let record = state.db.create_lesson_session(&lesson, items).unwrap();
        if let Some(results) = results {
            state.db.check_session(&record.id, results).unwrap();
        }
        record.id
    }

    fn result_items() -> Vec<NewItem> {
        vec![
            test_item("摇晃", false, "lesson000001"),
            test_item("粗壮", false, "lesson000001"),
        ]
    }

    #[tokio::test]
    async fn results_list_is_empty_by_default() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        let response = app.clone().oneshot(get_req("/api/results")).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_json(response).await, json!({ "sessions": [] }));

        let response = app
            .clone()
            .oneshot(get_req("/api/results/latest"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            body_json(response).await["error"],
            "还没有已提交检查的听写记录"
        );
    }

    #[tokio::test]
    async fn results_list_and_detail_expose_checked_session() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());

        let session_id = seed_session(&state, &result_items(), Some(&[(1, true), (2, false)]));

        let body = body_json(app.clone().oneshot(get_req("/api/results")).await.unwrap()).await;
        let sessions = body["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0]["id"], session_id.as_str());
        assert_eq!(sessions[0]["kind"], "lesson");
        assert_eq!(sessions[0]["title"], "第5课");
        assert_eq!(sessions[0]["item_count"], 2);
        assert_eq!(sessions[0]["correct"], 1);
        assert_eq!(sessions[0]["wrong"], 1);
        let finished = sessions[0]["finished_at"].as_str().unwrap();
        assert!(
            finished.contains('T') && finished.ends_with("+08:00"),
            "finished_at 必须是 RFC3339: {finished}"
        );

        let detail = body_json(
            app.clone()
                .oneshot(get_req(&format!("/api/results/{session_id}")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(detail["session"]["id"], session_id.as_str());
        let items = detail["items"].as_array().unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["text"], "摇晃");
        assert_eq!(items[0]["grade"], "good");
        assert_eq!(items[0]["correct"], true);
        assert_eq!(items[1]["grade"], "again");
        assert_eq!(items[1]["correct"], false);

        let wrong = detail["wrong_words"].as_array().unwrap();
        assert_eq!(wrong.len(), 1);
        assert_eq!(wrong[0]["text"], "粗壮");
        assert_eq!(wrong[0]["lapses"], 1);
        assert_eq!(wrong[0]["interval_days"], 1);
        assert!(wrong[0]["due_at"].as_str().unwrap().ends_with("+08:00"));

        let latest = body_json(
            app.clone()
                .oneshot(get_req("/api/results/latest"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(latest["session"]["id"], session_id.as_str());

        let latest_wrong = body_json(
            app.clone()
                .oneshot(get_req("/api/results/latest/wrong"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(latest_wrong["wrong_words"].as_array().unwrap().len(), 1);
        assert!(
            latest_wrong.get("items").is_none(),
            "wrong 视图不返回 items: {latest_wrong}"
        );

        let only_wrong = body_json(
            app.clone()
                .oneshot(get_req(&format!("/api/results/{session_id}/wrong")))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(only_wrong, latest_wrong);
    }

    #[tokio::test]
    async fn results_all_includes_unfinished_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());

        let unfinished = seed_session(&state, &result_items(), None);
        let finished = seed_session(&state, &result_items(), Some(&[(1, true), (2, true)]));

        let body = body_json(app.clone().oneshot(get_req("/api/results")).await.unwrap()).await;
        let sessions = body["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 1, "默认只看已提交检查的 session");
        assert_eq!(sessions[0]["id"], finished.as_str());

        let body = body_json(
            app.clone()
                .oneshot(get_req("/api/results?all=true&limit=10"))
                .await
                .unwrap(),
        )
        .await;
        let sessions = body["sessions"].as_array().unwrap();
        assert_eq!(sessions.len(), 2);
        let pending = sessions
            .iter()
            .find(|session| session["id"] == unfinished.as_str())
            .expect("all=true 必须包含未提交的 session");
        assert!(pending["finished_at"].is_null());

        let detail = body_json(
            app.clone()
                .oneshot(get_req(&format!("/api/results/{unfinished}")))
                .await
                .unwrap(),
        )
        .await;
        assert!(detail["items"][0]["grade"].is_null());
        assert!(detail["items"][0]["correct"].is_null());

        let response = app
            .clone()
            .oneshot(get_req(&format!("/api/results/{unfinished}/wrong")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(body_json(response).await["error"], "本次听写还没提交检查");
    }

    #[tokio::test]
    async fn results_reject_invalid_query_and_unknown_ids() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        for uri in [
            "/api/results?limit=0",
            "/api/results?limit=101",
            "/api/results?limit=abc",
            "/api/results?limit=",
        ] {
            let response = app.clone().oneshot(get_req(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{uri}");
            assert_eq!(
                body_json(response).await["error"],
                "limit 必须是 1–100 的整数",
                "{uri}"
            );
        }

        let response = app
            .clone()
            .oneshot(get_req("/api/results?all=maybe"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(
            body_json(response).await["error"],
            "all 只能是 true/false/1/0"
        );

        for uri in ["/api/results/nope", "/api/results/nope/wrong"] {
            let response = app.clone().oneshot(get_req(uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(
                body_json(response).await["error"],
                "session 不存在",
                "{uri}"
            );
        }
    }

    #[tokio::test]
    async fn results_get_is_not_protected_by_token() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, Some("s3cret"));
        let app = router(state.clone());
        let session_id = seed_session(&state, &result_items(), Some(&[(1, true), (2, true)]));

        let list = app.clone().oneshot(get_req("/api/results")).await.unwrap();
        assert_eq!(list.status(), StatusCode::OK);

        let wrong = app
            .clone()
            .oneshot(get_req(&format!("/api/results/{session_id}/wrong")))
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::OK);
    }

    // ------------------------------------------------------------------
    // Subject isolation (schema v2)
    // ------------------------------------------------------------------

    #[tokio::test]
    async fn import_requires_a_valid_subject() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let app = router(test_state(&opts, None, None));

        for payload in [
            json!({"title": "第5课", "text": "摇晃\n"}),
            json!({"title": "第5课", "subject": "math", "text": "摇晃\n"}),
        ] {
            let response = post(&app, "/api/lessons", payload).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let body = body_json(response).await;
            assert!(
                body["error"].as_str().unwrap().contains("subject"),
                "必须报出 subject: {body}"
            );
        }
    }

    #[tokio::test]
    async fn lessons_endpoint_reports_subject_and_grade() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        fs::create_dir_all(&opts.lessons_dir).unwrap();
        write_lesson_manifest_meta(
            &opts.lessons_dir,
            "英语 Unit 1",
            "Unit 1",
            "english",
            Some(3),
            Some(2),
            5,
            60_000,
        );
        write_lesson_manifest(&opts.lessons_dir, "第5课", "第5课", 2, 20_000);
        let app = router(test_state(&opts, None, None));

        let body = body_json(app.clone().oneshot(get_req("/api/lessons")).await.unwrap()).await;
        let lessons = body["lessons"].as_array().unwrap();
        let english = lessons
            .iter()
            .find(|lesson| lesson["title"] == "Unit 1")
            .expect("英语课时");
        assert_eq!(english["subject"], "english");
        assert_eq!(english["grade"], 3);
        assert_eq!(english["term"], 2);

        let chinese = lessons
            .iter()
            .find(|lesson| lesson["title"] == "第5课")
            .unwrap();
        assert_eq!(chinese["subject"], "chinese");
        assert!(chinese["grade"].is_null());
    }

    #[tokio::test]
    async fn session_manifest_carries_subject_and_grade() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        fs::create_dir_all(&opts.lessons_dir).unwrap();
        write_lesson_manifest_meta(
            &opts.lessons_dir,
            "英语 Unit 1",
            "Unit 1",
            "english",
            Some(3),
            Some(2),
            2,
            20_000,
        );
        let app = router(test_state(&opts, None, None));

        let response = post(
            &app,
            "/api/sessions",
            json!({"lesson_id": db::lesson_id("英语 Unit 1")}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["subject"], "english");
        assert_eq!(body["grade"], 3);
        assert_eq!(body["term"], 2);
    }

    #[tokio::test]
    async fn session_subject_follows_manifest_when_db_is_stale() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        fs::create_dir_all(&opts.lessons_dir).unwrap();
        write_lesson_manifest(&opts.lessons_dir, "第5课", "第5课", 2, 20_000);
        let app = router(test_state(&opts, None, None));

        // Prime the DB row as Chinese.
        let _ = body_json(app.clone().oneshot(get_req("/api/lessons")).await.unwrap()).await;

        // Re-generate the same directory as English without rescanning.
        write_lesson_manifest_meta(
            &opts.lessons_dir,
            "第5课",
            "第5课",
            "english",
            Some(4),
            Some(1),
            2,
            20_000,
        );

        let response = post(
            &app,
            "/api/sessions",
            json!({"lesson_id": db::lesson_id("第5课")}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["subject"], "english", "manifest 覆盖陈旧的 DB 行");
        assert_eq!(body["grade"], 4);
    }

    #[tokio::test]
    async fn review_cap_from_manifest_controls_mixing() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        fs::create_dir_all(&opts.lessons_dir).unwrap();
        write_lesson_manifest(&opts.lessons_dir, "第5课", "第5课", 2, 20_000);
        set_manifest_review_cap(&opts.lessons_dir.join("第5课"), 0);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());
        seed_subject_session(
            &state,
            "chinese",
            &result_items(),
            Some(&[(1, false), (2, true)]),
        );

        let response = post(
            &app,
            "/api/sessions",
            json!({"lesson_id": db::lesson_id("第5课")}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["items"].as_array().unwrap().len(), 2, "cap=0 不混入");
        assert!(body["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["review"] == false));

        // cap=1 still allows one review word.
        set_manifest_review_cap(&opts.lessons_dir.join("第5课"), 1);
        let response = post(
            &app,
            "/api/sessions",
            json!({"lesson_id": db::lesson_id("第5课")}),
        )
        .await;
        let body = body_json(response).await;
        let items = body["items"].as_array().unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(
            items.iter().filter(|item| item["review"] == true).count(),
            1
        );
    }

    #[tokio::test]
    async fn review_queue_endpoint_lists_playable_words_by_priority() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());
        seed_subject_session(
            &state,
            "chinese",
            &result_items(),
            Some(&[(1, false), (2, true)]),
        );

        let response = app
            .clone()
            .oneshot(get_req("/api/review/queue?subject=chinese"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["subject"], "chinese");
        assert_eq!(body["total"], 1, "只有标错的词到期");
        let words = body["words"].as_array().unwrap();
        assert_eq!(words.len(), 1);
        assert_eq!(words[0]["text"], "摇晃");
        assert_eq!(words[0]["last_grade"], "again");
        assert!(words[0]["due_at"].as_str().unwrap().ends_with("+08:00"));

        let response = app
            .clone()
            .oneshot(get_req("/api/review/queue"))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("subject"));
    }

    #[tokio::test]
    async fn custom_session_preserves_agent_order_and_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());
        let items = vec![
            test_item("甲", false, "lesson000001"),
            test_item("乙", false, "lesson000001"),
        ];
        seed_subject_session(&state, "chinese", &items, None);

        let response = post(
            &app,
            "/api/sessions/custom",
            json!({
                "subject": "chinese",
                "title": "第一单元综合复习",
                "grade": 3,
                "term": 1,
                "items": ["乙", "甲"],
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["kind"], "review");
        assert_eq!(body["title"], "第一单元综合复习");
        assert_eq!(body["subject"], "chinese");
        assert_eq!(body["grade"], 3);
        assert_eq!(body["term"], 1);
        let session_items = body["items"].as_array().unwrap();
        assert_eq!(session_items.len(), 2);
        assert_eq!(session_items[0]["text"], "乙");
        assert_eq!(session_items[1]["text"], "甲");
        assert!(session_items.iter().all(|item| item["review"] == true));

        let session_id = body["session_id"].as_str().unwrap().to_string();
        let response = app
            .clone()
            .oneshot(get_req(&format!("/api/sessions/{session_id}")))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let reloaded = body_json(response).await;
        assert_eq!(reloaded["title"], "第一单元综合复习");
        assert_eq!(reloaded["items"][0]["text"], "乙");
    }

    #[tokio::test]
    async fn custom_session_validates_items() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());
        seed_subject_session(&state, "chinese", &result_items(), None);

        let too_many: Vec<String> = (0..61).map(|index| format!("词{index}")).collect();
        let cases = [
            (
                json!({"subject": "chinese", "title": "复习", "items": too_many}),
                "60",
            ),
            (
                json!({"subject": "chinese", "title": "复习", "items": ["摇晃", "摇晃"]}),
                "重复",
            ),
            (
                json!({"subject": "chinese", "title": "复习", "items": ["不存在"]}),
                "未知词",
            ),
            (
                json!({"subject": "chinese", "title": "", "items": ["摇晃"]}),
                "title",
            ),
        ];
        for (payload, needle) in cases {
            let response = post(&app, "/api/sessions/custom", payload).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{needle}");
            let body = body_json(response).await;
            assert!(
                body["error"].as_str().unwrap().contains(needle),
                "错误必须包含 {needle}: {body}"
            );
        }
    }

    #[tokio::test]
    async fn review_session_honours_limit_and_validates_it() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());

        let items: Vec<NewItem> = (1..=5)
            .map(|index| test_item(&format!("词{index}"), false, "lesson000001"))
            .collect();
        seed_subject_session(
            &state,
            "chinese",
            &items,
            Some(&[(1, false), (2, false), (3, false), (4, false), (5, false)]),
        );

        let response = post(
            &app,
            "/api/sessions/review",
            json!({"subject": "chinese", "limit": 2}),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await["items"].as_array().unwrap().len(),
            2
        );

        let response = post(&app, "/api/sessions/review", json!({"subject": "chinese"})).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body_json(response).await["items"].as_array().unwrap().len(),
            5,
            "默认最多 20"
        );

        for limit in [0, 41] {
            let response = post(
                &app,
                "/api/sessions/review",
                json!({"subject": "chinese", "limit": limit}),
            )
            .await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "limit={limit}");
            assert!(body_json(response).await["error"]
                .as_str()
                .unwrap()
                .contains("limit"));
        }
    }

    #[tokio::test]
    async fn review_summary_groups_by_subject_and_review_requires_subject() {
        let temp = tempfile::tempdir().unwrap();
        let opts = test_opts(&temp);
        let state = test_state(&opts, None, None);
        let app = router(state.clone());

        seed_subject_session(
            &state,
            "chinese",
            &result_items(),
            Some(&[(1, false), (2, true)]),
        );
        seed_subject_session(
            &state,
            "english",
            &result_items(),
            Some(&[(1, false), (2, true)]),
        );

        let summary = body_json(
            app.clone()
                .oneshot(get_req("/api/review/summary"))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(summary["review_count"], 2);
        let subjects = summary["subjects"].as_array().unwrap();
        assert_eq!(subjects.len(), 2);
        let english = subjects
            .iter()
            .find(|subject| subject["subject"] == "english")
            .expect("english 分组");
        assert_eq!(english["wrong_count"], 1);
        assert_eq!(english["review_count"], 1);

        let response = post(&app, "/api/sessions/review", json!({})).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(body_json(response).await["error"]
            .as_str()
            .unwrap()
            .contains("subject"));

        let response = post(&app, "/api/sessions/review", json!({"subject": "english"})).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_json(response).await;
        assert_eq!(body["subject"], "english");
        assert_eq!(body["kind"], "review");
        assert_eq!(body["items"].as_array().unwrap().len(), 1, "只取英语错词");
    }
}
