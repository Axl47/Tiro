//! Dictation from other devices on the local network: Tiro for Android
//! (`android/`) records, and this Mac transcribes and cleans up.
//!
//! Off unless Labs › LAN dictation is on. The loopback local API is never
//! exposed: this listener speaks TLS with a self-signed certificate that
//! paired phones pin, checks a per-device token, and forwards only three
//! routes to the loopback API, one request per connection:
//!
//! ```text
//! phone ─TLS─▶ :47811 ─▶ allowlist + device token ─▶ 127.0.0.1 local API
//!                        GET /health · POST /transcriptions · POST /cleanup
//! ```
//!
//! Pairing (`hex lan pair`) prints a `tiro://pair` link and its QR code:
//! address, port, device token, and the certificate's SHA-256.

mod pairing;
mod proxy;

use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub use pairing::{LanCommand, run as run_command};

/// Fixed, so a pairing keeps working across restarts.
const DEFAULT_PORT: u16 = 47_811;
/// Each connection is one request; a phone dictates one clip at a time.
const MAX_CONNECTIONS: usize = 8;

fn port() -> u16 {
    std::env::var("TIRO_LAN_PORT")
        .ok()
        .and_then(|port| port.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// Follows the Labs switch for the life of the app: listens while it is on,
/// and closes the port while it is off.
pub fn start() {
    let started = std::thread::Builder::new()
        .name("lan-listener".into())
        .spawn(watch);
    if let Err(error) = started {
        tracing::warn!(%error, "LAN dictation listener didn't start");
    }
}

fn watch() {
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    let mut listening: Option<(TcpListener, native_tls::TlsAcceptor)> = None;
    loop {
        if !crate::labs::lan_dictation() {
            if listening.take().is_some() {
                tracing::info!("LAN dictation stopped listening");
            }
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let Some((listener, acceptor)) = &listening else {
            match bind() {
                Ok(bound) => listening = Some(bound),
                Err(error) => {
                    tracing::warn!(%error, "LAN dictation couldn't listen; retrying");
                    std::thread::sleep(Duration::from_secs(5));
                }
            }
            continue;
        };
        match listener.accept() {
            Ok((stream, peer)) => {
                if ACTIVE.load(Ordering::Acquire) >= MAX_CONNECTIONS {
                    continue;
                }
                ACTIVE.fetch_add(1, Ordering::AcqRel);
                let acceptor = acceptor.clone();
                let spawned = std::thread::Builder::new()
                    .name("lan-request".into())
                    .spawn(move || {
                        proxy::serve(stream, &acceptor, peer);
                        ACTIVE.fetch_sub(1, Ordering::AcqRel);
                    });
                if spawned.is_err() {
                    ACTIVE.fetch_sub(1, Ordering::AcqRel);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(error) => tracing::debug!(%error, "LAN accept failed"),
        }
    }
}

fn bind() -> color_eyre::Result<(TcpListener, native_tls::TlsAcceptor)> {
    let certificate = pairing::certificate()?;
    let identity =
        native_tls::Identity::from_pkcs8(certificate.pem.as_bytes(), certificate.key.as_bytes())?;
    let acceptor = native_tls::TlsAcceptor::builder(identity)
        .min_protocol_version(Some(native_tls::Protocol::Tlsv12))
        .build()?;
    let listener = TcpListener::bind(("0.0.0.0", port()))?;
    // Polled, so turning the switch off closes the port within a second.
    listener.set_nonblocking(true)?;
    tracing::info!(port = port(), "LAN dictation listening");
    Ok((listener, acceptor))
}
