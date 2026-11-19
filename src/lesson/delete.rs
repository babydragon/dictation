//! Delete a lesson (files + `lessons` row) and keep the child's history.
//!
//! Deleting a duplicate lesson must not lose review progress: words whose clip
//! came from the deleted lesson are repointed to another lesson that still
//! contains the same text (same subject). Only when no other lesson has the
//! word does its clip get cleared, which removes it from the review pool.
//!
//! `sessions` / `attempts` / `review_state` are never touched: results stay
//! queryable, only the audio source goes away.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::db::{ClipRef, Db, WordClipMove};
use crate::manifest::Manifest;

/// Why a delete plan could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum DeleteError {
    #[error("课时不存在")]
    NotFound,
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Where a word's audio moves to after the delete.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Repoint {
    pub text: String,
    pub to_lesson: String,
    pub to_lesson_id: String,
    #[serde(skip)]
    pub subject: String,
    #[serde(skip)]
    pub clip: ClipRef,
}

/// What `apply` will do. Produced by [`plan`] and safe to show before deleting.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DeletePlan {
    /// Lesson directory name (relative).
    pub lesson: String,
    pub lesson_id: String,
    pub title: String,
    /// Files under the lesson directory.
    pub files: usize,
    pub bytes: u64,
    /// Sessions referencing this lesson; kept by design.
    pub sessions: u32,
    pub words_repointed: Vec<Repoint>,
    /// Words with no other lesson left; their clip is cleared.
    pub words_cleared: Vec<String>,
    /// Same words with their subject, used by `apply` (`words_cleared` is display only).
    #[serde(skip)]
    pub cleared_words: Vec<(String, String)>,
}

/// One lesson in a duplicate group.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DuplicateLesson {
    pub dir: String,
    pub id: String,
    pub item_count: usize,
    pub total_ms: u32,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct DuplicateGroup {
    pub subject: String,
    pub title: String,
    pub lessons: Vec<DuplicateLesson>,
}

/// Find every `(subject, title)` that appears more than once.
pub fn duplicates(listings: &[crate::lesson::list::Listing]) -> Vec<DuplicateGroup> {
    let mut groups: Vec<DuplicateGroup> = Vec::new();
    for item in listings {
        if let Some(group) = groups
            .iter_mut()
            .find(|group| group.subject == item.subject && group.title == item.title)
        {
            group.lessons.push(DuplicateLesson {
                dir: item.dir.clone(),
                id: item.id.clone(),
                item_count: item.item_count,
                total_ms: item.total_ms,
            });
        } else {
            groups.push(DuplicateGroup {
                subject: item.subject.clone(),
                title: item.title.clone(),
                lessons: vec![DuplicateLesson {
                    dir: item.dir.clone(),
                    id: item.id.clone(),
                    item_count: item.item_count,
                    total_ms: item.total_ms,
                }],
            });
        }
    }
    groups.retain(|group| group.lessons.len() > 1);
    groups
}

/// Build a delete plan for `raw` (lesson directory name or 12-hex id).
pub fn plan(lessons_dir: &Path, db: &Db, raw: &str) -> Result<DeletePlan, DeleteError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(DeleteError::NotFound);
    }
    let entries = db.scan_lessons(lessons_dir).map_err(DeleteError::Other)?;
    let target = entries
        .iter()
        .find(|entry| entry.dir_name == raw || entry.id == raw)
        .ok_or(DeleteError::NotFound)?
        .clone();

    let target_dir = PathBuf::from(&target.dir);
    let root = fs::canonicalize(lessons_dir)
        .with_context(|| format!("读取课时目录失败: {}", lessons_dir.display()))
        .map_err(DeleteError::Other)?;
    let canonical = fs::canonicalize(&target_dir)
        .with_context(|| format!("读取课时目录失败: {}", target_dir.display()))
        .map_err(DeleteError::Other)?;
    if canonical.parent() != Some(root.as_path()) {
        return Err(DeleteError::Other(anyhow::anyhow!(
            "拒绝删除课时目录之外的路径"
        )));
    }

    let (files, bytes) = measure_dir(&canonical);

    // text -> (lesson id, dir, clip) for every *other* lesson of the same subject.
    let mut sources: Vec<(String, String, String, String, ClipRef)> = Vec::new();
    for entry in entries.iter().filter(|entry| entry.id != target.id) {
        let manifest_path = Path::new(&entry.dir).join("manifest.json");
        let Ok(raw_manifest) = fs::read_to_string(&manifest_path) else {
            continue;
        };
        let Ok(manifest) = serde_json::from_str::<Manifest>(&raw_manifest) else {
            continue;
        };
        if manifest.subject().as_str() != entry.subject {
            continue;
        }
        for item in &manifest.items {
            let Some(clip) = item.clips.first() else {
                continue;
            };
            sources.push((
                entry.subject.clone(),
                item.text.clone(),
                entry.id.clone(),
                entry.dir_name.clone(),
                ClipRef {
                    lesson_id: entry.id.clone(),
                    path: clip.path.clone(),
                    duration_ms: i64::from(clip.duration_ms),
                    gap_ms: i64::from(item.gap_ms),
                    repeat: i64::from(item.repeat.max(1)),
                },
            ));
        }
    }

    let words = db
        .words_for_clip_lesson(&target.id)
        .map_err(DeleteError::Other)?;
    let mut words_repointed = Vec::new();
    let mut words_cleared = Vec::new();
    let mut cleared_words = Vec::new();
    for (subject, text) in words {
        match sources.iter().find(|(source_subject, source_text, ..)| {
            *source_subject == subject && *source_text == text
        }) {
            Some((_, _, lesson_id, dir_name, clip)) => words_repointed.push(Repoint {
                text,
                to_lesson: dir_name.clone(),
                to_lesson_id: lesson_id.clone(),
                subject,
                clip: clip.clone(),
            }),
            None => {
                words_cleared.push(text.clone());
                cleared_words.push((subject.clone(), text));
            }
        }
    }

    let sessions = db
        .session_count_for_lesson(&target.id)
        .map_err(DeleteError::Other)?;

    Ok(DeletePlan {
        lesson: target.dir_name,
        lesson_id: target.id,
        title: target.title,
        files,
        bytes,
        sessions,
        words_repointed,
        words_cleared,
        cleared_words,
    })
}

/// Execute a plan: repoint words, drop the DB row, then remove the directory.
pub fn apply(lessons_dir: &Path, db: &Db, plan: &DeletePlan) -> Result<()> {
    let root = fs::canonicalize(lessons_dir).context("读取课时目录失败")?;
    let dir = root.join(&plan.lesson);
    let canonical = fs::canonicalize(&dir).context("读取课时目录失败")?;
    if canonical.parent() != Some(root.as_path()) {
        anyhow::bail!("拒绝删除课时目录之外的路径");
    }

    let moves: Vec<WordClipMove> = plan
        .words_repointed
        .iter()
        .map(|item| WordClipMove {
            subject: item.subject.clone(),
            text: item.text.clone(),
            clip: Some(item.clip.clone()),
        })
        .collect();
    // DB first: a failed DB update leaves the files intact and the plan re-runnable.
    db.apply_lesson_delete(&plan.lesson_id, &moves, &plan.cleared_words)?;
    fs::remove_dir_all(&canonical)
        .with_context(|| format!("删除课时目录失败: {}", canonical.display()))?;
    Ok(())
}

fn measure_dir(dir: &Path) -> (usize, u64) {
    let mut files = 0_usize;
    let mut bytes = 0_u64;
    let Ok(entries) = fs::read_dir(dir) else {
        return (files, bytes);
    };
    for entry in entries.flatten() {
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if meta.is_dir() {
            let (nested_files, nested_bytes) = measure_dir(&entry.path());
            files += nested_files;
            bytes += nested_bytes;
        } else if meta.is_file() {
            files += 1;
            bytes += meta.len();
        }
    }
    (files, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson::list::Listing;

    fn listing(subject: &str, title: &str, dir: &str, items: usize) -> Listing {
        Listing {
            id: format!("id-{dir}"),
            dir: dir.to_string(),
            title: title.to_string(),
            subject: subject.to_string(),
            grade: None,
            term: None,
            item_count: items,
            total_ms: 1000,
        }
    }

    #[test]
    fn duplicate_groups_keep_only_repeated_titles() {
        let groups = duplicates(&[
            listing("chinese", "第5课", "第5课", 20),
            listing("chinese", "第5课", "第5课-新", 20),
            listing("chinese", "第6课", "第6课", 10),
            listing("english", "第5课", "Unit 1", 9),
        ]);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].title, "第5课");
        assert_eq!(groups[0].lessons.len(), 2);
        assert_eq!(groups[0].subject, "chinese");
    }

    use crate::manifest::{Manifest, ManifestClip, ManifestDefaults};

    fn write_lesson(root: &Path, dir: &str, title: &str, texts: &[&str]) {
        let path = root.join(dir);
        fs::create_dir_all(path.join("clips")).unwrap();
        fs::write(path.join("lesson.txt"), texts.join("\n")).unwrap();
        fs::write(path.join("clips/01-abcd.wav"), b"x").unwrap();
        let manifest = Manifest {
            version: 1,
            title: title.to_string(),
            subject: crate::model::Subject::parse("chinese"),
            grade: None,
            term: None,
            review_cap: 8,
            generated_at: "2026-11-19T12:00:00+08:00".to_string(),
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
            items: texts
                .iter()
                .enumerate()
                .map(|(index, text)| crate::manifest::ManifestItem {
                    index: index + 1,
                    text: (*text).to_string(),
                    lang: crate::model::Lang::Zh,
                    announce: None,
                    clips: vec![ManifestClip {
                        path: format!("clips/{:02}-abcd.wav", index + 1),
                        duration_ms: 800,
                    }],
                    repeat: 2,
                    repeat_gap_ms: 1_200,
                    gap_ms: 6_000,
                })
                .collect(),
            full_track: None,
            total_ms: 10_000,
        };
        crate::manifest::write(&path.join("manifest.json"), &manifest).unwrap();
    }

    #[test]
    fn delete_repoints_words_and_keeps_history() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("lessons");
        fs::create_dir_all(&root).unwrap();
        write_lesson(&root, "第5课-旧", "第5课", &["甲", "乙"]);
        write_lesson(&root, "第5课-新", "第5课", &["甲", "丙"]);

        let db = Db::open(&temp.path().join("db.sqlite")).unwrap();
        let entries = db.scan_lessons(&root).unwrap();
        let old = entries
            .iter()
            .find(|entry| entry.dir_name == "第5课-旧")
            .unwrap()
            .clone();
        let new = entries
            .iter()
            .find(|entry| entry.dir_name == "第5课-新")
            .unwrap()
            .clone();
        let make_items = |lesson_id: &str, texts: &[&str]| -> Vec<crate::db::NewItem> {
            texts
                .iter()
                .map(|text| crate::db::NewItem {
                    text: (*text).to_string(),
                    lang: "zh".to_string(),
                    clip_lesson: lesson_id.to_string(),
                    clip_path: "clips/01-abcd.wav".to_string(),
                    duration_ms: 800,
                    gap_ms: 6_000,
                    repeat: 2,
                    repeat_gap_ms: 1_200,
                    is_review: false,
                })
                .collect()
        };
        // 旧课时的一节课：产生 sessions 行、words 行（clip 指向旧课时）
        db.create_lesson_session(&old, &make_items(&old.id, &["甲", "乙"]))
            .unwrap();

        let plan = plan(&root, &db, "第5课-旧").unwrap();
        assert_eq!(plan.sessions, 1);
        assert_eq!(plan.words_cleared, vec!["乙".to_string()]);
        assert_eq!(plan.words_repointed.len(), 1);
        assert_eq!(plan.words_repointed[0].to_lesson, "第5课-新");
        assert!(plan.files >= 2);
        assert!(plan.bytes > 0);

        apply(&root, &db, &plan).unwrap();

        assert!(!root.join("第5课-旧").exists(), "课时目录应删除");
        assert!(root.join("第5课-新").exists(), "保留的课时不受影响");
        assert!(
            db.lesson_by_id(&old.id).unwrap().is_none(),
            "lessons 行应删除"
        );

        assert!(
            db.lesson_by_id(&old.id).unwrap().is_none(),
            "lessons 行应删除"
        );
        assert!(
            db.words_for_clip_lesson(&old.id).unwrap().is_empty(),
            "不应再有词指向被删课时"
        );
        let moved = db.words_for_clip_lesson(&new.id).unwrap();
        assert_eq!(moved, vec![("chinese".to_string(), "甲".to_string())]);
        assert_eq!(
            db.session_count_for_lesson(&old.id).unwrap(),
            1,
            "历史会话必须保留"
        );
        let profiles = db.word_profiles("chinese", &["乙".to_string()]).unwrap();
        assert!(!profiles[0].has_clip, "无人提供的词应清空音源");
    }

    #[test]
    fn delete_rejects_unknown_lesson() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("lessons");
        fs::create_dir_all(&root).unwrap();
        let db = Db::open(&temp.path().join("db.sqlite")).unwrap();
        assert!(matches!(
            plan(&root, &db, "没有这一课"),
            Err(DeleteError::NotFound)
        ));
    }
}
