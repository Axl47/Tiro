//! Tiro, Labs › Save corrections: what became of a dictation after it was
//! pasted, read back from the field it went into.
//!
//! Only the characters Tiro pasted are ever read. The span is found at the
//! caret just after the paste, and then followed from the field's length and
//! caret alone: an edit inside the span moves its end, one before it moves
//! the whole span, and one after it changes nothing. Text around the span is
//! never requested, so it can't be stored or logged by accident.
//!
//! The field is polled, not asked once, because a chat composer empties when
//! you send: the label is the last good reading before the span went away.
//! Several dictations can feed one message before it is sent, so a field
//! holds several spans.
//!
//! A secure field is never read, and neither is anything while macOS has
//! secure input on, which is how it marks a password prompt anywhere.

use std::ops::Range;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant};

use crate::accessibility::{FieldReading, FocusedField};

use super::{Correction, CorrectionSource, CorrectionStatus, History, now_ms};

/// Quick enough to catch a last fix made just before pressing Return.
const ACTIVE_POLL: Duration = Duration::from_millis(400);
const IDLE_POLL: Duration = Duration::from_secs(1);
/// How long after its last change a field counts as being edited.
const ACTIVE_FOR: Duration = Duration::from_secs(30);
/// Chromium takes about 3 s to build its tree the first time it is asked.
const LOCATE_FOR: Duration = Duration::from_secs(8);
/// Real messages were sent up to 13 minutes after one of their dictations.
const WATCH_FOR: Duration = Duration::from_secs(15 * 60);
const MAX_FIELDS: usize = 8;
/// A reading sharing less than this with the paste is some other text.
const MIN_SIMILARITY: f32 = 0.5;

/// Above this many words a reading isn't compared, only accepted.
const MAX_COMPARED_WORDS: usize = 2_000;

/// The field operations a [`Tracker`] needs.
pub(super) trait Field {
    fn reading(&self) -> FieldReading;
    fn text(&self, range: Range<usize>) -> Option<String>;
}

impl Field for FocusedField {
    fn reading(&self) -> FieldReading {
        FocusedField::reading(self)
    }

    fn text(&self, range: Range<usize>) -> Option<String> {
        FocusedField::text(self, range)
    }
}

struct Span {
    id: u64,
    pasted: String,
    located: Option<Range<usize>>,
    /// The last reading that still looked like this paste.
    best: Option<String>,
    asked: Instant,
    secure: bool,
}

impl Span {
    fn new(id: u64, pasted: String, asked: Instant) -> Self {
        Self {
            id,
            pasted,
            located: None,
            best: None,
            asked,
            secure: false,
        }
    }

    fn outcome(self) -> (u64, Correction) {
        let (status, text) = match self.best {
            Some(best) if same_text(&best, &self.pasted) => {
                (CorrectionStatus::Unchanged, Some(self.pasted))
            }
            Some(best) => (CorrectionStatus::Edited, Some(best)),
            None if self.secure => (CorrectionStatus::Secure, None),
            None => (CorrectionStatus::Unreadable, None),
        };
        (
            self.id,
            Correction {
                text,
                status,
                source: CorrectionSource::PastedField,
                captured_ms: now_ms(),
            },
        )
    }
}

/// One field and the dictations pasted into it.
pub(super) struct Tracker<F> {
    field: F,
    /// The field's length and caret at the last reading.
    seen: Option<(usize, usize)>,
    spans: Vec<Span>,
    changed: Instant,
}

impl<F: Field> Tracker<F> {
    pub(super) fn new(field: F, now: Instant) -> Self {
        Self {
            field,
            seen: None,
            spans: Vec::new(),
            changed: now,
        }
    }

    pub(super) fn add(&mut self, id: u64, pasted: String, now: Instant) {
        self.spans.push(Span::new(id, pasted, now));
    }

    fn active(&self, now: Instant) -> bool {
        now.duration_since(self.changed) < ACTIVE_FOR
    }

    /// Takes one reading of the field, and returns the spans that ended.
    pub(super) fn tick(&mut self, now: Instant) -> Vec<(u64, Correction)> {
        let (length, caret) = match self.field.reading() {
            FieldReading::Gone => return self.end_all(),
            FieldReading::Secure => {
                for span in &mut self.spans {
                    span.secure = true;
                }
                return self.end_all();
            }
            // Someone is typing a password, maybe elsewhere: read nothing.
            FieldReading::SecureInputActive => {
                for span in self.spans.iter_mut().filter(|span| span.located.is_none()) {
                    span.secure = true;
                }
                return self.expire(now);
            }
            FieldReading::Text { length, caret } => (length, caret),
        };
        let edited = self.seen != Some((length, caret));
        if let Some((before, _)) = self.seen.filter(|_| edited) {
            // A paste still being located is the cause of this growth, not
            // typing that should extend the span before it.
            let extend = self.spans.iter().all(|span| span.located.is_some());
            let delta = length as isize - before as isize;
            for span in &mut self.spans {
                let Some(range) = span.located.as_mut() else {
                    continue;
                };
                *range = moved(range.clone(), delta, caret, |at| {
                    extend && self.field.text(at..at + 1).is_some_and(starts_a_word)
                });
            }
        }
        if edited {
            self.seen = Some((length, caret));
            self.changed = now;
        }
        let mut ended = Vec::new();
        for mut span in std::mem::take(&mut self.spans) {
            let Some(range) = span.located.clone() else {
                if let Some(range) = locate(&self.field, &span.pasted, caret) {
                    span.best = self.field.text(range.clone());
                    span.located = Some(range);
                    self.spans.push(span);
                } else if now.duration_since(span.asked) >= LOCATE_FOR {
                    ended.push(span.outcome());
                } else {
                    self.spans.push(span);
                }
                continue;
            };
            // Gone with the text around it: the message was sent. An empty
            // Chromium composer still reports one character.
            if length <= 1 || range.end > length || now.duration_since(span.asked) >= WATCH_FOR {
                ended.push(span.outcome());
                continue;
            }
            if edited
                && let Some(text) = self.field.text(range)
                && resembles(&span.pasted, &text)
            {
                span.best = Some(text);
            }
            self.spans.push(span);
        }
        ended
    }

    fn end_all(&mut self) -> Vec<(u64, Correction)> {
        self.spans.drain(..).map(Span::outcome).collect()
    }

    /// Ends what has waited too long while nothing could be read.
    fn expire(&mut self, now: Instant) -> Vec<(u64, Correction)> {
        let mut ended = Vec::new();
        for span in std::mem::take(&mut self.spans) {
            let limit = if span.located.is_some() {
                WATCH_FOR
            } else {
                LOCATE_FOR
            };
            if now.duration_since(span.asked) >= limit {
                ended.push(span.outcome());
            } else {
                self.spans.push(span);
            }
        }
        ended
    }
}

/// Where `span` is after the field changed length by `delta` and the caret
/// came to rest at `caret`. An insertion ended at the caret; a deletion
/// removed what follows it. `appended` says whether the text inserted at the
/// span's end continues its last word (fixing it) rather than starting new
/// text (which the recording never said).
fn moved(
    span: Range<usize>,
    delta: isize,
    caret: usize,
    appended: impl FnOnce(usize) -> bool,
) -> Range<usize> {
    if delta > 0 {
        let inserted = delta as usize;
        let at = caret.saturating_sub(inserted);
        if at <= span.start {
            span.start + inserted..span.end + inserted
        } else if at < span.end || (at == span.end && appended(at)) {
            span.start..span.end + inserted
        } else {
            span
        }
    } else if delta < 0 {
        let (from, to) = (caret, caret + delta.unsigned_abs());
        let kept_before = from.min(span.end).saturating_sub(span.start);
        let kept_after = span.end.saturating_sub(to.max(span.start));
        let start = span.start.min(from);
        start..start + kept_before + kept_after
    } else {
        span
    }
}

/// The span ending at the caret, when it is what was pasted.
fn locate(field: &impl Field, pasted: &str, caret: usize) -> Option<Range<usize>> {
    let length = pasted.encode_utf16().count();
    let range = caret.checked_sub(length)?..caret;
    let text = field.text(range.clone())?;
    same_text(&text, pasted).then_some(range)
}

/// Equal apart from spacing and the first letter's case, which pasting
/// after earlier text may change.
fn same_text(left: &str, right: &str) -> bool {
    let normal = |text: &str| -> Vec<String> {
        let mut words: Vec<String> = text.split_whitespace().map(str::to_owned).collect();
        if let Some(first) = words.first_mut() {
            *first = first.to_lowercase();
        }
        words
    };
    normal(left) == normal(right)
}

fn starts_a_word(text: String) -> bool {
    text.chars().next().is_some_and(char::is_alphanumeric)
}

/// Whether `text` is still recognisably the paste: enough words in common.
fn resembles(pasted: &str, text: &str) -> bool {
    let words = |value: &str| -> Vec<String> {
        value
            .split_whitespace()
            .map(|word| {
                word.trim_matches(|c: char| !c.is_alphanumeric())
                    .to_lowercase()
            })
            .filter(|word| !word.is_empty())
            .collect()
    };
    let (left, right) = (words(pasted), words(text));
    if right.is_empty() {
        return false;
    }
    if left.len().max(right.len()) > MAX_COMPARED_WORDS {
        return true;
    }
    let mut row = vec![0_usize; right.len() + 1];
    for word in &left {
        let mut diagonal = 0;
        for (index, other) in right.iter().enumerate() {
            let above = row[index + 1];
            row[index + 1] = if word == other {
                diagonal + 1
            } else {
                above.max(row[index])
            };
            diagonal = above;
        }
    }
    let common = row[right.len()] as f32;
    2.0 * common / (left.len() + right.len()) as f32 >= MIN_SIMILARITY
}

struct Watch {
    id: u64,
    text: String,
    pid: i32,
    asked: Instant,
}

/// Follows dictation `id`, just pasted as `text` into the app `pid`.
/// Needs Labs › Save corrections; the result lands in `history` once the
/// span goes away.
pub(crate) fn watch(history: &History, id: u64, text: &str, pid: i32) {
    static WORKER: OnceLock<Mutex<Sender<Watch>>> = OnceLock::new();
    if !crate::labs::save_corrections() {
        return;
    }
    let worker = WORKER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel();
        let history = history.clone();
        if let Err(error) = thread::Builder::new()
            .name("correction-watch".into())
            .spawn(move || run(&history, &receiver))
        {
            tracing::warn!(%error, "could not start the correction watcher");
        }
        Mutex::new(sender)
    });
    let sender = worker.lock().unwrap_or_else(|error| error.into_inner());
    let _ = sender.send(Watch {
        id,
        text: text.trim().to_owned(),
        pid,
        asked: Instant::now(),
    });
}

/// The worker: one thread for every field. It sleeps while nothing is
/// followed.
fn run(history: &History, receiver: &mpsc::Receiver<Watch>) {
    let mut trackers: Vec<Tracker<FocusedField>> = Vec::new();
    let mut waiting: Vec<Watch> = Vec::new();
    loop {
        let now = Instant::now();
        let poll = if !waiting.is_empty() || trackers.iter().any(|tracker| tracker.active(now)) {
            ACTIVE_POLL
        } else {
            IDLE_POLL
        };
        if waiting.is_empty() && trackers.is_empty() {
            crate::accessibility::release_enhanced_accessibility();
            match receiver.recv() {
                Ok(watch) => waiting.push(watch),
                Err(_) => return,
            }
        } else {
            match receiver.recv_timeout(poll) {
                Ok(watch) => waiting.push(watch),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        waiting.extend(receiver.try_iter());
        if !crate::labs::save_corrections() {
            // Off means off: nothing followed so far is saved.
            trackers.clear();
            waiting.clear();
            continue;
        }
        let now = Instant::now();
        let mut ended = Vec::new();
        for watch in std::mem::take(&mut waiting) {
            match crate::accessibility::focused_field(watch.pid) {
                Some(field) => {
                    let known = trackers
                        .iter_mut()
                        .find(|tracker| tracker.field.same(&field));
                    if let Some(tracker) = known {
                        tracker.add(watch.id, watch.text, watch.asked);
                    } else {
                        let mut tracker = Tracker::new(field, now);
                        tracker.add(watch.id, watch.text, watch.asked);
                        trackers.push(tracker);
                    }
                }
                None if now.duration_since(watch.asked) >= LOCATE_FOR => {
                    ended.push(Span::new(watch.id, watch.text, watch.asked).outcome());
                }
                None => waiting.push(watch),
            }
        }
        for tracker in &mut trackers {
            ended.append(&mut tracker.tick(now));
        }
        trackers.retain(|tracker| !tracker.spans.is_empty());
        while trackers.len() > MAX_FIELDS {
            ended.append(&mut trackers.remove(0).end_all());
        }
        for (id, correction) in ended {
            tracing::info!(id, status = ?correction.status, "dictation correction settled");
            if let Err(error) = history.correct(id, correction) {
                tracing::warn!(%error, id, "could not save a dictation's correction");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A field whose text and caret a test edits between ticks.
    #[derive(Clone)]
    struct Fake(Rc<RefCell<(Vec<u16>, usize, FakeMode)>>);

    #[derive(Clone, Copy, Debug, PartialEq)]
    enum FakeMode {
        Open,
        Gone,
        Secure,
        SecureInput,
    }

    impl Fake {
        fn new(text: &str) -> Self {
            let units: Vec<u16> = text.encode_utf16().collect();
            let caret = units.len();
            Self(Rc::new(RefCell::new((units, caret, FakeMode::Open))))
        }

        /// Replaces `at..at + removed` with `text` and leaves the caret after it.
        fn edit(&self, at: usize, removed: usize, text: &str) {
            let inserted: Vec<u16> = text.encode_utf16().collect();
            let mut state = self.0.borrow_mut();
            let count = inserted.len();
            state.0.splice(at..at + removed, inserted);
            state.1 = at + count;
        }

        fn set_mode(&self, mode: FakeMode) {
            self.0.borrow_mut().2 = mode;
        }
    }

    impl Field for Fake {
        fn reading(&self) -> FieldReading {
            let state = self.0.borrow();
            match state.2 {
                FakeMode::Open => FieldReading::Text {
                    length: state.0.len(),
                    caret: state.1,
                },
                FakeMode::Gone => FieldReading::Gone,
                FakeMode::Secure => FieldReading::Secure,
                FakeMode::SecureInput => FieldReading::SecureInputActive,
            }
        }

        fn text(&self, range: Range<usize>) -> Option<String> {
            let state = self.0.borrow();
            assert_ne!(state.2, FakeMode::Secure, "read a secure field");
            let units = state.0.get(range)?;
            Some(String::from_utf16_lossy(units))
        }
    }

    const PASTED: &str = "fix the pixie importer please";

    fn tracked(prefix: &str) -> (Fake, Tracker<Fake>, Instant) {
        let fake = Fake::new(&format!("{prefix}{PASTED}"));
        let start = Instant::now();
        let mut tracker = Tracker::new(fake.clone(), start);
        tracker.add(9, PASTED.into(), start);
        assert!(tracker.tick(start).is_empty());
        (fake, tracker, start)
    }

    fn settle(tracker: &mut Tracker<Fake>, fake: &Fake, start: Instant) -> Correction {
        fake.set_mode(FakeMode::Gone);
        let mut ended = tracker.tick(start + Duration::from_secs(1));
        assert_eq!(ended.len(), 1);
        ended.remove(0).1
    }

    #[test]
    fn an_edit_inside_the_span_is_the_correction() {
        let (fake, mut tracker, start) = tracked("");
        let pixie = PASTED.find("pixie").unwrap();
        fake.edit(pixie, 5, "Pixiv");
        tracker.tick(start);
        let correction = settle(&mut tracker, &fake, start);
        assert_eq!(correction.status, CorrectionStatus::Edited);
        assert_eq!(correction.text.unwrap(), "fix the Pixiv importer please");
    }

    #[test]
    fn surrounding_text_is_never_captured() {
        let (fake, mut tracker, start) = tracked("my secret preamble ");
        // Typing before and after the paste moves it and adds nothing to it.
        fake.edit(0, 0, "more ");
        tracker.tick(start);
        let end = fake.0.borrow().0.len();
        fake.edit(end, 0, " and a typed ending");
        tracker.tick(start);
        let correction = settle(&mut tracker, &fake, start);
        assert_eq!(correction.status, CorrectionStatus::Unchanged);
        assert_eq!(correction.text.unwrap(), PASTED);
    }

    #[test]
    fn fixing_the_last_word_counts_but_typing_on_does_not() {
        let (fake, mut tracker, start) = tracked("");
        let end = fake.0.borrow().0.len();
        fake.edit(end - 6, 6, "");
        tracker.tick(start);
        let end = fake.0.borrow().0.len();
        fake.edit(end, 0, "now");
        tracker.tick(start);
        let correction = settle(&mut tracker, &fake, start);
        assert_eq!(correction.text.unwrap(), "fix the pixie importer now");
    }

    #[test]
    fn sending_keeps_the_last_reading() {
        let (fake, mut tracker, start) = tracked("");
        fake.edit(0, 3, "mend");
        tracker.tick(start);
        let length = fake.0.borrow().0.len();
        fake.edit(0, length, "");
        let ended = tracker.tick(start + Duration::from_secs(1));
        assert_eq!(
            ended[0].1.text.as_deref(),
            Some("mend the pixie importer please")
        );
    }

    #[test]
    fn an_emptied_composer_ends_the_span_at_its_last_reading() {
        let (fake, mut tracker, start) = tracked("");
        fake.edit(0, 3, "mend");
        tracker.tick(start);
        let length = fake.0.borrow().0.len();
        // Chromium's empty composer holds a single placeholder character.
        fake.edit(0, length, "\n");
        let ended = tracker.tick(start + Duration::from_secs(1));
        assert_eq!(
            ended[0].1.text.as_deref(),
            Some("mend the pixie importer please")
        );
    }

    #[test]
    fn a_second_paste_does_not_join_the_first() {
        let (fake, mut tracker, start) = tracked("");
        let later = "and add tests";
        let end = fake.0.borrow().0.len();
        fake.edit(end, 0, later);
        tracker.add(10, later.into(), start);
        tracker.tick(start);
        fake.set_mode(FakeMode::Gone);
        let ended = tracker.tick(start);
        assert_eq!(ended[0].1.text.as_deref(), Some(PASTED));
        assert_eq!(ended[1].1.text.as_deref(), Some(later));
    }

    #[test]
    fn a_secure_field_is_never_read() {
        let fake = Fake::new(PASTED);
        fake.set_mode(FakeMode::Secure);
        let start = Instant::now();
        let mut tracker = Tracker::new(fake.clone(), start);
        tracker.add(9, PASTED.into(), start);
        let ended = tracker.tick(start);
        assert_eq!(ended[0].1.status, CorrectionStatus::Secure);
        assert_eq!(ended[0].1.text, None);
    }

    #[test]
    fn nothing_is_read_while_secure_input_is_on() {
        let fake = Fake::new(PASTED);
        fake.set_mode(FakeMode::SecureInput);
        let start = Instant::now();
        let mut tracker = Tracker::new(fake.clone(), start);
        tracker.add(9, PASTED.into(), start);
        assert!(tracker.tick(start).is_empty());
        let ended = tracker.tick(start + LOCATE_FOR);
        assert_eq!(ended[0].1.status, CorrectionStatus::Secure);
    }

    #[test]
    fn text_that_is_no_longer_the_paste_is_not_taken() {
        let (fake, mut tracker, start) = tracked("");
        fake.edit(0, PASTED.len(), "completely different words");
        tracker.tick(start);
        let correction = settle(&mut tracker, &fake, start);
        assert_eq!(correction.status, CorrectionStatus::Unchanged);
    }

    #[test]
    fn a_paste_that_cannot_be_found_is_unreadable() {
        let fake = Fake::new("something else entirely");
        let start = Instant::now();
        let mut tracker = Tracker::new(fake, start);
        tracker.add(9, PASTED.into(), start);
        assert!(tracker.tick(start).is_empty());
        let ended = tracker.tick(start + LOCATE_FOR);
        assert_eq!(ended[0].1.status, CorrectionStatus::Unreadable);
    }

    #[test]
    fn utf16_offsets_survive_emoji_before_the_span() {
        let (fake, mut tracker, start) = tracked("😀 ");
        fake.edit(0, 0, "😀");
        tracker.tick(start);
        let correction = settle(&mut tracker, &fake, start);
        assert_eq!(correction.text.unwrap(), PASTED);
    }
}
