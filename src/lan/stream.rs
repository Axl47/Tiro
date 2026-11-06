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
//! {"type":"final","text":…,"transcript":…}  the cleaned text to type
//! {"type":"error","code":…}
//! ```
//!
//! The Mac side is the loopback `/streams` API (`docs/streams.md`): its windows,
//! its yielding to the Mac's own dictation, and its one-live-stream limit. A
//! refused create (`409 stream-busy`, `503 model-not-loaded`) is answered as
//! that status before any frame, and the phone sends the whole clip to
//! `/dictate` instead.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::mpsc;
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
    partial: Option<(u64, String)>,
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
                self.partial = Some((segment, format!("{stable}{tentative}")));
            }
            "segment" => {
                self.segments
                    .insert(segment, event.get("text")?.as_str()?.to_owned());
                if self.partial.as_ref().is_some_and(|(at, _)| *at <= segment) {
                    self.partial = None;
                }
            }
            _ => return None,
        }
        let pending = self.partial.iter().map(|(_, text)| text.as_str());
        let text = self
            .segments
            .values()
            .map(String::as_str)
            .chain(pending)
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        (text != self.shown).then(|| {
            self.shown.clone_from(&text);
            text
        })
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
        let answer = match self.loopback.request("POST", &path, &headers, None, FINISH) {
            Some((200, body)) => serde_json::from_slice::<serde_json::Value>(&body)
                .ok()
                .and_then(|value| Some(value.get("text")?.as_str()?.trim().to_owned()))
                .map(|transcript| {
                    finished = started.elapsed();
                    let text = if transcript.is_empty() {
                        String::new()
                    } else {
                        super::dictate::cleanup(&self.loopback, &transcript)
                            .unwrap_or_else(|| transcript.clone())
                    };
                    serde_json::json!({ "type": "final", "text": text, "transcript": transcript })
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
    }
}
