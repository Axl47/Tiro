//! What cleanup leaves behind, removed without a model. Scribe keeps discourse
//! markers such as "but yeah" as content; in a prompt to an agent they carry
//! nothing. Also the whole of cleanup when the model can't run, so filler removal
//! never depends on it.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// "..., but yeah, the idea" reads as two sentences once the marker is gone.
static MID_SENTENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[,;]\s*(?:and|but|so)\s+yeah,?\s+(\S)").expect("mid-sentence marker pattern")
});
/// "Yeah, run the tests." / "But yeah, ..." at the start of a sentence.
static SENTENCE_START: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(^|[.!?]\s+)(?:(?:and|but|so)\s+)?(?:yeah|yep),?\s+(\S)")
        .expect("sentence-start marker pattern")
});
/// "... like some things and yeah." at the end.
static TRAILING: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[,\s]+(?:and|but|so)\s+yeah[.!]?\s*$").expect("trailing marker pattern")
});

fn upper(text: &str) -> String {
    text.to_uppercase()
}

/// `text` without hesitations and empty discourse markers.
pub fn strip(text: &str) -> String {
    let text = crate::dictation::filler_words::apply(text);
    let text = MID_SENTENCE.replace_all(&text, |captures: &Captures| {
        format!(". {}", upper(&captures[1]))
    });
    let text = SENTENCE_START.replace_all(&text, |captures: &Captures| {
        format!("{}{}", &captures[1], upper(&captures[2]))
    });
    let trimmed = TRAILING.replace(&text, "");
    let trimmed = trimmed.as_ref();
    if trimmed.len() != text.len() && !trimmed.ends_with(['.', '!', '?']) {
        format!("{trimmed}.")
    } else {
        trimmed.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::strip;

    #[test]
    fn removes_hesitations_and_keeps_sentence_case() {
        assert_eq!(strip("Ah, yeah, run the tests."), "Run the tests.");
        assert_eq!(strip("Um can you look"), "Can you look");
    }

    #[test]
    fn splits_a_sentence_around_a_marker() {
        assert_eq!(
            strip("make it feel fast, but yeah, the idea would be that."),
            "make it feel fast. The idea would be that."
        );
    }

    #[test]
    fn removes_a_trailing_marker() {
        assert_eq!(strip("like some things and yeah."), "like some things.");
    }

    #[test]
    fn leaves_real_uses_alone() {
        for text in [
            "Yeah.",
            "He said yeah to the plan.",
            "The yeah variable is unused.",
            "So we're using Convex.",
        ] {
            assert_eq!(strip(text), text);
        }
    }
}
