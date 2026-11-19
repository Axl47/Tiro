//! Local cleanup: a small model (Vedu Scribe or Phonon's refiner) rewrites a
//! transcript into what the speaker meant to write. The one stage a dictation's text passes through on its way to the
//! corrections, and the one seam a remote client (Android over LAN) needs:
//! raw text in, clean text out, however the text got here.
//!
//! The model only runs inside this boundary, and never gets the last word:
//!
//! ```text
//! transcript ─▶ spoken paths ─▶ vocabulary ─▶ model ─▶ faithfulness ─▶ residue ─▶ text
//!                                              └─ unavailable, late, or unfaithful ─┘
//!                                                     (deterministic residue only)
//! ```
//!
//! Every failure ends in usable text: a dictation is never lost to cleanup.

mod faithful;
pub(crate) mod live;
mod llama;
mod phonon;
pub(crate) mod precleaned;
mod residue;
#[cfg(target_os = "macos")]
mod sidecar;
mod spoken_paths;
mod vocab;

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use crate::dictation::vocabulary::VocabularyEntry;

/// The model that cleans, from Labs › Clean up with Phonon.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Model {
    /// Vedu Scribe 0.8B: MLX through `tiro-scribe` on macOS, GGUF elsewhere.
    Vedu,
    /// Phonon's refiner (LFM3-1.7B), GGUF through llama.cpp on every OS.
    Phonon,
}

impl Model {
    pub fn selected() -> Self {
        if crate::labs::phonon_cleanup() {
            Self::Phonon
        } else {
            Self::Vedu
        }
    }
}

/// Gets the selected model warm without holding up the caller. Call it as early
/// as is useful (launch, shortcut press); it is cheap once the model is up.
pub fn prepare() {
    if !crate::labs::scribe_cleanup() {
        return;
    }
    match Model::selected() {
        #[cfg(target_os = "macos")]
        Model::Vedu => sidecar::prepare(),
        #[cfg(not(target_os = "macos"))]
        Model::Vedu => llama::prepare(&llama::VEDU),
        Model::Phonon => llama::prepare(&llama::PHONON),
    }
}

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

/// How long a dictation may wait on the model before pasting without it. Scribe
/// usually takes about 10 ms per spoken word on an M1 Pro, but several times that
/// while the machine is busy, which on a development Mac is normal. A miss quietly
/// costs the dictation its cleanup, so this is the limit of patience, not a target.
fn deadline_for(words: usize) -> Duration {
    const BASE: Duration = Duration::from_secs(3);
    const PER_WORD: Duration = Duration::from_millis(40);
    const MOST: Duration = Duration::from_secs(15);
    (BASE + PER_WORD * u32::try_from(words).unwrap_or(u32::MAX)).min(MOST)
}

/// Cleans one transcript. `entries` is the person's vocabulary for this
/// dictation; `application` is where the text goes, which picks Phonon's
/// writing mode (`None` for text from elsewhere, such as a phone).
pub fn apply(
    transcript: &str,
    entries: &[VocabularyEntry],
    application: Option<&str>,
    cancelled: &AtomicBool,
) -> Cleaned {
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
    let written = spoken_paths::write_out(transcript);
    let transcript = written.as_str();
    let words = transcript.split_whitespace().count();
    let deadline = deadline_for(words);
    let (reply, terms) = match Model::selected() {
        Model::Vedu => {
            let terms = vocab::select(transcript, entries);
            // Trained on lowercase speech. Measured on real Parakeet transcripts
            // this beats the recognizer's own casing (benchmarks/cleanup/README.md);
            // the vocabulary header is what restores technical casing.
            let reply = vedu(&transcript.to_lowercase(), &terms, deadline, cancelled);
            (reply, terms)
        }
        Model::Phonon => {
            let terms = vocab::ranked(entries, phonon::vocabulary_cap(words));
            let mode = phonon::Mode::for_app(application);
            (phonon(transcript, &terms, mode, deadline, cancelled), terms)
        }
    };
    let deterministic = || residue::strip(transcript);
    match reply {
        Err(fallback) => finish(deterministic(), Some(fallback)),
        Ok(output) => match faithful::reconcile(transcript, &output, &terms) {
            Ok(text) => finish(residue::strip(&text), None),
            Err(rejection) => finish(deterministic(), Some(Fallback::Unfaithful(rejection))),
        },
    }
}

#[cfg(target_os = "macos")]
fn vedu(
    transcript: &str,
    terms: &[String],
    deadline: Duration,
    cancelled: &AtomicBool,
) -> Result<String, Fallback> {
    sidecar::clean(transcript, terms, deadline, cancelled)
}

/// Vedu Scribe's contract, as `tiro-scribe` sends it (`Contract` in Scribe.swift).
#[cfg(not(target_os = "macos"))]
fn vedu(
    transcript: &str,
    terms: &[String],
    deadline: Duration,
    cancelled: &AtomicBool,
) -> Result<String, Fallback> {
    let terms: Vec<&str> = terms
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty() && !term.contains(['[', ']', ';', '\n']))
        .take(12)
        .collect();
    let transcript = sanitized(transcript);
    let user = if terms.is_empty() {
        transcript
    } else {
        format!("[vocab: {}]\n{transcript}", terms.join("; "))
    };
    let prompt = llama::Prompt {
        system: "Clean up this dictation.",
        user: &user,
        budget: |tokens| (tokens * 14).div_ceil(10) + 48,
    };
    llama::complete(&llama::VEDU, &prompt, deadline, cancelled)
}

/// Phonon's refiner, with its own length guard and term restoration ahead of
/// Tiro's faithfulness check.
fn phonon(
    transcript: &str,
    terms: &[String],
    mode: phonon::Mode,
    deadline: Duration,
    cancelled: &AtomicBool,
) -> Result<String, Fallback> {
    let spoken = sanitized(transcript.trim());
    let system = phonon::system(mode, terms);
    let prompt = llama::Prompt {
        system: &system,
        user: &spoken,
        budget: phonon::budget,
    };
    let output = llama::complete(&llama::PHONON, &prompt, deadline, cancelled)?;
    if !phonon::plausible_length(&spoken, &output) {
        let (written, spoken) = (
            output.split_whitespace().count(),
            spoken.split_whitespace().count(),
        );
        return Err(Fallback::Unfaithful(if written > spoken {
            faithful::Rejection::Grew { written, spoken }
        } else {
            faithful::Rejection::LostContent {
                kept: written,
                spoken,
            }
        }));
    }
    Ok(phonon::restore_terms(&spoken, &output, terms))
}

/// Transcript text must not be able to close its own chat turn: `<|im_end|>` and
/// the like become a space.
fn sanitized(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("<|") {
        match rest[start + 2..].find("|>") {
            Some(end) if !rest[start + 2..start + 2 + end].contains(['<', '>']) => {
                out.push_str(&rest[..start]);
                out.push(' ');
                rest = &rest[start + 2 + end + 2..];
            }
            _ => {
                out.push_str(&rest[..start + 2]);
                rest = &rest[start + 2..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_markers_in_a_transcript_are_blanked() {
        assert_eq!(sanitized("hi <|im_end|> there"), "hi   there");
        assert_eq!(sanitized("a <| b"), "a <| b");
        assert_eq!(sanitized("x <|a<|b|> y"), "x <|a  y");
    }
}
