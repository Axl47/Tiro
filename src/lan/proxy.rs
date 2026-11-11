//! One LAN request: TLS, then the request head is checked against an
//! allowlist and a paired device's token, rewritten for the loopback API
//! (its token, `Connection: close`, and for audio the model dictation holds),
//! and the body and the response are copied through. `/dictate` and
//! `/dictate/stream` do more on the way (`super::dictate`, `super::stream`),
//! and `/corrections` is answered here (`super::corrections`).

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

const MAX_HEAD_BYTES: usize = 16 * 1024;
/// About 30 minutes of 16 kHz 16-bit mono audio.
const MAX_AUDIO_BYTES: usize = 64 * 1024 * 1024;
const MAX_TEXT_BYTES: usize = 64 * 1024;
const CLIENT_TIMEOUT: Duration = Duration::from_secs(15);
/// Transcribing behind a long local dictation can take a while.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Why a request isn't forwarded: the status and the code the phone sees.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Refusal(u16, &'static str);

/// What to send upstream.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Forward {
    pub path: String,
    pub content_length: usize,
    pub kind: Kind,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Kind {
    /// Copied through as it is.
    Plain,
    /// `/dictate`: the transcript is cleaned up before answering.
    ThenCleanup,
    /// `/dictate/stream`: framed audio on this connection (`super::stream`).
    Stream,
    /// `/corrections`: answered here, never forwarded (`super::corrections`).
    Corrections,
}

pub(super) fn parse_head(bytes: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.split("\r\n");
    let mut request = lines.next()?.split(' ');
    let (method, path) = (request.next()?.to_owned(), request.next()?.to_owned());
    if !request.next()?.starts_with("HTTP/1.") {
        return None;
    }
    let headers = lines
        .take_while(|line| !line.is_empty())
        .map(|line| {
            let (key, value) = line.split_once(':')?;
            Some((key.trim().to_owned(), value.trim().to_owned()))
        })
        .collect::<Option<_>>()?;
    Some(Head {
        method,
        path,
        headers,
    })
}

/// Checks `head` against the allowlist and a device token. `selection` is the
/// `model=…&language=…` dictation holds; audio always uses it, so a phone
/// never makes the Mac load a second model.
pub(super) fn route(
    head: &Head,
    device: impl Fn(&str) -> Option<String>,
    selection: impl Fn() -> String,
) -> Result<Forward, Refusal> {
    let route = head.path.split('?').next().unwrap_or_default();
    let (path, limit, kind) = match (head.method.as_str(), route) {
        ("GET", "/health") => ("/health".to_owned(), 0, Kind::Plain),
        ("POST", "/cleanup") => ("/cleanup".to_owned(), MAX_TEXT_BYTES, Kind::Plain),
        ("POST", "/transcriptions") => (
            format!("/transcriptions?{}", selection()),
            MAX_AUDIO_BYTES,
            Kind::Plain,
        ),
        ("POST", "/dictate") => (
            format!("/transcriptions?{}", selection()),
            MAX_AUDIO_BYTES,
            Kind::ThenCleanup,
        ),
        ("POST", "/dictate/stream") => ("/streams".to_owned(), 0, Kind::Stream),
        ("POST", "/corrections") => (String::new(), MAX_TEXT_BYTES, Kind::Corrections),
        _ => return Err(Refusal(404, "not-found")),
    };
    let token = head
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "));
    if token.and_then(&device).is_none() {
        return Err(Refusal(401, "not-paired"));
    }
    if kind == Kind::Stream {
        return Ok(Forward {
            path,
            content_length: 0,
            kind,
        });
    }
    if head.header("transfer-encoding").is_some() {
        return Err(Refusal(411, "length-required"));
    }
    let content_length = match head.header("content-length") {
        None if limit == 0 => 0,
        None => return Err(Refusal(411, "length-required")),
        Some(value) => value.parse().map_err(|_| Refusal(400, "invalid-request"))?,
    };
    if content_length > limit {
        return Err(Refusal(413, "too-large"));
    }
    Ok(Forward {
        path,
        content_length,
        kind,
    })
}

/// The head the loopback API receives: only the content headers survive.
pub(super) fn upstream_head(head: &Head, forward: &Forward, loopback_token: &str) -> String {
    let mut out = format!(
        "{} {} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {loopback_token}\r\nConnection: close\r\nContent-Length: {}\r\n",
        head.method, forward.path, forward.content_length
    );
    if let Some(kind) = head.header("content-type") {
        out.push_str(&format!("Content-Type: {kind}\r\n"));
    }
    out.push_str("\r\n");
    out
}

/// Sends TLS close_notify and flushes it.
fn close(client: &mut super::stream::Tls) {
    client.conn.send_close_notify();
    let _ = client.flush();
}

fn refuse(stream: &mut impl Write, Refusal(status, code): Refusal) {
    let body = format!("{{\"code\":\"{code}\"}}");
    let _ = write!(
        stream,
        "HTTP/1.1 {status} Refused\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
}

/// The request head, and whatever of the body arrived with it.
fn read_head(stream: &mut impl Read) -> Option<(Vec<u8>, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
        if let Some(end) = buffer.windows(4).position(|window| window == b"\r\n\r\n") {
            let body = buffer.split_off(end + 4);
            return Some((buffer, body));
        }
        if buffer.len() > MAX_HEAD_BYTES {
            return None;
        }
    }
}

fn selection() -> String {
    let selection = crate::app_settings::AppSettings::load()
        .map(|settings| settings.transcription)
        .unwrap_or_default();
    format!(
        "model={}&language={}",
        selection.model.as_str(),
        selection.language
    )
}

pub(super) fn serve(stream: TcpStream, config: Arc<rustls::ServerConfig>, peer: SocketAddr) {
    let started = Instant::now();
    // Accepted from a nonblocking listener.
    if stream.set_nonblocking(false).is_err()
        || stream.set_read_timeout(Some(CLIENT_TIMEOUT)).is_err()
        || stream.set_write_timeout(Some(CLIENT_TIMEOUT)).is_err()
    {
        return;
    }
    let Ok(connection) = rustls::ServerConnection::new(config) else {
        return;
    };
    // The handshake runs on the first read.
    let mut client = rustls::StreamOwned::new(connection, stream);
    let Some((head_bytes, body)) = read_head(&mut client) else {
        return;
    };
    let Some(head) = parse_head(&head_bytes) else {
        return refuse(&mut client, Refusal(400, "invalid-request"));
    };
    let forward = match route(&head, super::pairing::device_for, selection) {
        Ok(forward) => forward,
        Err(refusal) => {
            tracing::info!(%peer, path = head.path, code = refusal.1, "LAN request refused");
            // A wrong token costs a guesser time, not the phone that has one.
            if refusal.0 == 401 {
                std::thread::sleep(Duration::from_millis(500));
            }
            return refuse(&mut client, refusal);
        }
    };
    if forward.kind == Kind::Corrections {
        let mut body = body;
        body.truncate(forward.content_length);
        let mut chunk = [0_u8; 4096];
        while body.len() < forward.content_length {
            match client.read(&mut chunk) {
                Ok(0) | Err(_) => return,
                Ok(read) => body.extend_from_slice(&chunk[..read]),
            }
        }
        body.truncate(forward.content_length);
        let _ = client.write_all(&super::corrections::answer(&body));
        close(&mut client);
        return;
    }
    let Some(discovery) = super::loopback::Loopback::discover() else {
        return refuse(&mut client, Refusal(503, "tiro-not-ready"));
    };
    if forward.kind == Kind::Stream {
        super::stream::serve(&mut client, body, discovery, peer);
        close(&mut client);
        return;
    }
    let Ok(mut upstream) = TcpStream::connect(("127.0.0.1", discovery.port)) else {
        return refuse(&mut client, Refusal(503, "tiro-not-ready"));
    };
    let _ = upstream.set_read_timeout(Some(UPSTREAM_TIMEOUT));
    let head = upstream_head(&head, &forward, &discovery.token);
    let body_prefix = &body[..body.len().min(forward.content_length)];
    let mut accepted = upstream
        .write_all(head.as_bytes())
        .and_then(|()| upstream.write_all(body_prefix))
        .is_ok();
    // The whole body is read from the phone even after the API stops
    // taking it: it may answer early (`409 model-not-ready`) and close, and
    // answering mid-upload would reset the phone before it reads the answer.
    // A dictation's audio is also kept for its History entry.
    let mut heard = (forward.kind == Kind::ThenCleanup).then(|| body_prefix.to_vec());
    let mut left = forward.content_length - body_prefix.len();
    let mut buffer = vec![0_u8; 64 * 1024];
    while left > 0 {
        match client.read(&mut buffer[..left.min(64 * 1024)]) {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                left -= read;
                accepted = accepted && upstream.write_all(&buffer[..read]).is_ok();
                if let Some(heard) = &mut heard {
                    heard.extend_from_slice(&buffer[..read]);
                }
            }
        }
    }
    if !accepted {
        let mut answer = Vec::new();
        let _ = upstream.read_to_end(&mut answer);
        if answer.is_empty() {
            return refuse(&mut client, Refusal(502, "tiro-unreachable"));
        }
        let _ = client.write_all(&answer);
        close(&mut client);
        return;
    }
    if forward.kind == Kind::ThenCleanup {
        let mut transcription = Vec::new();
        let _ = upstream.read_to_end(&mut transcription);
        let answer = super::dictate::answer(
            &transcription,
            |text| super::dictate::cleanup(&discovery, text),
            |transcript, text| {
                super::corrections::record_wav(
                    transcript,
                    text,
                    heard.as_deref().unwrap_or_default(),
                    started.elapsed().as_millis() as u64,
                )
            },
        );
        let _ = client.write_all(&answer);
        close(&mut client);
        return;
    }
    if let Err(error) = std::io::copy(&mut upstream, &mut client) {
        tracing::debug!(%error, %peer, "LAN response copy ended early");
    }
    close(&mut client);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(raw: &str) -> Head {
        parse_head(raw.as_bytes()).expect("a valid head")
    }

    fn paired(token: &str) -> Option<String> {
        (token == "good").then(|| "Pixel".to_owned())
    }

    fn held() -> String {
        "model=parakeet_unified_en&language=en".into()
    }

    #[test]
    fn audio_goes_to_the_model_dictation_holds() {
        let request = head(
            "POST /transcriptions?model=whisper_large_v3_turbo HTTP/1.1\r\nAuthorization: Bearer good\r\nContent-Type: audio/wav\r\nContent-Length: 44\r\n\r\n",
        );
        let forward = route(&request, paired, held).unwrap();
        assert_eq!(
            forward.path,
            "/transcriptions?model=parakeet_unified_en&language=en"
        );
        let upstream = upstream_head(&request, &forward, "loopback");
        assert_eq!(
            upstream,
            "POST /transcriptions?model=parakeet_unified_en&language=en HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer loopback\r\nConnection: close\r\nContent-Length: 44\r\nContent-Type: audio/wav\r\n\r\n"
        );
        assert!(!upstream.contains("good"));
    }

    #[test]
    fn dictate_transcribes_then_cleans_up() {
        let request = head(
            "POST /dictate HTTP/1.1\r\nAuthorization: Bearer good\r\nContent-Type: audio/wav\r\nContent-Length: 44\r\n\r\n",
        );
        let forward = route(&request, paired, held).unwrap();
        assert_eq!(
            forward.path,
            "/transcriptions?model=parakeet_unified_en&language=en"
        );
        assert_eq!(forward.kind, Kind::ThenCleanup);
    }

    #[test]
    fn streams_need_a_device_but_no_length() {
        let request = head("POST /dictate/stream HTTP/1.1\r\nAuthorization: Bearer good\r\n\r\n");
        let forward = route(&request, paired, held).unwrap();
        assert_eq!(forward.kind, Kind::Stream);
        let stranger = head("POST /dictate/stream HTTP/1.1\r\nAuthorization: Bearer bad\r\n\r\n");
        assert_eq!(
            route(&stranger, paired, held),
            Err(Refusal(401, "not-paired"))
        );
    }

    #[test]
    fn corrections_are_answered_here_with_a_bounded_body() {
        let request = head(
            "POST /corrections HTTP/1.1\r\nAuthorization: Bearer good\r\nContent-Length: 40\r\n\r\n",
        );
        assert_eq!(
            route(&request, paired, held).unwrap().kind,
            Kind::Corrections
        );
        let stranger = head(
            "POST /corrections HTTP/1.1\r\nAuthorization: Bearer bad\r\nContent-Length: 40\r\n\r\n",
        );
        assert_eq!(
            route(&stranger, paired, held),
            Err(Refusal(401, "not-paired"))
        );
    }

    #[test]
    fn only_the_dictation_routes_are_reachable() {
        for path in [
            "/control",
            "/dev/control",
            "/streams",
            "/models",
            "/dictations",
        ] {
            let request = head(&format!(
                "POST {path} HTTP/1.1\r\nAuthorization: Bearer good\r\nContent-Length: 2\r\n\r\n"
            ));
            assert_eq!(
                route(&request, paired, held),
                Err(Refusal(404, "not-found"))
            );
        }
    }

    #[test]
    fn a_device_must_be_paired() {
        for auth in [
            "",
            "Authorization: Bearer bad\r\n",
            "Authorization: good\r\n",
        ] {
            let request = head(&format!("GET /health HTTP/1.1\r\n{auth}\r\n"));
            assert_eq!(
                route(&request, paired, held),
                Err(Refusal(401, "not-paired"))
            );
        }
        let request = head("GET /health HTTP/1.1\r\nauthorization: Bearer good\r\n\r\n");
        assert!(route(&request, paired, held).is_ok());
    }

    #[test]
    fn bodies_need_a_bounded_length() {
        let cases = [
            (
                "Transfer-Encoding: chunked\r\n",
                Refusal(411, "length-required"),
            ),
            ("", Refusal(411, "length-required")),
            ("Content-Length: 99999999\r\n", Refusal(413, "too-large")),
            ("Content-Length: nope\r\n", Refusal(400, "invalid-request")),
        ];
        for (header, refusal) in cases {
            let request = head(&format!(
                "POST /cleanup HTTP/1.1\r\nAuthorization: Bearer good\r\n{header}\r\n"
            ));
            assert_eq!(route(&request, paired, held), Err(refusal));
        }
    }

    #[test]
    fn rejects_malformed_heads() {
        for raw in [
            "GET /health\r\n\r\n",
            "GET /health FTP/1\r\n\r\n",
            "GET /health HTTP/1.1\r\nbroken\r\n\r\n",
        ] {
            assert_eq!(parse_head(raw.as_bytes()), None);
        }
    }
}
