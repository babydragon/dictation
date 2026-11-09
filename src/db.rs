//! SQLite persistence for lessons, sessions, attempts and review scheduling.
//!
//! All timestamps are Unix seconds. Queries are short, so a plain
//! `std::sync::Mutex` around the connection is enough; callers hold it only for
//! the duration of one small transaction.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
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

/// A lesson directory discovered under `--dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LessonEntry {
    pub id: String,
    /// Relative directory name (`第5课`); safe to expose to clients.
    pub dir_name: String,
    /// Absolute host path, used for filesystem access only; never returned.
    pub dir: String,
    pub title: String,
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

#[derive(Debug)]
pub struct CheckOutcome {
    pub correct: u32,
    pub wrong: u32,
    pub due_count: u32,
}

/// Review counters for the home screen.
///
/// `due_count` keeps the old "due now" meaning; `wrong_count` counts words
/// marked wrong last time; `review_count` is what the player should offer
/// (wrong now OR due now, each row counted once).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReviewSummary {
    pub due_count: u32,
    pub wrong_count: u32,
    pub review_count: u32,
}

/// One row of the `result list` view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub kind: String,
    pub title: String,
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
        let conn = Connection::open(path)
            .with_context(|| format!("打开数据库失败: {}", path.display()))?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    fn from_connection(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON; PRAGMA busy_timeout=5000;",
        )
        .context("初始化 SQLite 失败")?;
        conn.execute_batch(SCHEMA).context("建表失败")?;
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
                item_count: listing.item_count,
                total_ms: listing.total_ms,
            })
            .collect();

        let now = now_secs();
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        for entry in &entries {
            tx.execute(
                "INSERT INTO lessons (id, dir, title, item_count, total_ms, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(id) DO UPDATE SET
                   dir = excluded.dir,
                   title = excluded.title,
                   item_count = excluded.item_count,
                   total_ms = excluded.total_ms,
                   updated_at = excluded.updated_at",
                params![
                    entry.id,
                    entry.dir,
                    entry.title,
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
                "SELECT id, dir, title, item_count, total_ms FROM lessons WHERE id = ?1",
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
                        item_count: row.get::<_, i64>(3)? as usize,
                        total_ms: row.get::<_, i64>(4)? as u32,
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
               AND w.clip_lesson IS NOT NULL
               AND w.clip_path IS NOT NULL",
        );
        let mut args = vec![Value::Integer(now)];
        if !exclude_texts.is_empty() {
            let placeholders = vec!["?"; exclude_texts.len()].join(", ");
            sql.push_str(&format!(" AND w.text NOT IN ({placeholders})"));
            args.extend(exclude_texts.iter().cloned().map(Value::Text));
        }
        sql.push_str(" ORDER BY r.due_at ASC, w.id ASC LIMIT ?");
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
        lesson_id: &str,
        title: &str,
        items: &[NewItem],
    ) -> Result<SessionRecord> {
        anyhow::ensure!(!items.is_empty(), "课时没有可播放的片段");
        let id = uuid::Uuid::new_v4().simple().to_string();
        let now = now_secs();
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO sessions (id, kind, lesson_id, title, created_at)
             VALUES (?1, 'lesson', ?2, ?3, ?4)",
            params![id, lesson_id, title, now],
        )?;
        for (index, item) in items.iter().enumerate() {
            insert_session_item(&tx, &id, index + 1, item)?;
            upsert_word(&tx, item, now)?;
        }
        tx.commit()?;
        Ok(SessionRecord {
            id,
            kind: "lesson".to_string(),
            title: title.to_string(),
            items: items.to_vec(),
        })
    }

    /// Create a review session from words that are due now or were marked
    /// wrong last time. `None` when there is nothing to review.
    pub fn create_review_session(&self) -> Result<Option<SessionRecord>> {
        let now = now_secs();
        let conn = self.conn()?;
        let tx = conn.unchecked_transaction()?;
        let mut items = {
            let mut stmt = tx.prepare(
                "SELECT w.text, w.lang, w.clip_lesson, w.clip_path, w.duration_ms, w.gap_ms, w.repeat
                 FROM review_state r
                 JOIN words w ON w.id = r.word_id
                 WHERE r.last_grade = 'again' OR r.due_at <= ?1
                 ORDER BY r.due_at ASC, w.id ASC
                 LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![now, REVIEW_LIMIT as i64], review_item_from_row)?;
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
            "INSERT INTO sessions (id, kind, lesson_id, title, created_at)
             VALUES (?1, 'review', NULL, '错词复习', ?2)",
            params![id, now],
        )?;
        for (index, item) in items.iter().enumerate() {
            insert_session_item(&tx, &id, index + 1, item)?;
        }
        tx.commit()?;
        Ok(Some(SessionRecord {
            id,
            kind: "review".to_string(),
            title: "错词复习".to_string(),
            items: std::mem::take(&mut items),
        }))
    }

    pub fn review_summary(&self) -> Result<ReviewSummary> {
        let now = now_secs();
        let conn = self.conn()?;
        let summary = conn.query_row(
            "SELECT
               COALESCE(SUM(due_at <= ?1), 0),
               COALESCE(SUM(last_grade = 'again'), 0),
               COALESCE(SUM(last_grade = 'again' OR due_at <= ?1), 0)
             FROM review_state",
            [now],
            |row| {
                Ok(ReviewSummary {
                    due_count: row.get::<_, i64>(0)?.max(0) as u32,
                    wrong_count: row.get::<_, i64>(1)?.max(0) as u32,
                    review_count: row.get::<_, i64>(2)?.max(0) as u32,
                })
            },
        )?;
        Ok(summary)
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
            "SELECT s.id, s.kind, s.title, s.created_at, s.finished_at,
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

        let finished: Option<Option<i64>> = tx
            .query_row(
                "SELECT finished_at FROM sessions WHERE id = ?1",
                [session_id],
                |row| row.get(0),
            )
            .optional()?;
        match finished {
            None => return Err(CheckError::NotFound),
            Some(Some(_)) => return Err(CheckError::AlreadyFinished),
            Some(None) => {}
        }

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
            let word_id = upsert_word(&tx, &item, now)?;

            let grade = if correct { "good" } else { "again" };
            tx.execute(
                "INSERT INTO attempts (session_id, position, word_id, grade, source, created_at)
                 VALUES (?1, ?2, ?3, ?4, 'self', ?5)",
                params![session_id, position as i64, word_id, grade, now],
            )?;

            let previous: Option<(i64, i64)> = tx
                .query_row(
                    "SELECT interval_days, lapses FROM review_state WHERE word_id = ?1",
                    [word_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;

            let (interval_days, lapses) = if correct {
                let next = match previous {
                    None => 2,
                    Some((current, _)) => next_interval(current),
                };
                (next, previous.map(|(_, lapses)| lapses).unwrap_or(0))
            } else {
                (1, previous.map(|(_, lapses)| lapses).unwrap_or(0) + 1)
            };
            let due_at = now + interval_days * 86_400;

            tx.execute(
                "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(word_id) DO UPDATE SET
                   due_at = excluded.due_at,
                   interval_days = excluded.interval_days,
                   lapses = excluded.lapses,
                   last_grade = excluded.last_grade",
                params![word_id, due_at, interval_days, lapses, grade],
            )?;

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
}

fn session_summary_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionSummary> {
    Ok(SessionSummary {
        id: row.get(0)?,
        kind: row.get(1)?,
        title: row.get(2)?,
        created_at: row.get(3)?,
        finished_at: row.get(4)?,
        item_count: row.get::<_, i64>(5)?.max(0) as u32,
        correct: row.get::<_, i64>(6)?.max(0) as u32,
        wrong: row.get::<_, i64>(7)?.max(0) as u32,
    })
}

fn session_summary(conn: &Connection, session_id: &str) -> Result<Option<SessionSummary>> {
    let summary = conn
        .query_row(
            "SELECT s.id, s.kind, s.title, s.created_at, s.finished_at,
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
           (session_id, position, text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, is_review)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
            if item.is_review { 1_i64 } else { 0_i64 },
        ],
    )?;
    Ok(())
}

/// Insert or update a word, refreshing clip info from the latest session.
fn upsert_word(conn: &Connection, item: &NewItem, now: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO words
           (text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(text) DO UPDATE SET
           lang = excluded.lang,
           clip_lesson = excluded.clip_lesson,
           clip_path = excluded.clip_path,
           duration_ms = excluded.duration_ms,
           gap_ms = excluded.gap_ms,
           repeat = excluded.repeat,
           updated_at = excluded.updated_at",
        params![
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
        "SELECT id FROM words WHERE text = ?1",
        [&item.text],
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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(5))
            .unwrap();
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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(5))
            .unwrap();
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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(2))
            .unwrap();
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
        let second = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(2))
            .unwrap();
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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(3))
            .unwrap();

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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(3))
            .unwrap();
        db.check_session(&record.id, &[(1, true), (2, true), (3, false)])
            .unwrap();

        let summary = db.review_summary().unwrap();
        assert_eq!(summary.due_count, 0, "错词到期时间仍是明天");
        assert_eq!(summary.wrong_count, 1);
        assert_eq!(summary.review_count, 1);

        let review = db
            .create_review_session()
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
        assert!(db.create_review_session().unwrap().is_none());

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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(3))
            .unwrap();
        db.check_session(&record.id, &[(1, true), (2, true), (3, true)])
            .unwrap();

        let summary = db.review_summary().unwrap();
        assert_eq!(summary.due_count, 0);
        assert_eq!(summary.wrong_count, 0);
        assert_eq!(summary.review_count, 0);
        assert!(
            db.create_review_session().unwrap().is_none(),
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

        let review = db.create_review_session().unwrap().unwrap();
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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(3))
            .unwrap();
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
        let record = db
            .create_lesson_session("abc123def456", "大盘", &items(25))
            .unwrap();
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
        let review = db.create_review_session().unwrap().unwrap();
        assert_eq!(review.items.len(), REVIEW_LIMIT);
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
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO words (text, lang, clip_lesson, clip_path, duration_ms, gap_ms, repeat, updated_at)
             VALUES (?1, 'zh', ?2, ?3, 800, 5000, 2, ?4)
             ON CONFLICT(text) DO UPDATE SET
               clip_lesson = excluded.clip_lesson,
               clip_path = excluded.clip_path,
               duration_ms = excluded.duration_ms,
               gap_ms = excluded.gap_ms,
               repeat = excluded.repeat",
            params![text, clip_lesson, clip_path, due_at],
        )
        .unwrap();
        let word_id: i64 = conn
            .query_row("SELECT id FROM words WHERE text = ?1", [text], |row| {
                row.get(0)
            })
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

        let mix = db.review_items_for_mix(5, &[]).unwrap();
        assert_eq!(mix.len(), 5, "最多混入 5 个");
        assert_eq!(mix[0].text, "旧词1", "按 due_at 升序");
        assert!(mix.iter().all(|item| item.is_review));
        assert!(mix.iter().all(|item| item.repeat_gap_ms == 1_200));
        assert!(mix.iter().all(|item| item.clip_lesson == "abc123def456"));
        assert_eq!(mix[0].duration_ms, 800);
        assert_eq!(mix[0].gap_ms, 5_000);
        assert_eq!(mix[0].repeat, 2);

        let exclude = vec!["旧词1".to_string(), "旧词2".to_string()];
        let mix = db.review_items_for_mix(5, &exclude).unwrap();
        assert_eq!(mix.len(), 5);
        assert_eq!(mix[0].text, "旧词3", "本课已有的词被跳过");
    }

    #[test]
    fn review_items_for_mix_skips_words_without_clips() {
        let (_temp, db, _lesson) = setup();
        let text = "无音频";
        let conn = db.conn().unwrap();
        conn.execute(
            "INSERT INTO words (text, lang, updated_at) VALUES (?1, 'zh', ?2)",
            params![text, now_secs()],
        )
        .unwrap();
        let word_id: i64 = conn
            .query_row("SELECT id FROM words WHERE text = ?1", [text], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute(
            "INSERT INTO review_state (word_id, due_at, interval_days, lapses, last_grade)
             VALUES (?1, ?2, 1, 1, 'again')",
            params![word_id, now_secs() - 60],
        )
        .unwrap();
        drop(conn);

        assert!(db.review_items_for_mix(5, &[]).unwrap().is_empty());
    }

    #[test]
    fn mix_items_are_appended_after_lesson_items() {
        let (_temp, db, lesson) = setup();
        let now = now_secs();
        insert_review_word(&db, "旧词1", "abc123def456", "clips/old-1.wav", now - 120);
        insert_review_word(&db, "旧词2", "abc123def456", "clips/old-2.wav", now - 60);

        let mut all = items(3);
        let exclude: Vec<String> = all.iter().map(|item| item.text.clone()).collect();
        let mix = db.review_items_for_mix(5, &exclude).unwrap();
        assert_eq!(mix.len(), 2);
        all.extend(mix);

        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &all)
            .unwrap();
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
        assert!(db.review_items_for_mix(5, &[]).unwrap().is_empty());
        assert!(db
            .review_items_for_mix(5, &["词1".to_string()])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn lists_sessions_with_counts_and_hides_unfinished() {
        let (_temp, db, lesson) = setup();
        let done = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(3))
            .unwrap();
        db.check_session(&done.id, &[(1, true), (2, true), (3, false)])
            .unwrap();
        let pending = db
            .create_lesson_session(&lesson.id, &lesson.title, &items(2))
            .unwrap();

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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &session_items)
            .unwrap();

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
        let record = db
            .create_lesson_session(&lesson.id, &lesson.title, &all)
            .unwrap();
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
}
