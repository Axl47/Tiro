//! Dictation from other devices on the local network: Tiro for Android
//! (`android/`) records, and this Mac transcribes and cleans up.
//!
//! Off unless Labs › LAN dictation is on. The loopback local API is never
//! exposed: this listener speaks TLS with a self-signed certificate that
//! paired phones pin, checks a per-device token, and forwards only these
//! routes to the loopback API, one request per connection:
//!
//! ```text
//! phone ─TLS─▶ :47811 ─▶ allowlist + device token ─▶ 127.0.0.1 local API
//!                        GET /health · POST /transcriptions · POST /cleanup
//!                        POST /dictate (= /transcriptions, then /cleanup)
//!                        POST /dictate/stream (live, over `/streams`)
//!                        POST /corrections (answered here, `corrections`)
//! ```
//!
//! Pairing (`hex lan pair`) prints a `tiro://pair` link and its QR code:
//! address, port, device token, and the certificate's SHA-256. While it
//! listens, the Mac also advertises itself over Bonjour (`bonjour`), so a
//! phone finds it again after its address changes.

mod bonjour;
mod corrections;
mod dictate;
mod loopback;
mod pairing;
mod proxy;
mod stream;

use rustls::pki_types::pem::PemObject;
use std::net::TcpListener;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

pub use pairing::{LanCommand, run as run_command};
pub(crate) use pairing::{device_names, pair, qr_modules, revoke, unused_name};

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
/// and closes the port while it is off. Phone dictations go to `history`.
pub fn start(history: Option<crate::history::History>) {
    if let Some(history) = history {
        corrections::attach(history);
    }
    let started = std::thread::Builder::new()
        .name("lan-listener".into())
        .spawn(watch);
    if let Err(error) = started {
        tracing::warn!(%error, "LAN dictation listener didn't start");
    }
}

fn watch() {
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    let mut listening: Option<Listening> = None;
    loop {
        if !crate::labs::lan_dictation() {
            if listening.take().is_some() {
                tracing::info!("LAN dictation stopped listening");
            }
            std::thread::sleep(Duration::from_secs(1));
            continue;
        }
        let Some(Listening {
            listener, config, ..
        }) = &listening
        else {
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
                let config = Arc::clone(config);
                let spawned = std::thread::Builder::new()
                    .name("lan-request".into())
                    .spawn(move || {
                        proxy::serve(stream, config, peer);
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

/// The open port, and its Bonjour advert for as long as it's open.
struct Listening {
    listener: TcpListener,
    config: Arc<rustls::ServerConfig>,
    _advert: Option<bonjour::Advert>,
}

fn bind() -> color_eyre::Result<Listening> {
    let certificate = pairing::certificate()?;
    let config = server_config(&certificate.pem, &certificate.key)?;
    let listener = TcpListener::bind(("0.0.0.0", port()))?;
    // Polled, so turning the switch off closes the port within a second.
    listener.set_nonblocking(true)?;
    tracing::info!(port = port(), "LAN dictation listening");
    Ok(Listening {
        listener,
        config,
        _advert: bonjour::Advert::new(port(), &certificate.fingerprint),
    })
}

/// Always answers with the one certificate.
#[derive(Debug)]
struct OneCertificate(Arc<rustls::sign::CertifiedKey>);

impl rustls::server::ResolvesServerCert for OneCertificate {
    fn resolve(
        &self,
        _client_hello: rustls::server::ClientHello<'_>,
    ) -> Option<Arc<rustls::sign::CertifiedKey>> {
        Some(Arc::clone(&self.0))
    }
}

/// TLS from the PEM certificate and key, held in memory: nothing is imported
/// into a keychain, so macOS never asks for a keychain password.
///
/// The certificate goes in through a resolver rather than `with_single_cert`,
/// which re-parses it with a strict X.509 reader that refuses the version 1
/// certificates macOS's LibreSSL `openssl` makes. Phones pin the certificate's
/// fingerprint and don't care, and keeping it keeps every pairing valid.
fn server_config(pem: &str, key: &str) -> color_eyre::Result<Arc<rustls::ServerConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let chain = rustls::pki_types::CertificateDer::pem_slice_iter(pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()?;
    let key = rustls::pki_types::PrivateKeyDer::from_pem_slice(key.as_bytes())?;
    let signing_key = provider.key_provider.load_private_key(key)?;
    let certified = Arc::new(rustls::sign::CertifiedKey::new(chain, signing_key));
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(rustls::DEFAULT_VERSIONS)?
        .with_no_client_auth()
        .with_cert_resolver(Arc::new(OneCertificate(certified)));
    Ok(Arc::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    /// A TLS client's handshake against the openssl-made certificate the listener
    /// has always used, answered over TLS.
    #[test]
    fn serves_tls_with_an_openssl_certificate() {
        let dir = std::env::temp_dir().join(format!("tiro-lan-tls-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (cert, key) = (dir.join("cert.pem"), dir.join("key.pem"));
        let made = std::process::Command::new("/usr/bin/openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            ])
            .args(["-subj", "/CN=test", "-keyout"])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .output()
            .unwrap();
        assert!(made.status.success());
        let config = server_config(
            &std::fs::read_to_string(&cert).unwrap(),
            &std::fs::read_to_string(&key).unwrap(),
        )
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, peer) = listener.accept().unwrap();
            proxy::serve(stream, config, peer);
        });
        let mut client = std::process::Command::new("/usr/bin/openssl")
            .args(["s_client", "-quiet", "-connect"])
            .arg(address.to_string())
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        client
            .stdin
            .take()
            .unwrap()
            .write_all(b"GET /nope HTTP/1.1\r\nHost: x\r\n\r\n")
            .unwrap();
        let mut answer = String::new();
        client
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut answer)
            .unwrap();
        server.join().unwrap();
        std::fs::remove_dir_all(&dir).ok();
        assert!(answer.starts_with("HTTP/1.1 4"), "{answer}");
    }
}
