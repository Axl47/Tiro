//! Owns the `tiro-scribe` process (`native/scribe`): one warm model outside this
//! process, so exiting it returns all of the memory, a Metal fault can't reach
//! the app, and Cargo never links MLX. Spawned ahead of need and stopped after
//! [`IDLE_LIMIT`] without a dictation; the first press after that reloads it
//! while the person speaks.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::Fallback;

/// A warm helper holds about 750 MB; a dictation shortly after a press can wait
/// for a reload of about 1.5 s, so a helper that stops being used does not stay.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const IDLE_LIMIT: Duration = Duration::from_secs(15 * 60);

/// Pinned exactly: a new revision is a new benchmark.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(super) mod pinned {
    pub const REPOSITORY: &str = "heyvedu/vedu-scribe-0.8b";
    pub const REVISION: &str = "7dced5841c58d7d6e0118c4c123f222b35057871";
    /// `(name, bytes, sha256)`.
    pub const FILES: &[(&str, u64, &str)] = &[
        (
            "chat_template.jinja",
            7_755,
            "273d8e0e683b885071fb17e08d71e5f2a5ddfb5309756181681de4f5a1822d80",
        ),
        (
            "config.json",
            60_445,
            "a02e0e16a518b5a3baa307941b90f2b19f4730aa775c2babd3993d39de770654",
        ),
        (
            "model.safetensors",
            540_713_766,
            "9912660269b0a57dec85a368fa37b23ed7e76ca438b05b9b2238b4b887e7d2e6",
        ),
        (
            "model.safetensors.index.json",
            60_784,
            "437540ae055104cac70097c3b8706d5c46fd821f438b0fa06adddbed3683455f",
        ),
        (
            "tokenizer.json",
            19_989_339,
            "a5cd9732badce41de57e6efce8302930ded1c1188c5f81feb2bd6c24c4a1941f",
        ),
        (
            "tokenizer_config.json",
            1_502,
            "4d2fc7899270dc76b57a7aa1d5ec3234e6a25934c6e2aea6166f989f1d453892",
        ),
    ];
}

/// The weights' folder, named by revision so a new pin never mixes files.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn model_dir() -> color_eyre::Result<PathBuf> {
    Ok(crate::transcription::models::models_dir()?
        .join(format!("vedu-scribe-{}", &pinned::REVISION[..8])))
}

#[cfg(target_os = "macos")]
pub use macos::{clean, prepare};

/// No helper off macOS yet: MLX needs Apple silicon. Cleanup still strips
/// residue there; a remote engine would plug in here.
#[cfg(not(target_os = "macos"))]
pub fn prepare() {}

#[cfg(not(target_os = "macos"))]
pub fn clean(
    _transcript: &str,
    _vocab: &[String],
    _deadline: Duration,
    _cancelled: &AtomicBool,
) -> Result<String, Fallback> {
    Err(Fallback::Unavailable("no cleanup engine on this OS".into()))
}

#[cfg(target_os = "macos")]
mod macos {
    use std::io::{BufRead as _, BufReader, Read as _, Write as _};
    use std::process::{Child, ChildStdin, Command, Stdio};
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::sync::{Mutex, MutexGuard, TryLockError};
    use std::time::Instant;

    use color_eyre::eyre::{Result, bail, eyre};
    use serde::{Deserialize, Serialize};
    use sha2::{Digest as _, Sha256};

    use super::*;

    #[derive(Serialize)]
    struct Request<'a> {
        id: u64,
        text: &'a str,
        vocab: &'a [String],
    }

    /// One line from the helper: the `ready` event, or an answer by `id`.
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Line {
        event: Option<String>,
        id: Option<u64>,
        text: Option<String>,
        error: Option<String>,
        elapsed_ms: Option<u64>,
    }

    struct Helper {
        child: Child,
        stdin: ChildStdin,
        lines: Receiver<Line>,
        ready: bool,
        next_id: u64,
        last_used: Instant,
        /// Requests in a row that missed their deadline.
        missed: u32,
    }

    impl Helper {
        fn spawn() -> Result<Self> {
            Self::spawn_at(&helper_path()?, &model_dir()?)
        }

        fn spawn_at(binary: &std::path::Path, model: &std::path::Path) -> Result<Self> {
            let mut child = Command::new(binary)
                .arg("--model")
                .arg(model)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|error| eyre!("couldn't start {}: {error}", binary.display()))?;
            let stdin = child.stdin.take().ok_or_else(|| eyre!("no helper stdin"))?;
            let stdout = child
                .stdout
                .take()
                .ok_or_else(|| eyre!("no helper stdout"))?;
            let (send, lines) = mpsc::channel();
            // Ends with the helper; the channel closing is how a crash is seen.
            std::thread::Builder::new()
                .name("cleanup-helper-reader".into())
                .spawn(move || {
                    for line in BufReader::new(stdout).lines() {
                        let Ok(line) = line else { return };
                        let Ok(line) = serde_json::from_str::<Line>(&line) else {
                            continue;
                        };
                        if send.send(line).is_err() {
                            return;
                        }
                    }
                })?;
            tracing::info!(model = %model.display(), "cleanup helper starting");
            Ok(Self {
                child,
                stdin,
                lines,
                ready: false,
                next_id: 0,
                last_used: Instant::now(),
                missed: 0,
            })
        }

        /// Blocks until the helper announces it is warm, at most until `by`.
        /// Running out of time here is not the helper's fault: loading takes
        /// longer than a short dictation's deadline, so it keeps loading for
        /// the next one.
        fn wait_ready(&mut self, by: Instant, cancelled: &AtomicBool) -> Result<(), Fallback> {
            while !self.ready {
                match self.next_line(by, cancelled) {
                    Ok(line) => self.ready = line.event.as_deref() == Some("ready"),
                    Err(Fallback::Late) => {
                        return Err(Fallback::Unavailable("still loading".into()));
                    }
                    Err(other) => return Err(other),
                }
            }
            Ok(())
        }

        fn next_line(&self, by: Instant, cancelled: &AtomicBool) -> Result<Line, Fallback> {
            const POLL: Duration = Duration::from_millis(20);
            loop {
                if cancelled.load(Ordering::Relaxed) {
                    return Err(Fallback::Cancelled);
                }
                let left = by.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    return Err(Fallback::Late);
                }
                match self.lines.recv_timeout(left.min(POLL)) {
                    Ok(line) => return Ok(line),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => {
                        return Err(Fallback::Failed("the helper exited".into()));
                    }
                }
            }
        }

        fn clean(
            &mut self,
            transcript: &str,
            vocab: &[String],
            by: Instant,
            cancelled: &AtomicBool,
        ) -> Result<String, Fallback> {
            self.wait_ready(by, cancelled)?;
            let result = self.request(transcript, vocab, by, cancelled);
            match result {
                Ok(_) => self.missed = 0,
                Err(Fallback::Late) => self.missed += 1,
                Err(_) => {}
            }
            result
        }

        fn request(
            &mut self,
            transcript: &str,
            vocab: &[String],
            by: Instant,
            cancelled: &AtomicBool,
        ) -> Result<String, Fallback> {
            self.next_id += 1;
            self.last_used = Instant::now();
            let id = self.next_id;
            let mut line = serde_json::to_vec(&Request {
                id,
                text: transcript,
                vocab,
            })
            .map_err(|error| Fallback::Failed(error.to_string()))?;
            line.push(b'\n');
            self.stdin
                .write_all(&line)
                .and_then(|()| self.stdin.flush())
                .map_err(|error| Fallback::Failed(error.to_string()))?;
            loop {
                let reply = self.next_line(by, cancelled)?;
                // An answer to a dictation cancelled earlier: skip it.
                if reply.id != Some(id) {
                    continue;
                }
                if let Some(error) = reply.error {
                    return Err(Fallback::Failed(error));
                }
                tracing::info!(helper_ms = reply.elapsed_ms, "cleanup answered");
                return reply
                    .text
                    .ok_or_else(|| Fallback::Failed("an answer without text".into()));
            }
        }
    }

    impl Drop for Helper {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
    static PREPARING: AtomicBool = AtomicBool::new(false);
    static IDLE_WATCH: AtomicBool = AtomicBool::new(false);

    fn helper_slot() -> MutexGuard<'static, Option<Helper>> {
        HELPER.lock().unwrap_or_else(|error| error.into_inner())
    }

    /// Bundled in `Contents/Helpers`, beside its `mlx.metallib`. A checkout's
    /// loose binary finds it through `TIRO_SCRIBE_HELPER`.
    fn helper_path() -> Result<PathBuf> {
        if let Some(path) = std::env::var_os("TIRO_SCRIBE_HELPER") {
            return Ok(path.into());
        }
        let executable = std::env::current_exe()?;
        let contents = executable
            .parent()
            .and_then(|directory| directory.parent())
            .ok_or_else(|| eyre!("the executable has no bundle"))?;
        let helper = contents.join("Helpers/tiro-scribe");
        if helper.exists() {
            Ok(helper)
        } else {
            bail!("no helper at {}", helper.display())
        }
    }

    fn weights_present() -> bool {
        let Ok(directory) = model_dir() else {
            return false;
        };
        pinned::FILES.iter().all(|(name, bytes, _)| {
            std::fs::metadata(directory.join(name)).is_ok_and(|meta| meta.len() == *bytes)
        })
    }

    fn sha256_of(path: &std::path::Path) -> Result<String> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1 << 20];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    /// Fetches any missing weight file through `net`, verified against its pin.
    /// A file is only promoted to its real name after its hash matches.
    fn download_weights() -> Result<()> {
        let directory = model_dir()?;
        std::fs::create_dir_all(&directory)?;
        for (name, bytes, sha256) in pinned::FILES {
            let destination = directory.join(name);
            if std::fs::metadata(&destination).is_ok_and(|meta| meta.len() == *bytes) {
                continue;
            }
            let partial = directory.join(format!("{name}.partial"));
            let url = format!(
                "https://huggingface.co/{}/resolve/{}/{name}",
                pinned::REPOSITORY,
                pinned::REVISION
            );
            tracing::info!(file = name, bytes, "downloading cleanup model");
            crate::net::download(
                &crate::net::Download {
                    url: &url,
                    user_agent: &format!("tiro/{}", crate::VERSION),
                    size: Some(*bytes),
                    canceled: None,
                },
                &partial,
                |_| {},
            )?;
            let found = sha256_of(&partial)?;
            if found != *sha256 {
                let _ = std::fs::remove_file(&partial);
                bail!("{name} failed verification: {found}");
            }
            std::fs::rename(&partial, &destination)?;
        }
        Ok(())
    }

    /// Gets the model warm without holding up the caller. Call it as early as
    /// is useful (launch, shortcut press); it is cheap once the helper is up.
    pub fn prepare() {
        if !crate::labs::scribe_cleanup() {
            return;
        }
        // Runs on the capture path, and a request holds the lock throughout
        // inference: locked means a helper exists, so never wait for it.
        match HELPER.try_lock() {
            Err(TryLockError::WouldBlock) => return,
            Ok(slot) if slot.is_some() => return,
            _ => {}
        }
        if PREPARING.swap(true, Ordering::AcqRel) {
            return;
        }
        let started = std::thread::Builder::new()
            .name("cleanup-prepare".into())
            .spawn(|| {
                let outcome = (|| -> Result<()> {
                    if !weights_present() {
                        download_weights()?;
                    }
                    let helper = Helper::spawn()?;
                    *helper_slot() = Some(helper);
                    start_idle_watch();
                    Ok(())
                })();
                if let Err(error) = outcome {
                    tracing::warn!(%error, "cleanup model isn't available");
                }
                PREPARING.store(false, Ordering::Release);
            });
        if started.is_err() {
            PREPARING.store(false, Ordering::Release);
        }
    }

    fn start_idle_watch() {
        if IDLE_WATCH.swap(true, Ordering::AcqRel) {
            return;
        }
        let _ = std::thread::Builder::new()
            .name("cleanup-idle".into())
            .spawn(|| {
                loop {
                    std::thread::sleep(Duration::from_secs(30));
                    let mut slot = helper_slot();
                    if slot
                        .as_ref()
                        .is_some_and(|helper| helper.last_used.elapsed() >= IDLE_LIMIT)
                    {
                        tracing::info!("stopping the idle cleanup helper");
                        *slot = None;
                    }
                }
            });
    }

    /// One late answer is usually one long dictation, and reloading costs more
    /// than letting it finish. Two in a row mean it is wedged.
    const MISSES_BEFORE_RESTART: u32 = 2;

    /// One request against the warm helper. A helper that died, or keeps missing
    /// its deadline, is dropped; the next press reloads it.
    pub fn clean(
        transcript: &str,
        vocab: &[String],
        deadline: Duration,
        cancelled: &AtomicBool,
    ) -> Result<String, Fallback> {
        let by = Instant::now() + deadline;
        if helper_slot().is_none() {
            prepare();
        }
        // A press usually started the load already; give it the deadline.
        while helper_slot().is_none() {
            if !PREPARING.load(Ordering::Acquire) {
                return Err(Fallback::Unavailable("the helper didn't start".into()));
            }
            if cancelled.load(Ordering::Relaxed) {
                return Err(Fallback::Cancelled);
            }
            if Instant::now() >= by {
                return Err(Fallback::Unavailable("still loading".into()));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let mut slot = helper_slot();
        let helper = slot
            .as_mut()
            .ok_or_else(|| Fallback::Unavailable("the helper stopped".into()))?;
        let result = helper.clean(transcript, vocab, by, cancelled);
        if matches!(result, Err(Fallback::Failed(_))) || helper.missed >= MISSES_BEFORE_RESTART {
            *slot = None;
        }
        result
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::PermissionsExt as _;

        use super::*;

        /// A helper speaking the protocol from a shell script, slow to load.
        fn fake_helper(directory: &std::path::Path, load_seconds: f32, answer: &str) -> Helper {
            let script = directory.join("fake-helper");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nsleep {load_seconds}\necho '{{\"event\":\"ready\"}}'\nid=0\n\
                     while read -r _; do id=$((id+1)); {answer}; done\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            Helper::spawn_at(&script, directory).unwrap()
        }

        const ANSWER: &str = r#"echo "{\"id\":$id,\"text\":\"clean\"}""#;

        #[test]
        fn a_deadline_shorter_than_the_load_leaves_the_helper_loading() {
            let directory = tempfile_dir();
            let mut helper = fake_helper(&directory, 0.3, ANSWER);
            let cancelled = AtomicBool::new(false);
            let soon = Instant::now() + Duration::from_millis(20);
            assert_eq!(
                helper.clean("hi", &[], soon, &cancelled),
                Err(Fallback::Unavailable("still loading".into()))
            );
            assert_eq!(helper.missed, 0);
            let later = Instant::now() + Duration::from_secs(5);
            assert_eq!(helper.clean("hi", &[], later, &cancelled).unwrap(), "clean");
            let _ = std::fs::remove_dir_all(directory);
        }

        #[test]
        fn a_late_answer_counts_a_miss_and_is_not_mistaken_for_the_next() {
            let directory = tempfile_dir();
            let slow = format!("sleep 0.3; {ANSWER}");
            let mut helper = fake_helper(&directory, 0.0, &slow);
            let cancelled = AtomicBool::new(false);
            let later = || Instant::now() + Duration::from_secs(5);
            helper.wait_ready(later(), &cancelled).unwrap();
            let soon = Instant::now() + Duration::from_millis(20);
            assert_eq!(
                helper.clean("first", &[], soon, &cancelled),
                Err(Fallback::Late)
            );
            assert_eq!(helper.missed, 1);
            // The first request's stale answer arrives first and must be skipped.
            assert_eq!(
                helper.clean("next", &[], later(), &cancelled).unwrap(),
                "clean"
            );
            assert_eq!(helper.next_id, 2);
            assert_eq!(helper.missed, 0);
            let _ = std::fs::remove_dir_all(directory);
        }

        #[test]
        fn a_helper_that_dies_is_reported_as_failed() {
            let directory = tempfile_dir();
            let mut helper = fake_helper(&directory, 0.0, "exit 1");
            let cancelled = AtomicBool::new(false);
            let later = Instant::now() + Duration::from_secs(5);
            assert!(matches!(
                helper.clean("hi", &[], later, &cancelled),
                Err(Fallback::Failed(_))
            ));
            let _ = std::fs::remove_dir_all(directory);
        }

        fn tempfile_dir() -> PathBuf {
            static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let directory = std::env::temp_dir().join(format!(
                "tiro-helper-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&directory).unwrap();
            directory
        }
    }
}
