//! Pairing state in `<support>/lan/`, owner-only: the TLS certificate phones
//! pin, and the paired devices (only a hash of each token is kept). The
//! `hex lan` commands work on these files directly, with or without Tiro
//! running; the listener reads them on every connection.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use base64::Engine as _;
use clap::Subcommand;
use color_eyre::eyre::{Result, bail, eyre};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

#[derive(Subcommand)]
pub enum LanCommand {
    /// Pair a phone: prints a QR code to scan with Tiro for Android
    Pair {
        /// A name to tell devices apart, and to revoke this one by
        #[arg(long, default_value = "Android")]
        name: String,
        /// The address the phone reaches this Mac at (default: its LAN
        /// address; use 10.0.2.2 for the Android emulator)
        #[arg(long)]
        host: Option<String>,
    },
    /// List paired devices
    Devices,
    /// Unpair a device by name
    Revoke { name: String },
}

pub(super) struct Certificate {
    pub pem: String,
    pub key: String,
    /// SHA-256 of the DER certificate, lowercase hex: what phones pin.
    pub fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
struct Device {
    name: String,
    token_sha256: String,
    paired_at_ms: u64,
}

fn directory() -> Result<PathBuf> {
    let directory = crate::app_paths::support_dir()?.join("lan");
    fs::create_dir_all(&directory)?;
    fs::set_permissions(
        &directory,
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )?;
    Ok(directory)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn fingerprint_of(pem: &str) -> Result<String> {
    let body: String = pem
        .lines()
        .skip_while(|line| !line.starts_with("-----BEGIN CERTIFICATE"))
        .skip(1)
        .take_while(|line| !line.starts_with("-----END"))
        .collect();
    let der = base64::engine::general_purpose::STANDARD.decode(body)?;
    Ok(sha256_hex(&der))
}

/// The listener's certificate, made once with the system `openssl`.
pub(super) fn certificate() -> Result<Certificate> {
    let directory = directory()?;
    let (cert, key) = (directory.join("cert.pem"), directory.join("key.pem"));
    if !cert.exists() || !key.exists() {
        let status = Command::new("/usr/bin/openssl")
            .args([
                "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "3650",
            ])
            .args(["-subj", "/CN=Tiro LAN dictation", "-keyout"])
            .arg(&key)
            .arg("-out")
            .arg(&cert)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            bail!("openssl couldn't make the LAN certificate");
        }
        fs::set_permissions(&key, std::os::unix::fs::PermissionsExt::from_mode(0o600))?;
    }
    let pem = fs::read_to_string(&cert)?;
    Ok(Certificate {
        fingerprint: fingerprint_of(&pem)?,
        key: fs::read_to_string(&key)?,
        pem,
    })
}

fn devices_path() -> Result<PathBuf> {
    Ok(directory()?.join("devices.json"))
}

fn read_devices(path: &Path) -> Vec<Device> {
    fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_devices(path: &Path, devices: &[Device]) -> Result<()> {
    let partial = path.with_extension("json.partial");
    let mut file = fs::OpenOptions::new();
    std::os::unix::fs::OpenOptionsExt::mode(&mut file, 0o600);
    let mut file = file
        .write(true)
        .create(true)
        .truncate(true)
        .open(&partial)?;
    file.write_all(&serde_json::to_vec_pretty(devices)?)?;
    file.sync_all()?;
    fs::rename(partial, path)?;
    Ok(())
}

/// The paired device `token` belongs to, if any.
pub(super) fn device_for(token: &str) -> Option<String> {
    let hash = sha256_hex(token.as_bytes());
    read_devices(&devices_path().ok()?)
        .into_iter()
        .find(|device| device.token_sha256 == hash)
        .map(|device| device.name)
}

/// The address this Mac's default route leaves from: its LAN address on a
/// home network. Nothing is sent; connecting a UDP socket only picks a route.
fn lan_address() -> Option<String> {
    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    socket.connect("192.0.2.1:9").ok()?;
    Some(socket.local_addr().ok()?.ip().to_string())
}

fn pairing_link(host: &str, port: u16, token: &str, fingerprint: &str, name: &str) -> String {
    let query: String = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("host", host)
        .append_pair("port", &port.to_string())
        .append_pair("token", token)
        .append_pair("fp", fingerprint)
        .append_pair("name", name)
        .finish();
    format!("tiro://pair?{query}")
}

fn qr(text: &str) -> Result<String> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|error| eyre!("{error}"))?;
    Ok(code
        .render::<qrcode::render::unicode::Dense1x2>()
        .dark_color(qrcode::render::unicode::Dense1x2::Light)
        .light_color(qrcode::render::unicode::Dense1x2::Dark)
        .build())
}

/// A pairing just made: the device's name and the `tiro://pair` link its
/// QR code carries.
pub(crate) struct NewPairing {
    pub name: String,
    pub link: String,
}

/// Pairs a device as `name`, replacing one already paired by that name, at
/// `host` (default: this Mac's LAN address).
pub(crate) fn pair(name: &str, host: Option<&str>) -> Result<NewPairing> {
    let certificate = certificate()?;
    let host = match host {
        Some(host) => host.to_string(),
        None => lan_address().ok_or_else(|| eyre!("no LAN address; pass --host"))?,
    };
    let mut random = [0_u8; 32];
    getrandom::fill(&mut random).map_err(|error| eyre!("no secure random source: {error}"))?;
    let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let path = devices_path()?;
    let mut devices = read_devices(&path);
    devices.retain(|device| device.name != name);
    devices.push(Device {
        name: name.to_string(),
        token_sha256: sha256_hex(token.as_bytes()),
        paired_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_millis() as u64),
    });
    write_devices(&path, &devices)?;
    Ok(NewPairing {
        name: name.to_string(),
        link: pairing_link(&host, super::port(), &token, &certificate.fingerprint, name),
    })
}

/// Paired devices' names, oldest first.
pub(crate) fn device_names() -> Vec<String> {
    devices_path()
        .map(|path| {
            read_devices(&path)
                .into_iter()
                .map(|device| device.name)
                .collect()
        })
        .unwrap_or_default()
}

/// `base`, or `base 2`, `base 3`, … whichever no paired device has.
pub(crate) fn unused_name(base: &str) -> String {
    let taken = device_names();
    (1..)
        .map(|n| {
            if n == 1 {
                base.to_string()
            } else {
                format!("{base} {n}")
            }
        })
        .find(|name| !taken.contains(name))
        .unwrap_or_else(|| base.to_string())
}

pub(crate) fn revoke(name: &str) -> Result<()> {
    let path = devices_path()?;
    let mut devices = read_devices(&path);
    let before = devices.len();
    devices.retain(|device| device.name != name);
    if devices.len() == before {
        bail!("no paired device named {name}");
    }
    write_devices(&path, &devices)
}

/// `text`'s QR code as rows of modules, true for dark, without a quiet zone.
pub(crate) fn qr_modules(text: &str) -> Result<Vec<Vec<bool>>> {
    let code = qrcode::QrCode::new(text.as_bytes()).map_err(|error| eyre!("{error}"))?;
    let width = code.width();
    let colors = code.to_colors();
    Ok(colors
        .chunks(width)
        .map(|row| {
            row.iter()
                .map(|color| *color == qrcode::Color::Dark)
                .collect()
        })
        .collect())
}

pub fn run(command: &LanCommand) -> Result<()> {
    match command {
        LanCommand::Pair { name, host } => {
            let pairing = pair(name, host.as_deref())?;
            println!("{}", qr(&pairing.link)?);
            println!(
                "Scan with the phone's camera, or open this link on it:\n\n{}\n",
                pairing.link
            );
            if !crate::app_settings::AppSettings::saved_labs().lan_dictation {
                println!("LAN dictation is off. Turn it on with `hex labs on lan-dictation`.");
            }
        }
        LanCommand::Devices => {
            let devices = device_names();
            if devices.is_empty() {
                println!("No paired devices.");
            }
            for name in devices {
                println!("{name}");
            }
        }
        LanCommand::Revoke { name } => {
            revoke(name)?;
            println!("Unpaired {name}.");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_link_carries_everything_a_phone_needs() {
        let link = pairing_link("192.168.1.20", 47_811, "abc", "f00d", "Pixel 9");
        assert_eq!(
            link,
            "tiro://pair?host=192.168.1.20&port=47811&token=abc&fp=f00d&name=Pixel+9"
        );
    }

    #[test]
    fn fingerprints_the_der_certificate() {
        // "hello" in base64, as a stand-in for DER bytes.
        let pem = "-----BEGIN CERTIFICATE-----\naGVs\nbG8=\n-----END CERTIFICATE-----\n";
        assert_eq!(fingerprint_of(pem).unwrap(), sha256_hex(b"hello"));
    }

    #[test]
    fn devices_round_trip_without_their_tokens() {
        let directory = std::env::temp_dir().join(format!("tiro-lan-{}", std::process::id()));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("devices.json");
        let device = Device {
            name: "Pixel".into(),
            token_sha256: sha256_hex(b"secret"),
            paired_at_ms: 1,
        };
        write_devices(&path, std::slice::from_ref(&device)).unwrap();
        assert_eq!(read_devices(&path), [device]);
        assert!(!fs::read_to_string(&path).unwrap().contains("secret"));
        let mode =
            std::os::unix::fs::PermissionsExt::mode(&fs::metadata(&path).unwrap().permissions());
        assert_eq!(mode & 0o777, 0o600);
        let _ = fs::remove_dir_all(directory);
    }
}
