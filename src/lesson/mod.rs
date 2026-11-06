use std::collections::HashSet;
use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::model::{Cue, Item, Lang, Lesson, LessonConfig};

pub mod import;

/// Optional `lesson.toml` schema. Every field is optional so that a partial
/// file only overrides what the parent wrote.
#[derive(Debug, Default, Deserialize)]
struct LessonToml {
    title: Option<String>,
    rate: Option<f32>,
    gap: Option<DurationField>,
    repeat: Option<u8>,
    repeat_gap: Option<DurationField>,
    lead_in: Option<DurationField>,
    announce_index: Option<bool>,
    intro: Option<String>,
    intro_gap: Option<DurationField>,
    outro: Option<String>,
    voice_zh: Option<String>,
    voice_en: Option<String>,
    voice_mixed: Option<String>,
    cue: Option<String>,
    review_cap: Option<u8>,
}

/// Durations in TOML may be either a bare millisecond count or a string like `6s`.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum DurationField {
    Number(f64),
    Text(String),
}

impl DurationField {
    fn to_ms(&self) -> Result<u32> {
        match self {
            Self::Number(raw) => {
                if !raw.is_finite() || *raw < 0.0 || *raw > u32::MAX as f64 {
                    bail!("时长超出范围: {raw}（毫秒）");
                }
                Ok(raw.round() as u32)
            }
            Self::Text(raw) => parse_duration_ms(raw),
        }
    }
}

impl LessonToml {
    fn apply(self, config: &mut LessonConfig) -> Result<Option<String>> {
        if let Some(rate) = self.rate {
            config.rate = rate;
        }
        if let Some(gap) = self.gap {
            config.gap_ms = gap.to_ms()?;
        }
        if let Some(repeat) = self.repeat {
            config.repeat = repeat;
        }
        if let Some(repeat_gap) = self.repeat_gap {
            config.repeat_gap_ms = repeat_gap.to_ms()?;
        }
        if let Some(lead_in) = self.lead_in {
            config.lead_in_ms = lead_in.to_ms()?;
        }
        if let Some(announce_index) = self.announce_index {
            config.announce_index = announce_index;
        }
        if let Some(intro) = self.intro {
            // `intro = ""` 显式关闭开场白；`intro = "..."` 覆盖默认文案。
            config.intro = Some(intro);
        }
        if let Some(intro_gap) = self.intro_gap {
            config.intro_gap_ms = intro_gap.to_ms()?;
        }
        if let Some(outro) = self.outro {
            // `outro = ""` 显式关闭结束语；`outro = "..."` 覆盖默认文案。
            config.outro = Some(outro);
        }
        if let Some(voice) = self.voice_zh {
            config.voice_zh = Some(voice);
        }
        if let Some(voice) = self.voice_en {
            config.voice_en = Some(voice);
        }
        if let Some(voice) = self.voice_mixed {
            config.voice_mixed = Some(voice);
        }
        if let Some(cue) = self.cue {
            config.cue = parse_cue(&cue)?;
        }
        if let Some(review_cap) = self.review_cap {
            config.review_cap = review_cap;
        }
        Ok(self.title)
    }
}

/// Parse a lesson file. A sibling `lesson.toml` is applied when present.
pub fn parse_file(path: &Path) -> Result<Lesson> {
    let raw =
        fs::read_to_string(path).with_context(|| format!("读取词表失败: {}", path.display()))?;
    parse_str(&raw, path, None)
}

/// Parse raw text (used by tests and stdin).
pub fn parse_str(text: &str, source: &Path, title: Option<&str>) -> Result<Lesson> {
    let sidecar = load_sidecar(source)?;
    parse_lesson(text, source, title, sidecar)
}

/// Parse raw text with an explicit `lesson.toml` payload.
///
/// Unlike [`parse_str`], this never looks for a sibling `lesson.toml`: the
/// caller supplies the TOML (or `None`). `lesson import` uses it to validate
/// everything before writing either file, so a broken payload leaves no
/// half-written lesson on disk.
pub fn parse_with_toml(
    text: &str,
    source: &Path,
    title: Option<&str>,
    toml_text: Option<&str>,
) -> Result<Lesson> {
    let sidecar = match toml_text {
        Some(raw) if !raw.trim().is_empty() => {
            let raw = raw.strip_prefix('\u{feff}').unwrap_or(raw);
            let schema: LessonToml = toml::from_str(raw).context("解析 lesson.toml 失败")?;
            Some(schema)
        }
        _ => None,
    };
    parse_lesson(text, source, title, sidecar)
}

fn parse_lesson(
    text: &str,
    source: &Path,
    title: Option<&str>,
    toml_schema: Option<LessonToml>,
) -> Result<Lesson> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);

    let mut config = LessonConfig::default();
    let mut toml_title = None;
    if let Some(toml) = toml_schema {
        toml_title = toml.apply(&mut config)?;
    }
    validate_config(&config)?;

    let mut items: Vec<Item> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut heading: Option<String> = None;

    for (offset, raw_line) in text.lines().enumerate() {
        let line_no = offset + 1;
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            if heading.is_none() {
                let heading_text = rest.trim_start_matches('#').trim();
                if !heading_text.is_empty() {
                    heading = Some(heading_text.to_string());
                }
            }
            continue;
        }

        let (body, attrs) = split_inline(line);
        let display = strip_marker(&body);
        if display.is_empty() {
            tracing::warn!("第 {line_no} 行：去掉编号后为空，已跳过");
            continue;
        }

        let attrs = parse_attrs(&attrs, line_no)?;
        let text = if attrs.display_suffix.is_empty() {
            display.to_string()
        } else {
            format!("{display}{}", attrs.display_suffix)
        };
        let text = text.trim().to_string();
        if text.is_empty() {
            tracing::warn!("第 {line_no} 行：没有可用文本，已跳过");
            continue;
        }
        let char_count = text.chars().count();
        if char_count > 200 {
            bail!("第 {line_no} 行：单条长度 {char_count} 超过上限 200 字符");
        }

        if !seen.insert(text.clone()) {
            tracing::warn!("第 {line_no} 行：重复词「{text}」，保留首次出现的位置");
            continue;
        }

        let lang = match attrs.lang {
            Some(lang) => lang,
            None => detect_lang(&text),
        };

        let index = items.len() + 1;
        let item = Item {
            index,
            text,
            speak: attrs.speak,
            lang,
            rate: attrs.rate,
            gap_ms: attrs.gap_ms,
            repeat: attrs.repeat,
            note: attrs.note,
        };
        validate_item(&item, line_no)?;
        items.push(item);
    }

    if items.is_empty() {
        bail!(
            "词表为空：{}（去掉注释/编号后没有任何词条）",
            source.display()
        );
    }

    let title = title
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .or(toml_title.filter(|t| !t.trim().is_empty()))
        .or(heading)
        .or_else(|| {
            source
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "未命名".to_string());

    Ok(Lesson {
        title,
        source: source.to_path_buf(),
        config,
        items,
    })
}

/// Detect whether a line is Chinese, English, or mixed.
pub fn detect_lang(text: &str) -> Lang {
    let mut has_han = false;
    let mut has_latin = false;
    for c in text.chars() {
        if is_han(c) {
            has_han = true;
        } else if c.is_ascii_alphabetic() {
            has_latin = true;
        }
    }
    match (has_han, has_latin) {
        (true, true) => Lang::Mixed,
        (true, false) => Lang::Zh,
        (false, true) => Lang::En,
        // 纯数字/符号无法判断，按中文为主的使用场景回退。
        (false, false) => Lang::Zh,
    }
}

/// Parse `6000`, `6000ms`, `6s`, `1.2s` into milliseconds.
pub fn parse_duration_ms(raw: &str) -> Result<u32> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("时长不能为空（示例：6000 / 6000ms / 6s / 1.2s）");
    }
    let lower = raw.to_ascii_lowercase();
    let (number, multiplier) = if let Some(value) = lower.strip_suffix("ms") {
        (value.trim(), 1.0_f64)
    } else if let Some(value) = lower.strip_suffix('s') {
        (value.trim(), 1_000.0_f64)
    } else {
        (lower.as_str(), 1.0_f64)
    };
    if number.is_empty() {
        bail!("无法解析时长: {raw}");
    }
    let value: f64 = number
        .parse()
        .with_context(|| format!("无法解析时长: {raw}"))?;
    if !value.is_finite() || value < 0.0 {
        bail!("时长必须是非负数: {raw}");
    }
    let ms = (value * multiplier).round();
    if ms > u32::MAX as f64 {
        bail!("时长超出范围: {raw}");
    }
    Ok(ms as u32)
}

fn load_sidecar(source: &Path) -> Result<Option<LessonToml>> {
    let Some(parent) = source.parent() else {
        return Ok(None);
    };
    let path = parent.join("lesson.toml");
    if !path.is_file() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("读取 lesson.toml 失败: {}", path.display()))?;
    let raw = raw.strip_prefix('\u{feff}').unwrap_or(&raw);
    let schema: LessonToml = toml::from_str(raw)
        .with_context(|| format!("解析 lesson.toml 失败: {}", path.display()))?;
    Ok(Some(schema))
}

struct InlineAttrs {
    rate: Option<f32>,
    gap_ms: Option<u32>,
    repeat: Option<u8>,
    speak: Option<String>,
    lang: Option<Lang>,
    note: Option<String>,
    /// Text fragments that were not `key=value`, re-attached to the display text.
    display_suffix: String,
}

fn parse_attrs(parts: &[String], line_no: usize) -> Result<InlineAttrs> {
    let mut attrs = InlineAttrs {
        rate: None,
        gap_ms: None,
        repeat: None,
        speak: None,
        lang: None,
        note: None,
        display_suffix: String::new(),
    };

    for part in parts {
        // 一个 `|` 段内可以写多个空白分隔的 `key=value`。
        for token in part.split_whitespace() {
            let Some((key, value)) = token.split_once('=') else {
                // 不是 key=value，按字面正文处理（支持未转义的竖线）。
                attrs.display_suffix.push_str(" | ");
                attrs.display_suffix.push_str(token);
                continue;
            };
            let key = key.trim().to_ascii_lowercase();
            let value = value.trim();
            match key.as_str() {
                "rate" => {
                    let rate: f32 = value
                        .parse()
                        .with_context(|| format!("第 {line_no} 行：rate 不是数字: {value}"))?;
                    if !(0.5..=2.0).contains(&rate) {
                        bail!("第 {line_no} 行：rate 必须在 0.5–2.0 之间，当前 {rate}");
                    }
                    attrs.rate = Some(rate);
                }
                "gap" => {
                    attrs.gap_ms = Some(parse_duration_ms(value)?);
                }
                "repeat" => {
                    let repeat: u8 = value
                        .parse()
                        .with_context(|| format!("第 {line_no} 行：repeat 不是整数: {value}"))?;
                    if !(1..=10).contains(&repeat) {
                        bail!("第 {line_no} 行：repeat 必须在 1–10 之间，当前 {repeat}");
                    }
                    attrs.repeat = Some(repeat);
                }
                "speak" => {
                    if !value.is_empty() {
                        attrs.speak = Some(value.to_string());
                    }
                }
                "lang" => {
                    attrs.lang = Some(parse_lang(value)?);
                }
                "note" => {
                    if !value.is_empty() {
                        attrs.note = Some(value.to_string());
                    }
                }
                other => {
                    tracing::warn!("第 {line_no} 行：未知属性 `{other}`，已忽略");
                }
            }
        }
    }

    Ok(attrs)
}

fn parse_lang(raw: &str) -> Result<Lang> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "zh" | "cn" | "chinese" | "中文" => Ok(Lang::Zh),
        "en" | "english" | "英文" => Ok(Lang::En),
        "mixed" | "mix" | "中英" => Ok(Lang::Mixed),
        other => bail!("未知语言: {other}（可选：zh / en / mixed）"),
    }
}

fn parse_cue(raw: &str) -> Result<Cue> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "beep" => Ok(Cue::Beep),
        "none" => Ok(Cue::None),
        other => bail!("未知提示音: {other}（可选：beep / none）"),
    }
}

fn validate_config(config: &LessonConfig) -> Result<()> {
    if !(0.5..=2.0).contains(&config.rate) {
        bail!(
            "lesson.toml: rate 必须在 0.5–2.0 之间，当前 {}",
            config.rate
        );
    }
    if config.gap_ms > 120_000 {
        bail!("lesson.toml: gap 不能超过 120s，当前 {}ms", config.gap_ms);
    }
    if !(1..=10).contains(&config.repeat) {
        bail!(
            "lesson.toml: repeat 必须在 1–10 之间，当前 {}",
            config.repeat
        );
    }
    if config.review_cap > 15 {
        bail!(
            "lesson.toml: review_cap 必须在 0–15 之间，当前 {}",
            config.review_cap
        );
    }
    Ok(())
}

fn validate_item(item: &Item, line_no: usize) -> Result<()> {
    if let Some(rate) = item.rate {
        if !(0.5..=2.0).contains(&rate) {
            bail!("第 {line_no} 行：rate 必须在 0.5–2.0 之间，当前 {rate}");
        }
    }
    if let Some(gap) = item.gap_ms {
        if gap > 120_000 {
            bail!("第 {line_no} 行：gap 不能超过 120s，当前 {gap}ms");
        }
    }
    if let Some(repeat) = item.repeat {
        if !(1..=10).contains(&repeat) {
            bail!("第 {line_no} 行：repeat 必须在 1–10 之间，当前 {repeat}");
        }
    }
    Ok(())
}

/// Split a line on unescaped `|`. The first part is the display text, the rest
/// are inline attributes. `\|` becomes a literal pipe.
fn split_inline(line: &str) -> (String, Vec<String>) {
    let mut parts: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' && chars.peek() == Some(&'|') {
            current.push('|');
            chars.next();
        } else if c == '|' {
            parts.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(c);
        }
    }
    parts.push(current.trim().to_string());
    let mut iter = parts.into_iter();
    let first = iter.next().unwrap_or_default();
    (first, iter.collect())
}

/// Remove leading list markers such as `- `, `* `, `1. `, `1、`, `1)`, `（1）`.
fn strip_marker(line: &str) -> &str {
    let mut rest = line.trim();
    for _ in 0..4 {
        let trimmed = rest.trim_start();
        let mut advanced = false;

        if let Some(after) = trimmed
            .strip_prefix('-')
            .or_else(|| trimmed.strip_prefix('*'))
        {
            if after.starts_with(char::is_whitespace) {
                rest = after;
                advanced = true;
            }
        }

        if !advanced {
            if let Some(after) = strip_parenthesized_number(trimmed) {
                rest = after;
                advanced = true;
            }
        }

        if !advanced {
            if let Some(after) = strip_plain_number(trimmed) {
                rest = after;
                advanced = true;
            }
        }

        if !advanced {
            break;
        }
    }
    rest.trim()
}

fn strip_parenthesized_number(s: &str) -> Option<&str> {
    let mut chars = s.char_indices();
    let (_, first) = chars.next()?;
    let close = match first {
        '（' => '）',
        '(' => ')',
        _ => return None,
    };
    let body_start = first.len_utf8();
    let close_offset = s[body_start..].find(close)?;
    let body = &s[body_start..body_start + close_offset];
    let body = body.trim();
    if body.is_empty() || !body.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let after = &s[body_start + close_offset + close.len_utf8()..];
    // 全角括号习惯不带空格（`（4）旅行`）；半角括号要求空格或结束，避免误伤正文。
    if after.is_empty() || after.starts_with(char::is_whitespace) || close == '）' {
        Some(after)
    } else {
        None
    }
}

fn strip_plain_number(s: &str) -> Option<&str> {
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    let after_digits = &s[digits..];
    let sep = after_digits.chars().next()?;
    if !matches!(sep, '.' | '、' | ')' | '）') {
        return None;
    }
    let after = &after_digits[sep.len_utf8()..];
    let spaced = after.is_empty() || after.starts_with(char::is_whitespace);
    // 中文编号习惯不带空格（`1、摇晃`），ASCII 句点则要求空格或有分隔符，避免误伤小数。
    if spaced || matches!(sep, '、' | ')' | '）') {
        Some(after)
    } else {
        None
    }
}

fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF      // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF    // CJK Unified Ideographs
        | 0xF900..=0xFAFF    // CJK Compatibility Ideographs
        | 0x20000..=0x2FA1F  // Extensions B–F
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("dictation-lesson-{name}-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn parses_markers_attrs_and_comments() {
        let text = "\u{feff}# 第5课 词语听写\n\n1. 摇晃 | rate=0.8 gap=8s\n- 粗壮\n* 毕竟\n（4）旅行 | repeat=1\n5) apple\n6、library\n";
        let lesson = parse_str(text, &tmp_path("lesson.txt"), None).unwrap();

        assert_eq!(lesson.title, "第5课 词语听写");
        assert_eq!(lesson.items.len(), 6);
        assert_eq!(lesson.items[0].text, "摇晃");
        assert_eq!(lesson.items[0].rate, Some(0.8));
        assert_eq!(lesson.items[0].gap_ms, Some(8_000));
        assert_eq!(lesson.items[3].text, "旅行");
        assert_eq!(lesson.items[3].repeat, Some(1));
        assert_eq!(lesson.items[4].text, "apple");
        assert_eq!(lesson.items[5].text, "library");
        assert_eq!(lesson.items[0].index, 1);
        assert_eq!(lesson.items[5].index, 6);
    }

    #[test]
    fn escaped_pipe_stays_in_text() {
        let lesson = parse_str("A \\| B\n", &tmp_path("pipe.txt"), None).unwrap();
        assert_eq!(lesson.items[0].text, "A | B");
        assert_eq!(lesson.items[0].lang, Lang::En);
    }

    #[test]
    fn speak_and_note_are_kept_out_of_display() {
        let lesson = parse_str(
            "长大 | speak=长大的长 note=形容词 | lang=zh\n",
            &tmp_path("speak.txt"),
            None,
        )
        .unwrap();
        let item = &lesson.items[0];
        assert_eq!(item.text, "长大");
        assert_eq!(item.speak.as_deref(), Some("长大的长"));
        assert_eq!(item.note.as_deref(), Some("形容词"));
        assert_eq!(item.lang, Lang::Zh);
    }

    #[test]
    fn parses_explicit_toml_without_sidecar() {
        let lesson = parse_with_toml(
            "摇晃\n",
            &tmp_path("explicit/lesson.txt"),
            Some("第5课"),
            Some("rate = 0.8\ngap = \"2s\"\n"),
        )
        .unwrap();
        assert_eq!(lesson.title, "第5课");
        assert_eq!(lesson.config.rate, 0.8);
        assert_eq!(lesson.config.gap_ms, 2_000);
    }

    #[test]
    fn parse_with_toml_rejects_invalid_toml() {
        let source = tmp_path("bad/lesson.txt");
        assert!(parse_with_toml("摇晃\n", &source, Some("t"), Some("rate = 9.9\n")).is_err());
        assert!(parse_with_toml("摇晃\n", &source, Some("t"), Some("rate = oops\n")).is_err());
        // An empty payload behaves like "no lesson.toml".
        let lesson = parse_with_toml("摇晃\n", &source, Some("t"), Some("   \n")).unwrap();
        assert_eq!(lesson.items.len(), 1);
    }

    #[test]
    fn duplicate_keeps_first_occurrence() {
        let lesson = parse_str("摇晃\n摇晃\n粗壮\n", &tmp_path("dup.txt"), None).unwrap();
        assert_eq!(lesson.items.len(), 2);
        assert_eq!(lesson.items[0].text, "摇晃");
        assert_eq!(lesson.items[1].text, "粗壮");
    }

    #[test]
    fn rejects_long_lines() {
        let long = "a".repeat(201);
        assert!(parse_str(&long, &tmp_path("long.txt"), None).is_err());
    }

    #[test]
    fn rejects_out_of_range_rate() {
        assert!(parse_str("摇晃 | rate=2.5\n", &tmp_path("rate.txt"), None).is_err());
        assert!(parse_str("摇晃 | rate=0.1\n", &tmp_path("rate.txt"), None).is_err());
    }

    #[test]
    fn detects_languages() {
        assert_eq!(detect_lang("摇晃"), Lang::Zh);
        assert_eq!(detect_lang("apple"), Lang::En);
        assert_eq!(detect_lang("apple 苹果"), Lang::Mixed);
        assert_eq!(detect_lang("123"), Lang::Zh);
        assert_eq!(detect_lang("hello 123"), Lang::En);
    }

    #[test]
    fn intro_defaults_follow_lesson_language() {
        use crate::model::{DEFAULT_INTRO_EN, DEFAULT_INTRO_ZH};

        let zh = parse_str("摇晃\n", &tmp_path("intro-zh.txt"), None).unwrap();
        assert_eq!(zh.intro_lang(), Lang::Zh);
        assert_eq!(zh.intro_text(), Some(DEFAULT_INTRO_ZH));

        let en = parse_str("apple\nlibrary\n", &tmp_path("intro-en.txt"), None).unwrap();
        assert_eq!(en.intro_lang(), Lang::En);
        assert_eq!(en.intro_text(), Some(DEFAULT_INTRO_EN));

        let mixed = parse_str("apple 苹果\n", &tmp_path("intro-mixed.txt"), None).unwrap();
        assert_eq!(mixed.intro_lang(), Lang::Zh);
        assert_eq!(mixed.intro_text(), Some(DEFAULT_INTRO_ZH));

        let mut disabled = zh;
        disabled.config.intro = Some(String::new());
        assert_eq!(disabled.intro_text(), None);
        disabled.config.intro = Some("  自定义开场白  ".to_string());
        assert_eq!(disabled.intro_text(), Some("  自定义开场白  "));
    }

    #[test]
    fn duration_formats() {
        assert_eq!(parse_duration_ms("6000").unwrap(), 6_000);
        assert_eq!(parse_duration_ms("6000ms").unwrap(), 6_000);
        assert_eq!(parse_duration_ms("6s").unwrap(), 6_000);
        assert_eq!(parse_duration_ms("1.2s").unwrap(), 1_200);
        assert_eq!(parse_duration_ms("1.2345s").unwrap(), 1_235);
        assert!(parse_duration_ms("abc").is_err());
        assert!(parse_duration_ms("").is_err());
    }

    #[test]
    fn sidecar_toml_overrides_defaults() {
        let dir =
            std::env::temp_dir().join(format!("dictation-lesson-toml-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("lesson.toml"),
            "title = \"第9课\"\nrate = 1.1\ngap = \"3s\"\nrepeat = 3\nrepeat_gap = \"800ms\"\nlead_in = 1500\nannounce_index = true\nintro = \"请准备听写。\"\nintro_gap = \"2.5s\"\noutro = \"听写完毕。\"\nvoice_zh = \"Cherry\"\nreview_cap = 2\n",
        )
        .unwrap();
        let path = dir.join("lesson.txt");
        fs::write(&path, "# 被 toml 覆盖的标题\n摇晃\n").unwrap();

        let lesson = parse_file(&path).unwrap();
        assert_eq!(lesson.title, "第9课");
        assert_eq!(lesson.config.rate, 1.1);
        assert_eq!(lesson.config.gap_ms, 3_000);
        assert_eq!(lesson.config.repeat, 3);
        assert_eq!(lesson.config.repeat_gap_ms, 800);
        assert_eq!(lesson.config.lead_in_ms, 1_500);
        assert!(lesson.config.announce_index);
        assert_eq!(lesson.config.intro.as_deref(), Some("请准备听写。"));
        assert_eq!(lesson.config.intro_gap_ms, 2_500);
        assert_eq!(lesson.config.outro.as_deref(), Some("听写完毕。"));
        assert_eq!(lesson.config.voice_zh.as_deref(), Some("Cherry"));
        assert_eq!(lesson.config.review_cap, 2);
        assert_eq!(lesson.intro_text(), Some("请准备听写。"));
        assert_eq!(lesson.intro_lang(), Lang::Zh);
        assert_eq!(lesson.outro_text(), Some("听写完毕。"));
        assert_eq!(lesson.outro_lang(), Lang::Zh);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn outro_defaults_follow_lesson_language_and_overrides() {
        use crate::model::{DEFAULT_OUTRO_EN, DEFAULT_OUTRO_ZH};

        let mut zh = parse_str("摇晃\n", &tmp_path("outro-zh.txt"), None).unwrap();
        assert_eq!(zh.outro_lang(), Lang::Zh);
        assert_eq!(zh.outro_text(), Some(DEFAULT_OUTRO_ZH));

        let en = parse_str("apple\nlibrary\n", &tmp_path("outro-en.txt"), None).unwrap();
        assert_eq!(en.outro_lang(), Lang::En);
        assert_eq!(en.outro_text(), Some(DEFAULT_OUTRO_EN));

        let mixed = parse_str("apple 苹果\n", &tmp_path("outro-mixed.txt"), None).unwrap();
        assert_eq!(mixed.outro_lang(), Lang::Zh);
        assert_eq!(mixed.outro_text(), Some(DEFAULT_OUTRO_ZH));

        zh.config.outro = Some("自定义结束语。".to_string());
        assert_eq!(zh.outro_text(), Some("自定义结束语。"));

        // 只有结束语、没有开场白。
        zh.config.intro = Some(String::new());
        assert_eq!(zh.intro_text(), None);
        assert_eq!(zh.outro_text(), Some("自定义结束语。"));

        zh.config.outro = Some(String::new());
        assert_eq!(zh.outro_text(), None);
    }

    #[test]
    fn sidecar_empty_intro_disables_it() {
        let dir =
            std::env::temp_dir().join(format!("dictation-lesson-intro-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("lesson.toml"), "intro = \"\"\noutro = \"\"\n").unwrap();
        let path = dir.join("lesson.txt");
        fs::write(&path, "摇晃\n").unwrap();

        let lesson = parse_file(&path).unwrap();
        assert_eq!(lesson.config.intro.as_deref(), Some(""));
        assert_eq!(lesson.intro_text(), None);
        assert_eq!(lesson.config.outro.as_deref(), Some(""));
        assert_eq!(lesson.outro_text(), None);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn title_falls_back_to_file_stem() {
        let lesson = parse_str("摇晃\n", &PathBuf::from("/tmp/no-such/第7课.txt"), None).unwrap();
        assert_eq!(lesson.title, "第7课");
    }

    #[test]
    fn explicit_title_wins() {
        let text = "# 标题里的名字\n摇晃\n";
        let lesson = parse_str(text, &tmp_path("t.txt"), Some("手动标题")).unwrap();
        assert_eq!(lesson.title, "手动标题");
    }

    #[test]
    fn fixture_parses() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/lesson-20.txt");
        let lesson = parse_file(&path).unwrap();
        assert_eq!(lesson.items.len(), 20);
        assert_eq!(lesson.title, "第5课 词语听写");
        assert_eq!(lesson.items[3].gap_ms, Some(8_000));
        assert_eq!(lesson.items[18].lang, Lang::En);
    }
}
