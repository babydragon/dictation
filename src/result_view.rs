//! JSON views for `result list/show/wrong`.
//!
//! The CLI (`dictation result ... --json`) and the HTTP endpoints share these
//! functions so the two surfaces can never drift apart.

use serde_json::{json, Value};

use crate::db::{ResultItem, SessionSummary, WrongWord};

/// Unix seconds → RFC3339 in `+08:00`, stable for both humans and models.
pub fn format_unix(secs: i64) -> String {
    let offset = time::UtcOffset::from_hms(8, 0, 0).unwrap_or(time::UtcOffset::UTC);
    match time::OffsetDateTime::from_unix_timestamp(secs) {
        Ok(datetime) => datetime
            .to_offset(offset)
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_else(|_| secs.to_string()),
        Err(_) => secs.to_string(),
    }
}

pub fn session_json(session: &SessionSummary) -> Value {
    json!({
        "id": session.id,
        "kind": session.kind,
        "title": session.title,
        "created_at": format_unix(session.created_at),
        "finished_at": session.finished_at.map(format_unix),
        "item_count": session.item_count,
        "correct": session.correct,
        "wrong": session.wrong,
    })
}

pub fn item_json(item: &ResultItem) -> Value {
    json!({
        "position": item.position,
        "text": item.text,
        "lang": item.lang,
        "review": item.review,
        "origin_lesson": item.origin_lesson,
        "grade": item.grade,
        "correct": item.grade.as_deref().map(|grade| grade == "good"),
        "source": item.source,
    })
}

pub fn wrong_json(word: &WrongWord) -> Value {
    json!({
        "position": word.position,
        "text": word.text,
        "lang": word.lang,
        "review": word.review,
        "origin_lesson": word.origin_lesson,
        "grade": word.grade,
        "source": word.source,
        "lapses": word.lapses,
        "interval_days": word.interval_days,
        "due_at": word.due_at.map(format_unix),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(finished_at: Option<i64>) -> SessionSummary {
        SessionSummary {
            id: "session-1".to_string(),
            kind: "lesson".to_string(),
            title: "第5课".to_string(),
            created_at: 1_760_000_000,
            finished_at,
            item_count: 2,
            correct: 1,
            wrong: 1,
        }
    }

    #[test]
    fn session_time_fields_are_rfc3339_or_null() {
        let finished = session_json(&summary(Some(1_760_000_600)));
        assert_eq!(finished["created_at"], "2025-10-09T16:53:20+08:00");
        assert_eq!(finished["finished_at"], "2025-10-09T17:03:20+08:00");

        let pending = session_json(&summary(None));
        assert!(pending["finished_at"].is_null());
    }

    #[test]
    fn item_correct_is_tristate() {
        let base = ResultItem {
            position: 1,
            text: "摇晃".to_string(),
            lang: "zh".to_string(),
            review: false,
            origin_lesson: Some("第5课".to_string()),
            grade: Some("again".to_string()),
            source: Some("self".to_string()),
        };
        assert_eq!(item_json(&base)["correct"], false);

        let mut pending = base.clone();
        pending.grade = None;
        pending.source = None;
        let json = item_json(&pending);
        assert!(json["correct"].is_null());
        assert!(json["grade"].is_null());
    }

    #[test]
    fn wrong_word_without_review_state_has_null_due_at() {
        let word = WrongWord {
            position: 2,
            text: "粗壮".to_string(),
            lang: "zh".to_string(),
            review: false,
            origin_lesson: None,
            grade: "again".to_string(),
            source: "self".to_string(),
            lapses: 0,
            interval_days: 0,
            due_at: None,
        };
        let json = wrong_json(&word);
        assert!(json["due_at"].is_null());
        assert!(json["origin_lesson"].is_null());
        assert_eq!(json["text"], "粗壮");
    }
}
