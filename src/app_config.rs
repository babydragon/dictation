//! Host-side configuration (`/etc/dictation/config.toml`) and path helpers.
//!
//! The agent may run in a sandbox that cannot touch host paths: it only emits
//! text through stdin, and this CLI maps it into the writable directories
//! declared here. Resolution precedence:
//!
//! `--config <path>` > `DICTATION_CONFIG` > `/etc/dictation/config.toml`
//! (only when it exists) > built-in defaults.

use std::env;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Deserialize;

pub const DEFAULT_CONFIG_PATH: &str = "/etc/dictation/config.toml";
pub const DEFAULT_DB_FILE: &str = "dictation.db";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppConfig {
    /// Where lesson directories live (`<base>/lessons` by default).
    pub lessons_dir: PathBuf,
    /// Data directory; the SQLite file lives at `<data_dir>/dictation.db`.
    pub data_dir: PathBuf,
    /// Clip cache directory.
    pub cache_dir: PathBuf,
    /// Read-only static frontend directory.
    pub web_dir: PathBuf,
    pub host: String,
    pub port: u16,
    source: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct FileConfig {
    #[serde(default)]
    paths: PathsSection,
    #[serde(default)]
    server: ServerSection,
}

#[derive(Debug, Default, Deserialize)]
struct PathsSection {
    base: Option<PathBuf>,
    lessons: Option<PathBuf>,
    data: Option<PathBuf>,
    cache: Option<PathBuf>,
    web: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct ServerSection {
    host: Option<String>,
    port: Option<u16>,
}

impl AppConfig {
    /// Load configuration. `explicit` comes from `--config`.
    pub fn load(explicit: Option<PathBuf>) -> Result<Self> {
        let cwd = env::current_dir().context("读取当前目录失败")?;
        let path = match explicit {
            Some(path) => {
                if !path.is_file() {
                    bail!("配置文件不存在: {}", path.display());
                }
                Some(path)
            }
            None => match env::var("DICTATION_CONFIG") {
                Ok(raw) if !raw.trim().is_empty() => {
                    let path = PathBuf::from(raw.trim());
                    if !path.is_file() {
                        bail!("DICTATION_CONFIG 指向的文件不存在: {}", path.display());
                    }
                    Some(path)
                }
                _ => {
                    let default = PathBuf::from(DEFAULT_CONFIG_PATH);
                    if default.is_file() {
                        Some(default)
                    } else {
                        None
                    }
                }
            },
        };

        let file = match &path {
            Some(path) => {
                let raw = std::fs::read_to_string(path)
                    .with_context(|| format!("读取配置文件失败: {}", path.display()))?;
                let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
                toml::from_str::<FileConfig>(raw)
                    .with_context(|| format!("解析配置文件失败: {}", path.display()))?
            }
            None => FileConfig::default(),
        };

        Ok(Self::from_file(file, cwd, path))
    }

    fn from_file(file: FileConfig, cwd: PathBuf, source: Option<PathBuf>) -> Self {
        let base = match file.paths.base {
            Some(base) if base.is_absolute() => base,
            Some(base) => cwd.join(base),
            None => cwd.clone(),
        };
        let resolve = |value: Option<PathBuf>, fallback: &str| -> PathBuf {
            match value {
                Some(path) if path.is_absolute() => path,
                Some(path) => base.join(path),
                None => base.join(fallback),
            }
        };

        let lessons_dir = resolve(file.paths.lessons, "lessons");
        let data_dir = resolve(file.paths.data, "data");
        let cache_dir = resolve(file.paths.cache, "cache");
        let web_dir = match file.paths.web {
            Some(path) if path.is_absolute() => path,
            Some(path) => base.join(path),
            // The frontend normally ships next to the binary, not under data.
            None => cwd.join("web"),
        };

        let host = file
            .server
            .host
            .map(|host| host.trim().to_string())
            .filter(|host| !host.is_empty())
            .unwrap_or_else(|| "0.0.0.0".to_string());
        let port = file.server.port.unwrap_or(8080);

        Self {
            lessons_dir,
            data_dir,
            cache_dir,
            web_dir,
            host,
            port,
            source,
        }
    }

    /// SQLite file path: `<data_dir>/dictation.db`.
    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join(DEFAULT_DB_FILE)
    }

    /// Config file that was actually read, or `None` for built-in defaults.
    pub fn source(&self) -> Option<&Path> {
        self.source.as_deref()
    }

    /// Human-readable source label for `config show`.
    pub fn source_label(&self) -> String {
        match self.source() {
            Some(path) => path.display().to_string(),
            None => "内置默认（未找到 /etc/dictation/config.toml）".to_string(),
        }
    }
}

/// Turn a lesson title into a safe single-component directory name.
///
/// Path separators and control characters become `_`; `.`/`..`/empty fall back
/// to `lesson-<uuid8>` so the result can never escape `lessons_dir`.
pub fn safe_dir_name(raw: &str) -> String {
    let mut name = String::new();
    for ch in raw.trim().chars() {
        if ch.is_control() || matches!(ch, '/' | '\\') {
            name.push('_');
        } else {
            name.push(ch);
        }
    }
    let name = name.trim().trim_matches('.').trim().to_string();
    if name.is_empty() || name == "." || name == ".." {
        let short = uuid::Uuid::new_v4().simple().to_string();
        return format!("lesson-{}", &short[..8]);
    }
    name
}

/// Reject anything that is not a plain, single path component.
pub fn validate_dir_name(dir: &str) -> Result<()> {
    let dir = dir.trim();
    if dir.is_empty() {
        bail!("课时目录名不能为空");
    }
    if dir == "." || dir == ".." {
        bail!("课时目录名不能是 . 或 ..");
    }
    let path = Path::new(dir);
    if path.is_absolute() || path.components().count() != 1 {
        bail!("课时目录名不能包含路径分隔符或 ..：{dir}");
    }
    if dir
        .chars()
        .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
    {
        bail!("课时目录名包含非法字符：{dir}");
    }
    Ok(())
}

/// Join a validated directory name under `root` and prove the result stays
/// inside `root`.
pub fn resolve_lesson_dir(root: &Path, dir: &str) -> Result<PathBuf> {
    validate_dir_name(dir)?;
    let path = root.join(dir.trim());
    if !path.starts_with(root) {
        bail!("课时目录必须位于 {} 下", root.display());
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_config(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("config.toml");
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn relative_paths_resolve_under_absolute_base() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_config(
            temp.path(),
            &format!(
                r#"
[paths]
base = "{}"
lessons = "l"
data = "d"
cache = "c"
web = "w"
[server]
host = "127.0.0.1"
port = 9001
"#,
                temp.path().display()
            ),
        );
        let app = AppConfig::load(Some(path.clone())).unwrap();
        assert_eq!(app.lessons_dir, temp.path().join("l"));
        assert_eq!(app.data_dir, temp.path().join("d"));
        assert_eq!(app.db_path(), temp.path().join("d/dictation.db"));
        assert_eq!(app.cache_dir, temp.path().join("c"));
        assert_eq!(app.web_dir, temp.path().join("w"));
        assert_eq!(app.host, "127.0.0.1");
        assert_eq!(app.port, 9001);
        assert_eq!(app.source(), Some(path.as_path()));
    }

    #[test]
    fn relative_base_is_resolved_against_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_config(
            temp.path(),
            r#"
[paths]
base = "data-root"
lessons = "l"
"#,
        );
        let app = AppConfig::load(Some(path)).unwrap();
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(app.lessons_dir, cwd.join("data-root/l"));
    }

    #[test]
    fn absolute_paths_are_kept_as_is() {
        let temp = tempfile::tempdir().unwrap();
        let absolute = temp.path().join("elsewhere");
        let path = write_config(
            temp.path(),
            &format!(
                "[paths]\nbase = \"{}\"\nlessons = \"{}\"\ndata = \"{}\"\n",
                temp.path().join("base").display(),
                absolute.display(),
                absolute.display()
            ),
        );
        let app = AppConfig::load(Some(path)).unwrap();
        assert_eq!(app.lessons_dir, absolute);
        assert_eq!(app.data_dir, absolute);
        assert_eq!(app.cache_dir, temp.path().join("base/cache"));
    }

    #[test]
    fn defaults_when_config_has_only_a_base() {
        let temp = tempfile::tempdir().unwrap();
        let path = write_config(
            temp.path(),
            &format!("[paths]\nbase = \"{}\"\n", temp.path().display()),
        );
        let app = AppConfig::load(Some(path)).unwrap();
        assert_eq!(app.lessons_dir, temp.path().join("lessons"));
        assert_eq!(app.data_dir, temp.path().join("data"));
        assert_eq!(app.cache_dir, temp.path().join("cache"));
        assert_eq!(app.host, "0.0.0.0");
        assert_eq!(app.port, 8080);
    }

    #[test]
    fn explicit_missing_config_errors() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("nope.toml");
        assert!(AppConfig::load(Some(missing)).is_err());
    }

    #[test]
    fn safe_dir_names_never_escape() {
        assert_eq!(safe_dir_name("第5课"), "第5课");
        assert_eq!(safe_dir_name("a/b\\c"), "a_b_c");
        assert!(safe_dir_name("  ..  ").starts_with("lesson-"));
        assert!(safe_dir_name("   ").starts_with("lesson-"));
        assert!(safe_dir_name("..").starts_with("lesson-"));
        assert!(safe_dir_name("a\u{7}b").contains('_'));
    }

    #[test]
    fn rejects_path_like_dir_names() {
        assert!(validate_dir_name("../evil").is_err());
        assert!(validate_dir_name("a/b").is_err());
        assert!(validate_dir_name("/abs").is_err());
        assert!(validate_dir_name("..").is_err());
        assert!(validate_dir_name("").is_err());
        assert!(validate_dir_name("第5课").is_ok());
        assert!(validate_dir_name("lesson 5").is_ok());
    }

    #[test]
    fn resolve_lesson_dir_stays_under_root() {
        let root = Path::new("/srv/dictation/lessons");
        assert_eq!(
            resolve_lesson_dir(root, "第5课").unwrap(),
            root.join("第5课")
        );
        assert!(resolve_lesson_dir(root, "../etc").is_err());
    }
}
