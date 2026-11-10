//! Lesson import shared by the CLI (`lesson import`) and the HTTP API.
//!
//! All validation and file writing lives here so the two entry points can
//! never drift apart. The caller supplies a [`ImportRequest`]; the outcome
//! keeps the relative directory name (safe to expose over HTTP) and the
//! absolute path (host-local, never serialized).

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::app_config;
use crate::lesson;
use crate::model::Subject;
use crate::plan;

#[derive(Debug, Default)]
pub struct ImportRequest {
    pub title: Option<String>,
    pub text: String,
    pub toml: Option<String>,
    pub dir: Option<String>,
    pub force: bool,
    /// Canonical subject code (`chinese` / `english`); required.
    pub subject: String,
    pub grade: Option<u8>,
    pub term: Option<u8>,
}

#[derive(Debug)]
pub struct ImportOutcome {
    /// Directory name relative to `lessons_dir`; safe for HTTP responses.
    pub dir_name: String,
    /// Absolute, canonicalized lesson directory; host-local only.
    pub dir_path: PathBuf,
    pub title: String,
    pub subject: String,
    pub grade: Option<u8>,
    pub term: Option<u8>,
    pub item_count: usize,
    pub total_ms: u32,
    pub warnings: Vec<String>,
    pub created: bool,
}

/// The lesson already exists and `force` was not set.
#[derive(Debug)]
pub struct LessonExists {
    pub dir_name: String,
}

impl fmt::Display for LessonExists {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "课时已存在：{}（加 --force 覆盖，或换一个 --dir）",
            self.dir_name
        )
    }
}

impl std::error::Error for LessonExists {}

/// Validate, parse and persist a lesson under `lessons_dir`.
pub fn import(lessons_dir: &Path, req: ImportRequest) -> Result<ImportOutcome> {
    let title = req
        .title
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_string();
    if title.is_empty() {
        bail!("title 不能为空");
    }
    if req.text.trim().is_empty() {
        bail!("text 不能为空（去掉空行后没有任何词条）");
    }

    let dir_name = match req.dir.as_deref() {
        Some(dir) => {
            let dir = dir.trim().to_string();
            app_config::validate_dir_name(&dir)?;
            dir
        }
        None => app_config::safe_dir_name(&title),
    };
    let target = app_config::resolve_lesson_dir(lessons_dir, &dir_name)?;

    let subject = Subject::parse(&req.subject)
        .with_context(|| format!("subject 必须是 chinese 或 english，当前 {}", req.subject))?;
    if let Some(grade) = req.grade {
        if !(1..=6).contains(&grade) {
            bail!("grade 必须在 1–6 之间，当前 {grade}");
        }
    }
    if let Some(term) = req.term {
        if !(1..=2).contains(&term) {
            bail!("term 必须是 1（上）或 2（下），当前 {term}");
        }
    }
    let toml_body = merge_toml(req.toml.as_deref(), subject, req.grade, req.term)?;

    // Validate the payload before touching the filesystem.
    let lesson = lesson::parse_with_toml(
        &req.text,
        &target.join("lesson.txt"),
        Some(&title),
        Some(&toml_body),
    )?;

    let existed = target.exists();
    if existed && !req.force {
        return Err(anyhow::Error::new(LessonExists {
            dir_name: dir_name.clone(),
        }));
    }

    fs::create_dir_all(lessons_dir)
        .with_context(|| format!("创建课时根目录失败: {}", lessons_dir.display()))?;
    fs::create_dir_all(&target)
        .with_context(|| format!("创建课时目录失败: {}", target.display()))?;

    let root = fs::canonicalize(lessons_dir)
        .with_context(|| format!("解析课时根目录失败: {}", lessons_dir.display()))?;
    let canonical_target = fs::canonicalize(&target)
        .with_context(|| format!("解析课时目录失败: {}", target.display()))?;
    if !canonical_target.starts_with(&root) {
        bail!("目标目录不在课时根目录下：{}", target.display());
    }

    let mut body = String::new();
    let text_body = req.text.strip_prefix('\u{feff}').unwrap_or(&req.text);
    // Persist the title as a heading so `lesson plan/gen` (which parse the file
    // later) see the same title the agent imported.
    let heading = format!("# {title}");
    let first_line = text_body
        .lines()
        .find(|line| !line.trim().is_empty())
        .map(|line| line.trim().to_string());
    if first_line.as_deref() != Some(heading.as_str()) {
        body.push_str(&heading);
        body.push_str("\n\n");
    }
    body.push_str(text_body);
    if !body.ends_with('\n') {
        body.push('\n');
    }
    write_atomic(&canonical_target.join("lesson.txt"), body.as_bytes())?;

    let toml_path = canonical_target.join("lesson.toml");
    write_atomic(&toml_path, toml_body.as_bytes())?;

    let item_count = lesson.items.len();
    let total_ms = plan::estimate(&lesson).total_ms;

    Ok(ImportOutcome {
        dir_name,
        dir_path: canonical_target,
        title: lesson.title,
        subject: lesson.config.subject.as_str().to_string(),
        grade: lesson.config.grade,
        term: lesson.config.term,
        item_count,
        total_ms,
        warnings: Vec::new(),
        created: !existed,
    })
}

/// Merge the explicit subject/grade/term into the caller's TOML so a single
/// `lesson.toml` remains the source of truth for generation.
fn merge_toml(
    raw: Option<&str>,
    subject: Subject,
    grade: Option<u8>,
    term: Option<u8>,
) -> Result<String> {
    let mut value: toml::Value = match raw.filter(|raw| !raw.trim().is_empty()) {
        Some(raw) => raw.parse().context("lesson.toml 解析失败")?,
        None => toml::Value::Table(toml::map::Map::new()),
    };
    let table = value
        .as_table_mut()
        .context("lesson.toml 顶层必须是表（key = value）")?;
    table.insert(
        "subject".to_string(),
        toml::Value::String(subject.as_str().to_string()),
    );
    if let Some(grade) = grade {
        table.insert("grade".to_string(), toml::Value::Integer(i64::from(grade)));
    }
    if let Some(term) = term {
        table.insert("term".to_string(), toml::Value::Integer(i64::from(term)));
    }
    let mut body = toml::to_string(&value).context("序列化 lesson.toml 失败")?;
    if !body.ends_with('\n') {
        body.push('\n');
    }
    Ok(body)
}

/// Write through a unique temp file + rename, so a crash never leaves a
/// half-written lesson behind.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().context("目标路径没有父目录")?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "lesson".to_string());
    let short = uuid::Uuid::new_v4().simple().to_string();
    let tmp = parent.join(format!(".{name}.tmp-{}", &short[..8]));
    fs::write(&tmp, bytes).with_context(|| format!("写入失败: {}", tmp.display()))?;
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("写入失败: {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(title: &str, text: &str) -> ImportRequest {
        ImportRequest {
            title: Some(title.to_string()),
            text: text.to_string(),
            subject: "chinese".to_string(),
            ..ImportRequest::default()
        }
    }

    #[test]
    fn rejects_invalid_or_missing_subject() {
        let temp = tempfile::tempdir().unwrap();
        for subject in ["", "math"] {
            let mut req = request("第5课", "摇晃\n");
            req.subject = subject.to_string();
            let err = import(temp.path(), req).unwrap_err();
            assert!(
                format!("{err:#}").contains("subject"),
                "必须报出 subject 字段: {err:#}"
            );
        }
        assert!(!temp.path().join("第5课").exists(), "校验失败不能落盘");
    }

    #[test]
    fn writes_subject_grade_and_term_into_lesson_toml() {
        let temp = tempfile::tempdir().unwrap();
        let mut req = request("英语 Unit 1", "apple\nlibrary\n");
        req.subject = "english".to_string();
        req.grade = Some(3);
        req.term = Some(2);
        let outcome = import(temp.path(), req).unwrap();
        let raw = fs::read_to_string(outcome.dir_path.join("lesson.toml")).unwrap();
        assert!(raw.contains("subject = \"english\""), "{raw}");
        assert!(raw.contains("grade = 3"), "{raw}");
        assert!(raw.contains("term = 2"), "{raw}");
        assert_eq!(outcome.subject, "english");
        assert_eq!(outcome.grade, Some(3));
        assert_eq!(outcome.term, Some(2));
    }

    #[test]
    fn merges_meta_into_provided_toml_and_keeps_other_keys() {
        let temp = tempfile::tempdir().unwrap();
        let mut req = request("第5课", "摇晃\n");
        req.subject = "english".to_string();
        req.toml = Some("rate = 0.8\nsubject = \"chinese\"\n".to_string());
        let outcome = import(temp.path(), req).unwrap();
        let raw = fs::read_to_string(outcome.dir_path.join("lesson.toml")).unwrap();
        assert!(raw.contains("rate = 0.8"), "其他键必须保留: {raw}");
        assert!(
            raw.contains("subject = \"english\""),
            "payload 覆盖 toml: {raw}"
        );
        assert!(!raw.contains("chinese"), "{raw}");
    }

    #[test]
    fn writes_lesson_and_reports_relative_dir() {
        let temp = tempfile::tempdir().unwrap();
        let outcome = import(temp.path(), request("第5课", "摇晃\n粗壮\n")).unwrap();
        assert!(outcome.created);
        assert_eq!(outcome.dir_name, "第5课");
        assert_eq!(outcome.item_count, 2);
        assert!(outcome.dir_path.ends_with("第5课"));
        assert!(outcome.dir_path.join("lesson.txt").is_file());
    }

    #[test]
    fn duplicate_requires_force() {
        let temp = tempfile::tempdir().unwrap();
        import(temp.path(), request("第5课", "摇晃\n")).unwrap();
        let err = import(temp.path(), request("第5课", "摇晃\n")).unwrap_err();
        let exists = err.downcast_ref::<LessonExists>().expect("结构化冲突错误");
        assert_eq!(exists.dir_name, "第5课");
        assert!(format!("{err:#}").contains("--force"));

        let mut req = request("第5课", "粗壮\n");
        req.force = true;
        let outcome = import(temp.path(), req).unwrap();
        assert!(!outcome.created);
        assert_eq!(outcome.item_count, 1);
    }
}
