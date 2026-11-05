//! Picks the terms worth showing Vedu Scribe for one transcript: the model's
//! `[vocab: …]` header writes a term's exact spelling where the speech has it or
//! a mishearing of it. The model tolerates unrelated terms, so this pre-match
//! may be loose, but it allows only twelve.

use crate::dictation::cleanup::faithful::distance;
use crate::dictation::vocabulary::VocabularyEntry;

const LIMIT: usize = 12;
/// Spoken-word windows to compare a term to: "tan stack start".
const MAX_WINDOW: usize = 4;

/// What people dictating prompts for coding agents say and recognizers mangle.
/// The person's own vocabulary always ranks ahead of these.
const DEVELOPER_TERMS: &[&str] = &[
    "API", "ArkType", "Astro", "AWS", "Axios", "backend", "Biome", "Bun", "Claude", "Clerk",
    "CLI", "Codex", "Convex", "CSS", "debounce", "Deno", "Docker", "Dokploy", "ESLint", "Expo",
    "FastAPI", "frontend", "Gemini", "GitHub", "GraphQL", "Hono", "HTML", "HTTP", "JavaScript",
    "JSON", "JSX", "Kotlin", "Kubernetes", "LAN", "localhost", "macOS", "Metal", "middleware",
    "MLX", "Neovim", "Next.js", "Nginx", "Node.js", "npm", "OAuth", "OpenAI", "Parakeet",
    "pnpm", "PostgreSQL", "Prisma", "PR", "Python", "React", "README", "Redis", "refactor",
    "repo", "Rust", "SDK", "SQLite", "SSH", "Supabase", "Svelte", "Swift", "Tailwind",
    "TanStack", "TanStack Start", "Tauri", "TSX", "TypeScript", "useEffect", "useMemo",
    "useRef", "useState", "Vercel", "Vite", "Vitest", "Vulkan", "webhook", "WebSocket",
    "Whisper", "YAML", "Zod", "Zustand",
];

/// Letters and digits of `text`, lowercased: what a term sounds nearest to.
fn letters(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// How far `term` is from the nearest run of spoken words: 0 for an exact
/// hit, otherwise its edit distance as a share of its length, in hundredths.
/// `None` when nothing spoken resembles it.
fn closeness(term: &str, sounds_like: &[String], spoken: &[String]) -> Option<usize> {
    let wanted = letters(term);
    if wanted.is_empty() {
        return None;
    }
    let spoken_like: Vec<String> = sounds_like.iter().map(|hint| letters(hint)).collect();
    let mut best: Option<usize> = None;
    for start in 0..spoken.len() {
        let mut window = String::new();
        for part in spoken[start..].iter().take(MAX_WINDOW) {
            window.push_str(part);
            if window == wanted || spoken_like.contains(&window) {
                return Some(0);
            }
            // Same first letter: recognizers rarely miss the onset, and it
            // keeps "code" away from "Node.js".
            if window.chars().next() != wanted.chars().next() {
                continue;
            }
            let share = distance(&window, &wanted) * 100 / wanted.chars().count();
            if share <= 34 {
                best = Some(best.map_or(share, |found| found.min(share)));
            }
        }
    }
    best
}

/// The terms to send for `transcript`, the person's own first, then closest.
pub fn select(transcript: &str, entries: &[VocabularyEntry]) -> Vec<String> {
    let spoken: Vec<String> = transcript
        .split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(str::to_lowercase)
        .collect();
    let own = entries
        .iter()
        .map(|entry| (entry.term.as_str(), entry.sounds_like.as_slice()));
    let built_in = DEVELOPER_TERMS.iter().map(|term| (*term, &[] as &[String]));

    let mut scored: Vec<(bool, usize, &str)> = own
        .map(|entry| (true, entry))
        .chain(built_in.map(|entry| (false, entry)))
        .filter_map(|(mine, (term, hints))| {
            closeness(term, hints, &spoken).map(|score| (!mine, score, term))
        })
        .collect();
    scored.sort_by_key(|(not_mine, score, _)| (*not_mine, *score));

    let mut chosen: Vec<String> = Vec::new();
    for (_, _, term) in scored {
        if chosen.len() == LIMIT {
            break;
        }
        if !chosen.iter().any(|seen| seen.eq_ignore_ascii_case(term)) {
            chosen.push(term.to_owned());
        }
    }
    chosen
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(term: &str, sounds_like: &[&str]) -> VocabularyEntry {
        VocabularyEntry {
            term: term.into(),
            sounds_like: sounds_like.iter().map(|hint| (*hint).to_owned()).collect(),
        }
    }

    #[test]
    fn finds_mishearings_of_built_in_terms() {
        let terms = select("use archtype and the zustin store with zot", &[]);
        for expected in ["ArkType", "Zustand", "Zod"] {
            assert!(terms.iter().any(|term| term == expected), "{terms:?}");
        }
    }

    #[test]
    fn finds_terms_spoken_as_separate_words() {
        let terms = select("the use effect in tan stack start", &[]);
        for expected in ["useEffect", "TanStack Start"] {
            assert!(terms.iter().any(|term| term == expected), "{terms:?}");
        }
    }

    #[test]
    fn the_persons_own_terms_rank_first_and_use_their_sounds_like() {
        let entries = [entry("OpenSuperWhisper", &[]), entry("Tiro", &["tea row"])];
        let terms = select("rust and open super whisper in tea row", &entries);
        assert_eq!(&terms[..2], ["OpenSuperWhisper", "Tiro"]);
    }

    #[test]
    fn stays_quiet_about_terms_nobody_said() {
        assert!(select("please run the tests and tell me", &[]).is_empty());
    }

    #[test]
    fn never_sends_more_than_the_model_allows() {
        let speech = DEVELOPER_TERMS.join(" ");
        assert_eq!(select(&speech, &[]).len(), LIMIT);
    }
}
