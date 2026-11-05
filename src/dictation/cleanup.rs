//! Local cleanup: Vedu Scribe rewrites a transcript into what the speaker meant
//! to write. The one stage a dictation's text passes through on its way to the
//! corrections, and the one seam a remote client (Android over LAN) needs:
//! raw text in, clean text out, however the text got here.
//!
//! The model only runs inside this boundary, and never gets the last word:
//!
//! ```text
//! transcript ─▶ vocabulary pre-match ─▶ Scribe ─▶ faithfulness ─▶ residue ─▶ text
//!                                         └─ unavailable, late, or unfaithful ─┘
//!                                                     (deterministic residue only)
//! ```
//!
//! Every failure ends in usable text: a dictation is never lost to cleanup.

mod faithful;
mod residue;
mod sidecar;
mod vocab;

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::dictation::vocabulary::VocabularyEntry;

pub use sidecar::prepare;

/// Why a dictation's text skipped the model. `None` on [`Cleaned`] means the
/// model's text was used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Fallback {
    /// The helper or weights aren't here yet, or it is still loading.
    Unavailable(String),
    /// The deadline passed.
    Late,
    Cancelled,
    /// The helper answered with an error or died mid-request.
    Failed(String),
    /// The output failed the faithfulness check.
    Unfaithful(faithful::Rejection),
}

/// One cleanup's result. `text` is always usable.
#[derive(Clone, Debug)]
pub struct Cleaned {
    pub text: String,
    pub fallback: Option<Fallback>,
    pub latency: Duration,
}

/// How long a dictation may wait on the model. Long dictations get longer:
/// measured on an M1 Pro, Scribe takes roughly 10 ms per spoken word warm.
fn deadline_for(words: usize) -> Duration {
    const BASE: Duration = Duration::from_millis(1_500);
    const PER_WORD: Duration = Duration::from_millis(25);
    const MOST: Duration = Duration::from_secs(12);
    (BASE + PER_WORD * u32::try_from(words).unwrap_or(u32::MAX)).min(MOST)
}

/// Cleans one transcript. `entries` is the person's vocabulary for this
/// dictation; it is pre-matched here, so the model only sees likely terms.
pub fn apply(transcript: &str, entries: &[VocabularyEntry], cancelled: &AtomicBool) -> Cleaned {
    let started = Instant::now();
    let finish = |text: String, fallback: Option<Fallback>| {
        if let Some(reason) = &fallback {
            tracing::info!(?reason, "cleanup fell back to deterministic text");
        }
        Cleaned {
            text,
            fallback,
            latency: started.elapsed(),
        }
    };
    let terms = vocab::select(transcript, entries);
    let words = transcript.split_whitespace().count();
    // Trained on lowercase speech. Measured on real Parakeet transcripts this
    // beats the recognizer's own casing (benchmarks/cleanup/README.md); the
    // vocabulary header is what restores technical casing.
    let reply = sidecar::clean(
        &transcript.to_lowercase(),
        &terms,
        deadline_for(words),
        cancelled,
    );
    let deterministic = || residue::strip(transcript);
    match reply {
        Err(fallback) => finish(deterministic(), Some(fallback)),
        Ok(output) => match faithful::reconcile(transcript, &output, &terms) {
            Ok(text) => finish(residue::strip(&text), None),
            Err(rejection) => finish(deterministic(), Some(Fallback::Unfaithful(rejection))),
        },
    }
}
