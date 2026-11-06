//! Requests from the LAN front to the loopback local API, for the routes that
//! need more than copying one request through (`/dictate`, `/dictate/stream`).

use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::Duration;

/// The running app's local API, from its discovery file (`local-api.json`).
#[derive(serde::Deserialize)]
pub(super) struct Loopback {
    pub port: u16,
    pub token: String,
}

impl Loopback {
    pub fn discover() -> Option<Self> {
        let path = crate::app_paths::support_dir().ok()?.join("local-api.json");
        serde_json::from_slice(&std::fs::read(path).ok()?).ok()
    }

    /// Sends a request and returns the status and body of its answer.
    pub fn request(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, &[u8])>,
        timeout: Duration,
    ) -> Option<(u16, Vec<u8>)> {
        let mut upstream = self.open(method, path, headers, body, timeout)?;
        let mut response = Vec::new();
        upstream.read_to_end(&mut response).ok()?;
        let (status, body) = split(&response)?;
        Some((status, body.to_vec()))
    }

    /// Sends a request and returns the connection, to read a long answer
    /// (server-sent events) as it arrives.
    pub fn open(
        &self,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: Option<(&str, &[u8])>,
        timeout: Duration,
    ) -> Option<TcpStream> {
        let mut upstream = TcpStream::connect(("127.0.0.1", self.port)).ok()?;
        upstream.set_read_timeout(Some(timeout)).ok()?;
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nAuthorization: Bearer {}\r\nConnection: close\r\n",
            self.token
        );
        for (name, value) in headers {
            head.push_str(&format!("{name}: {value}\r\n"));
        }
        let content = body.map_or(&[][..], |(kind, bytes)| {
            head.push_str(&format!("Content-Type: {kind}\r\n"));
            bytes
        });
        if body.is_some() || method == "POST" {
            head.push_str(&format!("Content-Length: {}\r\n", content.len()));
        }
        head.push_str("\r\n");
        upstream.write_all(head.as_bytes()).ok()?;
        upstream.write_all(content).ok()?;
        Some(upstream)
    }
}

/// An HTTP response's status and body.
pub(super) fn split(response: &[u8]) -> Option<(u16, &[u8])> {
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
