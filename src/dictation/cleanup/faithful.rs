//! Keeps the model honest. A small model occasionally mangles a word it should
//! have copied (`dictation` → `dictionation`) or, rarely, rewrites too much. In
//! a prompt for a coding agent a mangled identifier is worse than a filler, so
//! the transcript has the final say:
//!
//! - a new word that is a near copy of a spoken word or vocabulary term is
//!   replaced by the original;
//! - output that lost most of the speech, lost a code-shaped token (a path, a
//!   `snake_case` name, a `--flag`), grew past it, or is largely new words is
//!   rejected outright, and the caller uses the transcript.

use std::collections::{HashMap, HashSet};

/// Why output was not used.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Rejection {
    LostContent { kept: usize, spoken: usize },
    LostToken(String),
    Grew { written: usize, spoken: usize },
    Invented { new: usize, written: usize },
}

/// Words below this length are too easily real fixes (`to` → `too`).
const MIN_REPAIR_LEN: usize = 6;

/// Word spans of `text`: runs of letters and digits, with inner apostrophes.
fn word_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut start = None;
    let is_word = |c: char| c.is_alphanumeric() || c == '\'' || c == '’';
    for (index, c) in text.char_indices() {
        match (is_word(c), start) {
            (true, None) => start = Some(index),
            (false, Some(from)) => {
                spans.push((from, index));
                start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = start {
        spans.push((from, text.len()));
    }
    spans
}

fn words(text: &str) -> Vec<String> {
    word_spans(text)
        .into_iter()
        .map(|(from, to)| text[from..to].to_lowercase())
        .collect()
}

/// Whitespace-separated tokens someone could paste into a terminal: a letter or
/// digit on both sides of `/`, `_` or `.`, or a leading `--`. Compared without
/// the punctuation sentences put around them.
fn code_tokens(text: &str) -> Vec<String> {
    let is_inner = |token: &str, mark: char| {
        token.match_indices(mark).any(|(at, _)| {
            token[..at].ends_with(char::is_alphanumeric)
                && token[at + 1..].starts_with(char::is_alphanumeric)
        })
    };
    text.split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c: char| {
                    matches!(c, ',' | ';' | ':' | '!' | '?' | '.' | '"' | '(' | ')')
                })
                .to_lowercase()
        })
        .filter(|token| {
            token.starts_with("--")
                || is_inner(token, '/')
                || is_inner(token, '_')
                || (is_inner(token, '.') && token.chars().any(char::is_alphabetic))
        })
        .collect()
}

/// Levenshtein distance over characters.
pub(super) fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            let next = (row[j + 1] + 1).min(row[j] + 1).min(diagonal + cost);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// A real grammatical change rather than a mangled copy: `connect` → `connecting`.
fn is_inflection(a: &str, b: &str) -> bool {
    const SUFFIXES: &[&str] = &["s", "es", "d", "ed", "r", "er", "ing", "ly", "'s"];
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    let stem = short
        .strip_suffix('e')
        .filter(|stem| stem.len() >= 3)
        .unwrap_or(short);
    long.strip_prefix(stem)
        .is_some_and(|rest| rest.is_empty() || rest == "e" || SUFFIXES.contains(&rest))
}

/// The closest near copy of `word` in `candidates`, if it is close enough to
/// be a mangling rather than a different word.
fn near_copy<'a>(word: &str, candidates: impl Iterator<Item = &'a str>) -> Option<&'a str> {
    let limit = word.chars().count() / 4;
    candidates
        .filter(|candidate| candidate.chars().count() >= MIN_REPAIR_LEN - 1)
        .filter(|candidate| !is_inflection(word, &candidate.to_lowercase()))
        .map(|candidate| (distance(word, &candidate.to_lowercase()), candidate))
        .filter(|(found, _)| (1..=limit).contains(found))
        .min_by_key(|(found, _)| *found)
        .map(|(_, candidate)| candidate)
}

/// Whether adjoining spoken words spell `word`: `use effect` → `useEffect`.
fn joins_spoken(word: &str, spoken: &[String]) -> bool {
    (0..spoken.len()).any(|start| {
        let mut joined = String::new();
        spoken[start..].iter().take(4).any(|part| {
            joined.push_str(part);
            joined == word
        })
    })
}

/// A replacement in the case its slot was written in.
fn in_case_of(written: &str, original: &str) -> String {
    let mut chars = original.chars();
    match (written.chars().next(), chars.next()) {
        (Some(first), Some(head))
            if first.is_uppercase() && original == original.to_lowercase() =>
        {
            head.to_uppercase().chain(chars).collect()
        }
        _ => original.to_owned(),
    }
}

/// Whether a recognizer's casing says something: `M1`, `ID`, `useEffect`.
fn is_meaningfully_cased(word: &str) -> bool {
    let mut chars = word.chars();
    let starts_upper = chars.next().is_some_and(char::is_uppercase);
    chars.any(|c| c.is_uppercase() || (starts_upper && c.is_ascii_digit()))
}

/// `output` with mangled copies repaired, or why it must not be used.
pub fn reconcile(transcript: &str, output: &str, vocab: &[String]) -> Result<String, Rejection> {
    let spoken = words(transcript);
    // Cleanup lowercases its input, so casing the recognizer heard comes back here.
    let cased: HashMap<String, &str> = word_spans(transcript)
        .into_iter()
        .map(|(from, to)| &transcript[from..to])
        .filter(|word| is_meaningfully_cased(word))
        .map(|word| (word.to_lowercase(), word))
        .collect();
    let spoken_set: HashSet<&str> = spoken.iter().map(String::as_str).collect();
    let vocab_words: Vec<String> = vocab.iter().flat_map(|term| words(term)).collect();
    let vocab_set: HashSet<&str> = vocab_words.iter().map(String::as_str).collect();

    let mut repaired = String::with_capacity(output.len());
    let mut copied_to = 0;
    let mut invented = 0;
    let spans = word_spans(output);
    for &(from, to) in &spans {
        let written = &output[from..to];
        let word = written.to_lowercase();
        let known = spoken_set.contains(word.as_str())
            || vocab_set.contains(word.as_str())
            || word.chars().any(|c| c.is_ascii_digit())
            || joins_spoken(&word, &spoken);
        if known {
            if !is_meaningfully_cased(written)
                && !vocab_set.contains(word.as_str())
                && let Some(original) = cased.get(&word)
            {
                repaired.push_str(&output[copied_to..from]);
                repaired.push_str(original);
                copied_to = to;
            }
            continue;
        }
        let original = (word.chars().count() >= MIN_REPAIR_LEN)
            .then(|| {
                near_copy(&word, vocab.iter().map(String::as_str))
                    .or_else(|| near_copy(&word, spoken.iter().map(String::as_str)))
            })
            .flatten();
        match original {
            Some(original) => {
                repaired.push_str(&output[copied_to..from]);
                repaired.push_str(&in_case_of(written, original));
                copied_to = to;
            }
            None => invented += 1,
        }
    }
    repaired.push_str(&output[copied_to..]);

    // A retraction can legitimately drop one of these, and then this costs that
    // dictation its cleanup. A silently missing path segment costs far more.
    let lowered = repaired.to_lowercase();
    if let Some(lost) = code_tokens(transcript)
        .into_iter()
        .find(|token| !lowered.contains(token.as_str()))
    {
        return Err(Rejection::LostToken(lost));
    }

    let (spoken_count, written) = (spoken.len(), spans.len());
    // Long dictations with real fillers and retractions still keep most words.
    if spoken_count >= 10 && written * 100 < spoken_count * 35 {
        return Err(Rejection::LostContent {
            kept: written,
            spoken: spoken_count,
        });
    }
    if written > spoken_count + spoken_count / 4 + 8 {
        return Err(Rejection::Grew {
            written,
            spoken: spoken_count,
        });
    }
    if written >= 6 && invented * 100 > written * 30 {
        return Err(Rejection::Invented {
            new: invented,
            written,
        });
    }
    Ok(repaired)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reconciled(spoken: &str, output: &str, vocab: &[&str]) -> Result<String, Rejection> {
        let vocab: Vec<String> = vocab.iter().map(|term| (*term).to_owned()).collect();
        reconcile(spoken, output, &vocab)
    }

    #[test]
    fn repairs_a_mangled_path_segment() {
        assert_eq!(
            reconciled(
                "the bug is in src slash dictation slash processor dot rs",
                "The bug is in src/dictionation/processor.rs.",
                &[],
            )
            .unwrap(),
            "The bug is in src/dictation/processor.rs."
        );
    }

    #[test]
    fn repairs_a_mangled_vocabulary_term_to_its_exact_spelling() {
        assert_eq!(
            reconciled(
                "i could use it like open super whisper for example",
                "I could use it like OpenSupersWhisper, for example.",
                &["OpenSuperWhisper"],
            )
            .unwrap(),
            "I could use it like OpenSuperWhisper, for example."
        );
    }

    #[test]
    fn keeps_a_vocabulary_correction_of_a_mishearing() {
        assert_eq!(
            reconciled(
                "use the zustin store",
                "Use the Zustand store.",
                &["Zustand"]
            )
            .unwrap(),
            "Use the Zustand store."
        );
    }

    #[test]
    fn keeps_joined_words_digits_and_grammatical_changes() {
        for (spoken, output) in [
            ("the use effect hook", "The useEffect hook."),
            ("set it to two hundred fifty", "Set it to 250."),
            ("model id quen three point five", "Model ID Quen3.5."),
            ("just connect to it", "Just connecting to it."),
        ] {
            assert_eq!(reconciled(spoken, output, &[]).unwrap(), output);
        }
    }

    #[test]
    fn restores_casing_the_recognizer_heard() {
        assert_eq!(
            reconciled(
                "why does it fail on the M1 with model ID four",
                "Why does it fail on the m1 with model id 4?",
                &[]
            )
            .unwrap(),
            "Why does it fail on the M1 with model ID 4?"
        );
    }

    #[test]
    fn an_empty_answer_to_pure_filler_is_fine() {
        assert_eq!(reconciled("um", "", &[]).unwrap(), "");
    }

    #[test]
    fn rejects_output_that_lost_the_speech() {
        let spoken = "we should move the webhook handler into its own module and test it";
        assert!(matches!(
            reconciled(spoken, "Move it.", &[]),
            Err(Rejection::LostContent { .. })
        ));
    }

    #[test]
    fn rejects_output_that_dropped_a_code_token() {
        assert_eq!(
            reconciled(
                "the bug is in src/dictation/processor.rs and handle_request",
                "The bug is in src/processor.rs and handle_request.",
                &[],
            ),
            Err(Rejection::LostToken("src/dictation/processor.rs".into()))
        );
    }

    #[test]
    fn code_tokens_ignore_sentence_punctuation_and_prose() {
        assert_eq!(
            reconciled(
                "um run cargo test --locked, and check api_key.",
                "Run cargo test --locked and check api_key.",
                &[]
            )
            .unwrap(),
            "Run cargo test --locked and check api_key."
        );
        assert!(code_tokens("Yes. It works, and/or not.").contains(&"and/or".to_owned()));
        assert!(code_tokens("Set it to 3.5 and stop.").is_empty());
    }

    #[test]
    fn rejects_an_answer_instead_of_a_cleanup() {
        let answer = "Sure! The capital of France is Paris, a lovely city known for its museums, \
                      cafes, and the famous tower that everyone visits every single year.";
        assert!(matches!(
            reconciled("what is the capital of france", answer, &[]),
            Err(Rejection::Grew { .. })
        ));
    }

    #[test]
    fn rejects_a_rewrite_into_new_words() {
        assert!(matches!(
            reconciled(
                "please make the login form faster today",
                "Optimize authentication interface performance immediately, thanks.",
                &[]
            ),
            Err(Rejection::Invented { .. })
        ));
    }
}
