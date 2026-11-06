use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::audio;

/// Bump when the audio pipeline changes in a way that invalidates cached clips.
pub const PIPELINE_VERSION: u32 = 5;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheMeta {
    pub duration_ms: u32,
    pub sample_rate: u32,
    pub provider: String,
    pub voice: String,
    pub rate: f32,
    pub text: String,
    pub pipeline_version: u32,
}

#[derive(Debug, Clone)]
pub struct CachedClip {
    pub wav_path: PathBuf,
    pub samples: Vec<i16>,
    pub meta: CacheMeta,
}

#[derive(Debug, Clone)]
pub struct Cache {
    root: PathBuf,
}

impl Cache {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Content hash key. `rate` is quantized to 3 decimals to avoid float noise.
    pub fn key(provider: &str, voice: &str, rate: f32, text: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(provider.as_bytes());
        hasher.update(b"|");
        hasher.update(voice.as_bytes());
        hasher.update(b"|");
        hasher.update(format!("{rate:.3}").as_bytes());
        hasher.update(b"|");
        hasher.update(text.as_bytes());
        hasher.update(b"|");
        hasher.update(PIPELINE_VERSION.to_be_bytes());
        hex::encode(hasher.finalize())[..32].to_string()
    }

    pub fn get(&self, key: &str) -> Result<Option<CachedClip>> {
        let wav_path = self.root.join(format!("{key}.wav"));
        let meta_path = self.root.join(format!("{key}.json"));
        if !wav_path.exists() || !meta_path.exists() {
            return Ok(None);
        }
        let meta_text = fs::read_to_string(&meta_path)
            .with_context(|| format!("读取缓存元数据失败: {}", meta_path.display()))?;
        let meta: CacheMeta = serde_json::from_str(&meta_text)
            .with_context(|| format!("解析缓存元数据失败: {}", meta_path.display()))?;
        if meta.pipeline_version != PIPELINE_VERSION {
            return Ok(None);
        }
        let samples = audio::read_wav_mono(&wav_path)?;
        Ok(Some(CachedClip {
            wav_path,
            samples,
            meta,
        }))
    }

    pub fn put(&self, key: &str, samples: &[i16], meta: &CacheMeta) -> Result<PathBuf> {
        fs::create_dir_all(&self.root)
            .with_context(|| format!("创建缓存目录失败: {}", self.root.display()))?;
        let wav_path = self.root.join(format!("{key}.wav"));
        let meta_path = self.root.join(format!("{key}.json"));
        audio::write_wav_mono(&wav_path, samples, meta.sample_rate)?;
        fs::write(&meta_path, serde_json::to_vec_pretty(meta)?)
            .with_context(|| format!("写缓存元数据失败: {}", meta_path.display()))?;
        Ok(wav_path)
    }
}

// ---------------------------------------------------------------------------
// Cache cleanup (`dictation clean`)
// ---------------------------------------------------------------------------

/// How aggressively to select cache entries for deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupOptions {
    /// Only entries no lesson references.
    pub unused: bool,
    /// Only entries whose files are all older than this.
    pub older_than: Option<Duration>,
}

/// One cache file selected for deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupFile {
    /// File name relative to the cache directory.
    pub name: String,
    pub bytes: u64,
}

#[derive(Debug, Clone, Default)]
pub struct CleanupPlan {
    /// `.wav` + `.json` files seen directly under the cache directory.
    pub scanned: usize,
    /// Cache entries whose key is referenced by a lesson clip.
    pub referenced: usize,
    pub files: Vec<CleanupFile>,
    pub errors: Vec<String>,
}

impl CleanupPlan {
    pub fn bytes(&self) -> u64 {
        self.files.iter().map(|file| file.bytes).sum()
    }
}

/// One cache entry (same key stem) and the files that actually exist.
#[derive(Debug, Default)]
struct CacheEntry {
    wav: Option<CacheFileMeta>,
    json: Option<CacheFileMeta>,
}

#[derive(Debug)]
struct CacheFileMeta {
    name: String,
    bytes: u64,
    modified: Option<SystemTime>,
}

impl CacheEntry {
    fn files(&self) -> impl Iterator<Item = &CacheFileMeta> {
        self.wav.iter().chain(self.json.iter())
    }
}

/// Plan a cleanup without touching the filesystem.
pub fn plan_cleanup(cache_dir: &Path, lessons_dir: &Path, options: &CleanupOptions) -> CleanupPlan {
    let mut plan = CleanupPlan::default();
    let referenced = referenced_hashes(lessons_dir, &mut plan.errors);

    let mut entries: BTreeMap<String, CacheEntry> = BTreeMap::new();
    match fs::read_dir(cache_dir) {
        Ok(read_dir) => {
            for entry in read_dir {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(err) => {
                        plan.errors.push(format!("读取缓存目录项失败: {err}"));
                        continue;
                    }
                };
                // `file_type()` does not follow symlinks, so links are skipped.
                match entry.file_type() {
                    Ok(file_type) if file_type.is_file() => {}
                    Ok(_) => continue,
                    Err(err) => {
                        plan.errors.push(format!(
                            "读取文件类型失败 {}: {err}",
                            entry.path().display()
                        ));
                        continue;
                    }
                }
                let path = entry.path();
                let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                    continue;
                };
                let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
                    continue;
                };
                let ext = ext.to_ascii_lowercase();
                if ext != "wav" && ext != "json" {
                    continue;
                }
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let metadata = entry.metadata();
                if metadata.is_err() {
                    plan.errors
                        .push(format!("读取文件信息失败: {}", path.display()));
                }
                let bytes = metadata.as_ref().map(|meta| meta.len()).unwrap_or(0);
                let modified = metadata.ok().and_then(|meta| meta.modified().ok());
                plan.scanned += 1;

                let slot = entries.entry(stem.to_string()).or_default();
                let meta = CacheFileMeta {
                    name: name.to_string(),
                    bytes,
                    modified,
                };
                if ext == "wav" {
                    slot.wav = Some(meta);
                } else {
                    slot.json = Some(meta);
                }
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            plan.errors
                .push(format!("读取缓存目录失败 {}: {err}", cache_dir.display()));
        }
    }

    let cutoff = options
        .older_than
        .and_then(|age| SystemTime::now().checked_sub(age));

    for (stem, entry) in &entries {
        let is_referenced = referenced
            .iter()
            .any(|hash8| stem.starts_with(hash8.as_str()));
        if is_referenced {
            plan.referenced += 1;
        }

        let old_enough = match options.older_than {
            None => true,
            Some(_) => match cutoff {
                None => false,
                Some(cutoff) => entry.files().all(|file| match file.modified {
                    Some(modified) => modified <= cutoff,
                    None => {
                        plan.errors.push(format!("无法读取修改时间: {}", file.name));
                        false
                    }
                }),
            },
        };

        let matches_unused = !options.unused || !is_referenced;
        if matches_unused && old_enough {
            plan.files.extend(entry.files().map(|file| CleanupFile {
                name: file.name.clone(),
                bytes: file.bytes,
            }));
        }
    }

    plan.files.sort_by(|a, b| a.name.cmp(&b.name));
    plan
}

/// Delete the planned files. Returns how many were removed and any failures.
pub fn apply_cleanup(cache_dir: &Path, files: &[CleanupFile]) -> (u32, Vec<String>) {
    let mut deleted = 0;
    let mut errors = Vec::new();
    for file in files {
        match fs::remove_file(cache_dir.join(&file.name)) {
            Ok(()) => deleted += 1,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => errors.push(format!("删除失败 {}: {err}", file.name)),
        }
    }
    (deleted, errors)
}

/// 8-char clip hashes referenced by `lessons_dir/*/clips/*.wav`.
fn referenced_hashes(lessons_dir: &Path, errors: &mut Vec<String>) -> HashSet<String> {
    let mut referenced = HashSet::new();
    if !lessons_dir.is_dir() {
        return referenced;
    }
    let lessons = match fs::read_dir(lessons_dir) {
        Ok(lessons) => lessons,
        Err(err) => {
            errors.push(format!("读取课时目录失败 {}: {err}", lessons_dir.display()));
            return referenced;
        }
    };
    for lesson in lessons {
        let lesson = match lesson {
            Ok(lesson) => lesson,
            Err(err) => {
                errors.push(format!("读取课时目录项失败: {err}"));
                continue;
            }
        };
        let clips = lesson.path().join("clips");
        if !clips.is_dir() {
            continue;
        }
        let files = match fs::read_dir(&clips) {
            Ok(files) => files,
            Err(err) => {
                errors.push(format!("读取片段目录失败 {}: {err}", clips.display()));
                continue;
            }
        };
        for file in files {
            let file = match file {
                Ok(file) => file,
                Err(err) => {
                    errors.push(format!("读取片段文件项失败: {err}"));
                    continue;
                }
            };
            if !file.file_type().map(|kind| kind.is_file()).unwrap_or(false) {
                continue;
            }
            let path = file.path();
            if !path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"))
            {
                continue;
            }
            let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
                continue;
            };
            let Some(hash8) = stem.rsplit('-').next() else {
                continue;
            };
            let hash8 = hash8.to_ascii_lowercase();
            if hash8.len() == 8 && hash8.chars().all(|ch| ch.is_ascii_hexdigit()) {
                referenced.insert(hash8);
            }
        }
    }
    referenced
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_is_stable_and_rate_sensitive() {
        let a = Cache::key("qwen", "Cherry", 0.9, "摇晃");
        let b = Cache::key("qwen", "Cherry", 0.9, "摇晃");
        let c = Cache::key("qwen", "Cherry", 0.8, "摇晃");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 32);
    }

    fn key(prefix: &str) -> String {
        format!("{prefix}{}", "0".repeat(32 - prefix.len()))
    }

    fn write_entry(cache: &Path, key: &str, wav_bytes: usize) {
        fs::create_dir_all(cache).unwrap();
        fs::write(cache.join(format!("{key}.wav")), vec![0u8; wav_bytes]).unwrap();
        fs::write(cache.join(format!("{key}.json")), b"{}").unwrap();
    }

    fn age_file(path: &Path, age: Duration) {
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    fn lesson_with_clips(lessons: &Path, name: &str, clips: &[&str]) {
        let dir = lessons.join(name).join("clips");
        fs::create_dir_all(&dir).unwrap();
        for clip in clips {
            fs::write(dir.join(clip), b"RIFF").unwrap();
        }
    }

    #[test]
    fn plans_unused_and_keeps_referenced_entries() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let lessons = temp.path().join("lessons");
        let key_a = key("aaaaaaaa");
        let key_b = key("bbbbbbbb");
        let key_c = key("cccccccc");
        write_entry(&cache, &key_a, 10);
        write_entry(&cache, &key_b, 20);
        write_entry(&cache, &key_c, 30);
        // Only `bbbbbbbb` is still used by a lesson clip.
        lesson_with_clips(&lessons, "第5课", &["01-bbbbbbbb.wav"]);

        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: true,
                older_than: None,
            },
        );
        assert_eq!(plan.scanned, 6);
        assert_eq!(plan.referenced, 1);
        assert_eq!(plan.files.len(), 4);
        assert!(plan.errors.is_empty());
        assert_eq!(plan.bytes(), 10 + 2 + 30 + 2);
        assert!(plan
            .files
            .iter()
            .all(|file| !file.name.starts_with("bbbbbbbb")));

        let (deleted, errors) = apply_cleanup(&cache, &plan.files);
        assert_eq!(deleted, 4);
        assert!(errors.is_empty());
        assert!(cache.join(format!("{key_b}.wav")).exists());
        assert!(cache.join(format!("{key_b}.json")).exists());
        assert!(!cache.join(format!("{key_a}.wav")).exists());
        assert!(!cache.join(format!("{key_a}.json")).exists());
        assert!(!cache.join(format!("{key_c}.wav")).exists());
        assert!(!cache.join(format!("{key_c}.json")).exists());
    }

    #[test]
    fn older_than_uses_file_mtime() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let lessons = temp.path().join("lessons");
        let old_key = key("11111111");
        let fresh_key = key("22222222");
        write_entry(&cache, &old_key, 10);
        write_entry(&cache, &fresh_key, 10);
        age_file(
            &cache.join(format!("{old_key}.wav")),
            Duration::from_secs(40 * 86_400),
        );
        age_file(
            &cache.join(format!("{old_key}.json")),
            Duration::from_secs(40 * 86_400),
        );

        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: false,
                older_than: Some(Duration::from_secs(30 * 86_400)),
            },
        );
        assert_eq!(plan.referenced, 0);
        assert_eq!(plan.files.len(), 2);
        assert!(plan
            .files
            .iter()
            .all(|file| file.name.starts_with("11111111")));

        // A stricter (longer) age keeps everything.
        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: false,
                older_than: Some(Duration::from_secs(60 * 86_400)),
            },
        );
        assert!(plan.files.is_empty());
    }

    #[test]
    fn unused_and_older_than_is_an_intersection() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let lessons = temp.path().join("lessons");
        let old_referenced = key("aaaaaaa1");
        let old_unused = key("bbbbbbb2");
        let fresh_unused = key("ccccccc3");
        for key in [&old_referenced, &old_unused, &fresh_unused] {
            write_entry(&cache, key, 10);
        }
        for key in [&old_referenced, &old_unused] {
            age_file(
                &cache.join(format!("{key}.wav")),
                Duration::from_secs(40 * 86_400),
            );
            age_file(
                &cache.join(format!("{key}.json")),
                Duration::from_secs(40 * 86_400),
            );
        }
        lesson_with_clips(&lessons, "第5课", &["01-aaaaaaa1.wav"]);

        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: true,
                older_than: Some(Duration::from_secs(30 * 86_400)),
            },
        );
        assert_eq!(plan.referenced, 1);
        assert_eq!(plan.files.len(), 2);
        assert!(plan
            .files
            .iter()
            .all(|file| file.name.starts_with("bbbbbbb2")));

        // Unused only would include the fresh entry too.
        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: true,
                older_than: None,
            },
        );
        assert_eq!(plan.files.len(), 4);
    }

    #[test]
    fn reads_hash8_from_last_clip_name_segment() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let lessons = temp.path().join("lessons");
        for prefix in ["ab12cd34", "deadbeef", "cafebabe"] {
            write_entry(&cache, &key(prefix), 10);
        }
        lesson_with_clips(
            &lessons,
            "第5课",
            &[
                "01-ab12cd34.wav",
                "idx-01-deadbeef.wav",
                "intro-CAFEBABE.wav",
                "outro-zzzzzzzz.wav",
                "no-hash.wav",
            ],
        );

        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: true,
                older_than: None,
            },
        );
        assert_eq!(plan.referenced, 3);
        assert!(plan.files.is_empty(), "all cache entries are referenced");
    }

    #[test]
    fn missing_dirs_are_empty_and_orphans_are_cleaned() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let lessons = temp.path().join("lessons");

        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: true,
                older_than: None,
            },
        );
        assert_eq!(plan.scanned, 0);
        assert!(plan.files.is_empty());
        assert!(plan.errors.is_empty());

        // A `.json` without its `.wav` is an orphan and gets cleaned too.
        fs::create_dir_all(&cache).unwrap();
        let orphan = key("99999999");
        fs::write(cache.join(format!("{orphan}.json")), b"{}").unwrap();
        let plan = plan_cleanup(
            &cache,
            &lessons,
            &CleanupOptions {
                unused: true,
                older_than: None,
            },
        );
        assert_eq!(plan.scanned, 1);
        assert_eq!(plan.files.len(), 1);
        assert!(plan.files[0].name.ends_with(".json"));
    }

    #[cfg(unix)]
    #[test]
    fn skips_symlinks_in_cache_dir() {
        let temp = tempfile::tempdir().unwrap();
        let cache = temp.path().join("cache");
        let outside = temp.path().join("outside.wav");
        fs::create_dir_all(&cache).unwrap();
        fs::write(&outside, b"secret").unwrap();
        std::os::unix::fs::symlink(&outside, cache.join("deadbeef.wav")).unwrap();

        let plan = plan_cleanup(
            &cache,
            &temp.path().join("lessons"),
            &CleanupOptions {
                unused: true,
                older_than: None,
            },
        );
        assert_eq!(plan.scanned, 0);
        assert!(plan.files.is_empty());
        assert!(outside.exists());
    }
}
