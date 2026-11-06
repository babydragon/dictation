use std::collections::HashMap;

use crate::model::{Lang, Lesson};

/// Assumed duration of the "第 N 个" announcement.
pub const ANNOUNCE_MS: u32 = 700;

/// Chinese speaking rate used by the estimator: milliseconds per character.
const ZH_MS_PER_CHAR: f64 = 260.0;
/// English speaking rate used by the estimator: milliseconds per character.
const EN_MS_PER_CHAR: f64 = 90.0;
/// Even a one-character item needs room to breathe.
const MIN_CLIP_MS: f64 = 400.0;

#[derive(Debug, Clone)]
pub struct PlannedItem {
    pub index: usize,
    pub text: String,
    pub lang: Lang,
    pub announce_ms: u32,
    pub clip_ms: u32,
    pub repeat: u8,
    pub repeat_gap_ms: u32,
    pub gap_ms: u32,
    pub total_ms: u32,
}

#[derive(Debug, Clone)]
pub struct Plan {
    pub lead_in_ms: u32,
    pub intro_ms: u32,
    pub intro_gap_ms: u32,
    pub intro_text: Option<String>,
    pub outro_ms: u32,
    pub outro_text: Option<String>,
    pub items: Vec<PlannedItem>,
    pub total_ms: u32,
}

/// Estimate without calling TTS. Uses character-count heuristics.
pub fn estimate(lesson: &Lesson) -> Plan {
    let (intro_text, intro_ms, intro_gap_ms) = match lesson.intro_text() {
        Some(text) => {
            let text = text.to_string();
            let ms = estimate_clip_ms(&text, lesson.intro_lang(), lesson.config.rate);
            (Some(text), ms, lesson.config.intro_gap_ms)
        }
        None => (None, 0, 0),
    };

    let (outro_text, outro_ms) = match lesson.outro_text() {
        Some(text) => {
            let text = text.to_string();
            let ms = estimate_clip_ms(&text, lesson.outro_lang(), lesson.config.rate);
            (Some(text), ms)
        }
        None => (None, 0),
    };

    let items: Vec<PlannedItem> = lesson
        .items
        .iter()
        .map(|item| {
            let rate = lesson.rate_for(item);
            let clip_ms = estimate_clip_ms(item.tts_text(), item.lang, rate);
            let announce_ms = if lesson.config.announce_index {
                ANNOUNCE_MS
            } else {
                0
            };
            planned_item(
                item.index,
                &item.text,
                item.lang,
                announce_ms,
                clip_ms,
                lesson.repeat_for(item),
                lesson.config.repeat_gap_ms,
                lesson.gap_for(item),
            )
        })
        .collect();
    finish(
        lesson.config.lead_in_ms,
        intro_ms,
        intro_gap_ms,
        intro_text,
        outro_ms,
        outro_text,
        items,
    )
}

/// Build the exact plan from measured clip durations.
pub fn build(
    lesson: &Lesson,
    clip_ms: &HashMap<usize, u32>,
    announce_ms: &HashMap<usize, u32>,
    intro_ms: u32,
    outro_ms: u32,
) -> Plan {
    let intro_text = lesson.intro_text().map(str::to_string);
    let intro_gap_ms = if intro_text.is_some() && intro_ms > 0 {
        lesson.config.intro_gap_ms
    } else {
        0
    };
    let outro_text = lesson.outro_text().map(str::to_string);

    let items: Vec<PlannedItem> = lesson
        .items
        .iter()
        .map(|item| {
            let clip_ms = clip_ms.get(&item.index).copied().unwrap_or(0);
            let announce_ms = if lesson.config.announce_index {
                announce_ms.get(&item.index).copied().unwrap_or(0)
            } else {
                0
            };
            planned_item(
                item.index,
                &item.text,
                item.lang,
                announce_ms,
                clip_ms,
                lesson.repeat_for(item),
                lesson.config.repeat_gap_ms,
                lesson.gap_for(item),
            )
        })
        .collect();
    finish(
        lesson.config.lead_in_ms,
        intro_ms,
        intro_gap_ms,
        intro_text,
        outro_ms,
        outro_text,
        items,
    )
}

/// Rough duration of one spoken item, in milliseconds.
pub fn estimate_clip_ms(text: &str, lang: Lang, rate: f32) -> u32 {
    let (mut han, mut latin, mut other) = (0usize, 0usize, 0usize);
    for c in text.chars() {
        if is_han(c) {
            han += 1;
        } else if c.is_ascii_alphanumeric() {
            latin += 1;
        } else if !c.is_whitespace() {
            other += 1;
        }
    }

    // Punctuation and bare digits follow the lesson's main language.
    let other_ms = match lang {
        Lang::Zh | Lang::Mixed => ZH_MS_PER_CHAR,
        Lang::En => EN_MS_PER_CHAR,
    };
    let base =
        han as f64 * ZH_MS_PER_CHAR + latin as f64 * EN_MS_PER_CHAR + other as f64 * other_ms;
    let base = base.max(MIN_CLIP_MS);
    let rate = if rate.is_finite() && rate > 0.0 {
        rate as f64
    } else {
        1.0
    };
    (base / rate).round() as u32
}

#[allow(clippy::too_many_arguments)]
fn planned_item(
    index: usize,
    text: &str,
    lang: Lang,
    announce_ms: u32,
    clip_ms: u32,
    repeat: u8,
    repeat_gap_ms: u32,
    gap_ms: u32,
) -> PlannedItem {
    let repeat = repeat.max(1);
    let total_ms = item_total_ms(announce_ms, clip_ms, repeat, repeat_gap_ms, gap_ms);
    PlannedItem {
        index,
        text: text.to_string(),
        lang,
        announce_ms,
        clip_ms,
        repeat,
        repeat_gap_ms,
        gap_ms,
        total_ms,
    }
}

fn item_total_ms(
    announce_ms: u32,
    clip_ms: u32,
    repeat: u8,
    repeat_gap_ms: u32,
    gap_ms: u32,
) -> u32 {
    let repeat = u32::from(repeat.max(1));
    let clip_part = u64::from(clip_ms) * u64::from(repeat);
    let gap_part = u64::from(repeat_gap_ms) * u64::from(repeat - 1);
    let total = u64::from(announce_ms) + clip_part + gap_part + u64::from(gap_ms);
    total.min(u64::from(u32::MAX)) as u32
}

fn finish(
    lead_in_ms: u32,
    intro_ms: u32,
    intro_gap_ms: u32,
    intro_text: Option<String>,
    outro_ms: u32,
    outro_text: Option<String>,
    items: Vec<PlannedItem>,
) -> Plan {
    let total: u64 = u64::from(lead_in_ms)
        + u64::from(intro_ms)
        + u64::from(intro_gap_ms)
        + u64::from(outro_ms)
        + items.iter().map(|i| u64::from(i.total_ms)).sum::<u64>();
    Plan {
        lead_in_ms,
        intro_ms,
        intro_gap_ms,
        intro_text,
        outro_ms,
        outro_text,
        items,
        total_ms: total.min(u64::from(u32::MAX)) as u32,
    }
}

fn is_han(c: char) -> bool {
    matches!(c as u32,
        0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xF900..=0xFAFF
        | 0x20000..=0x2FA1F
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lesson::parse_str;
    use std::path::Path;

    #[test]
    fn clip_estimate_uses_character_weights() {
        assert_eq!(estimate_clip_ms("摇晃", Lang::Zh, 1.0), 520);
        assert_eq!(estimate_clip_ms("摇", Lang::Zh, 1.0), 400);
        assert_eq!(estimate_clip_ms("apple", Lang::En, 1.0), 450);
        assert_eq!(estimate_clip_ms("apple", Lang::En, 2.0), 225);
        assert_eq!(estimate_clip_ms("苹果 apple", Lang::Mixed, 1.0), 970);
        assert_eq!(estimate_clip_ms("", Lang::Zh, 1.0), 400);
    }

    #[test]
    fn estimate_matches_formula() {
        let mut lesson = parse_str(
            "摇晃 | rate=0.8\napple | repeat=1 gap=2s\n",
            Path::new("/tmp/plan-estimate.txt"),
            None,
        )
        .unwrap();
        lesson.config.announce_index = true;
        let plan = estimate(&lesson);

        assert_eq!(plan.lead_in_ms, 3_000);
        assert_eq!(plan.items.len(), 2);
        let intro_text = lesson.intro_text().unwrap().to_string();
        assert_eq!(plan.intro_text.as_deref(), Some(intro_text.as_str()));
        assert_eq!(
            plan.intro_ms,
            estimate_clip_ms(&intro_text, Lang::Zh, lesson.config.rate)
        );
        assert_eq!(plan.intro_gap_ms, 1_500);
        let outro_text = lesson.outro_text().unwrap().to_string();
        assert_eq!(plan.outro_text.as_deref(), Some(outro_text.as_str()));
        assert_eq!(
            plan.outro_ms,
            estimate_clip_ms(&outro_text, Lang::Zh, lesson.config.rate)
        );
        // 第1条：520ms / 0.8 = 650ms，2 遍 + 1.2s 遍间 + 6s 间隔 + 700ms 题号
        assert_eq!(plan.items[0].clip_ms, 650);
        assert_eq!(plan.items[0].announce_ms, 700);
        assert_eq!(plan.items[0].total_ms, 700 + 650 * 2 + 1_200 + 6_000);
        assert_eq!(plan.items[1].repeat, 1);
        assert_eq!(plan.items[1].gap_ms, 2_000);
        let apple = estimate_clip_ms("apple", Lang::En, 0.9);
        assert_eq!(plan.items[1].clip_ms, apple);
        assert_eq!(plan.items[1].total_ms, 700 + apple + 2_000);

        let expected = 3_000
            + plan.intro_ms
            + 1_500
            + plan.outro_ms
            + plan.items[0].total_ms
            + plan.items[1].total_ms;
        assert_eq!(plan.total_ms, expected);
    }

    #[test]
    fn estimate_includes_intro_by_default_and_can_disable_it() {
        let mut lesson = parse_str("摇晃\n", Path::new("/tmp/plan-intro.txt"), None).unwrap();

        let plan = estimate(&lesson);
        assert!(plan.intro_ms > 0);
        assert_eq!(plan.intro_gap_ms, 1_500);
        assert_eq!(plan.intro_text.as_deref(), lesson.intro_text());
        assert!(plan.outro_ms > 0);
        assert_eq!(plan.outro_text.as_deref(), lesson.outro_text());
        // 默认不播题号。
        assert_eq!(plan.items[0].announce_ms, 0);

        let with_announce = {
            lesson.config.announce_index = true;
            estimate(&lesson)
        };
        assert_eq!(with_announce.items[0].announce_ms, ANNOUNCE_MS);
        assert_eq!(
            with_announce.total_ms,
            plan.total_ms + ANNOUNCE_MS,
            "题号只应增加播报时长"
        );

        lesson.config.intro = Some(String::new());
        let disabled = estimate(&lesson);
        assert_eq!(disabled.intro_text, None);
        assert_eq!(disabled.intro_ms, 0);
        assert_eq!(disabled.intro_gap_ms, 0);
        assert_eq!(disabled.outro_ms, plan.outro_ms, "关闭开场白不影响结束语");
        assert_eq!(
            disabled.total_ms,
            3_000 + with_announce.items[0].total_ms + plan.outro_ms,
            "关闭开场白后不应再计 intro / intro_gap"
        );

        lesson.config.outro = Some(String::new());
        let no_outro = estimate(&lesson);
        assert_eq!(no_outro.outro_text, None);
        assert_eq!(no_outro.outro_ms, 0);
        assert_eq!(
            no_outro.total_ms,
            3_000 + with_announce.items[0].total_ms,
            "关闭结束语后不应再计 outro"
        );
    }

    #[test]
    fn build_uses_measured_durations() {
        let mut lesson = parse_str("摇晃\n粗壮\n", Path::new("/tmp/plan-build.txt"), None).unwrap();
        lesson.config.announce_index = true;
        let mut clips = HashMap::new();
        clips.insert(1, 1_100);
        clips.insert(2, 900);
        let mut announces = HashMap::new();
        announces.insert(1, 700);
        announces.insert(2, 700);

        let plan = build(&lesson, &clips, &announces, 2_500, 1_800);
        assert_eq!(plan.intro_ms, 2_500);
        assert_eq!(plan.intro_gap_ms, 1_500);
        assert_eq!(plan.intro_text.as_deref(), lesson.intro_text());
        assert_eq!(plan.outro_ms, 1_800);
        assert_eq!(plan.outro_text.as_deref(), lesson.outro_text());
        assert_eq!(plan.items[0].clip_ms, 1_100);
        assert_eq!(plan.items[1].clip_ms, 900);
        let expected = 3_000
            + 2_500
            + 1_500
            + 1_800
            + (700 + 1_100 * 2 + 1_200 + 6_000)
            + (700 + 900 * 2 + 1_200 + 6_000);
        assert_eq!(plan.total_ms, expected);
    }

    #[test]
    fn build_tolerates_missing_measurements() {
        let lesson = parse_str("摇晃\n", Path::new("/tmp/plan-missing.txt"), None).unwrap();
        let plan = build(&lesson, &HashMap::new(), &HashMap::new(), 0, 0);
        assert_eq!(plan.items[0].clip_ms, 0);
        assert_eq!(plan.intro_ms, 0);
        assert_eq!(plan.intro_gap_ms, 0, "没有实测开场白时不应插入间隔");
        assert_eq!(plan.outro_ms, 0);
        assert_eq!(plan.outro_text.as_deref(), lesson.outro_text());
        // repeat_gap 仍然计入，因为播放器会真的插入静音。
        assert_eq!(plan.total_ms, 3_000 + 1_200 + 6_000);
    }
}
