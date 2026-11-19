//! Phonon's refiner contract, from its `phonon_refiner.json` and
//! `sidecar/polish_server.py` (`infatoshi/phonon`): the exact system turn it was
//! trained on, the writing mode for where the text goes, the vocabulary line,
//! the output budget, and the two checks Phonon runs on every answer.

use std::collections::HashSet;

const INSTRUCTION: &str = "Rewrite the raw dictation transcript into the exact text the speaker \
intended. Fix recognition errors and technical terms, keep the speaker's wording and casing \
style, do not add or drop content.";

/// Phonon's writing modes. Its fourth, `casual` (a chat message), writes chat-style
/// lowercase, which lost most phone dictations in Tiro's blind comparison, so chat
/// apps get `Minimal` (`benchmarks/cleanup/README.md`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Mode {
    /// Notes and documents; anything Tiro can't place.
    Minimal,
    /// An email or document.
    Formal,
    /// An instruction to an AI assistant: terminals, editors, coding agents.
    Prompt,
}

impl Mode {
    /// The mode for the app the text goes to, by its name.
    pub fn for_app(application: Option<&str>) -> Self {
        const PROMPT: &[&str] = &[
            "t3 code",
            "annex",
            "terminal",
            "iterm",
            "ghostty",
            "warp",
            "kitty",
            "alacritty",
            "wezterm",
            "visual studio code",
            "code",
            "cursor",
            "zed",
            "xcode",
            "chatgpt",
            "claude",
            "codex",
            "opencode",
        ];
        const FORMAL: &[&str] = &["mail", "outlook", "spark", "superhuman"];
        let Some(name) = application.map(str::to_lowercase) else {
            return Self::Minimal;
        };
        let names = |list: &[&str]| {
            list.iter()
                .any(|app| name == *app || name.starts_with(&format!("{app} ")))
        };
        if names(PROMPT) {
            Self::Prompt
        } else if names(FORMAL) {
            Self::Formal
        } else {
            Self::Minimal
        }
    }

    fn line(self) -> &'static str {
        match self {
            Self::Minimal => "Writing mode: minimal (notes and documents).",
            Self::Formal => "Writing mode: formal (an email or document).",
            Self::Prompt => "Writing mode: prompt (an instruction to an AI assistant).",
        }
    }
}

/// How many vocabulary terms the refiner was evaluated with for a transcript
/// of `words` words.
pub fn vocabulary_cap(words: usize) -> usize {
    (3 * words).clamp(30, 80)
}

/// The system turn: instruction, mode line, then the vocabulary line.
pub fn system(mode: Mode, vocab: &[String]) -> String {
    let mut system = format!("{INSTRUCTION}\n{}", mode.line());
    if !vocab.is_empty() {
        system.push_str("\nVocabulary the speaker may use: ");
        system.push_str(&vocab.join(", "));
    }
    system
}

/// At most 1.5 times the transcript's tokens plus 32 new tokens.
pub fn budget(tokens: usize) -> usize {
    (tokens * 3 / 2 + 32).clamp(8, 2048)
}

/// Phonon's length guard: an answer over twice the spoken words, or (past
/// eight words) under 0.4 of them, ran away or collapsed.
pub fn plausible_length(spoken: &str, output: &str) -> bool {
    let (spoken, written) = (
        spoken.split_whitespace().count(),
        output.split_whitespace().count(),
    );
    spoken == 0 || (written <= 2 * spoken && (spoken <= 8 || written * 5 >= spoken * 2))
}

fn letters(word: &str) -> String {
    word.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Reads as a term, as Phonon's `TERM_SHAPE` does: a capital at both ends, a
/// digit, or an inner hyphen (`MTK`, `M1`, `MMLU-Pro`).
fn term_shaped(core: &str) -> bool {
    let capped = core.chars().count() >= 2
        && core.starts_with(|c: char| c.is_ascii_uppercase())
        && core.ends_with(|c: char| c.is_ascii_uppercase() || c.is_ascii_digit());
    let hyphen = core.trim_matches('-').contains('-');
    capped || core.chars().any(|c| c.is_ascii_digit()) || hyphen
}

/// Phonon's term restoration: where the refiner swapped a one-word vocabulary
/// term the speaker said for another term they didn't ("native mtp" → "native
/// MTK"), the spoken term goes back. Only one-for-one swaps of term-shaped words.
pub fn restore_terms(spoken: &str, output: &str, vocab: &[String]) -> String {
    let terms: Vec<(String, &str)> = vocab
        .iter()
        .map(|term| term.trim())
        .filter(|term| !term.is_empty() && !term.contains(' '))
        .map(|term| (letters(term), term))
        .collect();
    if terms.is_empty() {
        return output.to_owned();
    }
    let said: Vec<String> = spoken.split_whitespace().map(letters).collect();
    let said_set: HashSet<&str> = said.iter().map(String::as_str).collect();
    let spans: Vec<(usize, usize)> = output
        .split_whitespace()
        .map(|word| {
            let start = word.as_ptr() as usize - output.as_ptr() as usize;
            (start, start + word.len())
        })
        .collect();
    let written: Vec<String> = spans.iter().map(|&(a, b)| letters(&output[a..b])).collect();
    let mut fixes = Vec::new();
    for (i, j) in one_for_one_swaps(&said, &written) {
        let Some((_, term)) = terms.iter().find(|(key, _)| *key == said[i]) else {
            continue;
        };
        let word = &output[spans[j].0..spans[j].1];
        let core_start = word.find(char::is_alphanumeric).unwrap_or(0);
        let core_end = word.rfind(char::is_alphanumeric).map_or(word.len(), |at| {
            at + word[at..].chars().next().map_or(1, char::len_utf8)
        });
        let core = &word[core_start..core_end];
        if !said_set.contains(written[j].as_str())
            && written[j] != letters(term)
            && term_shaped(core)
        {
            fixes.push((spans[j].0 + core_start, spans[j].0 + core_end, *term));
        }
    }
    let mut restored = output.to_owned();
    for (from, to, term) in fixes.into_iter().rev() {
        restored.replace_range(from..to, term);
    }
    restored
}

/// Index pairs where an alignment of `a` and `b` replaces words one for one:
/// a longest-common-subsequence alignment, then the equal-length gaps between
/// matches.
fn one_for_one_swaps(a: &[String], b: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut gap) = (0, 0, (0, 0));
    let mut pairs = Vec::new();
    let close = |gap: (usize, usize), i: usize, j: usize, pairs: &mut Vec<(usize, usize)>| {
        if i - gap.0 == j - gap.1 {
            pairs.extend((gap.0..i).zip(gap.1..j));
        }
    };
    while i < n && j < m {
        if a[i] == b[j] {
            close(gap, i, j, &mut pairs);
            i += 1;
            j += 1;
            gap = (i, j);
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    close(gap, n, m, &mut pairs);
    pairs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vocab(terms: &[&str]) -> Vec<String> {
        terms.iter().map(|term| (*term).to_owned()).collect()
    }

    #[test]
    fn the_system_turn_is_the_trained_one() {
        assert_eq!(
            system(Mode::Prompt, &vocab(&["Zustand", "pnpm"])),
            format!(
                "{INSTRUCTION}\nWriting mode: prompt (an instruction to an AI assistant).\n\
                 Vocabulary the speaker may use: Zustand, pnpm"
            )
        );
        assert!(!system(Mode::Minimal, &[]).contains("Vocabulary"));
    }

    #[test]
    fn modes_follow_the_app() {
        assert_eq!(Mode::for_app(Some("T3 Code (Nightly)")), Mode::Prompt);
        assert_eq!(Mode::for_app(Some("Ghostty")), Mode::Prompt);
        assert_eq!(Mode::for_app(Some("Mail")), Mode::Formal);
        assert_eq!(Mode::for_app(Some("WhatsApp")), Mode::Minimal);
        assert_eq!(Mode::for_app(Some("Codemagic")), Mode::Minimal);
        assert_eq!(Mode::for_app(None), Mode::Minimal);
    }

    #[test]
    fn the_length_guard_catches_runaways_and_collapses() {
        assert!(plausible_length("one two three", "One, two, three."));
        assert!(!plausible_length("a b", "a b c d e"));
        assert!(!plausible_length("a b c d e f g h i j", "a b c"));
        assert!(plausible_length("um yeah", "Yeah."));
    }

    #[test]
    fn a_swapped_term_the_speaker_said_goes_back() {
        assert_eq!(
            restore_terms(
                "use the native mtp path",
                "Use the native MTK path.",
                &vocab(&["MTP"])
            ),
            "Use the native MTP path."
        );
        // An ordinary word the model changed is left alone.
        assert_eq!(
            restore_terms("the mtp path", "The main path.", &vocab(&["MTP"])),
            "The main path."
        );
        // Nothing said matches a term.
        assert_eq!(
            restore_terms("run it", "Run M1.", &vocab(&["MTP"])),
            "Run M1."
        );
    }

    #[test]
    fn the_budget_scales_with_the_transcript() {
        assert_eq!(budget(0), 32);
        assert_eq!(budget(100), 182);
        assert_eq!(budget(10_000), 2048);
        assert_eq!(vocabulary_cap(5), 30);
        assert_eq!(vocabulary_cap(20), 60);
        assert_eq!(vocabulary_cap(500), 80);
    }
}
