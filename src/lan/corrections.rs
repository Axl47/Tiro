//! Phone dictations in History, and what became of them.
//!
//! Each dictation the phone gets back is recorded in the Mac's History
//! (application "Phone"), and its answer carries the entry's id. The phone
//! follows the span it typed into the field (`FieldSpan` in `android/`) and,
//! once you send the message or leave the field, posts the result:
//!
//! ```text
//! POST /corrections  {"entry": 42, "status": "edited", "text": "…"}
//!                    status: edited · unchanged · unreadable
//! ```
//!
//! Only entries recorded for a phone can be corrected this way, and only while
//! Labs › Save corrections is on. A correction typed in History still wins.

use std::sync::OnceLock;

use crate::history::{
    Correction, CorrectionSource, CorrectionStatus, History, HistoryDraft, HistoryKind, Recording,
};

/// The application History shows for a phone's dictations.
const PHONE: &str = "Phone";

static HISTORY: OnceLock<History> = OnceLock::new();

/// Called once at startup with the app's History.
pub(super) fn attach(history: History) {
    let _ = HISTORY.set(history);
}

/// Records a phone's dictation; its id goes back to the phone. `kept` is its
/// 16 kHz audio while Labs › Keep recordings is on.
pub(super) fn record(
    transcript: &str,
    text: &str,
    audio_ms: u64,
    kept: Option<&[f32]>,
    total_ms: u64,
) -> Option<u64> {
    if text.trim().is_empty() {
        return None;
    }
    let history = HISTORY.get()?;
    let draft = HistoryDraft {
        kind: HistoryKind::Dictation,
        raw_text: transcript.to_owned(),
        final_text: text.to_owned(),
        application: Some(PHONE.to_owned()),
        processing: None,
        audio_ms,
        inference_ms: 0,
        total_ms,
        recording: kept.and_then(Recording::keep),
        model: crate::history::dictation_model(),
    };
    history.record(draft).unwrap_or_else(|error| {
        tracing::warn!(%error, "could not save a phone dictation to History");
        None
    })
}

/// Records a dictation the phone sent whole, as a WAV (`POST /dictate`).
pub(super) fn record_wav(transcript: &str, text: &str, wav: &[u8], total_ms: u64) -> Option<u64> {
    let clip = crate::transcription::service::AudioClip::decode_wav(std::io::Cursor::new(wav)).ok();
    let kept = clip
        .as_ref()
        .filter(|_| crate::labs::keep_recordings())
        .map(|clip| clip.samples());
    let audio_ms = clip.as_ref().map_or(0, |clip| clip.duration_ms);
    record(transcript, text, audio_ms, kept, total_ms)
}

/// The fields a phone's answer gets: the entry to correct, and whether the
/// phone should follow it at all.
pub(super) fn answer_fields(entry: Option<u64>) -> serde_json::Value {
    match entry {
        Some(entry) => serde_json::json!({
            "entry": entry,
            "corrections": crate::labs::save_corrections(),
        }),
        None => serde_json::json!({}),
    }
}

#[derive(serde::Deserialize)]
struct Posted {
    entry: u64,
    status: PostedStatus,
    #[serde(default)]
    text: Option<String>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum PostedStatus {
    Edited,
    Unchanged,
    Unreadable,
}

/// Answers `POST /corrections`. Every answer but `503` is final: the phone
/// retries only a Mac it couldn't reach or that wasn't ready.
pub(super) fn answer(body: &[u8]) -> Vec<u8> {
    let Some(history) = HISTORY.get() else {
        return super::dictate::json(503, &serde_json::json!({ "code": "tiro-not-ready" }));
    };
    let Ok(posted) = serde_json::from_slice::<Posted>(body) else {
        return super::dictate::json(400, &serde_json::json!({ "code": "invalid-request" }));
    };
    let saved = crate::labs::save_corrections()
        && save(history, posted).unwrap_or_else(|error| {
            tracing::warn!(%error, "could not save a phone dictation's correction");
            false
        });
    super::dictate::json(200, &serde_json::json!({ "saved": saved }))
}

fn save(history: &History, posted: Posted) -> std::io::Result<bool> {
    let from_phone = history
        .entry(posted.entry)
        .is_some_and(|entry| entry.application.as_deref() == Some(PHONE));
    if !from_phone {
        return Ok(false);
    }
    let (status, text) = match posted.status {
        PostedStatus::Edited => (CorrectionStatus::Edited, posted.text),
        PostedStatus::Unchanged => (CorrectionStatus::Unchanged, posted.text),
        PostedStatus::Unreadable => (CorrectionStatus::Unreadable, None),
    };
    // A label nobody can read is no label.
    if status != CorrectionStatus::Unreadable && text.as_deref().is_none_or(str::is_empty) {
        return Ok(false);
    }
    tracing::info!(
        entry = posted.entry,
        ?status,
        "phone dictation correction settled"
    );
    history.correct(
        posted.entry,
        Correction {
            text,
            status,
            source: CorrectionSource::PastedField,
            captured_ms: crate::history::now_ms(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_parse_as_the_phone_sends_them() {
        let posted: Posted =
            serde_json::from_str(r#"{"entry":3,"status":"edited","text":"Hi."}"#).unwrap();
        assert_eq!(posted.entry, 3);
        assert!(matches!(posted.status, PostedStatus::Edited));
        let posted: Posted = serde_json::from_str(r#"{"entry":4,"status":"unreadable"}"#).unwrap();
        assert!(matches!(posted.status, PostedStatus::Unreadable));
        assert!(serde_json::from_str::<Posted>(r#"{"entry":4,"status":"secure"}"#).is_err());
    }
}
