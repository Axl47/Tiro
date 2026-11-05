//! `POST /cleanup`: text in, clean text out, through the same stage a dictation
//! uses. With `POST /transcriptions`, this is all a remote client needs: Android
//! over LAN records, sends audio, and sends the transcript here. See
//! `docs/tiro/android-lan.md`.

use std::net::TcpStream;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::http::{HttpResponse, body_read_response, read_body_bounded};

/// About ten minutes of speech; longer text belongs in several requests.
pub(super) const MAX_BYTES: usize = 64 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Deserialize)]
struct Request {
    text: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Response {
    text: String,
    raw: String,
    latency_ms: u64,
    /// Why the model's text wasn't used; absent when it was.
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback: Option<String>,
}

pub(super) fn handle(
    stream: &mut TcpStream,
    content_length: usize,
    mut body: Vec<u8>,
    shutdown: &AtomicBool,
) -> HttpResponse {
    if let Err(error) = read_body_bounded(
        stream,
        content_length,
        &mut body,
        shutdown,
        MAX_BYTES,
        READ_TIMEOUT,
    ) {
        return body_read_response(error);
    }
    let Ok(request) = serde_json::from_slice::<Request>(&body) else {
        return HttpResponse::code(400, "invalid-body");
    };
    let cleaned = crate::dictation::cleanup::apply(
        &request.text,
        &crate::dictation::vocabulary::entries(),
        shutdown,
    );
    HttpResponse::json(
        200,
        &Response {
            text: cleaned.text,
            raw: request.text,
            latency_ms: cleaned.latency.as_millis() as u64,
            fallback: cleaned.fallback.map(|reason| format!("{reason:?}")),
        },
    )
}
