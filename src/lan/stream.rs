//! `POST /dictate/stream`: a dictation transcribed while the phone records, on
//! one TLS connection. After the request head the phone sends frames (one
//! type byte, a big-endian `u32` length, the payload):
//!
//! ```text
//! 'A' 16 kHz s16le audio, as it records · 'F' finish · 'C' cancel
//! ```
//!
//! and reads newline-delimited JSON back:
//!
//! ```text
//! {"type":"partial","text":…}  provisional, for the phone's pill only
//! {"type":"final","text":…,"transcript":…,"entry":…,"corrections":…}
//!                              the cleaned text to type, and its History entry
//! {"type":"error","code":…}
//! ```
//!
//! The Mac side is the loopback `/streams` API (`docs/streams.md`): its windows,
//! its yielding to the Mac's own dictation, and its one-live-stream limit. A
//! refused create (`409 stream-busy`, `503 model-not-loaded`) is answered as
//! that status before any frame, and the phone sends the whole clip to
//! `/dictate` instead.
//!
//! Cleanup starts before the release, too: each sentence is cleaned once text
//! the stream has committed follows it ([`Precleaned`]), so the finish only
//! waits on the last one.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::loopback::Loopback;

pub(super) type Tls = rustls::StreamOwned<rustls::ServerConnection, TcpStream>;

/// Matches the loopback API's audio request limit.
const MAX_FRAME: usize = 256 * 1024;
/// How often the connection is checked for partials to send between frames.
const TICK: Duration = Duration::from_millis(50);
/// A phone that sends nothing for this long is gone.
const IDLE: Duration = Duration::from_secs(15);
const REQUEST: Duration = Duration::from_secs(5);
/// Twice the events' keep-alive interval.
const EVENTS: Duration = Duration::from_secs(30);
/// The loopback finish waits up to 120 s behind the Mac's own dictation.
const FINISH: Duration = Duration::from_secs(130);

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Frame {
    Audio(Vec<u8>),
    Finish,
    Cancel,
}

/// Takes the first whole frame off `buffer`; `Err` for a frame no phone sends.
pub(super) fn next_frame(buffer: &mut Vec<u8>) -> Result<Option<Frame>, ()> {
    let Some(&kind) = buffer.first() else {
        return Ok(None);
    };
    let Some(length) = buffer.get(1..5) else {
        return Ok(None);
    };
    let length = u32::from_be_bytes(length.try_into().map_err(|_| ())?) as usize;
    if length > MAX_FRAME {
        return Err(());
    }
    if buffer.len() < 5 + length {
        return Ok(None);
    }
    let payload: Vec<u8> = buffer.drain(..5 + length).skip(5).collect();
    match kind {
        b'A' if payload.len() % 2 == 0 => Ok(Some(Frame::Audio(payload))),
        b'F' => Ok(Some(Frame::Finish)),
        b'C' => Ok(Some(Frame::Cancel)),
        _ => Err(()),
    }
}

/// The live text so far, from `/streams` events: finished segments, then the
/// window being transcribed.
#[derive(Default)]
pub(super) struct Live {
    segments: BTreeMap<u64, String>,
    /// The window being transcribed: its segment, stable text, and tentative text.
    partial: Option<(u64, String, String)>,
    shown: String,
}

impl Live {
    /// The new live text when `event` changed it.
    pub fn apply(&mut self, event: &serde_json::Value) -> Option<String> {
        let segment = event.get("segment")?.as_u64()?;
        match event.get("type")?.as_str()? {
            "partial" => {
                let stable = event.get("stable")?.as_str()?;
                let tentative = event
                    .get("tentative")
                    .and_then(|t| t.as_str())
                    .unwrap_or("");
                self.partial = Some((segment, stable.to_owned(), tentative.to_owned()));
            }
            "segment" => {
                self.segments
                    .insert(segment, event.get("text")?.as_str()?.to_owned());
                if self.partial.as_ref().is_some_and(|(at, ..)| *at <= segment) {
                    self.partial = None;
                }
            }
            _ => return None,
        }
        let pending = self
            .partial
            .iter()
            .map(|(_, stable, tentative)| format!("{stable}{tentative}"));
        let text = joined(self.segments.values().cloned().chain(pending));
        (text != self.shown).then(|| {
            self.shown.clone_from(&text);
            text
        })
    }

    /// Text the stream won't revise: finished segments, then the stable part
    /// of the window being transcribed.
    pub fn committed(&self) -> String {
        let stable = self.partial.iter().map(|(_, stable, _)| stable.clone());
        joined(self.segments.values().cloned().chain(stable))
    }
}

fn joined(pieces: impl Iterator<Item = String>) -> String {
    pieces
        .map(|piece| piece.trim().to_owned())
        .filter(|piece| !piece.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

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

/// A stream's sentences cleaned while the phone still records. Each unit (see
/// [`units`]) is cleaned on its own once the one after it has begun. Measured
/// against cleaning the whole transcript in `benchmarks/cleanup/README.md`.
#[derive(Default)]
struct Precleaned {
    /// Set when the finish starts, or a cleanup failed: no new unit starts.
    closing: AtomicBool,
    /// `(raw, cleaned)` in transcript order. Held while a sentence cleans, so
    /// the finish waits for one already under way instead of repeating it.
    done: Mutex<Vec<(String, String)>>,
}

impl Precleaned {
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

pub(super) fn serve(client: &mut Tls, leftover: Vec<u8>, loopback: Loopback, peer: SocketAddr) {
    let create = br#"{"sampleFormat":"s16le","sampleRate":16000}"#;
    let created = loopback.request(
        "POST",
        "/streams",
        &[],
        Some(("application/json", create)),
        REQUEST,
    );
    let (id, owner) = match created {
        Some((201, body)) => match ids(&body) {
            Some(ids) => ids,
            None => return refuse(client, 502, br#"{"code":"tiro-unreachable"}"#),
        },
        Some((status, body)) => return refuse(client, status, &body),
        None => return refuse(client, 503, br#"{"code":"tiro-not-ready"}"#),
    };
    let stream = Stream {
        loopback,
        id,
        owner,
        timing: Default::default(),
        precleaned: Default::default(),
        heard: std::cell::RefCell::new(Heard::new(crate::labs::keep_recordings())),
        started: Instant::now(),
    };
    if client
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: application/x-ndjson\r\nConnection: close\r\n\r\n",
        )
        .and_then(|()| client.flush())
        .is_err()
    {
        return stream.cancel();
    }
    let (partials, live) = mpsc::channel();
    stream.relay_partials(partials);
    match stream.receive(client, leftover, &live) {
        Ok(()) => stream.finish(client),
        Err(Some(code)) => {
            tracing::info!(%peer, code, "LAN dictation stream ended");
            stream.cancel();
            send(
                client,
                &serde_json::json!({ "type": "error", "code": code }),
            );
        }
        Err(None) => stream.cancel(),
    }
}

/// One loopback stream.
struct Stream {
    loopback: Loopback,
    id: u64,
    owner: String,
    timing: std::cell::RefCell<Timing>,
    precleaned: Arc<Precleaned>,
    heard: std::cell::RefCell<Heard>,
    started: Instant,
}

/// The audio a stream was sent: its length, and with Labs › Keep recordings
/// on, the samples History keeps.
struct Heard {
    samples: usize,
    kept: Option<Vec<f32>>,
}

impl Heard {
    fn new(keep: bool) -> Self {
        Self {
            samples: 0,
            kept: keep.then(Vec::new),
        }
    }

    fn add(&mut self, pcm: &[u8]) {
        self.samples += pcm.len() / 2;
        if let Some(kept) = &mut self.kept {
            kept.extend(
                pcm.chunks_exact(2)
                    .map(|pair| f32::from(i16::from_le_bytes([pair[0], pair[1]])) / 32_768.0),
            );
        }
    }
}

/// Where a stream's time went, logged once when it finishes.
#[derive(Default)]
struct Timing {
    frames: u32,
    relay: Duration,
    slowest_relay: Duration,
}

impl Stream {
    /// Feeds audio frames upstream until the phone finishes, sending partials
    /// back between them. `Err` carries the code to send the phone, if any.
    fn receive(
        &self,
        client: &mut Tls,
        mut buffer: Vec<u8>,
        live: &mpsc::Receiver<String>,
    ) -> Result<(), Option<String>> {
        client.sock.set_read_timeout(Some(TICK)).map_err(|_| None)?;
        let mut heard = Instant::now();
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            while let Some(frame) =
                next_frame(&mut buffer).map_err(|()| Some("invalid-frame".to_owned()))?
            {
                match frame {
                    Frame::Audio(pcm) => self.audio(&pcm)?,
                    Frame::Finish => return Ok(()),
                    Frame::Cancel => return Err(None),
                }
            }
            while let Ok(text) = live.try_recv() {
                if !send(
                    client,
                    &serde_json::json!({ "type": "partial", "text": text }),
                ) {
                    return Err(None);
                }
            }
            match client.read(&mut chunk) {
                Ok(0) => return Err(None),
                Ok(read) => {
                    buffer.extend_from_slice(&chunk[..read]);
                    heard = Instant::now();
                }
                Err(error)
                    if matches!(error.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) =>
                {
                    if heard.elapsed() > IDLE {
                        return Err(None);
                    }
                }
                Err(_) => return Err(None),
            }
        }
    }

    fn audio(&self, pcm: &[u8]) -> Result<(), Option<String>> {
        let path = format!("/streams/{}/audio", self.id);
        let headers = [("X-Hex-Stream-Token", self.owner.as_str())];
        let started = Instant::now();
        let answer = self.loopback.request(
            "POST",
            &path,
            &headers,
            Some(("application/octet-stream", pcm)),
            REQUEST,
        );
        self.heard.borrow_mut().add(pcm);
        let mut timing = self.timing.borrow_mut();
        timing.frames += 1;
        timing.relay += started.elapsed();
        timing.slowest_relay = timing.slowest_relay.max(started.elapsed());
        match answer {
            Some((204, _)) => Ok(()),
            Some((_, body)) => Err(Some(code(&body))),
            None => Err(Some("tiro-unreachable".to_owned())),
        }
    }

    /// Transcribes the rest, cleans it up, and sends the phone the final.
    fn finish(&self, client: &mut Tls) {
        let path = format!("/streams/{}/finish", self.id);
        let headers = [("X-Hex-Stream-Token", self.owner.as_str())];
        let started = Instant::now();
        let mut finished = Duration::ZERO;
        let mut rest_words = 0;
        let answer = match self.loopback.request("POST", &path, &headers, None, FINISH) {
            Some((200, body)) => serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| Some(value.get("text")?.as_str()?.trim().to_owned()))
                .map(|transcript| {
                    finished = started.elapsed();
                    let text = self.precleaned.finish(&transcript, |rest| {
                        rest_words = rest.split_whitespace().count();
                        super::dictate::cleanup(&self.loopback, rest)
                    });
                    let heard = self.heard.borrow();
                    let entry = super::corrections::record(
                        &transcript,
                        &text,
                        heard.samples as u64 / 16,
                        heard.kept.as_deref(),
                        self.started.elapsed().as_millis() as u64,
                    );
                    let mut body = serde_json::json!({ "type": "final", "text": text, "transcript": transcript });
                    super::dictate::merge(&mut body, super::corrections::answer_fields(entry));
                    body
                })
                .unwrap_or_else(
                    || serde_json::json!({ "type": "error", "code": "tiro-unreachable" }),
                ),
            Some((_, body)) => serde_json::json!({ "type": "error", "code": code(&body) }),
            None => serde_json::json!({ "type": "error", "code": "tiro-unreachable" }),
        };
        send(client, &answer);
        let timing = self.timing.borrow();
        tracing::info!(
            frames = timing.frames,
            relay_ms = timing.relay.as_millis() as u64,
            slowest_relay_ms = timing.slowest_relay.as_millis() as u64,
            finish_ms = finished.as_millis() as u64,
            cleanup_ms = (started.elapsed() - finished).as_millis() as u64,
            rest_words,
            "LAN dictation stream finished"
        );
    }

    fn cancel(&self) {
        let path = format!("/streams/{}/cancel", self.id);
        let headers = [("X-Hex-Stream-Token", self.owner.as_str())];
        let _ = self
            .loopback
            .request("POST", &path, &headers, None, REQUEST);
    }

    /// Reads the stream's events on a thread of its own and hands each new
    /// live text to `partials`. Ends with the stream's events.
    fn relay_partials(&self, partials: mpsc::Sender<String>) {
        let path = format!("/streams/{}/events", self.id);
        let headers = [("X-Hex-Stream-Token", self.owner.as_str())];
        let Some(events) = self.loopback.open("GET", &path, &headers, None, EVENTS) else {
            return;
        };
        let precleaned = Arc::clone(&self.precleaned);
        let loopback = Loopback {
            port: self.loopback.port,
            token: self.loopback.token.clone(),
        };
        let _ = std::thread::Builder::new()
            .name("lan-stream-events".into())
            .spawn(move || {
                let mut live = Live::default();
                let lines = BufReader::new(events).lines().map_while(Result::ok);
                // The response head ends at the first empty line.
                for line in lines.skip_while(|line| !line.trim_end().is_empty()) {
                    let Some(data) = line.strip_prefix("data: ") else {
                        continue;
                    };
                    let Ok(event) = serde_json::from_str::<serde_json::Value>(data) else {
                        continue;
                    };
                    if matches!(
                        event.get("type").and_then(|t| t.as_str()),
                        Some("final" | "error" | "cancelled")
                    ) {
                        break;
                    }
                    if let Some(text) = live.apply(&event)
                        && partials.send(text).is_err()
                    {
                        break;
                    }
                    // Cleans on this thread: live text waits for at most one
                    // sentence, and the finish never waits on a queue.
                    precleaned.catch_up(&live.committed(), |sentence| {
                        super::dictate::cleanup(&loopback, sentence)
                    });
                }
            });
    }
}

fn ids(body: &[u8]) -> Option<(u64, String)> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some((
        value.get("id")?.as_u64()?,
        value.get("ownerToken")?.as_str()?.to_owned(),
    ))
}

fn code(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| Some(value.get("code")?.as_str()?.to_owned()))
        .unwrap_or_else(|| "tiro-unreachable".to_owned())
}

fn refuse(client: &mut Tls, status: u16, body: &[u8]) {
    let _ = write!(
        client,
        "HTTP/1.1 {status} Refused\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = client.write_all(body);
    let _ = client.flush();
}

fn send(client: &mut Tls, value: &serde_json::Value) -> bool {
    client
        .write_all(format!("{value}\n").as_bytes())
        .and_then(|()| client.flush())
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(kind: u8, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        bytes.extend_from_slice(payload);
        bytes
    }

    #[test]
    fn frames_come_off_whole_and_in_order() {
        let mut buffer = [frame(b'A', &[1, 0, 2, 0]), frame(b'F', &[])].concat();
        buffer.extend_from_slice(&[b'A', 0, 0]);
        assert_eq!(
            next_frame(&mut buffer),
            Ok(Some(Frame::Audio(vec![1, 0, 2, 0])))
        );
        assert_eq!(next_frame(&mut buffer), Ok(Some(Frame::Finish)));
        // The start of the next frame waits for the rest.
        assert_eq!(next_frame(&mut buffer), Ok(None));
        assert_eq!(buffer, [b'A', 0, 0]);
    }

    #[test]
    fn refuses_frames_no_phone_sends() {
        for bytes in [
            frame(b'X', &[]),
            frame(b'A', &[1, 2, 3]),
            vec![b'A', 0xff, 0xff, 0xff, 0xff],
        ] {
            assert_eq!(next_frame(&mut bytes.clone()), Err(()));
        }
    }

    #[test]
    fn live_text_is_segments_then_the_window() {
        let mut live = Live::default();
        let partial = |segment: u64, stable: &str, tentative: &str| serde_json::json!({ "type": "partial", "segment": segment, "stable": stable, "tentative": tentative });
        assert_eq!(
            live.apply(&partial(0, "so the", " plan")),
            Some("so the plan".into())
        );
        assert_eq!(live.apply(&partial(0, "so the", " plan")), None);
        let segment =
            serde_json::json!({ "type": "segment", "segment": 0, "text": "So the plan." });
        assert_eq!(live.apply(&segment), Some("So the plan.".into()));
        assert_eq!(
            live.apply(&partial(1, "next", "")),
            Some("So the plan. next".into())
        );
        assert_eq!(live.apply(&serde_json::json!({ "type": "ready" })), None);
        assert_eq!(live.committed(), "So the plan. next");
    }

    #[test]
    fn committed_text_leaves_out_the_tentative_tail() {
        let mut live = Live::default();
        live.apply(&serde_json::json!({ "type": "partial", "segment": 0, "stable": "Run it. Then", "tentative": " stop" }));
        assert_eq!(live.committed(), "Run it. Then");
    }

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
