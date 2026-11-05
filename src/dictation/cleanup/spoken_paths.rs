//! Writes spoken paths as paths before the model sees them. Scribe copies
//! `src/dictation/processor.rs` faithfully but drops segments from "src slash
//! dictation slash processor dot rs", so the model never gets the chance.
//!
//! Only unambiguous shapes convert: two or more slashes, or one with a file
//! extension. "The slash command" and "and slash or" stay as speech.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// A word, then `slash word` at least once, then optionally `dot ext`.
/// Recognizers sometimes write the first separator already: "src/slash foo".
static SPOKEN_PATH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b([\w.-]+)/?((?:\s*\bslash\s+[\w.-]+)+)(\s+dot\s+[a-z0-9]{1,5}\b)?")
        .expect("spoken path pattern")
});
static SEPARATOR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s*\bslash\s+").expect("spoken separator pattern"));
static EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\s+dot\s+").expect("spoken extension pattern"));

/// `transcript` with its spoken paths written out.
pub fn write_out(transcript: &str) -> String {
    SPOKEN_PATH
        .replace_all(transcript, |captures: &Captures| {
            let segments = &captures[2];
            let extension = captures.get(3).map_or("", |found| found.as_str());
            let slashes = SEPARATOR.find_iter(segments).count();
            if slashes < 2 && extension.is_empty() {
                return captures[0].to_owned();
            }
            format!(
                "{}{}{}",
                &captures[1],
                SEPARATOR.replace_all(segments, "/"),
                EXTENSION.replace(extension, ".")
            )
        })
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::write_out;

    #[test]
    fn writes_out_spoken_paths() {
        for (spoken, written) in [
            (
                "in src slash dictation slash processor dot rs like",
                "in src/dictation/processor.rs like",
            ),
            (
                "in src/slash dictation slash processor dot rs",
                "in src/dictation/processor.rs",
            ),
            (
                "open native slash scribe slash sources",
                "open native/scribe/sources",
            ),
            (
                "the readme slash notes dot md file",
                "the readme/notes.md file",
            ),
        ] {
            assert_eq!(write_out(spoken), written);
        }
    }

    #[test]
    fn leaves_ordinary_speech_alone() {
        for speech in [
            "the slash command is broken",
            "use and slash or here",
            "connect dot notation",
            "the dot rs file",
        ] {
            assert_eq!(write_out(speech), speech);
        }
    }
}
