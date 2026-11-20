//! SQLite persistence for lessons, sessions, attempts and review scheduling.
//!
//! All timestamps are Unix seconds. Queries are short, so a plain
//! `std::sync::Mutex` around the connection is enough; callers hold it only for
//! the duration of one small transaction.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use tracing::warn;

use crate::model::Lang;

/// Maximum number of due words in one review session.
pub const REVIEW_LIMIT: usize = 20;

/// Interval ladder for correct answers, in days.
pub const REVIEW_INTERVALS: [i64; 6] = [1, 2, 4, 7, 15, 30];

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS lessons (
  id TEXT PRIMARY KEY,
  dir TEXT NOT NULL,
  title TEXT NOT NULL,
  subject TEXT NOT NULL DEFAULT 'chinese',
  grade INTEGER,
  term INTEGER,
  item_count INTEGER NOT NULL,
  total_ms INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  lesson_id TEXT,
  subject TEXT NOT NULL DEFAULT 'chinese',
  grade INTEGER,
  term INTEGER,
  title TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  finished_at INTEGER
);

CREATE TABLE IF NOT EXISTS session_items (
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  text TEXT NOT NULL,
  lang TEXT NOT NULL,
  clip_lesson TEXT NOT NULL,
  clip_path TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  gap_ms INTEGER NOT NULL,
  repeat INTEGER NOT NULL,
  repeat_gap_ms INTEGER NOT NULL DEFAULT 1200,
  is_review INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, position)
);

CREATE TABLE IF NOT EXISTS words (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  subject TEXT NOT NULL,
  text TEXT NOT NULL,
  lang TEXT NOT NULL,
  clip_lesson TEXT,
  clip_path TEXT,
  duration_ms INTEGER,
  gap_ms INTEGER,
  repeat INTEGER,
  updated_at INTEGER NOT NULL,
  UNIQUE (subject, text)
);

CREATE TABLE IF NOT EXISTS attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  word_id INTEGER NOT NULL,
  grade TEXT NOT NULL,
  source TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS review_state (
  word_id INTEGER PRIMARY KEY,
  due_at INTEGER NOT NULL,
  interval_days INTEGER NOT NULL,
  lapses INTEGER NOT NULL DEFAULT 0,
  last_grade TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_review_due ON review_state(due_at);
CREATE INDEX IF NOT EXISTS idx_attempts_word ON attempts(word_id, created_at);

-- 分享结果导入的幂等键（v5）：同一份结果只入账一次。
CREATE TABLE IF NOT EXISTS result_imports (
  import_key TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
"#;

/// Current schema version, stored in `PRAGMA user_version`.
pub const SCHEMA_VERSION: i32 = 5;

/// Shared review priority order: wrong last time → least recently practiced →
/// `due_at` → word id. Every review selection query uses this so the criteria
/// cannot drift between mixing, queue and session creation.
///
/// "Least recently practiced" must come from `attempts`: early (not-yet-due)
/// practice does not touch `review_state`, so the derived
/// `due_at - interval_days*86400` would go stale. The correlated subquery uses
/// `idx_attempts_word(word_id, created_at)`.
const REVIEW_ORDER_BY: &str = "ORDER BY CASE WHEN r.last_grade = 'again' THEN 0 ELSE 1 END ASC,
              COALESCE((SELECT MAX(a.created_at) FROM attempts a WHERE a.word_id = w.id),
                       r.due_at - r.interval_days * 86400) ASC,
              r.due_at ASC,
              w.id ASC";

/// Inputs for [`Db::review_session_core`]: which words, how many, and what to
/// store on the session. Filtering by grade/term is separate from storing it,
/// because a lesson-scoped review must not filter by the word's clip lesson.
struct ReviewScope<'a> {
    subject: &'a str,
    title: &'a str,
    limit: u32,
    filter_grade: Option<u8>,
    filter_term: Option<u8>,
    store_grade: Option<u8>,
    store_term: Option<u8>,
    texts: Option<&'a [String]>,
}

fn configure(conn: &Connection) -> Result<()> {
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;")
        .context("初始化 SQLite 失败")
}

fn schema_version(conn: &Connection) -> Result<i32> {
    Ok(conn.query_row("PRAGMA user_version", [], |row| row.get(0))?)
}

fn table_exists(conn: &Connection, name: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )?)
}

/// Bring the database to [`SCHEMA_VERSION`].
///
/// A file-backed database is backed up before any upgrade; failure rolls the
/// whole migration back and aborts startup. Migrations are append-only: never
/// edit a shipped step.
fn migrate(conn: &mut Connection, path: Option<&Path>) -> Result<()> {
    let version = schema_version(conn)?;
    if version > SCHEMA_VERSION {
        anyhow::bail!(
            "数据库版本 v{version} 高于程序支持的 v{SCHEMA_VERSION}，请升级 dictation 二进制"
        );
    }
    if version == 0 && !table_exists(conn, "words")? {
        conn.execute_batch(SCHEMA).context("建表失败")?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        return Ok(());
    }

    // v0.1.0 databases never set user_version; they are v1 by definition.
    let from = if version == 0 { 1 } else { version };
    if from == SCHEMA_VERSION {
        return Ok(());
    }
    if from > SCHEMA_VERSION {
        anyhow::bail!("未知的数据库版本 v{from}");
    }

    if let Some(path) = path {
        backup_database(conn, path, from)?;
        prune_backups(path, 3)?;
    }

    // `PRAGMA foreign_keys` is a no-op inside a transaction, so switch it
    // around the migration and verify integrity before committing.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    let outcome = (|| -> Result<()> {
        let tx = conn.transaction()?;
        let mut current = from;
        if current == 1 {
            migrate_v1_to_v2(&tx)?;
            current = 2;
        }
        if current == 2 {
            migrate_v2_to_v3(&tx)?;
            current = 3;
        }
        if current == 3 {
            migrate_v3_to_v4(&tx)?;
            current = 4;
        }
        if current == 4 {
            migrate_v4_to_v5(&tx)?;
            current = 5;
        }
        if current != SCHEMA_VERSION {
            anyhow::bail!("缺少 v{current} → v{SCHEMA_VERSION} 的迁移");
        }
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        tx.commit()?;
        Ok(())
    })();
    conn.pragma_update(None, "foreign_keys", "ON")?;
    outcome
}

fn backup_database(conn: &Connection, path: &Path, from: i32) -> Result<()> {
    let now = time::OffsetDateTime::now_utc();
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let name = format!(
        "{}.bak-v{from}-{stamp}",
        path.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_else(|| "dictation.db".to_string())
    );
    let target = path.with_file_name(name);
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy().to_string()])
        .with_context(|| format!("迁移前备份失败: {}", target.display()))?;
    Ok(())
}

/// Keep only the newest `keep` backups created by [`backup_database`].
fn prune_backups(db_path: &Path, keep: usize) -> Result<()> {
    let (Some(parent), Some(name)) = (db_path.parent(), db_path.file_name()) else {
        return Ok(());
    };
    let prefix = format!("{}.bak-v", name.to_string_lossy());
    let mut files: Vec<PathBuf> = fs::read_dir(parent)
        .with_context(|| format!("读取备份目录失败: {}", parent.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .map(|name| name.to_string_lossy().starts_with(&prefix))
                    .unwrap_or(false)
        })
        .collect();
    files.sort();
    let remove = files.len().saturating_sub(keep);
    for path in files.into_iter().take(remove) {
        fs::remove_file(&path).with_context(|| format!("删除旧备份失败: {}", path.display()))?;
    }
    Ok(())
}

/// v1 → v2: add subject/grade/term and key words by `(subject, text)`.
fn migrate_v1_to_v2(tx: &Connection) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE lessons ADD COLUMN subject TEXT NOT NULL DEFAULT 'chinese';
         ALTER TABLE lessons ADD COLUMN grade INTEGER;
         ALTER TABLE lessons ADD COLUMN term INTEGER;
         ALTER TABLE sessions ADD COLUMN subject TEXT NOT NULL DEFAULT 'chinese';
         ALTER TABLE sessions ADD COLUMN grade INTEGER;
         ALTER TABLE sessions ADD COLUMN term INTEGER;
         CREATE TABLE words_v2 (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           subject TEXT NOT NULL,
           text TEXT NOT NULL,
           lang TEXT NOT NULL,
           clip_lesson TEXT,
           clip_path TEXT,
           duration_ms INTEGER,
           gap_ms INTEGER,
           repeat INTEGER,
           updated_at INTEGER NOT NULL,
           UNIQUE (subject, text)
         );",
    )?;

    {
        let mut stmt = tx.prepare(
            "SELECT id, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at
             FROM words",
        )?;
        let mut rows = stmt.query([])?;
        let mut insert = tx.prepare(
            "INSERT INTO words_v2
               (id, subject, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )?;
        while let Some(row) = rows.next()? {
            let text: String = row.get(1)?;
            insert.execute(params![
                row.get::<_, i64>(0)?,
                subject_for_text(&text),
                text,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, i64>(8)?,
            ])?;
        }
    }

    tx.execute_batch("DROP TABLE words; ALTER TABLE words_v2 RENAME TO words;")?;
    tx.execute(
        "UPDATE sessions
         SET subject = COALESCE(
           (SELECT subject FROM lessons WHERE lessons.id = sessions.lesson_id),
           'chinese'
         )",
        [],
    )?;

    let violations = tx
        .prepare("PRAGMA foreign_key_check")?
        .query_map([], |_| Ok(()))?
        .count();
    if violations > 0 {
        anyhow::bail!("迁移后外键检查失败（{violations} 处）");
    }
    Ok(())
}

/// v2 → v3: store the per-session repeat gap so a saved session can be
/// replayed exactly (`GET /api/sessions/{id}`).
fn migrate_v2_to_v3(tx: &Connection) -> Result<()> {
    tx.execute_batch(
        "ALTER TABLE session_items ADD COLUMN repeat_gap_ms INTEGER NOT NULL DEFAULT 1200;",
    )?;
    Ok(())
}

/// v3 → v4: index only, no column changes. Review selection orders by "least
/// recently practiced", which reads `MAX(attempts.created_at)` per word.
/// v4 → v5: 分享结果导入的幂等表。
fn migrate_v4_to_v5(tx: &Connection) -> Result<()> {
    tx.execute_batch(
        "CREATE TABLE IF NOT EXISTS result_imports (
           import_key TEXT PRIMARY KEY,
           session_id TEXT NOT NULL,
           created_at INTEGER NOT NULL
         );",
    )?;
    Ok(())
}

fn migrate_v3_to_v4(tx: &Connection) -> Result<()> {
    tx.execute_batch(
        "CREATE INDEX IF NOT EXISTS idx_attempts_word ON attempts(word_id, created_at);",
    )?;
    Ok(())
}

/// Migration-only heuristic: Latin letters mean English, everything else is
/// Chinese (the user's current data is entirely Chinese).
fn subject_for_text(text: &str) -> &'static str {
    if text.chars().any(|c| c.is_ascii_alphabetic()) {
        "english"
    } else {
        "chinese"
    }
}

/// A lesson directory discovered under `--dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LessonEntry {
    pub id: String,
    /// Relative directory name (`第5课`); safe to expose to clients.
    pub dir_name: String,
    /// Absolute host path, used for filesystem access only; never returned.
    pub dir: String,
    pub title: String,
    pub subject: String,
    pub grade: Option<u8>,
    pub term: Option<u8>,
    pub item_count: usize,
    pub total_ms: u32,
}

impl LessonEntry {
    /// Projection shared with the CLI (`lesson::list`); the HTTP list endpoint
    /// serializes this so both surfaces stay identical.
    pub fn listing(&self) -> crate::lesson::list::Listing {
        crate::lesson::list::Listing {
            id: self.id.clone(),
            dir: self.dir_name.clone(),
            title: self.title.clone(),
            subject: self.subject.clone(),
            grade: self.grade,
            term: self.term,
            item_count: self.item_count,
            total_ms: self.total_ms,
        }
    }
}

/// One row of `session_items` plus the fields the response needs.
#[derive(Debug, Clone)]
pub struct NewItem {
    pub text: String,
    pub lang: String,
    pub clip_lesson: String,
    pub clip_path: String,
    pub duration_ms: u32,
    pub gap_ms: u32,
    pub repeat: u8,
    pub repeat_gap_ms: u32,
    pub is_review: bool,
}

#[derive(Debug, Clone)]
pub struct SessionRecord {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub items: Vec<NewItem>,
}

type CustomWordRow = (
    String,
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

type StoredSessionRow = (
    String,
    Option<String>,
    String,
    Option<i64>,
    Option<i64>,
    String,
);

/// One playable word in the agent-facing review queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewQueueWord {
    pub text: String,
    pub lang: String,
    pub last_grade: String,
    pub lapses: i64,
    pub due_at: i64,
    pub interval_days: i64,
    pub last_review_at: i64,
    pub overdue_days: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewQueue {
    /// Total playable due words in scope, before `limit`.
    pub total: u32,
    pub words: Vec<ReviewQueueWord>,
}

/// A session reloaded from SQLite, for `GET /api/sessions/{id}`.
#[derive(Debug)]
pub struct StoredSession {
    pub record: SessionRecord,
    pub subject: String,
    pub grade: Option<u8>,
    pub term: Option<u8>,
    pub lesson_id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum CustomSessionError {
    #[error("未知词")]
    Unknown(Vec<String>),
    #[error("缺少音频")]
    Unplayable(Vec<String>),
    #[error(transparent)]
    Db(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for CustomSessionError {
    fn from(err: rusqlite::Error) -> Self {
        CustomSessionError::Db(err.into())
    }
}

#[derive(Debug)]
pub struct CheckOutcome {
    pub correct: u32,
    pub wrong: u32,
    pub due_count: u32,
}

/// Where a word's audio lives after a lesson is deleted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipRef {
    pub lesson_id: String,
    pub path: String,
    pub duration_ms: i64,
    pub gap_ms: i64,
    pub repeat: i64,
}

/// One word affected by deleting a lesson: repoint to `clip`, or clear it.
#[derive(Debug, Clone)]
pub struct WordClipMove {
    pub subject: String,
    pub text: String,
    pub clip: Option<ClipRef>,
}

/// Review counters for one lesson's word set (home lesson card "复习 N").
///
/// All counters may overlap: `due` and `wrong` are independent, and `review`
/// is their union, matching [`ReviewSummary`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TextReviewCounts {
    pub learned: u32,
    pub due: u32,
    pub wrong: u32,
    pub review: u32,
}

/// Everything `GET /api/words` reports for one word: identity, playability and
/// practice statistics. The server does not judge mastery; callers decide.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordProfile {
    pub text: String,
    pub lang: String,
    pub has_clip: bool,
    pub attempts: u32,
    pub wrong: u32,
    pub correct: u32,
    /// Consecutive correct answers counting back from the latest attempt.
    pub correct_streak: u32,
    /// Wrong answers among the five most recent attempts.
    pub wrong_recent: u32,
    pub last_grade: Option<String>,
    pub last_attempt_at: Option<i64>,
    pub lapses: u32,
    pub interval_days: u32,
    pub due_at: Option<i64>,
}

/// Review counters for the home screen.
///
/// `due_count` keeps the old "due now" meaning; `wrong_count` counts words
/// marked wrong last time; `review_count` is what the player should offer
/// (wrong now OR due now, each row counted once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewSummary {
    pub due_count: u32,
    pub wrong_count: u32,
    pub review_count: u32,
    /// Per-subject breakdown, ordered by subject code.
    pub subjects: Vec<SubjectSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectSummary {
    pub subject: String,
    pub due_count: u32,
    pub wrong_count: u32,
    pub review_count: u32,
    /// Days since the oldest due word was scheduled, 0 when nothing is late.
    pub oldest_overdue_days: u32,
}

/// One row of the `result list` view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub kind: String,
    pub title: String,
    pub subject: String,
    pub grade: Option<u8>,
    pub term: Option<u8>,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub item_count: u32,
    pub correct: u32,
    pub wrong: u32,
}

/// One checked (or not yet checked) item of a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultItem {
    pub position: u32,
    pub text: String,
    pub lang: String,
    pub review: bool,
    /// Title of the lesson the clip came from; mixed review words keep their
    /// original lesson.
    pub origin_lesson: Option<String>,
    pub grade: Option<String>,
    pub source: Option<String>,
}

/// A wrong item plus its current review scheduling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrongWord {
    pub position: u32,
    pub text: String,
    pub lang: String,
    pub review: bool,
    pub origin_lesson: Option<String>,
    pub grade: String,
    pub source: String,
    pub lapses: i64,
    pub interval_days: i64,
    pub due_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResult {
    pub summary: SessionSummary,
    pub items: Vec<ResultItem>,
    pub wrong_words: Vec<WrongWord>,
}

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error("session 不存在")]
    NotFound,
    #[error("本次听写已经提交过检查")]
    AlreadyFinished,
    #[error("{0}")]
    BadRequest(String),
    #[error(transparent)]
    Db(#[from] anyhow::Error),
}

impl From<rusqlite::Error> for CheckError {
    fn from(err: rusqlite::Error) -> Self {
        CheckError::Db(err.into())
    }
}

#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("创建数据目录失败: {}", parent.display()))?;
            }
        }
        let mut conn = Connection::open(path)
            .with_context(|| format!("打开数据库失败: {}", path.display()))?;
        configure(&conn)?;
        migrate(&mut conn, Some(path))?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn)?;
        migrate(&mut conn, None)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    fn conn(&self) -> Result<MutexGuard<'_, Connection>> {
        self.conn
            .lock()
            .map_err(|_| anyhow::anyhow!("数据库锁已被污染"))
    }

    /// Scan `lessons_dir` for `*/manifest.json`, upsert and return them sorted
    /// by title. Directories with a broken manifest are skipped with a warning.
    ///
    /// The filesystem read and the ordering come from [`crate::lesson::list::scan`]
    /// so the CLI (`lesson list`) and `GET /api/lessons` cannot drift apart.
    pub fn scan_lessons(&self, lessons_dir: &Path) -> Result<Vec<LessonEntry>> {
        let listings = crate::lesson::list::scan(lessons_dir)?;

        let entries: Vec<LessonEntry> = listings
            .into_iter()
            .map(|listing| LessonEntry {
                id: listing.id,
                dir_name: listing.dir.clone(),
                dir: lessons_dir.join(&listing.dir).to_string_lossy().to_string(),
                title: listing.title,
                subject: listing.subject,
                grade: listing.grade,
                term: listing.term,
                item_count: listing.item_count,
                total_ms: listing.total_ms,
            })
            .collect();

        let now = now_secs();
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        for entry in &entries {
            tx.execute(
                "INSERT INTO lessons
                   (id, dir, title, subject, grade, term, item_count, total_ms, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
                 ON CONFLICT(id) DO UPDATE SET
                   dir = excluded.dir,
                   title = excluded.title,
                   subject = excluded.subject,
                   grade = excluded.grade,
                   term = excluded.term,
                   item_count = excluded.item_count,
                   total_ms = excluded.total_ms,
                   updated_at = excluded.updated_at",
                params![
                    entry.id,
                    entry.dir,
                    entry.title,
                    entry.subject,
                    entry.grade,
                    entry.term,
                    entry.item_count as i64,
                    entry.total_ms as i64,
                    now,
                ],
            )?;
        }
        tx.commit()?;

        Ok(entries)
    }

    pub fn lesson_by_id(&self, id: &str) -> Result<Option<LessonEntry>> {
        let conn = self.conn()?;
        let entry = conn
            .query_row(
                "SELECT id, dir, title, subject, grade, term, item_count, total_ms
                 FROM lessons WHERE id = ?1",
                [id],
                |row| {
                    let dir: String = row.get(1)?;
                    let dir_name = Path::new(&dir)
                        .file_name()
                        .map(|name| name.to_string_lossy().to_string())
                        .unwrap_or_default();
                    Ok(LessonEntry {
                        id: row.get(0)?,
                        dir_name,
                        dir,
                        title: row.get(2)?,
                        subject: row.get(3)?,
                        grade: row.get::<_, Option<i64>>(4)?.map(|v| v as u8),
                        term: row.get::<_, Option<i64>>(5)?.map(|v| v as u8),
                        item_count: row.get::<_, i64>(6)? as usize,
                        total_ms: row.get::<_, i64>(7)? as u32,
                    })
                },
            )
            .optional()?;
        Ok(entry)
    }

    /// Pick review words to append to a lesson session.
    ///
    /// Same criteria as [`Db::create_review_session`] (wrong last time or due
    /// now), skipping texts already in the lesson. Clip info comes from
    /// `words`, so the player loads audio from the original lesson.
    pub fn review_items_for_mix(
        &self,
        subject: &str,
        limit: u32,
        exclude_texts: &[String],
    ) -> Result<Vec<NewItem>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let now = now_secs();
        let conn = self.conn()?;

        let mut sql = String::from(
            "SELECT w.text, w.lang, w.clip_lesson, w.clip_path, w.duration_ms, w.gap_ms, w.repeat
             FROM review_state r
             JOIN words w ON w.id = r.word_id
             WHERE (r.last_grade = 'again' OR r.due_at <= ?1)
               AND w.subject = ?2
               AND w.clip_lesson IS NOT NULL
               AND w.clip_path IS NOT NULL",
        );
        let mut args = vec![Value::Integer(now), Value::Text(subject.to_string())];
        if !exclude_texts.is_empty() {
            let placeholders = vec!["?"; exclude_texts.len()].join(", ");
            sql.push_str(&format!(" AND w.text NOT IN ({placeholders})"));
            args.extend(exclude_texts.iter().cloned().map(Value::Text));
        }
        sql.push_str(&format!(" {REVIEW_ORDER_BY} LIMIT ?"));
        args.push(Value::Integer(i64::from(limit)));

        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(
            rusqlite::params_from_iter(args.iter()),
            review_item_from_row,
        )?;
        let mut items = Vec::new();
        for row in rows {
            if let Some(item) = row? {
                items.push(item);
            }
        }
        Ok(items)
    }

    /// Create a lesson session: sessions + session_items + words upsert.
    pub fn create_lesson_session(
        &self,
        lesson: &LessonEntry,
        items: &[NewItem],
    ) -> Result<SessionRecord> {
        anyhow::ensure!(!items.is_empty(), "课时没有可播放的片段");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = now_secs();
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO sessions (id, kind, lesson_id, subject, grade, term, title, created_at)
             VALUES (?1, 'lesson', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                lesson.id,
                lesson.subject,
                lesson.grade,
                lesson.term,
                lesson.title,
                now
            ],
        )?;
        for (index, item) in items.iter().enumerate() {
            insert_session_item(&tx, &id, index + 1, item)?;
            upsert_word(&tx, &lesson.subject, item, now)?;
        }
        tx.commit()?;
        Ok(SessionRecord {
            id,
            kind: "lesson".to_string(),
            title: lesson.title.clone(),
            items: items.to_vec(),
        })
    }

    /// Agent-facing review pool: playable due words, highest priority first.
    pub fn review_queue(
        &self,
        subject: &str,
        limit: u32,
        grade: Option<u8>,
        term: Option<u8>,
    ) -> Result<ReviewQueue> {
        let now = now_secs();
        let conn = self.conn()?;
        let filter = "FROM review_state r
             JOIN words w ON w.id = r.word_id
             LEFT JOIN lessons l ON l.id = w.clip_lesson
             WHERE (r.last_grade = 'again' OR r.due_at <= ?1)
               AND w.subject = ?3
               AND (?4 IS NULL OR l.grade = ?4)
               AND (?5 IS NULL OR l.term = ?5)
               AND w.clip_lesson IS NOT NULL
               AND w.clip_path IS NOT NULL";
        let count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) {filter}"),
            params![
                now,
                0_i64,
                subject,
                grade.map(i64::from),
                term.map(i64::from)
            ],
            |row| row.get(0),
        )?;

        let mut stmt = conn.prepare(&format!(
            "SELECT w.text, w.lang, r.last_grade, r.lapses, r.due_at, r.interval_days,
                    r.due_at - r.interval_days * 86400,
                    CASE WHEN r.due_at < ?1 THEN (?1 - r.due_at) / 86400 ELSE 0 END
             {filter}
             {REVIEW_ORDER_BY}
             LIMIT ?2"
        ))?;
        let words = stmt
            .query_map(
                params![
                    now,
                    i64::from(limit.max(1)),
                    subject,
                    grade.map(i64::from),
                    term.map(i64::from)
                ],
                |row| {
                    Ok(ReviewQueueWord {
                        text: row.get(0)?,
                        lang: row.get(1)?,
                        last_grade: row.get(2)?,
                        lapses: row.get(3)?,
                        due_at: row.get(4)?,
                        interval_days: row.get(5)?,
                        last_review_at: row.get(6)?,
                        overdue_days: row.get::<_, i64>(7)?.max(0) as u32,
                    })
                },
            )?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(ReviewQueue {
            total: count.max(0) as u32,
            words,
        })
    }

    /// Create a custom review session from an explicit, ordered word list.
    /// All texts must already exist (same subject) with a playable clip.
    pub fn create_custom_session(
        &self,
        subject: &str,
        title: &str,
        grade: Option<u8>,
        term: Option<u8>,
        texts: &[String],
    ) -> std::result::Result<SessionRecord, CustomSessionError> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;

        let mut items = Vec::with_capacity(texts.len());
        let mut unknown = Vec::new();
        let mut unplayable = Vec::new();
        for text in texts {
            let row: Option<CustomWordRow> = tx
                .query_row(
                    "SELECT lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat
                     FROM words WHERE subject = ?1 AND text = ?2",
                    params![subject, text],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                        ))
                    },
                )
                .optional()?;
            let Some((lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat)) = row else {
                unknown.push(text.clone());
                continue;
            };
            let (Some(clip_lesson), Some(clip_path)) = (clip_lesson, clip_path) else {
                unplayable.push(text.clone());
                continue;
            };
            items.push(NewItem {
                text: text.clone(),
                lang,
                clip_lesson,
                clip_path,
                duration_ms: duration_ms.unwrap_or(0).max(0) as u32,
                gap_ms: gap_ms.unwrap_or(6_000).max(0) as u32,
                repeat: repeat.unwrap_or(2).clamp(1, 10) as u8,
                repeat_gap_ms: 1_200,
                is_review: true,
            });
        }
        if !unknown.is_empty() {
            return Err(CustomSessionError::Unknown(unknown));
        }
        if !unplayable.is_empty() {
            return Err(CustomSessionError::Unplayable(unplayable));
        }
        if items.is_empty() {
            return Err(CustomSessionError::Unknown(Vec::new()));
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = now_secs();
        tx.execute(
            "INSERT INTO sessions (id, kind, lesson_id, subject, grade, term, title, created_at)
             VALUES (?1, 'review', NULL, ?2, ?3, ?4, ?5, ?6)",
            params![id, subject, grade, term, title, now],
        )?;
        for (index, item) in items.iter().enumerate() {
            insert_session_item(&tx, &id, index + 1, item)?;
        }
        tx.commit()?;

        Ok(SessionRecord {
            id,
            kind: "review".to_string(),
            title: title.to_string(),
            items,
        })
    }

    /// Reload a stored session (used by `GET /api/sessions/{id}`).
    pub fn stored_session(&self, session_id: &str) -> Result<Option<StoredSession>> {
        let conn = self.conn()?;
        let row: Option<StoredSessionRow> = conn
            .query_row(
                "SELECT kind, lesson_id, subject, grade, term, title
                 FROM sessions WHERE id = ?1",
                [session_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((kind, lesson_id, subject, grade, term, title)) = row else {
            return Ok(None);
        };

        let mut stmt = conn.prepare(
            "SELECT text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat,
                    repeat_gap_ms, is_review
             FROM session_items WHERE session_id = ?1 ORDER BY position ASC",
        )?;
        let items = stmt
            .query_map([session_id], |row| {
                Ok(NewItem {
                    text: row.get(0)?,
                    lang: row.get(1)?,
                    clip_lesson: row.get(2)?,
                    clip_path: row.get(3)?,
                    duration_ms: row.get::<_, i64>(4)?.max(0) as u32,
                    gap_ms: row.get::<_, i64>(5)?.max(0) as u32,
                    repeat: row.get::<_, i64>(6)?.clamp(1, 10) as u8,
                    repeat_gap_ms: row.get::<_, i64>(7)?.max(0) as u32,
                    is_review: row.get::<_, i64>(8)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(Some(StoredSession {
            record: SessionRecord {
                id: session_id.to_string(),
                kind,
                title,
                items,
            },
            subject,
            grade: grade.map(|value| value as u8),
            term: term.map(|value| value as u8),
            lesson_id,
        }))
    }

    /// Create a review session with the default limit and no grade filter.
    pub fn create_review_session(&self, subject: &str) -> Result<Option<SessionRecord>> {
        self.create_review_session_filtered(subject, REVIEW_LIMIT as u32, None, None)
    }

    /// Lesson-scoped review: only words from `texts` (the lesson manifest's
    /// word set) that are due or were marked wrong. `texts` must come from the
    /// manifest, not from `words.clip_lesson`, which points at the lesson that
    /// last referenced the word.
    ///
    /// `grade`/`term` are stored on the session for display only; they do not
    /// filter, because a word's clip may live in an earlier lesson.
    pub fn create_lesson_review_session(
        &self,
        subject: &str,
        title: &str,
        texts: &[String],
        limit: u32,
        grade: Option<u8>,
        term: Option<u8>,
    ) -> Result<Option<SessionRecord>> {
        self.review_session_core(ReviewScope {
            subject,
            title,
            limit,
            filter_grade: None,
            filter_term: None,
            store_grade: grade,
            store_term: term,
            texts: Some(texts),
        })
    }

    /// Create a review session from words that are due now or were marked
    /// wrong last time. `None` when there is nothing to review.
    ///
    /// `grade`/`term` filter by the lesson that produced the word's clip
    /// (`words.clip_lesson` → `lessons.grade/term`); both are optional.
    pub fn create_review_session_filtered(
        &self,
        subject: &str,
        limit: u32,
        grade: Option<u8>,
        term: Option<u8>,
    ) -> Result<Option<SessionRecord>> {
        self.review_session_core(ReviewScope {
            subject,
            title: "错词复习",
            limit,
            filter_grade: grade,
            filter_term: term,
            store_grade: grade,
            store_term: term,
            texts: None,
        })
    }

    fn review_session_core(&self, scope: ReviewScope<'_>) -> Result<Option<SessionRecord>> {
        let ReviewScope {
            subject,
            title,
            limit,
            filter_grade,
            filter_term,
            store_grade,
            store_term,
            texts,
        } = scope;
        if limit == 0 || texts.map(|texts| texts.is_empty()).unwrap_or(false) {
            return Ok(None);
        }
        let now = now_secs();
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let mut sql = String::from(
            "SELECT w.text, w.lang, w.clip_lesson, w.clip_path, w.duration_ms, w.gap_ms, w.repeat
             FROM review_state r
             JOIN words w ON w.id = r.word_id
             LEFT JOIN lessons l ON l.id = w.clip_lesson
             WHERE (r.last_grade = 'again' OR r.due_at <= ?1)
               AND w.subject = ?3
               AND (?4 IS NULL OR l.grade = ?4)
               AND (?5 IS NULL OR l.term = ?5)",
        );
        let mut args = vec![
            Value::Integer(now),
            Value::Integer(i64::from(limit)),
            Value::Text(subject.to_string()),
            filter_grade
                .map(|value| Value::Integer(i64::from(value)))
                .unwrap_or(Value::Null),
            filter_term
                .map(|value| Value::Integer(i64::from(value)))
                .unwrap_or(Value::Null),
        ];
        if let Some(texts) = texts {
            let placeholders = vec!["?"; texts.len()].join(", ");
            sql.push_str(&format!(" AND w.text IN ({placeholders})"));
            args.extend(texts.iter().cloned().map(Value::Text));
        }
        sql.push_str(&format!(" {REVIEW_ORDER_BY} LIMIT ?2"));

        let mut items = {
            let mut stmt = tx.prepare(&sql)?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(args.iter()),
                review_item_from_row,
            )?;
            let mut collected = Vec::new();
            for row in rows {
                match row? {
                    Some(item) => collected.push(item),
                    None => warn!("跳过没有音频片段的复习词"),
                }
            }
            collected
        };
        if items.is_empty() {
            return Ok(None);
        }

        let id = uuid::Uuid::new_v4().simple().to_string();
        tx.execute(
            "INSERT INTO sessions (id, kind, lesson_id, subject, grade, term, title, created_at)
             VALUES (?1, 'review', NULL, ?3, ?4, ?5, ?6, ?2)",
            params![id, now, subject, store_grade, store_term, title],
        )?;
        for (index, item) in items.iter().enumerate() {
            insert_session_item(&tx, &id, index + 1, item)?;
        }
        tx.commit()?;
        Ok(Some(SessionRecord {
            id,
            kind: "review".to_string(),
            title: title.to_string(),
            items: std::mem::take(&mut items),
        }))
    }

    /// Per-lesson review counters, computed from a lesson's manifest word set.
    ///
    /// `learned` counts words that have a `review_state` row (i.e. have been
    /// practised at least once); `due`/`wrong`/`review` use the same criteria
    /// as the rest of the app and may overlap.
    pub fn review_counts_for_texts(
        &self,
        subject: &str,
        texts: &[String],
    ) -> Result<TextReviewCounts> {
        if texts.is_empty() {
            return Ok(TextReviewCounts::default());
        }
        let now = now_secs();
        let conn = self.conn()?;
        let placeholders = vec!["?"; texts.len()].join(", ");
        let sql = format!(
            "SELECT COUNT(*),
                    COALESCE(SUM(r.due_at <= ?2), 0),
                    COALESCE(SUM(r.last_grade = 'again'), 0),
                    COALESCE(SUM(r.last_grade = 'again' OR r.due_at <= ?2), 0)
             FROM review_state r
             JOIN words w ON w.id = r.word_id
             WHERE w.subject = ?1 AND w.text IN ({placeholders})"
        );
        let mut args = vec![Value::Text(subject.to_string()), Value::Integer(now)];
        args.extend(texts.iter().cloned().map(Value::Text));
        let counts = conn.query_row(&sql, rusqlite::params_from_iter(args.iter()), |row| {
            Ok(TextReviewCounts {
                learned: row.get::<_, i64>(0)?.max(0) as u32,
                due: row.get::<_, i64>(1)?.max(0) as u32,
                wrong: row.get::<_, i64>(2)?.max(0) as u32,
                review: row.get::<_, i64>(3)?.max(0) as u32,
            })
        })?;
        Ok(counts)
    }

    /// Texts of words practised at least once (`review_state`), oldest id
    /// first. Used when `GET /api/words` has no lesson scope: without a lesson
    /// range a word cannot be attributed to one lesson reliably.
    pub fn learned_texts(&self, subject: &str) -> Result<Vec<String>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT w.text
             FROM words w JOIN review_state r ON r.word_id = w.id
             WHERE w.subject = ?1
             ORDER BY w.id",
        )?;
        let rows = stmt.query_map([subject], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// Per-word identity, playability and practice statistics, in the order of
    /// `texts`. Unknown texts keep their slot with zeroed defaults.
    pub fn word_profiles(&self, subject: &str, texts: &[String]) -> Result<Vec<WordProfile>> {
        let mut profiles: Vec<WordProfile> = texts
            .iter()
            .map(|text| WordProfile {
                text: text.clone(),
                lang: String::new(),
                has_clip: false,
                attempts: 0,
                wrong: 0,
                correct: 0,
                correct_streak: 0,
                wrong_recent: 0,
                last_grade: None,
                last_attempt_at: None,
                lapses: 0,
                interval_days: 0,
                due_at: None,
            })
            .collect();
        if texts.is_empty() {
            return Ok(profiles);
        }

        let mut by_text: HashMap<String, usize> = HashMap::new();
        for (index, text) in texts.iter().enumerate() {
            by_text.insert(text.clone(), index);
        }
        let placeholders = vec!["?"; texts.len()].join(", ");
        let conn = self.conn()?;

        let mut by_word_id: HashMap<i64, usize> = HashMap::new();
        {
            let sql = format!(
                "SELECT w.id, w.text, w.lang,
                        CASE WHEN w.clip_lesson IS NOT NULL AND w.clip_path IS NOT NULL
                             THEN 1 ELSE 0 END,
                        COALESCE(r.lapses, 0), COALESCE(r.interval_days, 0),
                        r.due_at, r.last_grade
                 FROM words w
                 LEFT JOIN review_state r ON r.word_id = w.id
                 WHERE w.subject = ?1 AND w.text IN ({placeholders})"
            );
            let mut args = vec![Value::Text(subject.to_string())];
            args.extend(texts.iter().cloned().map(Value::Text));
            let mut stmt = conn.prepare(&sql)?;
            let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)? != 0,
                    row.get::<_, i64>(4)?.max(0) as u32,
                    row.get::<_, i64>(5)?.max(0) as u32,
                    row.get::<_, Option<i64>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            })?;
            for row in rows {
                let (word_id, text, lang, has_clip, lapses, interval_days, due_at, last_grade) =
                    row?;
                let Some(&index) = by_text.get(&text) else {
                    continue;
                };
                by_word_id.insert(word_id, index);
                let profile = &mut profiles[index];
                profile.lang = lang;
                profile.has_clip = has_clip;
                profile.lapses = lapses;
                profile.interval_days = interval_days;
                profile.due_at = due_at;
                profile.last_grade = last_grade;
            }
        }

        if by_word_id.is_empty() {
            return Ok(profiles);
        }

        // Aggregating in Rust keeps `correct_streak` / `wrong_recent` readable
        // and is fine at a few thousand attempts per subject.
        let sql = format!(
            "SELECT a.word_id, a.grade, a.created_at
             FROM attempts a
             WHERE a.word_id IN ({})
             ORDER BY a.word_id, a.created_at DESC, a.id DESC",
            vec!["?"; by_word_id.len()].join(", ")
        );
        let args: Vec<Value> = by_word_id.keys().copied().map(Value::Integer).collect();
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(args.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        let mut seen_per_word: HashMap<i64, u32> = HashMap::new();
        for row in rows {
            let (word_id, grade, created_at) = row?;
            let Some(&index) = by_word_id.get(&word_id) else {
                continue;
            };
            let profile = &mut profiles[index];
            profile.attempts += 1;
            let seen = seen_per_word.entry(word_id).or_insert(0);
            if *seen == 0 {
                profile.last_attempt_at = Some(created_at);
            }
            if grade == "again" {
                profile.wrong += 1;
                if *seen < 5 {
                    profile.wrong_recent += 1;
                }
            } else {
                profile.correct += 1;
                if *seen == profile.correct_streak {
                    profile.correct_streak += 1;
                }
            }
            *seen += 1;
        }
        Ok(profiles)
    }

    /// `(subject, text)` for every word whose clip came from `lesson_id`.
    pub fn words_for_clip_lesson(&self, lesson_id: &str) -> Result<Vec<(String, String)>> {
        let conn = self.conn()?;
        let mut stmt =
            conn.prepare("SELECT subject, text FROM words WHERE clip_lesson = ?1 ORDER BY id")?;
        let rows = stmt.query_map([lesson_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Number of sessions that reference a lesson (history is kept on delete).
    pub fn session_count_for_lesson(&self, lesson_id: &str) -> Result<u32> {
        let conn = self.conn()?;
        let count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM sessions WHERE lesson_id = ?1",
            [lesson_id],
            |row| row.get(0),
        )?;
        Ok(count.max(0) as u32)
    }

    /// Drop a lesson row after repointing (or clearing) the words it provided.
    pub fn apply_lesson_delete(
        &self,
        lesson_id: &str,
        repointed: &[WordClipMove],
        cleared: &[(String, String)],
    ) -> Result<()> {
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let now = now_secs();
        for item in repointed {
            let Some(clip) = &item.clip else {
                continue;
            };
            tx.execute(
                "UPDATE words SET clip_lesson = ?1, clip_path = ?2, duration_ms = ?3,
                        gap_ms = ?4, repeat = ?5, updated_at = ?6
                 WHERE subject = ?7 AND text = ?8",
                params![
                    clip.lesson_id,
                    clip.path,
                    clip.duration_ms,
                    clip.gap_ms,
                    clip.repeat,
                    now,
                    item.subject,
                    item.text
                ],
            )?;
        }
        for (subject, text) in cleared {
            tx.execute(
                "UPDATE words SET clip_lesson = NULL, clip_path = NULL, duration_ms = NULL,
                        gap_ms = NULL, repeat = NULL, updated_at = ?1
                 WHERE subject = ?2 AND text = ?3",
                params![now, subject, text],
            )?;
        }
        tx.execute("DELETE FROM lessons WHERE id = ?1", [lesson_id])?;
        tx.commit()?;
        Ok(())
    }

    pub fn review_summary(&self) -> Result<ReviewSummary> {
        let now = now_secs();
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT w.subject,
                    COALESCE(SUM(r.due_at <= ?1), 0),
                    COALESCE(SUM(r.last_grade = 'again'), 0),
                    COALESCE(SUM(r.last_grade = 'again' OR r.due_at <= ?1), 0),
                    COALESCE(MAX(CASE
                      WHEN r.last_grade = 'again' OR r.due_at <= ?1
                      THEN ?1 - r.due_at ELSE 0 END), 0)
             FROM review_state r
             JOIN words w ON w.id = r.word_id
             WHERE w.clip_path IS NOT NULL AND w.clip_path <> ''
             GROUP BY w.subject
             ORDER BY w.subject",
        )?;
        let subjects = stmt
            .query_map([now], |row| {
                let overdue_secs = row.get::<_, i64>(4)?.max(0);
                Ok(SubjectSummary {
                    subject: row.get(0)?,
                    due_count: row.get::<_, i64>(1)?.max(0) as u32,
                    wrong_count: row.get::<_, i64>(2)?.max(0) as u32,
                    review_count: row.get::<_, i64>(3)?.max(0) as u32,
                    oldest_overdue_days: (overdue_secs / 86_400) as u32,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        Ok(ReviewSummary {
            due_count: subjects.iter().map(|s| s.due_count).sum(),
            wrong_count: subjects.iter().map(|s| s.wrong_count).sum(),
            review_count: subjects.iter().map(|s| s.review_count).sum(),
            subjects,
        })
    }

    /// Recent sessions, newest first. Finished ones only unless
    /// `include_unfinished` is set.
    pub fn list_sessions(
        &self,
        limit: u32,
        include_unfinished: bool,
    ) -> Result<Vec<SessionSummary>> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(
            "SELECT s.id, s.kind, s.title, s.subject, s.grade, s.term, s.created_at, s.finished_at,
                    (SELECT COUNT(*) FROM session_items i WHERE i.session_id = s.id),
                    COALESCE(SUM(a.grade = 'good'), 0),
                    COALESCE(SUM(a.grade = 'again'), 0)
             FROM sessions s
             LEFT JOIN attempts a ON a.session_id = s.id
             WHERE (?1 <> 0 OR s.finished_at IS NOT NULL)
             GROUP BY s.id
             ORDER BY COALESCE(s.finished_at, s.created_at) DESC, s.rowid DESC
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![
                if include_unfinished { 1_i64 } else { 0_i64 },
                i64::from(limit.max(1))
            ],
            session_summary_row,
        )?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Most recent session that has been checked.
    pub fn latest_finished_session(&self) -> Result<Option<SessionSummary>> {
        Ok(self.list_sessions(1, false)?.into_iter().next())
    }

    /// Full result for one session, including per-item grades and the wrong
    /// words with their review scheduling. `None` when the id is unknown.
    pub fn session_result(&self, session_id: &str) -> Result<Option<SessionResult>> {
        let conn = self.conn()?;
        let Some(summary) = session_summary(&conn, session_id)? else {
            return Ok(None);
        };

        let items = {
            let mut stmt = conn.prepare(
                "SELECT i.position, i.text, i.lang, i.is_review,
                        COALESCE(l.title, i.clip_lesson),
                        a.grade, a.source
                 FROM session_items i
                 LEFT JOIN attempts a
                   ON a.session_id = i.session_id AND a.position = i.position
                 LEFT JOIN lessons l ON l.id = i.clip_lesson
                 WHERE i.session_id = ?1
                 ORDER BY i.position ASC",
            )?;
            let rows = stmt.query_map([session_id], |row| {
                Ok(ResultItem {
                    position: row.get::<_, i64>(0)?.max(0) as u32,
                    text: row.get(1)?,
                    lang: row.get(2)?,
                    review: row.get::<_, i64>(3)? != 0,
                    origin_lesson: row.get(4)?,
                    grade: row.get(5)?,
                    source: row.get(6)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        let wrong_words = {
            let mut stmt = conn.prepare(
                "SELECT i.position, i.text, i.lang, i.is_review,
                        COALESCE(l.title, i.clip_lesson),
                        a.grade, a.source,
                        COALESCE(r.lapses, 0),
                        COALESCE(r.interval_days, 0),
                        r.due_at
                 FROM session_items i
                 JOIN attempts a
                   ON a.session_id = i.session_id AND a.position = i.position
                 LEFT JOIN lessons l ON l.id = i.clip_lesson
                 LEFT JOIN words w ON w.text = i.text
                 LEFT JOIN review_state r ON r.word_id = w.id
                 WHERE i.session_id = ?1 AND a.grade = 'again'
                 ORDER BY i.position ASC",
            )?;
            let rows = stmt.query_map([session_id], |row| {
                Ok(WrongWord {
                    position: row.get::<_, i64>(0)?.max(0) as u32,
                    text: row.get(1)?,
                    lang: row.get(2)?,
                    review: row.get::<_, i64>(3)? != 0,
                    origin_lesson: row.get(4)?,
                    grade: row.get(5)?,
                    source: row.get(6)?,
                    lapses: row.get(7)?,
                    interval_days: row.get(8)?,
                    due_at: row.get(9)?,
                })
            })?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };

        Ok(Some(SessionResult {
            summary,
            items,
            wrong_words,
        }))
    }

    /// Record the check for one session. Rejects duplicate submissions and
    /// partial coverage.
    pub fn check_session(
        &self,
        session_id: &str,
        results: &[(usize, bool)],
    ) -> std::result::Result<CheckOutcome, CheckError> {
        let now = now_secs();
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;

        let session_row: Option<(Option<i64>, String)> = tx
            .query_row(
                "SELECT finished_at, subject FROM sessions WHERE id = ?1",
                [session_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let subject = match session_row {
            None => return Err(CheckError::NotFound),
            Some((Some(_), _)) => return Err(CheckError::AlreadyFinished),
            Some((None, subject)) => subject,
        };

        let positions: Vec<i64> = {
            let mut stmt = tx.prepare(
                "SELECT position FROM session_items WHERE session_id = ?1 ORDER BY position",
            )?;
            let rows = stmt.query_map([session_id], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<Vec<i64>>>()?
        };
        if positions.is_empty() {
            return Err(CheckError::BadRequest(
                "本次 session 没有可检查的条目".to_string(),
            ));
        }

        let mut by_position: BTreeMap<usize, bool> = BTreeMap::new();
        for (position, correct) in results {
            if by_position.insert(*position, *correct).is_some() {
                return Err(CheckError::BadRequest(format!(
                    "position {position} 重复提交"
                )));
            }
            if !positions.contains(&(*position as i64)) {
                return Err(CheckError::BadRequest(format!(
                    "position {position} 不属于本次 session"
                )));
            }
        }
        if by_position.len() != positions.len() {
            return Err(CheckError::BadRequest(
                "results 必须覆盖本次 session 的全部 position".to_string(),
            ));
        }

        let mut correct_count = 0_u32;
        let mut wrong_count = 0_u32;
        for (position, correct) in by_position {
            let item = tx.query_row(
                "SELECT text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat
                 FROM session_items WHERE session_id = ?1 AND position = ?2",
                params![session_id, position as i64],
                |row| {
                    Ok(NewItem {
                        text: row.get(0)?,
                        lang: row.get(1)?,
                        clip_lesson: row.get(2)?,
                        clip_path: row.get(3)?,
                        duration_ms: row.get::<_, i64>(4)? as u32,
                        gap_ms: row.get::<_, i64>(5)? as u32,
                        repeat: row.get::<_, i64>(6)? as u8,
                        repeat_gap_ms: 1_200,
                        is_review: false,
                    })
                },
            )?;
            let word_id = upsert_word(&tx, &subject, &item, now)?;

            let grade = if correct { "good" } else { "again" };
            tx.execute(
                "INSERT INTO attempts (session_id, position, word_id, grade, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'self', ?5)",
                params![session_id, position as i64, word_id, grade, now],
            )?;

            schedule_after_grade(&tx, word_id, correct, now)?;

            if correct {
                correct_count += 1;
            } else {
                wrong_count += 1;
            }
        }

        tx.execute(
            "UPDATE sessions SET finished_at = ?1 WHERE id = ?2",
            params![now, session_id],
        )?;

        let due_count: i64 = tx.query_row(
            "SELECT COUNT(*) FROM review_state WHERE due_at <= ?1",
            [now],
            |row| row.get(0),
        )?;
        tx.commit()?;

        Ok(CheckOutcome {
            correct: correct_count,
            wrong: wrong_count,
            due_count: due_count.max(0) as u32,
        })
    }

    /// 导入一份 `dictation.result/v1`（见 `docs/share-publish.md` §9）。
    ///
    /// - 幂等：同一份内容只入账一次（`result_imports.import_key`）；
    /// - 时间基准是 `parsed.finished_at`（旅途中的听写不会被推迟复习）；
    /// - 词按 `(subject, text)` 归属；本机没有该词音频时片段留空，
    ///   因此不会进入复习池（`has_clip` 过滤），但历史记录照常可查。
    pub fn import_result(
        &self,
        parsed: &crate::result_import::ParsedImport,
        clips: &crate::result_import::ClipIndex,
    ) -> std::result::Result<ImportOutcome, crate::result_import::ImportError> {
        use crate::result_import::ImportError;

        let key = parsed.import_key();
        let imported_at = now_secs();
        let conn = self.conn().map_err(ImportError::Db)?;
        let tx = conn
            .unchecked_transaction()
            .map_err(|err| ImportError::Db(err.into()))?;

        if let Some(session_id) = tx
            .query_row(
                "SELECT session_id FROM result_imports WHERE import_key = ?1",
                [&key],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(|err| ImportError::Db(err.into()))?
        {
            let (correct, wrong) = attempt_counts(&tx, &session_id).map_err(ImportError::Db)?;
            return Ok(ImportOutcome {
                status: ImportStatus::Duplicate,
                session_id,
                correct,
                wrong,
            });
        }

        // 同一科目、同一完成时刻但内容不同：多半是改过判定后重复导出。
        let conflict: Option<String> = tx
            .query_row(
                "SELECT ri.session_id
                   FROM result_imports ri
                   JOIN sessions s ON s.id = ri.session_id
                  WHERE s.subject = ?1 AND s.finished_at = ?2 AND ri.import_key <> ?3
                  LIMIT 1",
                params![parsed.subject, parsed.finished_at, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|err| ImportError::Db(err.into()))?;
        if conflict.is_some() {
            return Err(ImportError::Conflict(
                "同一时间的听写结果已存在但内容不同；确认是否重复导出后再导入".to_string(),
            ));
        }

        // 只有本机确实存在的课时才写入 lesson_id，避免悬挂引用。
        let lesson_id = match parsed.lesson_id.as_deref() {
            Some(id) if lesson_exists(&tx, id).map_err(ImportError::Db)? => Some(id.to_string()),
            _ => None,
        };

        let session_id = uuid::Uuid::new_v4().to_string();
        tx.execute(
            "INSERT INTO sessions
               (id, kind, lesson_id, subject, grade, term, title, created_at, finished_at)
             VALUES (?1, 'imported', ?2, ?3, NULL, NULL, ?4, ?5, ?5)",
            params![
                session_id,
                lesson_id,
                parsed.subject,
                parsed.title,
                parsed.finished_at
            ],
        )
        .map_err(|err| ImportError::Db(err.into()))?;

        let mut correct = 0_u32;
        let mut wrong = 0_u32;
        for item in &parsed.items {
            let word_id = ensure_import_word(&tx, &parsed.subject, item, parsed.finished_at, clips)
                .map_err(ImportError::Db)?;
            let (clip_lesson, clip_path, duration_ms, gap_ms, repeat) =
                word_clip_columns(&tx, word_id).map_err(ImportError::Db)?;
            tx.execute(
                "INSERT INTO session_items
                   (session_id, position, text, lang, clip_lesson, clip_path,
                    duration_ms, gap_ms, repeat, repeat_gap_ms, is_review)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 1200, ?10)",
                params![
                    session_id,
                    item.position as i64,
                    item.text,
                    item.lang,
                    clip_lesson,
                    clip_path,
                    duration_ms as i64,
                    gap_ms as i64,
                    repeat as i64,
                    i64::from(item.review),
                ],
            )
            .map_err(|err| ImportError::Db(err.into()))?;

            let grade = if item.correct { "good" } else { "again" };
            tx.execute(
                "INSERT INTO attempts (session_id, position, word_id, grade, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'import', ?5)",
                params![
                    session_id,
                    item.position as i64,
                    word_id,
                    grade,
                    parsed.finished_at
                ],
            )
            .map_err(|err| ImportError::Db(err.into()))?;

            schedule_after_grade(&tx, word_id, item.correct, parsed.finished_at)
                .map_err(ImportError::Db)?;

            if item.correct {
                correct += 1;
            } else {
                wrong += 1;
            }
        }

        tx.execute(
            "INSERT INTO result_imports (import_key, session_id, created_at)
             VALUES (?1, ?2, ?3)",
            params![key, session_id, imported_at],
        )
        .map_err(|err| ImportError::Db(err.into()))?;
        tx.commit().map_err(|err| ImportError::Db(err.into()))?;

        Ok(ImportOutcome {
            status: ImportStatus::Imported,
            session_id,
            correct,
            wrong,
        })
    }
}

/// 导入结果的状态：首次入账 / 重复导入。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStatus {
    Imported,
    Duplicate,
}

impl ImportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ImportStatus::Imported => "imported",
            ImportStatus::Duplicate => "duplicate",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ImportOutcome {
    pub status: ImportStatus,
    pub session_id: String,
    pub correct: u32,
    pub wrong: u32,
}

fn attempt_counts(conn: &Connection, session_id: &str) -> Result<(u32, u32)> {
    let (correct, wrong): (i64, i64) = conn.query_row(
        "SELECT COALESCE(SUM(grade = 'good'), 0), COALESCE(SUM(grade = 'again'), 0)
           FROM attempts WHERE session_id = ?1",
        [session_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((correct.max(0) as u32, wrong.max(0) as u32))
}

fn lesson_exists(conn: &Connection, lesson_id: &str) -> Result<bool> {
    let found: Option<i64> = conn
        .query_row("SELECT 1 FROM lessons WHERE id = ?1", [lesson_id], |row| {
            row.get(0)
        })
        .optional()?;
    Ok(found.is_some())
}

/// 导入的词：已存在就复用（保留本机音源），不存在则建一条无音源的记录。
fn ensure_import_word(
    conn: &Connection,
    subject: &str,
    item: &crate::result_import::ImportItem,
    at: i64,
    clips: &crate::result_import::ClipIndex,
) -> Result<i64> {
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM words WHERE subject = ?1 AND text = ?2",
            params![subject, item.text],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
    {
        return Ok(id);
    }
    // 新词：优先用本机课时清单里的片段（导入方通常还没在本机做过这一课）
    match clips.get(&item.text) {
        Some(clip) => {
            conn.execute(
                "INSERT INTO words
                   (subject, text, lang, clip_lesson, clip_path, duration_ms, gap_ms,
                    repeat, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    subject,
                    item.text,
                    item.lang,
                    clip.clip_lesson,
                    clip.clip_path,
                    clip.duration_ms as i64,
                    clip.gap_ms as i64,
                    clip.repeat as i64,
                    at,
                ],
            )?;
        }
        None => {
            conn.execute(
                "INSERT INTO words
                   (subject, text, lang, clip_lesson, clip_path, duration_ms, gap_ms,
                    repeat, updated_at)
                 VALUES (?1, ?2, ?3, NULL, NULL, NULL, NULL, NULL, ?4)",
                params![subject, item.text, item.lang, at],
            )?;
        }
    }
    Ok(conn.last_insert_rowid())
}

/// 片段字段原始行：`clip_lesson, clip_path, duration_ms, gap_ms, repeat`。
type WordClipRow = (
    Option<String>,
    Option<String>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
);

/// 导入的会话沿用该词在本机的片段信息；没有音频则留空（不进复习池）。
fn word_clip_columns(conn: &Connection, word_id: i64) -> Result<(String, String, u32, u32, u8)> {
    let row: Option<WordClipRow> = conn
        .query_row(
            "SELECT clip_lesson, clip_path, duration_ms, gap_ms, repeat
               FROM words WHERE id = ?1",
            [word_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (clip_lesson, clip_path, duration_ms, gap_ms, repeat) = row.unwrap_or_default();
    Ok((
        clip_lesson.unwrap_or_default(),
        clip_path.unwrap_or_default(),
        duration_ms.unwrap_or(0).max(0) as u32,
        gap_ms.unwrap_or(0).max(0) as u32,
        repeat.unwrap_or(1).clamp(1, 10) as u8,
    ))
}

fn session_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
    Ok(SessionSummary {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        subject: row.get(3)?,
        grade: row.get::<_, Option<i64>>(4)?.map(|v| v as u8),
        term: row.get::<_, Option<i64>>(5)?.map(|v| v as u8),
        created_at: row.get(6)?,
        finished_at: row.get(7)?,
        item_count: row.get::<_, i64>(8)?.max(0) as u32,
        correct: row.get::<_, i64>(9)?.max(0) as u32,
        wrong: row.get::<_, i64>(10)?.max(0) as u32,
    })
}

fn session_summary(conn: &Connection, session_id: &str) -> Result<Option<SessionSummary>> {
    let summary = conn
        .query_row(
            "SELECT s.id, s.kind, s.title, s.subject, s.grade, s.term, s.created_at, s.finished_at,
                    (SELECT COUNT(*) FROM session_items i WHERE i.session_id = s.id),
                    COALESCE(SUM(a.grade = 'good'), 0),
                    COALESCE(SUM(a.grade = 'again'), 0)
             FROM sessions s
             LEFT JOIN attempts a ON a.session_id = s.id
             WHERE s.id = ?1
             GROUP BY s.id",
            [session_id],
            session_summary_row,
        )
        .optional()?;
    Ok(summary)
}

/// Lesson id = first 12 hex chars of `sha256(dir_name)`.
pub fn lesson_id(dir_name: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(dir_name.as_bytes());
    hex::encode(hasher.finalize())[..12].to_string()
}

pub fn lang_str(lang: Lang) -> &'static str {
    match lang {
        Lang::Zh => "zh",
        Lang::En => "en",
        Lang::Mixed => "mixed",
    }
}

/// Next interval for a correct answer: 1→2→4→7→15→30, capped at 30.
/// 按判分更新复习调度（`check_session` 与结果导入共用）。
///
/// `at` 是该次练习实际发生的时间：实时检查用 `now`，导入用 `finished_at`。
/// Early practice（未到期且不是待纠正错词）答对不推进间隔；答错照常回退。
fn schedule_after_grade(conn: &Connection, word_id: i64, correct: bool, at: i64) -> Result<()> {
    let previous: Option<(i64, i64, i64, String)> = conn
        .query_row(
            "SELECT interval_days, lapses, due_at, last_grade
             FROM review_state WHERE word_id = ?1",
            [word_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;

    let normal_review = previous
        .as_ref()
        .map(|(_, _, due_at, last_grade)| last_grade == "again" || *due_at <= at)
        .unwrap_or(true);
    if correct && !normal_review {
        return Ok(());
    }

    let (interval_days, lapses) = if correct {
        let next = match previous.as_ref() {
            None => 2,
            Some((current, _, _, _)) => next_interval(*current),
        };
        (
            next,
            previous
                .as_ref()
                .map(|(_, lapses, _, _)| *lapses)
                .unwrap_or(0),
        )
    } else {
        (
            1,
            previous
                .as_ref()
                .map(|(_, lapses, _, _)| *lapses)
                .unwrap_or(0)
                + 1,
        )
    };
    let due_at = at + interval_days * 86_400;
    let grade = if correct { "good" } else { "again" };

    conn.execute(
        "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(word_id) DO UPDATE SET
           due_at = excluded.due_at,
           interval_days = excluded.interval_days,
           lapses = excluded.lapses,
           last_grade = excluded.last_grade",
        params![word_id, due_at, interval_days, lapses, grade],
    )?;
    Ok(())
}

fn next_interval(current: i64) -> i64 {
    for interval in REVIEW_INTERVALS {
        if interval > current {
            return interval;
        }
    }
    *REVIEW_INTERVALS.last().unwrap_or(&30)
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Map a `words`/`review_state` join row to a review item. Returns `None`
/// when the word has no stored clip and therefore cannot be played.
fn review_item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Option<NewItem>> {
    let text: String = row.get(0)?;
    let lang: String = row.get(1)?;
    let clip_lesson: Option<String> = row.get(2)?;
    let clip_path: Option<String> = row.get(3)?;
    let duration_ms: Option<i64> = row.get(4)?;
    let gap_ms: Option<i64> = row.get(5)?;
    let repeat: Option<i64> = row.get(6)?;
    let (Some(clip_lesson), Some(clip_path)) = (clip_lesson, clip_path) else {
        return Ok(None);
    };
    Ok(Some(NewItem {
        text,
        lang,
        clip_lesson,
        clip_path,
        duration_ms: duration_ms.unwrap_or(0).max(0) as u32,
        gap_ms: gap_ms.unwrap_or(6_000).max(0) as u32,
        repeat: repeat.unwrap_or(2).clamp(1, 10) as u8,
        repeat_gap_ms: 1_200,
        is_review: true,
    }))
}

fn insert_session_item(
    conn: &Connection,
    session_id: &str,
    position: usize,
    item: &NewItem,
) -> Result<()> {
    conn.execute(
        "INSERT INTO session_items
           (session_id, position, text, lang, clip_lesson, clip_path,
            duration_ms, gap_ms, repeat, repeat_gap_ms, is_review)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            session_id,
            position as i64,
            item.text,
            item.lang,
            item.clip_lesson,
            item.clip_path,
            item.duration_ms as i64,
            item.gap_ms as i64,
            item.repeat as i64,
            item.repeat_gap_ms as i64,
            if item.is_review { 1_i64 } else { 0_i64 },
        ],
    )?;
    Ok(())
}

/// Insert or update a word, refreshing clip info from the latest session.
/// Identity is `(subject, text)`, so the same text can exist per subject.
fn upsert_word(conn: &Connection, subject: &str, item: &NewItem, now: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO words
           (subject, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT(subject, text) DO UPDATE SET
           lang = excluded.lang,
           clip_lesson = excluded.clip_lesson,
           clip_path = excluded.clip_path,
           duration_ms = excluded.duration_ms,
           gap_ms = excluded.gap_ms,
           repeat = excluded.repeat,
           updated_at = excluded.updated_at",
        params![
            subject,
            item.text,
            item.lang,
            item.clip_lesson,
            item.clip_path,
            item.duration_ms as i64,
            item.gap_ms as i64,
            item.repeat as i64,
            now,
        ],
    )?;
    let id = conn.query_row(
        "SELECT id FROM words WHERE subject = ?1 AND text = ?2",
        params![subject, item.text],
        |row| row.get(0),
    )?;
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{self, Manifest, ManifestClip, ManifestDefaults, ManifestItem};
    use std::path::PathBuf;

    fn sample_manifest(title: &str, count: usize) -> Manifest {
        Manifest {
            version: 1,
            title: title.to_string(),
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
            intro: None,
            intro_text: None,
            outro: None,
            outro_text: None,
            items: (1..=count)
                .map(|index| ManifestItem {
                    index,
                    text: format!("词{index}"),
                    lang: Lang::Zh,
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
            total_ms: 9_000,
        }
    }

    fn lesson_dir(root: &Path, name: &str, count: usize) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(dir.join("clips")).unwrap();
        manifest::write(&dir.join("manifest.json"), &sample_manifest(name, count)).unwrap();
        dir
    }

    fn items(count: usize) -> Vec<NewItem> {
        (1..=count)
            .map(|index| NewItem {
                text: format!("词{index}"),
                lang: "zh".to_string(),
                clip_lesson: "abc123def456".to_string(),
                clip_path: format!("clips/{index:02}-abcd.wav"),
                duration_ms: 1_000,
                gap_ms: 6_000,
                repeat: 2,
                repeat_gap_ms: 1_200,
                is_review: false,
            })
            .collect()
    }

    fn setup() -> (tempfile::TempDir, Db, LessonEntry) {
        let temp = tempfile::tempdir().unwrap();
        let lessons_root = temp.path().join("lessons");
        lesson_dir(&lessons_root, "第5课", 5);
        let db = Db::open(&temp.path().join("data/dictation.db")).unwrap();
        let scanned = db.scan_lessons(&lessons_root).unwrap();
        assert_eq!(scanned.len(), 1);
        (temp, db, scanned[0].clone())
    }

    #[test]
    fn lesson_id_is_stable_and_short() {
        let id = lesson_id("第5课");
        assert_eq!(id, lesson_id("第5课"));
        assert_ne!(id, lesson_id("第6课"));
        assert_eq!(id.len(), 12);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn scans_manifest_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("lessons");
        lesson_dir(&root, "第5课", 5);
        lesson_dir(&root, "第6课", 3);
        // A directory without a manifest must be ignored.
        fs::create_dir_all(root.join("empty")).unwrap();
        let db = Db::open(&temp.path().join("db.sqlite")).unwrap();

        let lessons = db.scan_lessons(&root).unwrap();
        assert_eq!(lessons.len(), 2);
        assert_eq!(lessons[0].title, "第5课");
        assert_eq!(lessons[0].item_count, 5);
        assert_eq!(lessons[1].item_count, 3);
        assert_eq!(
            db.lesson_by_id(&lessons[0].id).unwrap().unwrap().dir,
            root.join("第5课").to_string_lossy()
        );
        assert!(db.lesson_by_id("nope").unwrap().is_none());

        // Re-scan is idempotent.
        assert_eq!(db.scan_lessons(&root).unwrap().len(), 2);
    }

    #[test]
    fn creates_lesson_session_and_lists_items() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(5)).unwrap();
        assert_eq!(record.kind, "lesson");
        assert_eq!(record.items.len(), 5);
        assert_eq!(record.items[0].text, "词1");
        assert!(!record.items[0].is_review);

        let conn = db.conn().unwrap();
        let positions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_items WHERE session_id = ?1",
                [&record.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(positions, 5);
    }

    #[test]
    fn check_all_correct_schedules_two_days() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(5)).unwrap();
        let results: Vec<(usize, bool)> = (1..=5).map(|position| (position, true)).collect();

        let before = now_secs();
        let outcome = db.check_session(&record.id, &results).unwrap();
        assert_eq!(outcome.correct, 5);
        assert_eq!(outcome.wrong, 0);
        assert_eq!(outcome.due_count, 0);

        let conn = db.conn().unwrap();
        let (interval, due_at, lapses): (i64, i64, i64) = conn
            .query_row(
                "SELECT interval_days, due_at, lapses FROM review_state LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(interval, 2, "无记录时从 2 天开始");
        assert_eq!(lapses, 0);
        assert!(due_at >= before + 2 * 86_400 && due_at <= now_secs() + 2 * 86_400);

        let attempts: i64 = conn
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(attempts, 5);
    }

    #[test]
    fn wrong_answer_due_tomorrow_and_increments_lapses() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(2)).unwrap();
        let before = now_secs();
        let outcome = db
            .check_session(&record.id, &[(1, true), (2, false)])
            .unwrap();
        assert_eq!(outcome.correct, 1);
        assert_eq!(outcome.wrong, 1);

        let conn = db.conn().unwrap();
        let (interval, due_at, lapses, grade): (i64, i64, i64, String) = conn
            .query_row(
                "SELECT r.interval_days, r.due_at, r.lapses, r.last_grade
                 FROM review_state r JOIN words w ON w.id = r.word_id
                 WHERE w.text = '词2'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert_eq!(interval, 1);
        assert_eq!(lapses, 1);
        assert_eq!(grade, "again");
        assert!(due_at >= before + 86_400 && due_at <= now_secs() + 86_400);
        drop(conn);

        // A second correct answer on the same word moves past 1 day.
        let second = db.create_lesson_session(&lesson, &items(2)).unwrap();
        db.check_session(&second.id, &[(1, true), (2, true)])
            .unwrap();
        let conn = db.conn().unwrap();
        let interval: i64 = conn
            .query_row(
                "SELECT r.interval_days FROM review_state r JOIN words w ON w.id = r.word_id
                 WHERE w.text = '词2'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(interval, 2, "1 天后答对应升到 2 天");
    }

    #[test]
    fn rejects_duplicate_and_incomplete_checks() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(3)).unwrap();

        let err = db.check_session(&record.id, &[(1, true)]).unwrap_err();
        assert!(matches!(err, CheckError::BadRequest(_)));

        let err = db
            .check_session(&record.id, &[(1, true), (1, false), (2, true)])
            .unwrap_err();
        assert!(matches!(err, CheckError::BadRequest(_)));

        let err = db
            .check_session(&record.id, &[(1, true), (2, true), (9, true)])
            .unwrap_err();
        assert!(matches!(err, CheckError::BadRequest(_)));

        db.check_session(&record.id, &[(1, true), (2, true), (3, true)])
            .unwrap();
        let err = db
            .check_session(&record.id, &[(1, true), (2, true), (3, true)])
            .unwrap_err();
        assert!(matches!(err, CheckError::AlreadyFinished));

        let err = db
            .check_session("missing-session", &[(1, true)])
            .unwrap_err();
        assert!(matches!(err, CheckError::NotFound));
    }

    #[test]
    fn review_session_contains_wrong_word_immediately() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(3)).unwrap();
        db.check_session(&record.id, &[(1, true), (2, true), (3, false)])
            .unwrap();

        let summary = db.review_summary().unwrap();
        assert_eq!(summary.due_count, 0, "错词到期时间仍是明天");
        assert_eq!(summary.wrong_count, 1);
        assert_eq!(summary.review_count, 1);

        let review = db
            .create_review_session("chinese")
            .unwrap()
            .expect("刚标错的词当天就该能复习");
        assert_eq!(review.kind, "review");
        assert_eq!(review.title, "错词复习");
        assert_eq!(review.items.len(), 1);
        assert_eq!(review.items[0].text, "词3");
        assert!(review.items[0].is_review);

        // Mark it correct during the review: it leaves the review queue.
        db.check_session(&review.id, &[(1, true)]).unwrap();
        let summary = db.review_summary().unwrap();
        assert_eq!(summary.due_count, 0);
        assert_eq!(summary.wrong_count, 0);
        assert_eq!(summary.review_count, 0);
        assert!(db.create_review_session("chinese").unwrap().is_none());

        let conn = db.conn().unwrap();
        let (interval, grade): (i64, String) = conn
            .query_row(
                "SELECT r.interval_days, r.last_grade
                 FROM review_state r JOIN words w ON w.id = r.word_id
                 WHERE w.text = '词3'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(interval, 2, "1 天 → 2 天");
        assert_eq!(grade, "good");
    }

    #[test]
    fn review_session_only_when_due() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(3)).unwrap();
        db.check_session(&record.id, &[(1, true), (2, true), (3, true)])
            .unwrap();

        let summary = db.review_summary().unwrap();
        assert_eq!(summary.due_count, 0);
        assert_eq!(summary.wrong_count, 0);
        assert_eq!(summary.review_count, 0);
        assert!(
            db.create_review_session("chinese").unwrap().is_none(),
            "全部答对且未到期时不应有复习 session"
        );

        // Force everything due in the past.
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "UPDATE review_state SET due_at = ?1",
                params![now_secs() - 60],
            )
            .unwrap();
        }
        let summary = db.review_summary().unwrap();
        assert_eq!(summary.due_count, 3);
        assert_eq!(summary.wrong_count, 0);
        assert_eq!(summary.review_count, 3);

        let review = db.create_review_session("chinese").unwrap().unwrap();
        assert_eq!(review.kind, "review");
        assert_eq!(review.title, "错词复习");
        assert_eq!(review.items.len(), 3);
        assert!(review.items.iter().all(|item| item.is_review));
        assert_eq!(review.items[0].clip_path, "clips/01-abcd.wav");

        // Review sessions never write words on their own.
        let conn = db.conn().unwrap();
        let words: i64 = conn
            .query_row("SELECT COUNT(*) FROM words", [], |row| row.get(0))
            .unwrap();
        assert_eq!(words, 3);
        let sessions: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sessions WHERE kind = 'review'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(sessions, 1);
    }

    #[test]
    fn wrong_and_due_rows_are_counted_once() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(3)).unwrap();
        // One wrong (due tomorrow), two correct (due in 2 days).
        db.check_session(&record.id, &[(1, true), (2, true), (3, false)])
            .unwrap();
        // Make the wrong word also due now: it must count once, not twice.
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "UPDATE review_state SET due_at = ?1 WHERE last_grade = 'again'",
                params![now_secs() - 60],
            )
            .unwrap();
        }
        let summary = db.review_summary().unwrap();
        assert_eq!(summary.due_count, 1);
        assert_eq!(summary.wrong_count, 1);
        assert_eq!(summary.review_count, 1);
    }

    #[test]
    fn review_session_caps_at_twenty_words() {
        let (_temp, db, _lesson) = setup();
        let record = db.create_lesson_session(&_lesson, &items(25)).unwrap();
        let results: Vec<(usize, bool)> = (1..=25).map(|position| (position, false)).collect();
        db.check_session(&record.id, &results).unwrap();
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "UPDATE review_state SET due_at = ?1",
                params![now_secs() - 60],
            )
            .unwrap();
        }
        let review = db.create_review_session("chinese").unwrap().unwrap();
        assert_eq!(review.items.len(), REVIEW_LIMIT);
    }

    fn insert_review_word_full(
        db: &Db,
        subject: &str,
        text: &str,
        due_at: i64,
        interval_days: i64,
        last_grade: &str,
    ) {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO words
               (subject, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
             VALUES (?1, ?2, 'zh', 'abc123def456', 'clips/x.wav', 800, 6000, 2, 0)",
            params![subject, text],
        )
        .unwrap();
        let word_id: i64 = conn
            .query_row(
                "SELECT id FROM words WHERE subject = ?1 AND text = ?2",
                params![subject, text],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![word_id, due_at, interval_days, last_grade],
        )
        .unwrap();
    }

    fn insert_review_word_in_lesson(
        db: &Db,
        subject: &str,
        text: &str,
        clip_lesson: &str,
        due_at: i64,
        interval_days: i64,
        last_grade: &str,
    ) {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO words
               (subject, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
             VALUES (?1, ?2, 'zh', ?3, 'clips/x.wav', 800, 6000, 2, 0)",
            params![subject, text, clip_lesson],
        )
        .unwrap();
        let word_id: i64 = conn
            .query_row(
                "SELECT id FROM words WHERE subject = ?1 AND text = ?2",
                params![subject, text],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
             VALUES (?1, ?2, ?3, 0, ?4)",
            params![word_id, due_at, interval_days, last_grade],
        )
        .unwrap();
    }

    fn insert_lesson_row(db: &Db, id: &str, grade: Option<u8>, term: Option<u8>) {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO lessons
               (id, dir, title, subject, grade, term, item_count, total_ms, updated_at)
             VALUES (?1, ?1, ?1, 'chinese', ?2, ?3, 0, 0, 0)",
            params![id, grade, term],
        )
        .unwrap();
    }

    #[test]
    fn review_queue_reports_priority_and_skips_unplayable_words() {
        let (_temp, db, _lesson) = setup();
        let now = now_secs();
        insert_review_word_full(&db, "chinese", "旧词", now - 10 * 86_400, 30, "good");
        insert_review_word_full(&db, "chinese", "错词", now - 86_400, 1, "again");
        // No clip: cannot be played, must not be offered to the agent.
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO words (subject, text, lang, updated_at)
                 VALUES ('chinese', '无音频', 'zh', 0)",
                [],
            )
            .unwrap();
            let id: i64 = conn
                .query_row("SELECT id FROM words WHERE text = '无音频'", [], |row| {
                    row.get(0)
                })
                .unwrap();
            conn.execute(
                "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
                 VALUES (?1, ?2, 1, 0, 'again')",
                params![id, now - 86_400],
            )
            .unwrap();
        }

        let queue = db.review_queue("chinese", 10, None, None).unwrap();
        assert_eq!(queue.total, 2);
        let texts: Vec<&str> = queue.words.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(texts, ["错词", "旧词"]);
        let wrong = &queue.words[0];
        assert_eq!(wrong.last_grade, "again");
        assert_eq!(wrong.interval_days, 1);
        assert_eq!(wrong.overdue_days, 1);
        assert_eq!(queue.words[1].overdue_days, 10);
    }

    #[test]
    fn custom_session_preserves_order_and_marks_review() {
        let (temp, db, lesson) = setup();
        for text in ["a", "b", "c"] {
            insert_review_word_in_lesson(
                &db,
                "chinese",
                text,
                &lesson.id,
                now_secs() - 86_400,
                1,
                "good",
            );
        }
        let record = db
            .create_custom_session(
                "chinese",
                "第一单元综合复习",
                Some(3),
                Some(1),
                &["c".to_string(), "a".to_string()],
            )
            .unwrap();
        assert_eq!(record.title, "第一单元综合复习");
        assert_eq!(record.kind, "review");
        assert_eq!(record.items.len(), 2);
        assert_eq!(record.items[0].text, "c");
        assert_eq!(record.items[1].text, "a");
        assert!(record.items.iter().all(|item| item.is_review));

        let stored = db.stored_session(&record.id).unwrap().unwrap();
        assert_eq!(stored.subject, "chinese");
        assert_eq!(stored.grade, Some(3));
        assert_eq!(stored.term, Some(1));
        assert_eq!(stored.record.items[0].text, "c");
        assert_eq!(stored.record.items[1].text, "a");
        drop(temp);
    }

    #[test]
    fn custom_session_reports_unknown_and_unplayable_words() {
        let (_temp, db, lesson) = setup();
        insert_review_word_in_lesson(
            &db,
            "chinese",
            "可播放",
            &lesson.id,
            now_secs() - 86_400,
            1,
            "good",
        );
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "INSERT INTO words (subject, text, lang, updated_at)
                 VALUES ('chinese', '无音频', 'zh', 0)",
                [],
            )
            .unwrap();
        }

        let err = db
            .create_custom_session("chinese", "复习", None, None, &["不存在".to_string()])
            .unwrap_err();
        assert!(matches!(err, CustomSessionError::Unknown(ref words) if words == &["不存在"]));

        let err = db
            .create_custom_session("chinese", "复习", None, None, &["无音频".to_string()])
            .unwrap_err();
        assert!(matches!(err, CustomSessionError::Unplayable(ref words) if words == &["无音频"]));
    }

    #[test]
    fn review_session_filters_limit_grade_and_term() {
        let (_temp, db, lesson) = setup();
        {
            let conn = db.conn().unwrap();
            conn.execute(
                "UPDATE lessons SET grade = 3, term = 1 WHERE id = ?1",
                [&lesson.id],
            )
            .unwrap();
        }
        insert_lesson_row(&db, "lesson000004", Some(4), Some(2));
        let now = now_secs();

        for index in 1..=4 {
            insert_review_word_in_lesson(
                &db,
                "chinese",
                &format!("三{index}"),
                &lesson.id,
                now - 86_400,
                1,
                "again",
            );
        }
        for index in 1..=3 {
            insert_review_word_in_lesson(
                &db,
                "chinese",
                &format!("四{index}"),
                "lesson000004",
                now - 86_400,
                1,
                "again",
            );
        }

        let limited = db
            .create_review_session_filtered("chinese", 2, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(limited.items.len(), 2, "limit 生效");

        let grade3 = db
            .create_review_session_filtered("chinese", 40, Some(3), None)
            .unwrap()
            .unwrap();
        assert_eq!(grade3.items.len(), 4);
        assert!(grade3.items.iter().all(|item| item.text.starts_with('三')));

        let grade4_term2 = db
            .create_review_session_filtered("chinese", 40, Some(4), Some(2))
            .unwrap()
            .unwrap();
        assert_eq!(grade4_term2.items.len(), 3);
        assert!(grade4_term2
            .items
            .iter()
            .all(|item| item.text.starts_with('四')));

        let all = db
            .create_review_session_filtered("chinese", 40, None, None)
            .unwrap()
            .unwrap();
        assert_eq!(all.items.len(), 7);
    }

    #[test]
    fn review_mix_prioritises_again_then_oldest_last_review() {
        let (_temp, db, _lesson) = setup();
        let now = now_secs();
        // A: correct, interval 1d, due 3d ago  -> last reviewed 4d ago.
        insert_review_word_full(&db, "chinese", "A", now - 3 * 86_400, 1, "good");
        // B: correct, interval 30d, due 5d ago -> last reviewed 35d ago.
        insert_review_word_full(&db, "chinese", "B", now - 5 * 86_400, 30, "good");
        // C: marked wrong last time, always first.
        insert_review_word_full(&db, "chinese", "C", now - 86_400, 1, "again");
        // D: correct, due 40d ago -> last reviewed 41d ago (stalest).
        insert_review_word_full(&db, "chinese", "D", now - 40 * 86_400, 1, "good");

        let mix = db.review_items_for_mix("chinese", 10, &[]).unwrap();
        let texts: Vec<&str> = mix.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["C", "D", "B", "A"]);
    }

    #[test]
    fn review_selection_is_scoped_to_subject() {
        let (_temp, db, lesson) = setup();
        let now = now_secs();
        insert_review_word_for(
            &db,
            "chinese",
            "语文错词",
            &lesson.id,
            "clips/zh.wav",
            now - 10,
        );
        insert_review_word_for(
            &db,
            "english",
            "apple",
            &lesson.id,
            "clips/en.wav",
            now - 10,
        );

        let zh = db.review_items_for_mix("chinese", 5, &[]).unwrap();
        assert_eq!(zh.len(), 1);
        assert_eq!(zh[0].text, "语文错词");

        let en = db.review_items_for_mix("english", 5, &[]).unwrap();
        assert_eq!(en.len(), 1);
        assert_eq!(en[0].text, "apple");

        let review = db.create_review_session("english").unwrap().unwrap();
        assert_eq!(review.items.len(), 1);
        assert_eq!(review.items[0].text, "apple");

        let conn = db.conn().unwrap();
        let subject: String = conn
            .query_row(
                "SELECT subject FROM sessions WHERE id = ?1",
                [&review.id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(subject, "english");
    }

    #[test]
    fn interval_ladder() {
        assert_eq!(next_interval(1), 2);
        assert_eq!(next_interval(2), 4);
        assert_eq!(next_interval(4), 7);
        assert_eq!(next_interval(7), 15);
        assert_eq!(next_interval(15), 30);
        assert_eq!(next_interval(30), 30);
        assert_eq!(next_interval(99), 30);
    }

    /// Seed a `words` + `review_state` pair without going through a session.
    fn insert_review_word(db: &Db, text: &str, clip_lesson: &str, clip_path: &str, due_at: i64) {
        insert_review_word_for(db, "chinese", text, clip_lesson, clip_path, due_at);
    }

    fn insert_review_word_for(
        db: &Db,
        subject: &str,
        text: &str,
        clip_lesson: &str,
        clip_path: &str,
        due_at: i64,
    ) {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO words (subject, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
             VALUES (?1, ?2, 'zh', ?3, ?4, 800, 5000, 2, ?5)
             ON CONFLICT(subject, text) DO UPDATE SET
               clip_lesson = excluded.clip_lesson,
               clip_path = excluded.clip_path,
               duration_ms = excluded.duration_ms,
               gap_ms = excluded.gap_ms,
               repeat = excluded.repeat",
            params![subject, text, clip_lesson, clip_path, due_at],
        )
        .unwrap();
        let word_id: i64 = conn
            .query_row(
                "SELECT id FROM words WHERE subject = ?1 AND text = ?2",
                params![subject, text],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
             VALUES (?1, ?2, 1, 1, 'again')
             ON CONFLICT(word_id) DO UPDATE SET
               due_at = excluded.due_at,
               last_grade = excluded.last_grade",
            params![word_id, due_at],
        )
        .unwrap();
    }

    #[test]
    fn review_items_for_mix_orders_caps_and_excludes_known_texts() {
        let (_temp, db, _lesson) = setup();
        let now = now_secs();
        for index in 1..=7 {
            insert_review_word(
                &db,
                &format!("旧词{index}"),
                "abc123def456",
                &format!("clips/old-{index}.wav"),
                now + index,
            );
        }

        let mix = db.review_items_for_mix("chinese", 5, &[]).unwrap();
        assert_eq!(mix.len(), 5, "最多混入 5 个");
        assert_eq!(mix[0].text, "旧词1", "按 due_at 升序");
        assert!(mix.iter().all(|item| item.is_review));
        assert!(mix.iter().all(|item| item.repeat_gap_ms == 1_200));
        assert!(mix.iter().all(|item| item.clip_lesson == "abc123def456"));
        assert_eq!(mix[0].duration_ms, 800);
        assert_eq!(mix[0].gap_ms, 5_000);
        assert_eq!(mix[0].repeat, 2);

        let exclude = vec!["旧词1".to_string(), "旧词2".to_string()];
        let mix = db.review_items_for_mix("chinese", 5, &exclude).unwrap();
        assert_eq!(mix.len(), 5);
        assert_eq!(mix[0].text, "旧词3", "本课已有的词被跳过");
    }

    #[test]
    fn review_items_for_mix_skips_words_without_clips() {
        let (_temp, db, _lesson) = setup();
        let text = "无音频";
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO words (subject, text, lang, updated_at) VALUES ('chinese', ?1, 'zh', ?2)",
            params![text, now_secs()],
        )
        .unwrap();
        let word_id: i64 = conn
            .query_row(
                "SELECT id FROM words WHERE subject = 'chinese' AND text = ?1",
                [text],
                |row| row.get(0),
            )
            .unwrap();
        conn.execute(
            "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
             VALUES (?1, ?2, 1, 1, 'again')",
            params![word_id, now_secs() - 60],
        )
        .unwrap();
        drop(conn);

        assert!(db
            .review_items_for_mix("chinese", 5, &[])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn mix_items_are_appended_after_lesson_items() {
        let (_temp, db, lesson) = setup();
        let now = now_secs();
        insert_review_word(&db, "旧词1", "abc123def456", "clips/old-1.wav", now - 120);
        insert_review_word(&db, "旧词2", "abc123def456", "clips/old-2.wav", now - 60);

        let mut all = items(3);
        let exclude: Vec<String> = all.iter().map(|item| item.text.clone()).collect();
        let mix = db.review_items_for_mix("chinese", 5, &exclude).unwrap();
        assert_eq!(mix.len(), 2);
        all.extend(mix);

        let record = db.create_lesson_session(&lesson, &all).unwrap();
        assert_eq!(record.items.len(), 5);
        assert!(!record.items[2].is_review);
        assert!(record.items[3].is_review);
        assert!(record.items[4].is_review);
        assert_eq!(record.items[3].text, "旧词1");
        assert_eq!(record.items[3].clip_lesson, "abc123def456");

        // Persisted positions keep the same order, review flag included.
        let conn = db.conn().unwrap();
        let (position, is_review): (i64, i64) = conn
            .query_row(
                "SELECT position, is_review FROM session_items
                 WHERE session_id = ?1 AND text = '旧词1'",
                [&record.id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(position, 4);
        assert_eq!(is_review, 1);
    }

    #[test]
    fn mix_returns_nothing_when_no_review_words() {
        let (_temp, db, _lesson) = setup();
        assert!(db
            .review_items_for_mix("chinese", 5, &[])
            .unwrap()
            .is_empty());
        assert!(db
            .review_items_for_mix("chinese", 5, &["词1".to_string()])
            .unwrap()
            .is_empty());
    }

    /// `(interval_days, due_at, lapses, last_grade)` for one word.
    fn review_row(db: &Db, text: &str) -> (i64, i64, i64, String) {
        let conn = db.conn().unwrap();
        conn.query_row(
            "SELECT r.interval_days, r.due_at, r.lapses, r.last_grade
             FROM review_state r JOIN words w ON w.id = r.word_id
             WHERE w.subject = 'chinese' AND w.text = ?1",
            [text],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap()
    }

    fn add_attempt(db: &Db, text: &str, created_at: i64) {
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO attempts (session_id, position, word_id, grade, source, created_at)
             SELECT 's-old', 1, w.id, 'good', 'self', ?2
             FROM words w WHERE w.subject = 'chinese' AND w.text = ?1",
            params![text, created_at],
        )
        .unwrap();
    }

    #[test]
    fn review_order_prefers_words_not_practised_recently() {
        let (_temp, db, _lesson) = setup();
        let now = now_secs();
        // Same due_at and interval, so only the real attempt time separates
        // them: early practice must push a word to the back.
        insert_review_word_full(&db, "chinese", "刚练过", now - 86_400, 1, "good");
        insert_review_word_full(&db, "chinese", "很久没练", now - 86_400, 1, "good");
        add_attempt(&db, "刚练过", now - 3_600);

        let queue = db.review_queue("chinese", 10, None, None).unwrap();
        let texts: Vec<&str> = queue.words.iter().map(|word| word.text.as_str()).collect();
        assert_eq!(texts, ["很久没练", "刚练过"]);

        let review = db.create_review_session("chinese").unwrap().unwrap();
        let texts: Vec<&str> = review.items.iter().map(|item| item.text.as_str()).collect();
        assert_eq!(texts, ["很久没练", "刚练过"]);
    }

    #[test]
    fn early_practice_keeps_the_schedule_untouched() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(3)).unwrap();
        db.check_session(&record.id, &[(1, true), (2, true), (3, true)])
            .unwrap();
        let before = review_row(&db, "词1");
        assert_eq!(before.0, 2, "首次答对 → 2 天");

        // A custom session may include a word that is not due yet.
        let custom = db
            .create_custom_session("chinese", "巩固", None, None, &["词1".to_string()])
            .unwrap();
        db.check_session(&custom.id, &[(1, true)]).unwrap();
        assert_eq!(review_row(&db, "词1"), before, "提前复习答对不能改调度");

        // A wrong answer always counts, even when the word was not due.
        let custom = db
            .create_custom_session("chinese", "巩固", None, None, &["词2".to_string()])
            .unwrap();
        db.check_session(&custom.id, &[(1, false)]).unwrap();
        let after = review_row(&db, "词2");
        assert_eq!(after.0, 1);
        assert_eq!(after.2, 1, "答错要记一次回退");
        assert_eq!(after.3, "again");
    }

    #[test]
    fn correcting_a_wrong_word_advances_even_before_due() {
        let (_temp, db, lesson) = setup();
        let record = db.create_lesson_session(&lesson, &items(3)).unwrap();
        db.check_session(&record.id, &[(1, true), (2, true), (3, false)])
            .unwrap();

        // 词3 is 'again' with due_at in the future; it must not be mistaken
        // for early practice.
        let review = db.create_review_session("chinese").unwrap().unwrap();
        assert_eq!(review.items[0].text, "词3");
        db.check_session(&review.id, &[(1, true)]).unwrap();

        let after = review_row(&db, "词3");
        assert_eq!(after.0, 2, "1 天 → 2 天");
        assert_eq!(after.3, "good");
    }

    #[test]
    fn lists_sessions_with_counts_and_hides_unfinished() {
        let (_temp, db, lesson) = setup();
        let done = db.create_lesson_session(&lesson, &items(3)).unwrap();
        db.check_session(&done.id, &[(1, true), (2, true), (3, false)])
            .unwrap();
        let pending = db.create_lesson_session(&lesson, &items(2)).unwrap();

        let finished = db.list_sessions(20, false).unwrap();
        assert_eq!(finished.len(), 1);
        assert_eq!(finished[0].id, done.id);
        assert_eq!(finished[0].kind, "lesson");
        assert_eq!(finished[0].title, "第5课");
        assert_eq!(finished[0].item_count, 3);
        assert_eq!(finished[0].correct, 2);
        assert_eq!(finished[0].wrong, 1);
        assert!(finished[0].finished_at.is_some());

        let all = db.list_sessions(20, true).unwrap();
        assert_eq!(all.len(), 2);
        assert!(all.iter().any(|session| session.id == pending.id));
        assert!(all
            .iter()
            .find(|session| session.id == pending.id)
            .unwrap()
            .finished_at
            .is_none());

        let latest = db.latest_finished_session().unwrap().unwrap();
        assert_eq!(latest.id, done.id);
        assert_eq!(db.list_sessions(1, false).unwrap().len(), 1);
    }

    #[test]
    fn session_result_reports_items_and_wrong_words() {
        let (_temp, db, lesson) = setup();
        let mut session_items = items(3);
        for item in &mut session_items {
            item.clip_lesson = lesson.id.clone();
        }
        let record = db.create_lesson_session(&lesson, &session_items).unwrap();

        let pending = db.session_result(&record.id).unwrap().unwrap();
        assert!(pending.summary.finished_at.is_none());
        assert_eq!(pending.items.len(), 3);
        assert!(pending
            .items
            .iter()
            .all(|item| item.grade.is_none() && item.source.is_none()));
        assert!(pending.wrong_words.is_empty());

        db.check_session(&record.id, &[(1, true), (2, false), (3, true)])
            .unwrap();
        let result = db.session_result(&record.id).unwrap().unwrap();
        let grades: Vec<_> = result
            .items
            .iter()
            .map(|item| item.grade.as_deref())
            .collect();
        assert_eq!(grades, vec![Some("good"), Some("again"), Some("good")]);
        assert!(result
            .items
            .iter()
            .all(|item| item.source.as_deref() == Some("self")));
        assert_eq!(result.items[0].position, 1);
        assert_eq!(result.items[0].origin_lesson.as_deref(), Some("第5课"));
        assert!(!result.items[0].review);
        assert_eq!(result.summary.correct, 2);
        assert_eq!(result.summary.wrong, 1);

        assert_eq!(result.wrong_words.len(), 1);
        let wrong = &result.wrong_words[0];
        assert_eq!(wrong.position, 2);
        assert_eq!(wrong.text, "词2");
        assert_eq!(wrong.grade, "again");
        assert_eq!(wrong.source, "self");
        assert_eq!(wrong.origin_lesson.as_deref(), Some("第5课"));
        assert_eq!(wrong.lapses, 1);
        assert_eq!(wrong.interval_days, 1);
        assert!(wrong.due_at.is_some());

        assert!(db.session_result("nope").unwrap().is_none());
    }

    #[test]
    fn wrong_review_word_keeps_origin_lesson() {
        let (temp, db, lesson) = setup();
        let root = temp.path().join("lessons");
        lesson_dir(&root, "第3课", 2);
        let scanned = db.scan_lessons(&root).unwrap();
        let other = scanned
            .iter()
            .find(|entry| entry.title == "第3课")
            .unwrap()
            .clone();

        let mut all = items(2);
        for item in &mut all {
            item.clip_lesson = lesson.id.clone();
        }
        all.push(NewItem {
            text: "旧词".to_string(),
            lang: "zh".to_string(),
            clip_lesson: other.id.clone(),
            clip_path: "clips/old.wav".to_string(),
            duration_ms: 800,
            gap_ms: 6_000,
            repeat: 2,
            repeat_gap_ms: 1_200,
            is_review: true,
        });
        let record = db.create_lesson_session(&lesson, &all).unwrap();
        db.check_session(&record.id, &[(1, true), (2, true), (3, false)])
            .unwrap();

        let result = db.session_result(&record.id).unwrap().unwrap();
        let review_item = &result.items[2];
        assert!(review_item.review);
        assert_eq!(review_item.origin_lesson.as_deref(), Some("第3课"));
        assert_eq!(result.wrong_words.len(), 1);
        assert_eq!(result.wrong_words[0].text, "旧词");
        assert_eq!(
            result.wrong_words[0].origin_lesson.as_deref(),
            Some("第3课")
        );
        assert!(result.wrong_words[0].review);
    }

    // ------------------------------------------------------------------
    // schema migrations
    // ------------------------------------------------------------------

    const V1_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS lessons (
  id TEXT PRIMARY KEY,
  dir TEXT NOT NULL,
  title TEXT NOT NULL,
  item_count INTEGER NOT NULL,
  total_ms INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sessions (
  id TEXT PRIMARY KEY,
  kind TEXT NOT NULL,
  lesson_id TEXT,
  title TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE TABLE IF NOT EXISTS session_items (
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  text TEXT NOT NULL,
  lang TEXT NOT NULL,
  clip_lesson TEXT NOT NULL,
  clip_path TEXT NOT NULL,
  duration_ms INTEGER NOT NULL,
  gap_ms INTEGER NOT NULL,
  repeat INTEGER NOT NULL,
  is_review INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (session_id, position)
);
CREATE TABLE IF NOT EXISTS words (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  text TEXT NOT NULL UNIQUE,
  lang TEXT NOT NULL,
  clip_lesson TEXT,
  clip_path TEXT,
  duration_ms INTEGER,
  gap_ms INTEGER,
  repeat INTEGER,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS attempts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  session_id TEXT NOT NULL,
  position INTEGER NOT NULL,
  word_id INTEGER NOT NULL,
  grade TEXT NOT NULL,
  source TEXT NOT NULL,
  created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS review_state (
  word_id INTEGER PRIMARY KEY,
  due_at INTEGER NOT NULL,
  interval_days INTEGER NOT NULL,
  lapses INTEGER NOT NULL DEFAULT 0,
  last_grade TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_review_due ON review_state(due_at);
"#;

    fn write_v1_db(path: &Path, version: i32) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .unwrap();
        conn.execute_batch(V1_SCHEMA).unwrap();
        conn.execute(
            "INSERT INTO lessons (id, dir, title, item_count, total_ms, updated_at)
             VALUES ('l1', '/srv/lessons/第5课', '第5课', 2, 1000, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO words (id, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
             VALUES (7, '摇晃', 'zh', 'l1', 'clips/a.wav', 1000, 6000, 2, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO words (id, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
             VALUES (9, 'apple', 'en', 'l1', 'clips/b.wav', 900, 6000, 2, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
             VALUES (7, 100, 1, 2, 'again')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO sessions (id, kind, lesson_id, title, created_at)
             VALUES ('s1', 'lesson', 'l1', '第5课', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_items
               (session_id, position, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, is_review)
             VALUES ('s1', 1, '摇晃', 'zh', 'l1', 'clips/a.wav', 1000, 6000, 2, 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO attempts (session_id, position, word_id, grade, source, created_at)
             VALUES ('s1', 1, 7, 'good', 'self', 0)",
            [],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
    }

    fn backup_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.contains(".bak-v"))
            .collect()
    }

    #[test]
    fn migrates_legacy_database_to_v2() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dictation.db");
        // Deployed v0.1.0 databases never set user_version, so this is the
        // realistic legacy case.
        write_v1_db(&path, 0);

        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);

        let words: Vec<(i64, String, String)> = {
            let mut stmt = conn
                .prepare("SELECT id, subject, text FROM words ORDER BY id")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            words,
            vec![
                (7, "chinese".to_string(), "摇晃".to_string()),
                (9, "english".to_string(), "apple".to_string()),
            ],
            "id 必须保留，科目按字符集回填"
        );

        let subject: String = conn
            .query_row("SELECT subject FROM lessons WHERE id = 'l1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(subject, "chinese");
        let session_subject: String = conn
            .query_row("SELECT subject FROM sessions WHERE id = 's1'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(session_subject, "chinese");

        // Same text may now exist once per subject, and new ids keep growing.
        conn.execute(
            "INSERT INTO words (subject, text, lang, updated_at)
             VALUES ('english', '摇晃', 'zh', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO words (subject, text, lang, updated_at)
             VALUES ('chinese', '新的', 'zh', 0)",
            [],
        )
        .unwrap();
        let new_id: i64 = conn
            .query_row("SELECT id FROM words WHERE text = '新的'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(new_id > 9, "自增序列必须接着旧数据: {new_id}");

        let repeat_gap: i64 = conn
            .query_row(
                "SELECT repeat_gap_ms FROM session_items WHERE session_id = 's1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(repeat_gap, 1_200, "v3 新列必须有默认值");

        assert_eq!(backup_files(temp.path()).len(), 1, "迁移前必须备份");
    }

    #[test]
    fn migration_is_idempotent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dictation.db");
        write_v1_db(&path, 1);

        drop(Db::open(&path).unwrap());
        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        let version: i32 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM words", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        assert_eq!(backup_files(temp.path()).len(), 1, "第二次打开不应再备份");
    }

    #[test]
    fn rejects_newer_schema_version() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dictation.db");
        write_v1_db(&path, SCHEMA_VERSION + 1);

        let err = Db::open(&path).err().expect("版本过高必须拒绝");
        let message = format!("{err:#}");
        assert!(
            message.contains("版本") && message.contains("升级"),
            "错误必须提示升级二进制: {message}"
        );
    }

    #[test]
    fn migration_adds_the_attempts_index() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dictation.db");
        write_v1_db(&path, 1);

        let db = Db::open(&path).unwrap();
        let conn = db.conn().unwrap();
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master
                 WHERE type = 'index' AND name = 'idx_attempts_word'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1, "v4 必须建立 attempts 索引");
    }

    #[test]
    fn keeps_only_the_latest_three_backups() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("dictation.db");
        for index in 0..5 {
            let name = format!("dictation.db.bak-v1-2026010{index}T000000Z");
            fs::write(temp.path().join(name), b"x").unwrap();
        }
        prune_backups(&path, 3).unwrap();
        let mut left = backup_files(temp.path());
        left.sort();
        assert_eq!(
            left,
            vec![
                "dictation.db.bak-v1-20260102T000000Z".to_string(),
                "dictation.db.bak-v1-20260103T000000Z".to_string(),
                "dictation.db.bak-v1-20260104T000000Z".to_string(),
            ]
        );
    }

    // ---- 分享结果导入（v5） ----

    fn import_payload(finished_at: &str, wrong_first: bool) -> String {
        format!(
            r#"{{
              "schema": "dictation.result/v1",
              "lesson": {{ "lesson_id": "l1", "content_hash": "sha256:abc",
                           "title": "第5课", "subject": "chinese" }},
              "session": {{ "kind": "lesson", "started_at": "2026-11-19T10:00:00+08:00",
                            "finished_at": "{finished_at}", "device": "iPad", "student": "" }},
              "items": [
                {{ "position": 1, "text": "摇晃", "lang": "zh", "review": false,
                   "correct": {} }},
                {{ "position": 2, "text": "粗壮", "lang": "zh", "review": false,
                   "correct": true }}
              ]
            }}"#,
            if wrong_first { "false" } else { "true" }
        )
    }

    fn parse_import(json: &str) -> crate::result_import::ParsedImport {
        crate::result_import::parse(json.as_bytes()).unwrap()
    }

    #[test]
    fn import_result_is_idempotent() {
        let db = Db::open_in_memory().unwrap();
        let parsed = parse_import(&import_payload("2026-11-19T10:05:12+08:00", true));

        let first = db
            .import_result(&parsed, &crate::result_import::ClipIndex::default())
            .unwrap();
        assert_eq!(first.status, ImportStatus::Imported);
        assert_eq!((first.correct, first.wrong), (1, 1));

        let second = db
            .import_result(&parsed, &crate::result_import::ClipIndex::default())
            .unwrap();
        assert_eq!(second.status, ImportStatus::Duplicate);
        assert_eq!(second.session_id, first.session_id);
        assert_eq!((second.correct, second.wrong), (1, 1));

        let conn = db.conn().unwrap();
        let sessions: i64 = conn
            .query_row("SELECT COUNT(*) FROM sessions", [], |row| row.get(0))
            .unwrap();
        let attempts: i64 = conn
            .query_row("SELECT COUNT(*) FROM attempts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(sessions, 1, "重复导入不新建 session");
        assert_eq!(attempts, 2, "重复导入不重复写 attempts");
        let sources: Vec<String> = {
            let mut stmt = conn
                .prepare("SELECT DISTINCT source FROM attempts")
                .unwrap();
            stmt.query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(sources, vec!["import".to_string()]);
    }

    #[test]
    fn import_schedules_from_finished_at_and_marks_words() {
        let db = Db::open_in_memory().unwrap();
        let parsed = parse_import(&import_payload("2026-11-19T10:05:12+08:00", true));
        let outcome = db
            .import_result(&parsed, &crate::result_import::ClipIndex::default())
            .unwrap();

        {
            let conn = db.conn().unwrap();
            let (due_at, interval, lapses, grade): (i64, i64, i64, String) = conn
                .query_row(
                    "SELECT r.due_at, r.interval_days, r.lapses, r.last_grade
                       FROM review_state r JOIN words w ON w.id = r.word_id
                      WHERE w.text = '摇晃'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                )
                .unwrap();
            assert_eq!(grade, "again");
            assert_eq!(interval, 1);
            assert_eq!(lapses, 1);
            assert_eq!(due_at, parsed.finished_at + 86_400, "以完成时刻为基准");
        }

        let result = db.session_result(&outcome.session_id).unwrap().unwrap();
        assert_eq!(result.summary.item_count, 2);
        assert_eq!(result.summary.correct, 1);
        assert_eq!(result.summary.wrong, 1);
        assert_eq!(result.wrong_words.len(), 1);
        assert_eq!(result.wrong_words[0].text, "摇晃");
    }

    #[test]
    fn imported_word_without_local_audio_stays_out_of_review_pool() {
        let db = Db::open_in_memory().unwrap();
        let parsed = parse_import(&import_payload("2026-11-19T10:05:12+08:00", true));
        db.import_result(&parsed, &crate::result_import::ClipIndex::default())
            .unwrap();

        let pool = db
            .review_items_for_mix("chinese", 10, &[])
            .unwrap_or_default();
        assert!(pool.is_empty(), "没有本机片段的词不进复习池");
        let summary = db.review_summary().unwrap();
        assert_eq!(
            summary.review_count, 0,
            "待复习统计与复习池一致，只算能播的词"
        );
    }

    #[test]
    fn same_finished_at_with_different_verdict_conflicts() {
        let db = Db::open_in_memory().unwrap();
        let first = parse_import(&import_payload("2026-11-19T10:05:12+08:00", true));
        db.import_result(&first, &crate::result_import::ClipIndex::default())
            .unwrap();

        let second = parse_import(&import_payload("2026-11-19T10:05:12+08:00", false));
        let err = db
            .import_result(&second, &crate::result_import::ClipIndex::default())
            .unwrap_err();
        assert!(matches!(
            err,
            crate::result_import::ImportError::Conflict(_)
        ));
    }

    #[test]
    fn import_uses_clip_index_for_new_words() {
        let dir = tempfile::tempdir().unwrap();
        let lesson = dir.path().join("第5课");
        fs::create_dir_all(&lesson).unwrap();
        fs::write(
            lesson.join("manifest.json"),
            r#"{
              "version": 1, "title": "第5课", "subject": "chinese",
              "generated_at": "2026-11-19T10:00:00+08:00",
              "defaults": {"gap_ms": 6000, "repeat": 2, "repeat_gap_ms": 1200,
                           "lead_in_ms": 3000, "intro_gap_ms": 1500},
              "items": [
                {"index": 1, "text": "摇晃", "lang": "zh",
                 "clips": [{"path": "clips/01-a.wav", "duration_ms": 1000}],
                 "repeat": 2, "repeat_gap_ms": 1200, "gap_ms": 6000},
                {"index": 2, "text": "粗壮", "lang": "zh",
                 "clips": [{"path": "clips/02-b.wav", "duration_ms": 900}],
                 "repeat": 2, "repeat_gap_ms": 1200, "gap_ms": 6000}
              ],
              "total_ms": 20000
            }"#,
        )
        .unwrap();

        let db = Db::open_in_memory().unwrap();
        let parsed = parse_import(&import_payload("2026-11-19T10:05:12+08:00", true));
        let clips = crate::result_import::ClipIndex::scan(dir.path(), "chinese");
        db.import_result(&parsed, &clips).unwrap();

        let pool = db.review_items_for_mix("chinese", 10, &[]).unwrap();
        assert_eq!(pool.len(), 1, "导入的词从课时清单补到了音源");
        assert_eq!(pool[0].text, "摇晃");
        assert_eq!(db.review_summary().unwrap().review_count, 1);
    }
}
