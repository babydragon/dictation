//! 分享结果回传：`dictation.result/v1` 的解析、校验与幂等键。
//!
//! 契约：[`docs/share-publish.md`](../docs/share-publish.md) §9。
//! CLI（`dictation result import`）与 HTTP（`POST /api/results/import`）
//! 共用本模块；落库在 [`crate::db::Db::import_result`]。
//!
//! 设计要点：
//! - **自包含**：`items[].text` 足够入库，不要求服务端还能匹配到课时；
//! - **幂等**：忽略客户端 id，按规范化内容求 sha256；
//! - **时间基准**用 `session.finished_at`，不是导入时刻。

use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;

pub const RESULT_SCHEMA: &str = "dictation.result/v1";
/// 未来时间超过该容差即钳制到当前时刻（设备时钟偏差）。
pub const FUTURE_TOLERANCE_SECS: i64 = 300;
pub const MAX_ITEMS: usize = 200;
pub const MAX_TEXT_CHARS: usize = 200;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Conflict(String),
    #[error(transparent)]
    Db(#[from] anyhow::Error),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportItem {
    pub position: usize,
    pub text: String,
    pub lang: String,
    pub review: bool,
    pub correct: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedImport {
    pub content_hash: String,
    pub lesson_id: Option<String>,
    pub title: String,
    pub subject: String,
    pub started_at: Option<i64>,
    pub finished_at: i64,
    pub student: String,
    pub items: Vec<ImportItem>,
}

impl ParsedImport {
    pub fn correct(&self) -> u32 {
        self.items.iter().filter(|item| item.correct).count() as u32
    }

    pub fn wrong(&self) -> u32 {
        self.items.iter().filter(|item| !item.correct).count() as u32
    }

    /// 幂等键：`sha256(内容)`，**忽略任何客户端 id**。
    /// 字段固定、按 position 排序，RFC3339 先解析成 Unix 秒。
    pub fn import_key(&self) -> String {
        #[derive(Serialize)]
        struct KeyItem<'a> {
            position: usize,
            text: &'a str,
            correct: bool,
        }
        #[derive(Serialize)]
        struct Key<'a> {
            content_hash: &'a str,
            subject: &'a str,
            finished_at: i64,
            student: &'a str,
            items: Vec<KeyItem<'a>>,
        }

        let mut items: Vec<&ImportItem> = self.items.iter().collect();
        items.sort_by_key(|item| item.position);
        let payload = Key {
            content_hash: &self.content_hash,
            subject: &self.subject,
            finished_at: self.finished_at,
            student: &self.student,
            items: items
                .into_iter()
                .map(|item| KeyItem {
                    position: item.position,
                    text: &item.text,
                    correct: item.correct,
                })
                .collect(),
        };
        let mut hasher = Sha256::new();
        hasher.update(serde_json::to_vec(&payload).expect("导入键序列化不会失败"));
        format!("sha256:{}", hex::encode(hasher.finalize()))
    }
}

// ---------------------------------------------------------------------------
// 片段索引：导入时把 `(subject, text)` 对上本机课时的音频
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClipRef {
    pub clip_lesson: String,
    pub clip_path: String,
    pub duration_ms: u32,
    pub gap_ms: u32,
    pub repeat: u8,
}

/// 按科目扫描 `lessons/` 的 `manifest.json`，建立 `text → 片段` 映射。
///
/// 导入的词如果本机 `words` 里还没有（例如新课时还没在家做过），用它补上音源，
/// 否则这些错词永远不会进复习池。同文本取**先扫描到的**课时（与课时列表顺序一致）。
#[derive(Debug, Default, Clone)]
pub struct ClipIndex {
    entries: std::collections::HashMap<String, ClipRef>,
}

impl ClipIndex {
    pub fn scan(lessons_dir: &Path, subject: &str) -> Self {
        let mut entries = std::collections::HashMap::new();
        let Ok(dirs) = std::fs::read_dir(lessons_dir) else {
            return Self { entries };
        };
        for entry in dirs.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let manifest_path = dir.join("manifest.json");
            let Ok(raw) = std::fs::read_to_string(&manifest_path) else {
                continue;
            };
            let Ok(manifest) = serde_json::from_str::<crate::manifest::Manifest>(&raw) else {
                continue;
            };
            if manifest.subject().as_str() != subject {
                continue;
            }
            let Some(dir_name) = dir.file_name().map(|n| n.to_string_lossy().to_string()) else {
                continue;
            };
            let clip_lesson = crate::db::lesson_id(&dir_name);
            for item in &manifest.items {
                let Some(clip) = item.clips.first() else {
                    continue;
                };
                entries.entry(item.text.clone()).or_insert_with(|| ClipRef {
                    clip_lesson: clip_lesson.clone(),
                    clip_path: clip.path.clone(),
                    duration_ms: clip.duration_ms,
                    gap_ms: item.gap_ms,
                    repeat: item.repeat.max(1),
                });
            }
        }
        Self { entries }
    }

    pub fn get(&self, text: &str) -> Option<&ClipRef> {
        self.entries.get(text)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ---------------------------------------------------------------------------
// 解析
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct Wire {
    schema: String,
    lesson: WireLesson,
    session: WireSession,
    items: Vec<WireItem>,
}

#[derive(Debug, Deserialize)]
struct WireLesson {
    #[serde(default)]
    lesson_id: String,
    #[serde(default)]
    content_hash: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    subject: String,
    #[serde(default)]
    url: String,
}

#[derive(Debug, Deserialize)]
struct WireSession {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    started_at: String,
    finished_at: String,
    #[serde(default)]
    device: String,
    #[serde(default)]
    student: String,
}

#[derive(Debug, Deserialize)]
struct WireItem {
    position: usize,
    text: String,
    #[serde(default)]
    lang: String,
    #[serde(default)]
    review: bool,
    correct: bool,
}

fn parse_time(raw: &str, field: &str) -> Result<i64, ImportError> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ImportError::BadRequest(format!("缺少 session.{field}")));
    }
    OffsetDateTime::parse(text, &Rfc3339)
        .map(|time| time.unix_timestamp())
        .map_err(|_| ImportError::BadRequest(format!("session.{field} 不是 RFC3339 时间：{text}")))
}

fn infer_lang(text: &str) -> &'static str {
    let has_han = text
        .chars()
        .any(|c| matches!(c as u32, 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF));
    let has_latin = text.chars().any(|c| c.is_ascii_alphabetic());
    if has_han && !has_latin {
        "zh"
    } else if has_latin && !has_han {
        "en"
    } else {
        "mixed"
    }
}

pub fn parse(bytes: &[u8]) -> Result<ParsedImport, ImportError> {
    let wire: Wire = serde_json::from_slice(bytes)
        .map_err(|err| ImportError::BadRequest(format!("结果 JSON 解析失败：{err}")))?;
    if wire.schema != RESULT_SCHEMA {
        return Err(ImportError::BadRequest(format!(
            "不支持的格式：{}（需要 {RESULT_SCHEMA}）",
            wire.schema
        )));
    }

    let subject = wire.lesson.subject.trim().to_string();
    if subject != "chinese" && subject != "english" {
        return Err(ImportError::BadRequest(format!(
            "lesson.subject 必须是 chinese / english，收到：{:?}",
            wire.lesson.subject
        )));
    }

    if wire.items.is_empty() {
        return Err(ImportError::BadRequest("items 不能为空".to_string()));
    }
    if wire.items.len() > MAX_ITEMS {
        return Err(ImportError::BadRequest(format!(
            "items 最多 {MAX_ITEMS} 条"
        )));
    }

    let mut seen = std::collections::BTreeSet::new();
    let mut items = Vec::with_capacity(wire.items.len());
    for item in wire.items {
        if item.position == 0 {
            return Err(ImportError::BadRequest(
                "items[].position 从 1 开始".to_string(),
            ));
        }
        if !seen.insert(item.position) {
            return Err(ImportError::BadRequest(format!(
                "position {} 重复",
                item.position
            )));
        }
        let text = item.text.trim().to_string();
        if text.is_empty() {
            return Err(ImportError::BadRequest(format!(
                "position {} 的文本为空",
                item.position
            )));
        }
        if text.chars().count() > MAX_TEXT_CHARS {
            return Err(ImportError::BadRequest(format!(
                "position {} 的文本超过 {MAX_TEXT_CHARS} 字符",
                item.position
            )));
        }
        let lang = match item.lang.trim() {
            "zh" | "en" | "mixed" => item.lang.trim().to_string(),
            _ => infer_lang(&text).to_string(),
        };
        items.push(ImportItem {
            position: item.position,
            text,
            lang,
            review: item.review,
            correct: item.correct,
        });
    }
    items.sort_by_key(|item| item.position);

    let mut finished_at = parse_time(&wire.session.finished_at, "finished_at")?;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    if finished_at > now + FUTURE_TOLERANCE_SECS {
        finished_at = now; // 设备时钟偏快，按导入时刻计
    }
    let started_at = if wire.session.started_at.trim().is_empty() {
        None
    } else {
        Some(parse_time(&wire.session.started_at, "started_at")?)
    };

    let title = {
        let title = wire.lesson.title.trim();
        if title.is_empty() {
            "导入的听写".to_string()
        } else {
            title.to_string()
        }
    };
    let lesson_id = {
        let id = wire.lesson.lesson_id.trim();
        if id.is_empty() {
            None
        } else {
            Some(id.to_string())
        }
    };

    let _ = (wire.lesson.url, wire.session.kind, wire.session.device);

    Ok(ParsedImport {
        content_hash: wire.lesson.content_hash.trim().to_string(),
        lesson_id,
        title,
        subject,
        started_at,
        finished_at,
        student: wire.session.student.trim().to_string(),
        items,
    })
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"{
      "schema": "dictation.result/v1",
      "lesson": {
        "lesson_id": "a1b2c3d4e5f6",
        "content_hash": "sha256:9f2c",
        "title": "第5课",
        "subject": "chinese",
        "url": "https://audio.example.com/lessons/a1b2c3d4e5f6/lesson.json"
      },
      "session": {
        "kind": "lesson",
        "started_at": "2026-11-19T10:00:00+08:00",
        "finished_at": "2026-11-19T10:05:12+08:00",
        "device": "iPad",
        "student": ""
      },
      "items": [
        { "position": 1, "text": "摇晃", "lang": "zh", "review": false, "correct": false },
        { "position": 2, "text": "粗壮", "lang": "zh", "review": false, "correct": true }
      ],
      "summary": { "total": 2, "correct": 1, "wrong": 1 }
    }"#;

    #[test]
    fn parses_valid_payload() {
        let parsed = parse(VALID.as_bytes()).unwrap();
        assert_eq!(parsed.subject, "chinese");
        assert_eq!(parsed.title, "第5课");
        assert_eq!(parsed.lesson_id.as_deref(), Some("a1b2c3d4e5f6"));
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(parsed.correct(), 1);
        assert_eq!(parsed.wrong(), 1);
        assert_eq!(parsed.finished_at, 1_795_053_912); // +08:00 → UTC
    }

    #[test]
    fn import_key_is_stable_and_order_independent() {
        let a = parse(VALID.as_bytes()).unwrap();
        let mut b = parse(VALID.as_bytes()).unwrap();
        b.items.reverse(); // 顺序不同，内容相同
        assert_eq!(a.import_key(), b.import_key());
        assert!(a.import_key().starts_with("sha256:"));

        let mut c = parse(VALID.as_bytes()).unwrap();
        c.items[0].correct = true;
        assert_ne!(a.import_key(), c.import_key(), "判定变化必须换键");
    }

    #[test]
    fn rejects_wrong_schema_and_subject() {
        let err = parse(br#"{"schema":"x","lesson":{},"session":{},"items":[]}"#).unwrap_err();
        assert!(matches!(err, ImportError::BadRequest(_)));
        let err = parse(VALID.replace("\"chinese\"", "\"math\"").as_bytes()).unwrap_err();
        assert!(err.to_string().contains("subject"));
    }

    #[test]
    fn rejects_duplicate_positions_and_empty_text() {
        let payload = VALID.replace(
            r#"{ "position": 2, "text": "粗壮", "lang": "zh", "review": false, "correct": true }"#,
            r#"{ "position": 1, "text": "粗壮", "lang": "zh", "review": false, "correct": true }"#,
        );
        let err = parse(payload.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("重复"));

        let payload = VALID.replace(r#""text": "粗壮""#, r#""text": "  ""#);
        let err = parse(payload.as_bytes()).unwrap_err();
        assert!(err.to_string().contains("文本为空"));
    }

    #[test]
    fn future_finished_at_is_clamped() {
        let payload = VALID.replace("2026-11-19T10:05:12+08:00", "2099-01-01T00:00:00+00:00");
        let parsed = parse(payload.as_bytes()).unwrap();
        let now = OffsetDateTime::now_utc().unix_timestamp();
        assert!((now - parsed.finished_at).abs() < 60);
    }

    #[test]
    fn clip_index_scans_manifests_by_subject() {
        let dir = tempfile::tempdir().unwrap();
        let lesson = dir.path().join("第5课");
        std::fs::create_dir_all(&lesson).unwrap();
        std::fs::write(
            lesson.join("manifest.json"),
            r#"{
              "version": 1, "title": "第5课", "subject": "chinese",
              "generated_at": "2026-11-19T10:00:00+08:00",
              "defaults": {"gap_ms": 6000, "repeat": 2, "repeat_gap_ms": 1200,
                           "lead_in_ms": 3000, "intro_gap_ms": 1500},
              "items": [
                {"index": 1, "text": "摇晃", "lang": "zh",
                 "clips": [{"path": "clips/01-a.wav", "duration_ms": 1000}],
                 "repeat": 2, "repeat_gap_ms": 1200, "gap_ms": 6000}
              ],
              "total_ms": 10000
            }"#,
        )
        .unwrap();

        let chinese = ClipIndex::scan(dir.path(), "chinese");
        let clip = chinese.get("摇晃").unwrap();
        assert_eq!(clip.clip_path, "clips/01-a.wav");
        assert_eq!(clip.duration_ms, 1000);
        assert_eq!(clip.clip_lesson, crate::db::lesson_id("第5课"));
        assert!(chinese.get("不存在").is_none());

        let english = ClipIndex::scan(dir.path(), "english");
        assert!(english.is_empty());
    }

    #[test]
    fn infers_lang_when_missing() {
        let payload = VALID.replace(r#""lang": "zh""#, r#""lang": """#);
        let parsed = parse(payload.as_bytes()).unwrap();
        assert_eq!(parsed.items[0].lang, "zh");
    }
}
