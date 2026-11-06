//! P1 HTTP service: lesson list, sessions, review scheduling and media.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::extract::rejection::JsonRejection;
use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;
use tracing::{info, warn};

use crate::db::{self, CheckError, Db, NewItem, SessionRecord};
use crate::manifest::{Manifest, ManifestClip, ManifestDefaults};

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
    pub host: String,
    pub port: u16,
}

pub async fn serve(opts: ServeOptions) -> Result<()> {
    let db = Db::open(&opts.data_path)
        .with_context(|| format!("打开数据库失败: {}", opts.data_path.display()))?;

    let lessons = match db.scan_lessons(&opts.lessons_dir) {
        Ok(lessons) => {
            info!(
                "扫描到 {} 个课时：{}",
                lessons.len(),
                opts.lessons_dir.display()
            );
            lessons.len()
        }
        Err(err) => {
            warn!("扫描课时目录失败（{}）：{err}", opts.lessons_dir.display());
            0
        }
    };
    if !opts.web_dir.is_dir() {
        warn!(
            "前端目录不存在：{}（API 仍可用，静态页面会返回 500）",
            opts.web_dir.display()
        );
    }

    let state = AppState {
        db: Arc::new(db),
        lessons_dir: opts.lessons_dir.clone(),
        web_dir: opts.web_dir.clone(),
    };
    let app = router(state);

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

fn router(state: AppState) -> Router {
    Router::new()
        .route("/api/health", get(health))
        .route("/api/lessons", get(list_lessons))
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
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
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
        (self.status, Json(json!({ "error": self.message }))).into_response()
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
