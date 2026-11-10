use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Language of a dictation item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Zh,
    En,
    Mixed,
}

/// Optional cue sound before an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cue {
    Beep,
    None,
}

/// TTS backend selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    Doubao,
    Qwen,
    Mock,
}

impl std::str::FromStr for ProviderKind {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "doubao" | "volcano" | "volcengine" => Ok(Self::Doubao),
            "qwen" | "bailian" | "dashscope" => Ok(Self::Qwen),
            "mock" | "test" => Ok(Self::Mock),
            other => anyhow::bail!("未知 provider: {other}（可选：doubao / qwen / mock）"),
        }
    }
}

pub const DEFAULT_INTRO_ZH: &str = "请准备好纸和笔，听写马上开始。";
pub const DEFAULT_INTRO_EN: &str =
    "Please get your paper and pencil ready. The dictation will begin shortly.";
pub const DEFAULT_OUTRO_ZH: &str = "听写结束，请检查一遍。";
pub const DEFAULT_OUTRO_EN: &str = "That's the end of the dictation. Please check your answers.";

/// Curriculum subject. Distinct from [`Lang`]: an English lesson may still
/// contain Chinese glosses, and a Chinese lesson may contain Latin acronyms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Subject {
    #[default]
    Chinese,
    English,
}

impl Subject {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Chinese => "chinese",
            Self::English => "english",
        }
    }

    /// Only the canonical API values are accepted; `lang` already uses
    /// `zh`/`en`, so accepting those here would be ambiguous.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim() {
            "chinese" => Some(Self::Chinese),
            "english" => Some(Self::English),
            _ => None,
        }
    }
}

/// Lesson-level defaults. Every item may override `rate`, `gap_ms`, `repeat`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LessonConfig {
    /// Curriculum subject; words are keyed by `(subject, text)`.
    pub subject: Subject,
    /// 1–6, optional metadata used for filtering.
    pub grade: Option<u8>,
    /// 1 (上) or 2 (下), optional metadata.
    pub term: Option<u8>,
    /// Speech rate, 0.5..=2.0. 1.0 is normal.
    pub rate: f32,
    /// Silence after each item, in milliseconds.
    pub gap_ms: u32,
    /// How many times each item is read.
    pub repeat: u8,
    /// Silence between repeated readings of the same item.
    pub repeat_gap_ms: u32,
    /// Silence before the first item.
    pub lead_in_ms: u32,
    /// Read "第 N 个" before the word. Default off: the answer sheet carries
    /// the numbers, and per-item announcements slow a session down.
    pub announce_index: bool,
    /// Optional opening sentence. `None` = auto by lesson language;
    /// `Some("")` = disable the intro.
    pub intro: Option<String>,
    /// Silence between the opening sentence and the first item.
    pub intro_gap_ms: u32,
    /// Optional closing sentence. `None` = auto by lesson language;
    /// `Some("")` = disable the outro.
    pub outro: Option<String>,
    pub voice_zh: Option<String>,
    pub voice_en: Option<String>,
    pub voice_mixed: Option<String>,
    pub cue: Cue,
    /// Maximum number of due review words mixed into a session.
    pub review_cap: u8,
}

impl Default for LessonConfig {
    fn default() -> Self {
        Self {
            subject: Subject::Chinese,
            grade: None,
            term: None,
            rate: 0.9,
            gap_ms: 6_000,
            repeat: 2,
            repeat_gap_ms: 1_200,
            lead_in_ms: 3_000,
            announce_index: false,
            intro: None,
            intro_gap_ms: 1_500,
            outro: None,
            voice_zh: None,
            voice_en: None,
            voice_mixed: None,
            cue: Cue::None,
            review_cap: 8,
        }
    }
}

/// A single dictation item: a word, phrase, or sentence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    /// 1-based position in the lesson.
    pub index: usize,
    /// Text shown on the answer sheet. Never spoken unless `speak` is None.
    pub text: String,
    /// Text sent to TTS, when it differs from `text` (polyphonic characters, etc.).
    pub speak: Option<String>,
    pub lang: Lang,
    /// Per-item rate override.
    pub rate: Option<f32>,
    /// Per-item silence-after override.
    pub gap_ms: Option<u32>,
    /// Per-item repeat override.
    pub repeat: Option<u8>,
    /// Answer-sheet note (not spoken in P0).
    pub note: Option<String>,
}

impl Item {
    /// Text that should actually be synthesized.
    pub fn tts_text(&self) -> &str {
        self.speak.as_deref().unwrap_or(&self.text)
    }
}

/// A parsed lesson.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Lesson {
    pub title: String,
    pub source: PathBuf,
    pub config: LessonConfig,
    pub items: Vec<Item>,
}

impl Lesson {
    /// Intro language: English only when every item is English; Chinese
    /// otherwise (mixed lessons get the Chinese intro).
    pub fn intro_lang(&self) -> Lang {
        if !self.items.is_empty() && self.items.iter().all(|item| item.lang == Lang::En) {
            Lang::En
        } else {
            Lang::Zh
        }
    }

    /// Resolve the opening sentence. `None` disables the intro.
    pub fn intro_text(&self) -> Option<&str> {
        match self.config.intro.as_deref() {
            Some(text) if text.trim().is_empty() => None,
            Some(text) => Some(text),
            None => Some(match self.intro_lang() {
                Lang::En => DEFAULT_INTRO_EN,
                Lang::Zh | Lang::Mixed => DEFAULT_INTRO_ZH,
            }),
        }
    }

    pub fn outro_lang(&self) -> Lang {
        self.intro_lang()
    }

    /// Resolve the closing sentence. `None` disables the outro.
    pub fn outro_text(&self) -> Option<&str> {
        match self.config.outro.as_deref() {
            Some(text) if text.trim().is_empty() => None,
            Some(text) => Some(text),
            None => Some(match self.outro_lang() {
                Lang::En => DEFAULT_OUTRO_EN,
                Lang::Zh | Lang::Mixed => DEFAULT_OUTRO_ZH,
            }),
        }
    }

    pub fn rate_for(&self, item: &Item) -> f32 {
        item.rate.unwrap_or(self.config.rate)
    }

    pub fn gap_for(&self, item: &Item) -> u32 {
        item.gap_ms.unwrap_or(self.config.gap_ms)
    }

    pub fn repeat_for(&self, item: &Item) -> u8 {
        item.repeat.unwrap_or(self.config.repeat).max(1)
    }
}
