//! Shared lesson listing for `dictation lesson list` and `GET /api/lessons`.
//!
//! One reader and one serializer: the CLI must never reimplement this.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::warn;

use crate::db::lesson_id;
use crate::manifest::Manifest;

/// One lesson directory discovered under the lessons root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Listing {
    /// `sha256(dir_name)` truncated to 12 hex chars.
    pub id: String,
    /// Relative directory name; safe to expose (never an absolute host path).
    pub dir: String,
    pub title: String,
    pub item_count: usize,
    pub total_ms: u32,
}

/// Scan `root` for `*/manifest.json` and return listings sorted by
/// `title`, then `dir`.
///
/// Directories without a manifest are ignored; unreadable or malformed
/// manifests are skipped with a warning so one broken lesson cannot hide the
/// rest.
pub fn scan(root: &Path) -> Result<Vec<Listing>> {
    let mut listings = Vec::new();
    if root.is_dir() {
        for entry in
            fs::read_dir(root).with_context(|| format!("读取课时目录失败: {}", root.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.json");
            if !manifest_path.is_file() {
                continue;
            }
            let raw = match fs::read_to_string(&manifest_path) {
                Ok(raw) => raw,
                Err(err) => {
                    warn!("跳过 {}：读取 manifest 失败（{err}）", path.display());
                    continue;
                }
            };
            let manifest: Manifest = match serde_json::from_str(&raw) {
                Ok(manifest) => manifest,
                Err(err) => {
                    warn!("跳过 {}：manifest 解析失败（{err}）", path.display());
                    continue;
                }
            };
            let dir = path
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_default();
            if dir.is_empty() {
                continue;
            }
            listings.push(Listing {
                id: lesson_id(&dir),
                dir,
                title: manifest.title,
                item_count: manifest.items.len(),
                total_ms: manifest.total_ms,
            });
        }
    }
    listings.sort_by(|a, b| a.title.cmp(&b.title).then_with(|| a.dir.cmp(&b.dir)));
    Ok(listings)
}

/// The single JSON shape used by both the CLI and the HTTP endpoint.
pub fn json(listing: &Listing) -> Value {
    json!({
        "id": listing.id,
        "dir": listing.dir,
        "title": listing.title,
        "item_count": listing.item_count,
        "total_ms": listing.total_ms,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::lesson_id;
    use crate::manifest::{self, Manifest, ManifestClip, ManifestDefaults, ManifestItem};
    use crate::model::Lang;
    use std::fs;

    fn manifest(title: &str, items: usize, total_ms: u32) -> Manifest {
        Manifest {
            version: 1,
            title: title.to_string(),
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
            total_ms,
        }
    }

    fn lesson_dir(root: &Path, dir: &str, title: &str, items: usize, total_ms: u32) {
        let path = root.join(dir);
        fs::create_dir_all(&path).unwrap();
        manifest::write(
            &path.join("manifest.json"),
            &manifest(title, items, total_ms),
        )
        .unwrap();
    }

    #[test]
    fn scans_sorts_and_skips_broken_directories() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        lesson_dir(root, "第6课", "第6课", 2, 20_000);
        lesson_dir(root, "第5课", "第5课", 3, 30_000);
        // Same title, different dir: tie-break by dir.
        lesson_dir(root, "第5课副本", "第5课", 1, 10_000);
        // No manifest at all.
        fs::create_dir_all(root.join("空目录")).unwrap();
        // Broken manifest.
        fs::create_dir_all(root.join("坏课时")).unwrap();
        fs::write(root.join("坏课时/manifest.json"), "{bad").unwrap();

        let listings = scan(root).unwrap();
        let dirs: Vec<&str> = listings.iter().map(|l| l.dir.as_str()).collect();
        assert_eq!(dirs, ["第5课", "第5课副本", "第6课"]);

        assert_eq!(listings[0].id, lesson_id("第5课"));
        assert_eq!(listings[0].id.len(), 12);
        assert_eq!(listings[0].item_count, 3);
        assert_eq!(listings[0].total_ms, 30_000);
    }

    #[test]
    fn missing_root_yields_empty_list() {
        let temp = tempfile::tempdir().unwrap();
        assert!(scan(&temp.path().join("nope")).unwrap().is_empty());
    }

    #[test]
    fn json_contains_id_and_relative_dir_only() {
        let listing = Listing {
            id: "a1b2c3d4e5f6".to_string(),
            dir: "第5课".to_string(),
            title: "第5课 词语听写".to_string(),
            item_count: 20,
            total_ms: 181_160,
        };
        let value = json(&listing);
        assert_eq!(value["id"], "a1b2c3d4e5f6");
        assert_eq!(value["dir"], "第5课");
        assert_eq!(value["title"], "第5课 词语听写");
        assert_eq!(value["item_count"], 20);
        assert_eq!(value["total_ms"], 181_160);
        assert_eq!(value.as_object().unwrap().len(), 5, "字段集合固定: {value}");
    }
}
