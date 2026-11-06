//! Tiro: every Keep recordings WAV also goes to the Storage Box, with its
//! History entry beside it as JSON, so the local folder can stay a small
//! cache (`recordings::MAX_RECORDING_BYTES`) for Transcribe again.
//!
//! A new recording is hard-linked into an outbox beside `history.json`
//! (`recordings-outbox/YYYY/MM/<time>-<id>.{wav,json}`), so it costs no extra
//! space and survives the store sweeping its own copy. One worker per
//! process mirrors the outbox to [`DESTINATION`] with `rsync
//! --remove-source-files`, right after each recording and every
//! [`RETRY_INTERVAL`] while anything is left, so files queued while the box
//! was unreachable go once it is back. `tiro-box` is an ssh alias
//! (`~/.ssh/config`) for a subaccount confined to the recordings folder.
//!
//! Only the real data folder is archived: a benchmark's or test build's
//! own folder sends nothing unless `TIRO_ARCHIVE_DESTINATION` names an rsync
//! destination (which also overrides the real one; empty turns it off).
//!
//! The JSON is the entry as it was recorded; a later Transcribe again
//! changes only the local History.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, mpsc};
use std::thread;
use std::time::Duration;

use super::HistoryEntry;

/// Where the outbox goes: an ssh alias and a folder in the account's home.
const DESTINATION: &str = "tiro-box:recordings/";
const OUTBOX: &str = "recordings-outbox";
const RETRY_INTERVAL: Duration = Duration::from_secs(10 * 60);

const DESTINATION_OVERRIDE: &str = "TIRO_ARCHIVE_DESTINATION";

/// The process's one archive worker and the outbox it drains.
pub(super) struct Archive {
    outbox: PathBuf,
    wake: Mutex<mpsc::Sender<()>>,
}

static ARCHIVE: OnceLock<Option<Archive>> = OnceLock::new();

/// Starts the worker for the outbox beside the history file at `history`
/// (once per process; later calls return the same one), or nothing when
/// this data folder isn't archived. It first sends whatever an earlier run
/// left queued.
pub(super) fn start(history: &Path) -> Option<&'static Archive> {
    ARCHIVE
        .get_or_init(|| destination().map(|destination| spawn(history, destination)))
        .as_ref()
}

/// The rsync destination for this process's recordings, if any.
fn destination() -> Option<String> {
    if let Some(destination) = std::env::var_os(DESTINATION_OVERRIDE) {
        return Some(destination.into_string().ok()?).filter(|destination| !destination.is_empty());
    }
    let real_folder = std::env::var_os("HEX_APPLICATION_SUPPORT_DIR").is_none()
        && !crate::app_paths::is_macos_test_build();
    real_folder.then(|| DESTINATION.to_owned())
}

fn spawn(history: &Path, destination: String) -> Archive {
    let outbox = history.with_file_name(OUTBOX);
    let (wake, woken) = mpsc::channel();
    let worker_outbox = outbox.clone();
    if let Err(error) = thread::Builder::new()
        .name("recordings-archive".into())
        .spawn(move || run(&worker_outbox, &destination, &woken))
    {
        tracing::warn!(%error, "could not start the recordings archive");
    }
    Archive {
        outbox,
        wake: Mutex::new(wake),
    }
}

impl Archive {
    /// Queues `entry`'s recording at `wav` and wakes the worker. A failure
    /// is logged; the local recording is unaffected.
    pub(super) fn add(&self, wav: &Path, entry: &HistoryEntry) {
        if let Err(error) = queue(&self.outbox, wav, entry) {
            tracing::warn!(%error, id = entry.id, "could not queue a recording for the archive");
            return;
        }
        let wake = self.wake.lock().unwrap_or_else(|error| error.into_inner());
        let _ = wake.send(());
    }
}

/// Links the WAV and writes the entry's JSON under the outbox's month folder.
fn queue(outbox: &Path, wav: &Path, entry: &HistoryEntry) -> io::Result<()> {
    let time = UtcTime::from_ms(entry.timestamp_ms);
    let folder = outbox
        .join(format!("{:04}", time.year))
        .join(format!("{:02}", time.month));
    fs::create_dir_all(&folder)?;
    restrict_directories(outbox, &folder)?;
    let stem = format!("{}-{}", time.stamp(), entry.id);
    let target = folder.join(format!("{stem}.wav"));
    if fs::hard_link(wav, &target).is_err() {
        fs::copy(wav, &target)?;
    }
    // Written aside and renamed, so rsync (which skips `*.tmp`) never sends half.
    let json = folder.join(format!("{stem}.json"));
    let temporary = json.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&temporary)?;
        super::restrict_to_owner(&file)?;
        serde_json::to_writer_pretty(&mut file, entry).map_err(io::Error::other)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    fs::rename(&temporary, &json)
}

/// The worker: sends the outbox, then waits for a new recording or the
/// retry interval; stops when the process drops its last sender (never, in
/// the app).
fn run(outbox: &Path, destination: &str, woken: &mpsc::Receiver<()>) {
    loop {
        if has_files(outbox) {
            send(outbox, destination);
        }
        match woken.recv_timeout(RETRY_INTERVAL) {
            Ok(()) => while woken.try_recv().is_ok() {},
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// One `rsync` of the outbox to the box; it deletes each file it delivered.
fn send(outbox: &Path, destination: &str) {
    let mut source = outbox.as_os_str().to_owned();
    source.push("/");
    let result = crate::desktop::background_process::command("rsync")
        .args([
            "-a",
            "--remove-source-files",
            "--exclude=*.tmp",
            "--timeout=120",
            "-e",
            "ssh -o BatchMode=yes -o ConnectTimeout=15",
        ])
        .arg(&source)
        .arg(destination)
        .output();
    match result {
        Ok(output) if output.status.success() => {
            tracing::info!("sent queued recordings to the archive");
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason: String = stderr.trim().chars().take(512).collect();
            tracing::warn!(status = %output.status, reason, "could not send recordings to the archive; will retry");
        }
        Err(error) => {
            tracing::warn!(%error, "could not run rsync for the recordings archive; will retry");
        }
    }
}

/// Whether any file (not just empty month folders) waits in the outbox.
fn has_files(directory: &Path) -> bool {
    fs::read_dir(directory).is_ok_and(|entries| {
        entries.flatten().any(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => has_files(&entry.path()),
            Ok(_) => !entry.file_name().to_string_lossy().ends_with(".tmp"),
            Err(_) => false,
        })
    })
}

#[cfg(unix)]
fn restrict_directories(outbox: &Path, folder: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    for directory in folder
        .ancestors()
        .take_while(|path| path.starts_with(outbox))
    {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn restrict_directories(_outbox: &Path, _folder: &Path) -> io::Result<()> {
    Ok(())
}

/// A UTC calendar time, enough to name and file a recording.
#[derive(Debug, PartialEq)]
struct UtcTime {
    year: i64,
    month: u32,
    day: u32,
    seconds_of_day: u64,
}

impl UtcTime {
    /// Howard Hinnant's `civil_from_days` over Unix milliseconds.
    fn from_ms(ms: u64) -> Self {
        let seconds = ms / 1000;
        let days = (seconds / 86_400) as i64 + 719_468;
        let era = days.div_euclid(146_097);
        let day_of_era = days.rem_euclid(146_097);
        let year_of_era =
            (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let shifted_month = (5 * day_of_year + 2) / 153;
        let day = (day_of_year - (153 * shifted_month + 2) / 5 + 1) as u32;
        let month = if shifted_month < 10 {
            shifted_month + 3
        } else {
            shifted_month - 9
        } as u32;
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        Self {
            year,
            month,
            day,
            seconds_of_day: seconds % 86_400,
        }
    }

    /// `2026-11-06T14-03-22Z`: sortable and safe in any file system.
    fn stamp(&self) -> String {
        let (hour, minute, second) = (
            self.seconds_of_day / 3600,
            self.seconds_of_day / 60 % 60,
            self.seconds_of_day % 60,
        );
        format!(
            "{:04}-{:02}-{:02}T{hour:02}-{minute:02}-{second:02}Z",
            self.year, self.month, self.day
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let directory =
            std::env::temp_dir().join(format!("tiro-archive-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn names_recordings_by_their_utc_time() {
        assert_eq!(UtcTime::from_ms(0).stamp(), "1970-01-01T00-00-00Z");
        // 2026-11-06 14:03:22.500 UTC
        assert_eq!(
            UtcTime::from_ms(1_793_973_802_500).stamp(),
            "2026-11-06T14-03-22Z"
        );
        // Leap day, and the last second of a year.
        assert_eq!(
            UtcTime::from_ms(1_709_164_800_000).stamp(),
            "2024-02-29T00-00-00Z"
        );
        assert_eq!(
            UtcTime::from_ms(1_767_225_599_000).stamp(),
            "2025-12-31T23-59-59Z"
        );
    }

    #[test]
    fn queues_the_recording_and_its_entry_by_month() {
        let directory = temp_dir("queue");
        let wav = directory.join("7.wav");
        fs::write(&wav, b"RIFF").unwrap();
        let outbox = directory.join(OUTBOX);
        let mut store = super::super::HistoryStore::open(
            directory.join("history.json"),
            super::super::HistoryRetention::Week,
            1_793_973_802_500,
        );
        let id = store
            .record(
                super::super::HistoryDraft {
                    kind: super::super::HistoryKind::Dictation,
                    raw_text: "um hello".into(),
                    final_text: "Hello.".into(),
                    application: None,
                    processing: None,
                    audio_ms: 900,
                    inference_ms: 80,
                    total_ms: 120,
                    recording: None,
                    model: Some("Parakeet".into()),
                },
                1_793_973_802_500,
            )
            .unwrap()
            .unwrap();
        let entry = store.entry(id).unwrap().clone();

        queue(&outbox, &wav, &entry).unwrap();
        // The store sweeping its own copy leaves the queued one.
        fs::remove_file(&wav).unwrap();

        let folder = outbox.join("2026").join("11");
        let stem = format!("2026-11-06T14-03-22Z-{id}");
        assert_eq!(
            fs::read(folder.join(format!("{stem}.wav"))).unwrap(),
            b"RIFF"
        );
        let json: serde_json::Value =
            serde_json::from_slice(&fs::read(folder.join(format!("{stem}.json"))).unwrap())
                .unwrap();
        assert_eq!(json["raw_text"], "um hello");
        assert_eq!(json["final_text"], "Hello.");
        assert!(has_files(&outbox));
    }

    #[test]
    fn an_outbox_of_empty_folders_has_nothing_to_send() {
        let directory = temp_dir("empty");
        fs::create_dir_all(directory.join("2026/11")).unwrap();
        fs::write(directory.join("2026/11/a.json.tmp"), b"").unwrap();
        assert!(!has_files(&directory));
    }
}
