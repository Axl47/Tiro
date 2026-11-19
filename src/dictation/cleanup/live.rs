//! Cleanup while this computer's own dictation still records. Each recording's
//! stream publishes its text as it goes (`dictation::live_text`); a worker
//! cleans its finished sentences ([`Precleaned`]), so the finish only waits on
//! the last one.
//!
//! The stream never decides the text. The finish reuses a sentence only when the
//! final transcript has it exactly, cleaned with the same model, app, and
//! vocabulary, and cleans the rest in one call, as without any of this.
//!
//! ```text
//! live text ─▶ watcher ─▶ session (one per recording) ─▶ worker: catch_up
//!                                    │
//! final transcript ─▶ finish ────────┴─▶ reused sentences + the rest cleaned
//! ```

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, mpsc};
use std::time::Instant;

use super::precleaned::{Precleaned, first_unit};
use super::{Cleaned, Model};
use crate::dictation::vocabulary::VocabularyEntry;

/// Recordings kept for a finish: the one recording now and those whose
/// transcripts may still be on their way.
const KEPT: usize = 4;

/// What a recording's sentences were cleaned with. A finish with anything
/// else cleans from scratch.
#[derive(PartialEq)]
struct Setup {
    model: Model,
    application: Option<String>,
    vocabulary: Vec<VocabularyEntry>,
}

/// One recording's cleaning.
#[derive(Default)]
struct Session {
    precleaned: Precleaned,
    /// Set by the watcher as the recording ends: only then may a finish use it.
    ended: AtomicBool,
    /// The newest text cleanup was handed, after the Labs transforms.
    heard: Mutex<String>,
    setup: OnceLock<Setup>,
}

struct State {
    /// The recording publishing text now.
    current: Option<Arc<Session>>,
    /// Newest last.
    kept: VecDeque<Arc<Session>>,
}

static STATE: Mutex<State> = Mutex::new(State {
    current: None,
    kept: VecDeque::new(),
});
static STARTED: AtomicBool = AtomicBool::new(false);
/// The recording starting now skips processing, so nothing cleans it.
static RAW: AtomicBool = AtomicBool::new(false);

fn state() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|error| error.into_inner())
}

/// Whether dictation streams should publish their text for cleanup: once
/// [`start`]ed, while Labs › Clean up is on.
pub(crate) fn wanted() -> bool {
    STARTED.load(Ordering::Acquire) && crate::labs::scribe_cleanup()
}

/// Tells cleanup whether the dictation starting now skips processing (Dictate
/// without processing), as it starts.
pub(crate) fn set_raw(raw: bool) {
    RAW.store(raw, Ordering::Release);
}

/// Starts cleaning recordings as they go. Call once, from the listener that
/// processes their transcripts (it reads the focused app at each recording's
/// first text).
pub(crate) fn start() {
    if STARTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let (texts, heard) = mpsc::channel::<(Arc<Session>, String)>();
    crate::dictation::live_text::watch(move |partial| {
        let mut state = state();
        let Some(partial) = partial else {
            if let Some(session) = state.current.take() {
                session.ended.store(true, Ordering::Release);
            }
            return;
        };
        if !crate::labs::scribe_cleanup() || RAW.load(Ordering::Acquire) {
            return;
        }
        let session = match &state.current {
            Some(session) => session.clone(),
            None => {
                let session = Arc::new(Session::default());
                keep(&mut state.kept, session.clone());
                state.current = Some(session.clone());
                session
            }
        };
        drop(state);
        let _ = texts.send((session, partial.stable.clone()));
    });
    let spawned = std::thread::Builder::new()
        .name("cleanup-live".into())
        .spawn(move || work(&heard));
    if let Err(error) = spawned {
        tracing::warn!(%error, "could not start cleaning while recording");
    }
}

/// Adds `session` to the kept ones, closing the oldest past [`KEPT`].
fn keep(kept: &mut VecDeque<Arc<Session>>, session: Arc<Session>) {
    if kept.len() == KEPT
        && let Some(oldest) = kept.pop_front()
    {
        oldest.precleaned.close();
    }
    kept.push_back(session);
}

/// Cleans each recording's newest text, skipping texts a newer one of the same
/// recording replaced while a sentence cleaned.
fn work(heard: &mpsc::Receiver<(Arc<Session>, String)>) {
    while let Ok(mut next) = heard.recv() {
        while let Ok(newer) = heard.try_recv() {
            if !Arc::ptr_eq(&newer.0, &next.0) {
                catch_up(&next.0, &next.1);
            }
            next = newer;
        }
        catch_up(&next.0, &next.1);
    }
}

fn catch_up(session: &Session, stable: &str) {
    let setup = session.setup.get_or_init(|| {
        let context = crate::dictation::vocabulary::focused().unwrap_or_default();
        Setup {
            model: Model::selected(),
            vocabulary: crate::dictation::config::dictation_profiles().cleanup_vocabulary(&context),
            application: context.application,
        }
    });
    let text = crate::dictation::processor::labs_transforms(stable);
    *session
        .heard
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = text.clone();
    let never = AtomicBool::new(false);
    session.precleaned.catch_up(&text, |unit| {
        let cleaned = super::apply(
            unit,
            &setup.vocabulary,
            setup.application.as_deref(),
            &never,
        );
        tracing::debug!(
            words = unit.split_whitespace().count(),
            latency_ms = cleaned.latency.as_millis() as u64,
            fallback = ?cleaned.fallback,
            "cleaned a sentence while recording"
        );
        cleaned.fallback.is_none().then_some(cleaned.text)
    });
}

/// Cleans a finished dictation's `transcript`, reusing what was cleaned while
/// it recorded when the recording is found; otherwise [`super::apply`].
pub fn finish(
    transcript: &str,
    vocabulary: &[VocabularyEntry],
    application: Option<&str>,
    cancelled: &AtomicBool,
) -> Cleaned {
    let setup = Setup {
        model: Model::selected(),
        application: application.map(str::to_owned),
        vocabulary: vocabulary.to_vec(),
    };
    let Some(session) = take(transcript, &setup) else {
        return super::apply(transcript, vocabulary, application, cancelled);
    };
    let started = Instant::now();
    let (mut rest, mut rest_words) = (None, 0);
    let text = session.precleaned.finish(transcript, |remaining| {
        rest_words = remaining.split_whitespace().count();
        let cleaned = super::apply(remaining, vocabulary, application, cancelled);
        let text = cleaned.text.clone();
        rest = Some(cleaned);
        Some(text)
    });
    tracing::info!(
        reused = session.precleaned.reused(),
        rest_words,
        "dictation cleanup reused sentences cleaned while recording"
    );
    Cleaned {
        text,
        fallback: rest.and_then(|cleaned| cleaned.fallback),
        latency: started.elapsed(),
    }
}

/// The newest ended recording cleaned with `setup` whose text opens as
/// `transcript` does, out of the kept ones.
fn take(transcript: &str, setup: &Setup) -> Option<Arc<Session>> {
    let opening = first_unit(transcript)?;
    let mut state = state();
    let found = state.kept.iter().rposition(|session| {
        session.ended.load(Ordering::Acquire)
            && session.setup.get() == Some(setup)
            && first_unit(
                &session
                    .heard
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()),
            )
            .is_some_and(|heard| heard == opening)
    })?;
    state.kept.remove(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(heard: &str, ended: bool) -> Arc<Session> {
        let session = Session::default();
        *session.heard.lock().unwrap() = heard.into();
        session.ended.store(ended, Ordering::Release);
        let _ = session.setup.set(setup("Ghostty"));
        Arc::new(session)
    }

    fn setup(application: &str) -> Setup {
        Setup {
            model: Model::Vedu,
            application: Some(application.into()),
            vocabulary: Vec::new(),
        }
    }

    #[test]
    fn a_finish_takes_the_newest_ended_recording_that_opens_the_same_way() {
        let older = session("Run the tests. Then", true);
        let newer = session("Run the tests. And", true);
        let other = session("Something else.", true);
        let recording = session("Run the tests.", false);
        {
            let mut state = state();
            state.kept.clear();
            for session in [&older, &newer, &other, &recording] {
                state.kept.push_back(session.clone());
            }
        }
        let found = take("Run the tests. Then stop.", &setup("Ghostty")).unwrap();
        assert!(Arc::ptr_eq(&found, &newer));
        // A different app, or no recording opening this way, finds nothing.
        assert!(take("Run the tests.", &setup("Mail")).is_none());
        assert!(take("Nothing like it.", &setup("Ghostty")).is_none());
        // The recording still going is never taken.
        let found = take("Run the tests.", &setup("Ghostty")).unwrap();
        assert!(Arc::ptr_eq(&found, &older));
        assert!(take("Run the tests.", &setup("Ghostty")).is_none());
        state().kept.clear();
    }
}
