//! Cleanup while the speaker is still talking: each sentence is cleaned once
//! text after it has begun, so the finish only waits on the last one. Phone
//! streams (`lan::stream`) and the Mac's own dictations (`desktop`) share it.
//! Measured against cleaning the whole transcript in
//! `benchmarks/cleanup/README.md`.

use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// Where each sentence of `text` starts and ends: split after `.`, `?` or `!`
/// and the whitespace that follows. Ranges exclude surrounding whitespace.
fn sentences(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut found = Vec::new();
    let mut start = None;
    let mut chars = text.char_indices().peekable();
    while let Some((at, c)) = chars.next() {
        if c.is_whitespace() {
            continue;
        }
        let begun = *start.get_or_insert(at);
        let next_is_space = chars.peek().is_some_and(|(_, next)| next.is_whitespace());
        if matches!(c, '.' | '?' | '!') && next_is_space {
            found.push(begun..at + c.len_utf8());
            start = None;
        }
    }
    if let Some(begun) = start {
        found.push(begun..text.trim_end().len());
    }
    found
}

/// Openings that take back what came before ("No, five attempts"). Cleanup can
/// only drop the retracted words when it sees both, so a sentence opening with
/// one is never cleaned apart from the sentence before it. Mirrored in
/// `benchmarks/cleanup/bench.py`.
const CORRECTIONS: &[&str] = &[
    "no",
    "nope",
    "wait",
    "actually",
    "sorry",
    "oops",
    "scratch",
    "i mean",
    "or",
    "rather",
    "not",
    "instead",
    "make that",
    "change that",
    "never mind",
];

fn opens_with_correction(sentence: &str) -> bool {
    let opening = sentence
        .split(|c: char| !(c.is_alphabetic() || c == '\''))
        .filter(|word| !word.is_empty())
        .take(2)
        .map(str::to_lowercase)
        .collect::<Vec<_>>()
        .join(" ");
    CORRECTIONS
        .iter()
        .any(|cue| opening == *cue || opening.starts_with(&format!("{cue} ")))
}

/// The pieces of `text` cleanup can take one at a time: its sentences, with a
/// sentence that opens with a correction joined to the one before.
fn units(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut found: Vec<std::ops::Range<usize>> = Vec::new();
    for range in sentences(text) {
        match found.last_mut() {
            Some(last) if opens_with_correction(&text[range.clone()]) => last.end = range.end,
            _ => found.push(range),
        }
    }
    found
}

/// The first unit of `text`, if it has one.
pub fn first_unit(text: &str) -> Option<&str> {
    units(text).into_iter().next().map(|range| &text[range])
}

/// A recording's sentences cleaned while it still records. Each unit (see
/// [`units`]) is cleaned on its own once the one after it has begun. Measured
/// against cleaning the whole transcript in `benchmarks/cleanup/README.md`.
#[derive(Default)]
pub struct Precleaned {
    /// Set when the finish starts, or a cleanup failed: no new unit starts.
    closing: AtomicBool,
    /// `(raw, cleaned)` in transcript order. Held while a sentence cleans, so
    /// the finish waits for one already under way instead of repeating it.
    done: Mutex<Vec<(String, String)>>,
    /// How many units the finish took from `done`.
    reused: AtomicUsize,
}

impl Precleaned {
    /// Starts no new unit; one under way still finishes.
    pub fn close(&self) {
        self.closing.store(true, Ordering::Release);
    }

    /// How many units the finish reused.
    pub fn reused(&self) -> usize {
        self.reused.load(Ordering::Acquire)
    }

    /// Cleans the units of `committed` that are finished and not cleaned yet.
    /// A unit is finished once the next has three words, enough to tell
    /// whether it opens with a correction. A unit that changed replaces the old
    /// one and those after it. A failed cleanup leaves the rest to the finish.
    pub fn catch_up(&self, committed: &str, mut cleanup: impl FnMut(&str) -> Option<String>) {
        let mut done = self.done.lock().unwrap_or_else(|error| error.into_inner());
        let mut finished = units(committed);
        if finished
            .pop()
            .is_some_and(|next| committed[next].split_whitespace().count() < 3)
        {
            finished.pop();
        }
        for (index, range) in finished.into_iter().enumerate() {
            let sentence = &committed[range];
            if done.get(index).is_some_and(|(raw, _)| raw == sentence) {
                continue;
            }
            if self.closing.load(Ordering::Acquire) {
                return;
            }
            done.truncate(index);
            let Some(cleaned) = cleanup(sentence) else {
                self.closing.store(true, Ordering::Release);
                return;
            };
            done.push((sentence.to_owned(), cleaned));
        }
    }

    /// The clean text of `transcript`: the leading sentences cleaned already,
    /// then the rest through `cleanup` in one go (raw if it fails).
    pub fn finish(&self, transcript: &str, cleanup: impl FnOnce(&str) -> Option<String>) -> String {
        self.closing.store(true, Ordering::Release);
        let done = self.done.lock().unwrap_or_else(|error| error.into_inner());
        let ranges = units(transcript);
        let reused = ranges
            .iter()
            .zip(done.iter())
            .take_while(|(range, (raw, _))| transcript[(*range).clone()] == **raw)
            .count();
        self.reused.store(reused, Ordering::Release);
        let rest = ranges
            .get(reused)
            .map_or("", |range| &transcript[range.start..]);
        let rest = (!rest.is_empty()).then(|| cleanup(rest).unwrap_or_else(|| rest.to_owned()));
        done[..reused]
            .iter()
            .map(|(_, cleaned)| cleaned.clone())
            .chain(rest)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_split_after_end_punctuation_and_space() {
        let text = " So, the plan.  Is it 3.5? Yes! then we";
        let found: Vec<&str> = sentences(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(found, ["So, the plan.", "Is it 3.5?", "Yes!", "then we"]);
        assert!(sentences("  ").is_empty());
    }

    fn cleaned(text: &str) -> Option<String> {
        Some(format!("<{text}>"))
    }

    #[test]
    fn a_correction_stays_with_the_sentence_it_corrects() {
        let text = "Use three attempts. No actually five. Then stop. Actually, never mind.";
        let found: Vec<&str> = units(text).into_iter().map(|r| &text[r]).collect();
        assert_eq!(
            found,
            [
                "Use three attempts. No actually five.",
                "Then stop. Actually, never mind."
            ]
        );
        // "Nothing" and "Order" aren't "no" and "or".
        assert_eq!(units("Go. Nothing else. Order it.").len(), 3);
    }

    #[test]
    fn cleans_only_units_the_next_has_begun_after() {
        let precleaned = Precleaned::default();
        // Two words can't yet tell "No, wait" from "Not now, …".
        precleaned.catch_up("One. Two three", cleaned);
        assert!(precleaned.done.lock().unwrap().is_empty());
        precleaned.catch_up("One. Two three four. Five six seven", cleaned);
        let done = precleaned.done.lock().unwrap().clone();
        assert_eq!(
            done,
            [
                ("One.".to_owned(), "<One.>".to_owned()),
                ("Two three four.".to_owned(), "<Two three four.>".to_owned())
            ]
        );
    }

    #[test]
    fn the_finish_reuses_matching_sentences_and_cleans_the_rest_at_once() {
        let precleaned = Precleaned::default();
        precleaned.catch_up("One. Two. Three and four", cleaned);
        let text = precleaned.finish("One. Two. Three and four. Five", |rest| {
            assert_eq!(rest, "Three and four. Five");
            Some("Rest.".into())
        });
        assert_eq!(text, "<One.> <Two.> Rest.");
        assert_eq!(precleaned.done.lock().unwrap().len(), 2);
    }

    #[test]
    fn a_sentence_the_final_changed_is_cleaned_again() {
        let precleaned = Precleaned::default();
        precleaned.catch_up("One. Too. Three and four", cleaned);
        let text = precleaned.finish("One. Two. Three", |rest| {
            assert_eq!(rest, "Two. Three");
            None
        });
        assert_eq!(text, "<One.> Two. Three");
    }

    #[test]
    fn a_failed_cleanup_stops_cleaning_early() {
        let precleaned = Precleaned::default();
        precleaned.catch_up("One. Two three four", |_| None);
        precleaned.catch_up("One. Two three four. Five six", |_| panic!("gave up"));
        let text = precleaned.finish("One. Two", |rest| Some(format!("<{rest}>")));
        assert_eq!(text, "<One. Two>");
    }

    #[test]
    fn nothing_starts_once_the_finish_has() {
        let precleaned = Precleaned::default();
        assert_eq!(precleaned.finish("", |_| panic!("no cleanup")), "");
        precleaned.catch_up("One. Two three four", |_| panic!("too late"));
    }
}
