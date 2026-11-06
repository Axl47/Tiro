//! `POST /dictate`: audio in, clean text out, in one LAN request. The proxy
//! forwards the audio to the loopback `/transcriptions` as usual, then this
//! sends the transcript through the loopback `/cleanup` before answering, so a
//! phone pays for one TLS connection and one round trip instead of two.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

const CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

/// The phone's answer, from the loopback transcription's whole response. A
/// refusal (`409 model-not-ready`, …) passes through untouched. A failed
/// cleanup falls back to the raw transcript: it is still better than losing
/// the dictation, and the Mac's cleanup already falls back to its own
/// deterministic pass whenever its model can't run.
pub(super) fn answer(
    transcription: &[u8],
    cleanup: impl FnOnce(&str) -> Option<String>,
) -> Vec<u8> {
    let Some((status, body)) = split(transcription) else {
        return json(502, &serde_json::json!({ "code": "tiro-unreachable" }));
    };
    if !(200..300).contains(&status) {
        return transcription.to_vec();
    }
    let Some(transcript) = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| Some(value.get("transcript")?.as_str()?.trim().to_owned()))
    else {
        return json(502, &serde_json::json!({ "code": "tiro-unreachable" }));
    };
    let text = if transcript.is_empty() {
        String::new()
    } else {
        cleanup(&transcript).unwrap_or_else(|| transcript.clone())
    };
    json(
        200,
        &serde_json::json!({ "text": text, "transcript": transcript }),
    )
}

/// Cleans `text` through the loopback API's `/cleanup`.
pub(super) fn cleanup(port: u16, token: &str, text: &str) -> Option<String> {
    let body = serde_json::json!({ "text": text }).to_string();
    let mut upstream = TcpStream::connect(("127.0.0.1", port)).ok()?;
    upstream.set_read_timeout(Some(CLEANUP_TIMEOUT)).ok()?;
    write!(
        upstream,
        "POST /cleanup HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {token}\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .ok()?;
    let mut response = Vec::new();
    upstream.read_to_end(&mut response).ok()?;
    let (status, body) = split(&response)?;
    if status != 200 {
        tracing::info!(
            status,
            "LAN dictation cleanup refused; typing the transcript"
        );
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    Some(value.get("text")?.as_str()?.to_owned())
}

/// An HTTP response's status and body.
fn split(response: &[u8]) -> Option<(u16, &[u8])> {
    let end = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")?;
    let status = std::str::from_utf8(&response[..end])
        .ok()?
        .split(' ')
        .nth(1)?
        .parse()
        .ok()?;
    Some((status, &response[end + 4..]))
}

fn json(status: u16, body: &serde_json::Value) -> Vec<u8> {
    let body = body.to_string();
    format!(
        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn transcribed(status: u16, body: &str) -> Vec<u8> {
        format!(
            "HTTP/1.1 {status} OK\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
        .into_bytes()
    }

    fn body(response: &[u8]) -> serde_json::Value {
        let (_, body) = split(response).expect("a response");
        serde_json::from_slice(body).expect("JSON")
    }

    #[test]
    fn cleans_the_transcript() {
        let response = answer(
            &transcribed(200, r#"{"transcript":" um hi there "}"#),
            |text| {
                assert_eq!(text, "um hi there");
                Some("Hi there.".into())
            },
        );
        assert_eq!(split(&response).map(|(status, _)| status), Some(200));
        assert_eq!(
            body(&response),
            serde_json::json!({ "text": "Hi there.", "transcript": "um hi there" })
        );
    }

    #[test]
    fn a_failed_cleanup_types_the_transcript() {
        let response = answer(&transcribed(200, r#"{"transcript":"hi"}"#), |_| None);
        assert_eq!(body(&response)["text"], "hi");
    }

    #[test]
    fn silence_skips_cleanup() {
        let response = answer(&transcribed(200, r#"{"transcript":""}"#), |_| {
            panic!("no cleanup")
        });
        assert_eq!(body(&response)["text"], "");
    }

    #[test]
    fn refusals_pass_through() {
        let refusal = transcribed(409, r#"{"code":"model-not-ready"}"#);
        assert_eq!(answer(&refusal, |_| panic!("no cleanup")), refusal);
    }

    #[test]
    fn garbage_is_unreachable() {
        let response = answer(b"nonsense", |_| panic!("no cleanup"));
        assert_eq!(split(&response).map(|(status, _)| status), Some(502));
    }
}
