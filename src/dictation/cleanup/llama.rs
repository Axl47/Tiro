//! Owns a llama.cpp `llama-server` process for cleanup models shipped as GGUF:
//! Phonon's refiner everywhere, and Vedu Scribe off macOS. Like `tiro-scribe`,
//! the model runs outside this process, so stopping it returns all of its
//! memory and a GPU fault costs one cleanup, not the app.
//!
//! The server is a pinned llama.cpp release, downloaded with the weights into
//! the models folder and checked by SHA-256. It listens on loopback behind a
//! per-launch key, and runs under a small shell that stops it when Tiro's end
//! of a pipe closes, so a crashed or killed Tiro never leaves it running.

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, bail, eyre};
use sha2::{Digest as _, Sha256};

use super::Fallback;

/// A warm server holds about 1 GB, so one that stops being used does not stay.
const IDLE_LIMIT: Duration = Duration::from_secs(15 * 60);
/// One late answer is usually one long dictation; two in a row mean it is wedged.
const MISSES_BEFORE_RESTART: u32 = 2;
/// Room for a long dictation, its prompt, and its answer.
const CONTEXT_TOKENS: u32 = 8192;
/// The longest a start may take: the first start on a Mac compiles Metal
/// shaders (about 15 s on an M1 Pro), and a slow disk reads the weights.
const START_LIMIT: Duration = Duration::from_secs(180);

/// A pinned download: `(url, bytes, sha256)`.
type Pin = (&'static str, u64, &'static str);

/// A cleanup model as one pinned GGUF file.
pub(super) struct Gguf {
    /// The folder under `models/`, named by revision so a new pin never mixes files.
    pub folder: &'static str,
    pub file: &'static str,
    pub pin: Pin,
}

/// Phonon's refiner: a full fine-tune of LiquidAI's LFM3-1.7B, Q4_K_S. The file
/// Phonon itself ships for its Windows, Linux, iPhone, and Android builds.
pub(super) const PHONON: Gguf = Gguf {
    folder: "phonon-refiner-1.7b-f5e269a",
    file: "phonon-refiner-1.7b-Q4_K_S-e4.gguf",
    pin: (
        "https://huggingface.co/Infatoshi/phonon-refiner-1.7b/resolve/f5e269abf1fd05de59cb43947515d3c3194b79cf/phonon-refiner-1.7b-Q4_K_S-e4.gguf",
        988_453_504,
        "8222be26ec129471331af4e13eefed9c5852cbb489f4e7043ceb6c4496f56856",
    ),
};

/// Vedu Scribe at Tiro's pinned revision (`7dced584`), converted to Q8_0. It
/// wrote the MLX model's text on 272 of 303 History dictations, and most of
/// the rest differ by a comma (`benchmarks/cleanup/README.md`).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(super) const VEDU: Gguf = Gguf {
    folder: "vedu-scribe-gguf-7dced584",
    file: "vedu-scribe-0.8b-Q8_0.gguf",
    pin: (
        "https://huggingface.co/sumitsah35/vedu-scribe-0.8b-gguf/resolve/4b72e79051192cf42dc75e097dd50862b527437a/vedu-scribe-0.8b-Q8_0.gguf",
        811_843_296,
        "72ca37256b0b38e3699dfd1bf110284c4e93e1cc45bff20d707cd3fd2546f079",
    ),
};

/// The llama.cpp release Phonon pins (`b12132`, the first that knows LFM3), as
/// `(folder, archive pin)`. Linux takes the Vulkan build, which runs on the CPU
/// when no Vulkan driver loads.
fn runtime() -> Option<(&'static str, Pin)> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some((
            "llama-b12132-macos-arm64",
            (
                "https://github.com/ggml-org/llama.cpp/releases/download/b12132/llama-b12132-bin-macos-arm64.tar.gz",
                16_671_755,
                "0f7b107b860d3e9b7066ddc2b90e2f417641a4fd5f37355a603a1e52a98dc461",
            ),
        ))
    } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Some((
            "llama-b12132-linux-vulkan-x64",
            (
                "https://github.com/ggml-org/llama.cpp/releases/download/b12132/llama-b12132-bin-ubuntu-vulkan-x64.tar.gz",
                35_887_664,
                "ff02b3e3535b0b195d846846ff8a88175bf938a5c0415fb562a7587004756109",
            ),
        ))
    } else {
        None
    }
}

/// One completion: a system and a user turn, decoded greedily with thinking off.
pub(super) struct Prompt<'a> {
    pub system: &'a str,
    pub user: &'a str,
    /// The most new tokens, from the user turn's token count.
    pub budget: fn(usize) -> usize,
}

struct Server {
    model: &'static str,
    /// The wrapper shell; closing `stdin` stops the server and then it.
    child: Child,
    stdin: Option<ChildStdin>,
    base: String,
    key: String,
    agent: ureq::Agent,
    ready: bool,
    last_used: Instant,
    missed: u32,
}

impl Server {
    fn spawn(model: &'static Gguf) -> Result<Self> {
        let (folder, _) = runtime().ok_or_else(|| eyre!("no llama.cpp build for this computer"))?;
        let models = crate::transcription::models::models_dir()?;
        let binary = models.join(folder).join("llama-b12132/llama-server");
        let weights = models.join(model.folder).join(model.file);
        // A free port, released for the server to take. Another process taking
        // it in between fails the health check, and the next press respawns.
        let port = std::net::TcpListener::bind("127.0.0.1:0")?
            .local_addr()?
            .port();
        let key = random_key()?;
        let mut child = crate::desktop::background_process::command("/bin/sh")
            .arg("-c")
            .arg(
                r#""$@" & server=$!; trap 'kill $server 2>/dev/null' EXIT HUP TERM; while read -r _; do :; done"#,
            )
            .arg("tiro-cleanup")
            .arg(&binary)
            .args(["--model".as_ref(), weights.as_os_str()])
            .args(["--host", "127.0.0.1", "--port", &port.to_string()])
            .args(["--api-key", &key])
            .args(["--ctx-size", &CONTEXT_TOKENS.to_string()])
            .args(["--n-gpu-layers", "99", "--parallel", "1"])
            // The whole model on the first GPU: with an integrated GPU beside a
            // discrete one (Vulkan), llama.cpp would otherwise split the layers.
            .args(["--split-mode", "none"])
            .args(["--jinja", "--reasoning-format", "none", "--no-ui"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| eyre!("couldn't start {}: {error}", binary.display()))?;
        let stdin = child.stdin.take();
        tracing::info!(model = model.file, port, "cleanup server starting");
        Ok(Self {
            model: model.file,
            child,
            stdin,
            base: format!("http://127.0.0.1:{port}"),
            key,
            agent: crate::net::loopback_agent(|config| config.http_status_as_error(false)),
            ready: false,
            last_used: Instant::now(),
            missed: 0,
        })
    }

    /// Waits for `/health` until `by`. Loading takes longer than a short
    /// dictation's deadline, so running out here leaves it loading.
    fn wait_ready(&mut self, by: Instant, cancelled: &AtomicBool) -> Result<(), Fallback> {
        while !self.ready {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(Fallback::Failed(format!("the server exited: {status}")));
            }
            let health = self.agent.get(format!("{}/health", self.base)).call();
            if health.is_ok_and(|response| response.status() == 200) {
                tracing::info!(model = self.model, "cleanup server ready");
                self.ready = true;
                break;
            }
            if cancelled.load(Ordering::Relaxed) {
                return Err(Fallback::Cancelled);
            }
            if Instant::now() >= by {
                return Err(Fallback::Unavailable("still loading".into()));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        Ok(())
    }

    /// Waits until the server answers, then runs two short completions, so no
    /// dictation is the one that loads the weights into memory and runs cold.
    fn warm(&mut self) -> Result<()> {
        let never = AtomicBool::new(false);
        let prompt = Prompt {
            system: "Clean up this dictation.",
            user: "um so can you run the tests and then, uh, push it",
            budget: |tokens| tokens + 16,
        };
        let started = Instant::now();
        for _ in 0..2 {
            self.complete(&prompt, started + START_LIMIT, &never)
                .map_err(|fallback| eyre!("warming up: {fallback:?}"))?;
        }
        tracing::info!(
            model = self.model,
            warm_ms = started.elapsed().as_millis() as u64,
            "cleanup server warm"
        );
        Ok(())
    }

    fn complete(
        &mut self,
        prompt: &Prompt,
        by: Instant,
        cancelled: &AtomicBool,
    ) -> Result<String, Fallback> {
        self.wait_ready(by, cancelled)?;
        self.last_used = Instant::now();
        let (send, answer) = mpsc::channel();
        let (agent, base, key) = (self.agent.clone(), self.base.clone(), self.key.clone());
        let (system, user, budget) = (
            prompt.system.to_owned(),
            prompt.user.to_owned(),
            prompt.budget,
        );
        // The request runs on its own thread so a deadline or cancel returns at
        // once; the server finishes it, and the next request queues behind it.
        std::thread::Builder::new()
            .name("cleanup-request".into())
            .spawn(move || {
                let _ = send.send(request(&agent, &base, &key, &system, &user, budget));
            })
            .map_err(|error| Fallback::Failed(error.to_string()))?;
        let result = loop {
            if cancelled.load(Ordering::Relaxed) {
                break Err(Fallback::Cancelled);
            }
            let left = by.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break Err(Fallback::Late);
            }
            match answer.recv_timeout(left.min(Duration::from_millis(20))) {
                Ok(reply) => break reply.map_err(Fallback::Failed),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => {
                    break Err(Fallback::Failed("the request thread ended".into()));
                }
            }
        };
        match result {
            Ok(_) => self.missed = 0,
            Err(Fallback::Late) => self.missed += 1,
            Err(_) => {}
        }
        result
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Closing the pipe ends the shell's loop; its trap stops the server.
        drop(self.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if !matches!(self.child.try_wait(), Ok(None)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `/tokenize` for the budget, then `/v1/chat/completions` with the model's
/// template and thinking off. Errors never carry the dictated text.
fn request(
    agent: &ureq::Agent,
    base: &str,
    key: &str,
    system: &str,
    user: &str,
    budget: fn(usize) -> usize,
) -> Result<String, String> {
    let post = |path: &str, body: serde_json::Value| -> Result<serde_json::Value, String> {
        let mut response = agent
            .post(format!("{base}{path}"))
            .header("authorization", format!("Bearer {key}"))
            .header("content-type", "application/json")
            .send(body.to_string())
            .map_err(|error| format!("{path}: {}", crate::net::reason(&error)))?;
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(4 << 20)
            .read_to_string()
            .map_err(|error| format!("{path}: {error}"))?;
        if status != 200 {
            return Err(format!("{path}: HTTP {status}"));
        }
        serde_json::from_str(&text).map_err(|error| format!("{path}: {error}"))
    };
    let tokens = post("/tokenize", serde_json::json!({ "content": user }))?["tokens"]
        .as_array()
        .map_or(0, Vec::len);
    let reply = post(
        "/v1/chat/completions",
        serde_json::json!({
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user },
            ],
            "chat_template_kwargs": { "enable_thinking": false },
            "temperature": 0,
            "top_k": 1,
            "max_tokens": budget(tokens),
            "cache_prompt": true,
        }),
    )?;
    reply["choices"][0]["message"]["content"]
        .as_str()
        .map(|text| {
            text.replace("<think>", "")
                .replace("</think>", "")
                .trim()
                .to_owned()
        })
        .ok_or_else(|| "an answer without text".into())
}

fn random_key() -> Result<String> {
    let mut bytes = [0u8; 24];
    getrandom::fill(&mut bytes).map_err(|error| eyre!("no random key: {error}"))?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

static SERVER: Mutex<Option<Server>> = Mutex::new(None);
static PREPARING: AtomicBool = AtomicBool::new(false);
static IDLE_WATCH: AtomicBool = AtomicBool::new(false);
/// Set while a download for cleanup runs: dictations don't wait for it.
static DOWNLOADING: AtomicBool = AtomicBool::new(false);

fn slot() -> MutexGuard<'static, Option<Server>> {
    SERVER.lock().unwrap_or_else(|error| error.into_inner())
}

/// Gets `model`'s server warm without holding up the caller: downloads what is
/// missing, then starts it. Cheap once it is up.
pub(super) fn prepare(model: &'static Gguf) {
    // Runs on the capture path, and a request holds the lock throughout
    // inference: locked means a server exists, so never wait for it.
    match SERVER.try_lock() {
        Err(TryLockError::WouldBlock) => return,
        Ok(slot)
            if slot
                .as_ref()
                .is_some_and(|server| server.model == model.file) =>
        {
            return;
        }
        _ => {}
    }
    if PREPARING.swap(true, Ordering::AcqRel) {
        return;
    }
    let started = std::thread::Builder::new()
        .name("cleanup-prepare".into())
        .spawn(move || {
            let outcome = (|| -> Result<()> {
                install(model)?;
                let mut server = Server::spawn(model)?;
                server.warm()?;
                *slot() = Some(server);
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

/// One completion against `model`'s warm server, started if it isn't. A server
/// that failed, or keeps missing its deadline, is stopped; the next press
/// restarts it.
pub(super) fn complete(
    model: &'static Gguf,
    prompt: &Prompt,
    deadline: Duration,
    cancelled: &AtomicBool,
) -> Result<String, Fallback> {
    let by = Instant::now() + deadline;
    let current = |slot: &Option<Server>| slot.as_ref().is_some_and(|s| s.model == model.file);
    if !current(&slot()) {
        prepare(model);
    }
    // A press usually started the load already; give it the deadline, unless
    // it is a download, which takes minutes.
    while !current(&slot()) {
        if DOWNLOADING.load(Ordering::Acquire) {
            return Err(Fallback::Unavailable("downloading".into()));
        }
        if !PREPARING.load(Ordering::Acquire) {
            return Err(Fallback::Unavailable(
                "the cleanup server didn't start".into(),
            ));
        }
        if cancelled.load(Ordering::Relaxed) {
            return Err(Fallback::Cancelled);
        }
        if Instant::now() >= by {
            return Err(Fallback::Unavailable("still loading".into()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut slot = slot();
    let server = slot
        .as_mut()
        .filter(|server| server.model == model.file)
        .ok_or_else(|| Fallback::Unavailable("the cleanup server stopped".into()))?;
    let result = server.complete(prompt, by, cancelled);
    if matches!(result, Err(Fallback::Failed(_))) || server.missed >= MISSES_BEFORE_RESTART {
        *slot = None;
    }
    result
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
                let mut slot = slot();
                if slot
                    .as_ref()
                    .is_some_and(|s| s.last_used.elapsed() >= IDLE_LIMIT)
                {
                    tracing::info!("stopping the idle cleanup server");
                    *slot = None;
                }
            }
        });
}

/// Downloads and verifies what `model` needs that isn't here: the llama.cpp
/// release (unpacked once verified) and the weights.
fn install(model: &Gguf) -> Result<()> {
    let (folder, archive) =
        runtime().ok_or_else(|| eyre!("no llama.cpp build for this computer"))?;
    let models = crate::transcription::models::models_dir()?;
    let runtime_dir = models.join(folder);
    if !runtime_dir.join("llama-b12132/llama-server").exists() {
        std::fs::create_dir_all(&runtime_dir)?;
        let tarball = runtime_dir.join("llama.tar.gz");
        fetch(archive, &tarball)?;
        let unpacked = crate::desktop::background_process::command("tar")
            .arg("-xzf")
            .arg(&tarball)
            .arg("-C")
            .arg(&runtime_dir)
            .status()?;
        if !unpacked.success() {
            bail!("couldn't unpack the llama.cpp release: {unpacked}");
        }
        std::fs::remove_file(&tarball)?;
    }
    let weights = models.join(model.folder);
    std::fs::create_dir_all(&weights)?;
    let file = weights.join(model.file);
    if !std::fs::metadata(&file).is_ok_and(|meta| meta.len() == model.pin.1) {
        fetch(model.pin, &file)?;
    }
    Ok(())
}

/// Downloads `pin` to `destination` through `net`, promoting it to its real
/// name only after its hash matches.
fn fetch((url, bytes, sha256): Pin, destination: &Path) -> Result<()> {
    let partial = PathBuf::from(format!("{}.partial", destination.display()));
    tracing::info!(bytes, file = %destination.display(), "downloading for cleanup");
    DOWNLOADING.store(true, Ordering::Release);
    let downloaded = crate::net::download(
        &crate::net::Download {
            url,
            user_agent: &format!("tiro/{}", crate::VERSION),
            size: Some(bytes),
            canceled: None,
        },
        &partial,
        |_| {},
    );
    DOWNLOADING.store(false, Ordering::Release);
    downloaded?;
    let found = sha256_of(&partial)?;
    if found != sha256 {
        let _ = std::fs::remove_file(&partial);
        bail!("{} failed verification: {found}", destination.display());
    }
    std::fs::rename(&partial, destination)?;
    Ok(())
}

fn sha256_of(path: &Path) -> Result<String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    /// The wrapper must stop what it started once Tiro's end of the pipe closes.
    #[cfg(unix)]
    #[test]
    fn closing_the_pipe_stops_the_server() {
        let mut child = crate::desktop::background_process::command("/bin/sh")
            .arg("-c")
            .arg(
                r#""$@" & server=$!; echo $server; trap 'kill $server 2>/dev/null' EXIT HUP TERM; while read -r _; do :; done"#,
            )
            .arg("tiro-cleanup")
            .args(["sleep", "30"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut pid = String::new();
        let mut stdout = child.stdout.take().unwrap();
        let mut byte = [0u8; 1];
        while stdout.read(&mut byte).unwrap() == 1 && byte[0] != b'\n' {
            pid.push(byte[0] as char);
        }
        child.stdin.take().unwrap().write_all(b"\n").unwrap();
        // The pipe closed when it was dropped above.
        child.wait().unwrap();
        let alive = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .unwrap()
            .success();
        assert!(!alive, "the server outlived its pipe");
    }
}
