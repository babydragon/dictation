use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::model::{Lang, Subject};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: u32,
    pub title: String,
    /// Optional for backwards compatibility; missing means `chinese`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<Subject>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub term: Option<u8>,
    /// Maximum review words mixed into a lesson session; 0 disables mixing.
    #[serde(default = "default_review_cap")]
    pub review_cap: u8,
    pub generated_at: String,
    pub defaults: ManifestDefaults,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intro: Option<ManifestClip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intro_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outro: Option<ManifestClip>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outro_text: Option<String>,
    pub items: Vec<ManifestItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_track: Option<String>,
    pub total_ms: u32,
}

fn default_review_cap() -> u8 {
    8
}

impl Manifest {
    /// Subject of the lesson; old manifests without the field are Chinese.
    pub fn subject(&self) -> Subject {
        self.subject.unwrap_or_default()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestDefaults {
    pub gap_ms: u32,
    pub repeat: u8,
    pub repeat_gap_ms: u32,
    pub lead_in_ms: u32,
    /// Silence between the opening sentence and the first item.
    #[serde(default)]
    pub intro_gap_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestItem {
    pub index: usize,
    pub text: String,
    pub lang: Lang,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub announce: Option<ManifestClip>,
    pub clips: Vec<ManifestClip>,
    pub repeat: u8,
    pub repeat_gap_ms: u32,
    pub gap_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestClip {
    pub path: String,
    pub duration_ms: u32,
}

pub fn write(path: &Path, manifest: &Manifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("创建目录失败: {}", parent.display()))?;
        }
    }
    let mut json = serde_json::to_string_pretty(manifest).context("序列化 manifest 失败")?;
    json.push('\n');
    fs::write(path, json).with_context(|| format!("写 manifest 失败: {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        Manifest {
            version: MANIFEST_VERSION,
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
                path: "clips/intro-abcd1234.wav".to_string(),
                duration_ms: 2_600,
            }),
            intro_text: Some("请准备好纸和笔，听写马上开始。".to_string()),
            outro: Some(ManifestClip {
                path: "clips/outro-beef5678.wav".to_string(),
                duration_ms: 1_900,
            }),
            outro_text: Some("听写结束，请检查一遍。".to_string()),
            items: vec![ManifestItem {
                index: 1,
                text: "摇晃".to_string(),
                lang: Lang::Zh,
                announce: Some(ManifestClip {
                    path: "clips/idx-01-abcd1234.wav".to_string(),
                    duration_ms: 700,
                }),
                clips: vec![ManifestClip {
                    path: "clips/01-abcd1234.wav".to_string(),
                    duration_ms: 1_100,
                }],
                repeat: 2,
                repeat_gap_ms: 1_200,
                gap_ms: 6_000,
            }],
            full_track: Some("full.mp3".to_string()),
            total_ms: 9_200,
        }
    }

    #[test]
    fn writes_and_roundtrips() {
        let dir = std::env::temp_dir().join(format!("dictation-manifest-{}", uuid::Uuid::new_v4()));
        let path = dir.join("manifest.json");
        write(&path, &sample_manifest()).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.contains("\"version\": 1"));
        assert!(raw.ends_with('\n'));

        let parsed: Manifest = serde_json::from_str(&raw).unwrap();
        assert_eq!(parsed.title, "第5课");
        assert_eq!(parsed.items.len(), 1);
        assert_eq!(parsed.items[0].lang, Lang::Zh);
        assert_eq!(parsed.items[0].clips[0].duration_ms, 1_100);
        assert_eq!(parsed.defaults.intro_gap_ms, 1_500);
        assert_eq!(parsed.intro.as_ref().unwrap().duration_ms, 2_600);
        assert_eq!(
            parsed.intro_text.as_deref(),
            Some("请准备好纸和笔，听写马上开始。")
        );
        assert_eq!(parsed.outro.as_ref().unwrap().duration_ms, 1_900);
        assert_eq!(parsed.outro_text.as_deref(), Some("听写结束，请检查一遍。"));
        assert_eq!(parsed.full_track.as_deref(), Some("full.mp3"));
        assert_eq!(parsed.total_ms, 9_200);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn omits_absent_optional_fields() {
        let mut manifest = sample_manifest();
        manifest.full_track = None;
        manifest.items[0].announce = None;
        manifest.intro = None;
        manifest.intro_text = None;
        manifest.outro = None;
        manifest.outro_text = None;
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(!json.contains("\"full_track\""));
        assert!(!json.contains("\"announce\""));
        assert!(!json.contains("\"intro\":"));
        assert!(!json.contains("\"intro_text\":"));
        assert!(!json.contains("\"outro\":"));
        assert!(!json.contains("\"outro_text\":"));
        assert!(json.contains("\"intro_gap_ms\":1500"));
    }

    #[test]
    fn review_cap_defaults_to_eight_for_old_manifests() {
        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value.as_object_mut().unwrap().remove("review_cap");
        let parsed: Manifest = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.review_cap, 8);
    }

    #[test]
    fn old_manifest_without_subject_reads_as_chinese() {
        use crate::model::Subject;

        let mut value = serde_json::to_value(sample_manifest()).unwrap();
        value.as_object_mut().unwrap().remove("subject");
        value.as_object_mut().unwrap().remove("grade");
        value.as_object_mut().unwrap().remove("term");

        let parsed: Manifest = serde_json::from_value(value).unwrap();
        assert_eq!(parsed.subject(), Subject::Chinese);
        assert_eq!(parsed.grade, None);
        assert_eq!(parsed.term, None);
    }
}
