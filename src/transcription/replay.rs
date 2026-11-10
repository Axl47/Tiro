//! `hex replay-dictations`: saved recordings through the path a dictation
//! takes (the transcription engine's live stream, its whole-clip fallback,
//! and the vocabulary), to measure a vocabulary against real speech. Each
//! clip prints as one JSON line; nothing is pasted or saved.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, WrapErr, eyre};
use serde::Deserialize;

use crate::app_settings::LabsSettings;
use crate::dictation::vocabulary::{self, VocabularyBoost, VocabularyEntry};
use crate::parakeet::Parakeet;
use crate::transcription::engine::{Offline, TranscriptionEngine};
use crate::transcription::models::TranscriptionSelection;
use crate::transcription::{Transcriber, WarmTranscriber};

const BLOCK_SAMPLES: usize = 2_560;
const BLOCK_SECONDS: f64 = 0.16;
/// Held back from the stream, as a capture's release tail is.
const HELD_SAMPLES: usize = 3_200;

#[derive(Deserialize)]
struct Clip {
    id: String,
    audio: PathBuf,
}

/// What one run replays with.
pub struct Options {
    pub model: PathBuf,
    /// A list in `settings.json`'s `vocabulary` shape; none transcribes as heard.
    pub vocabulary: Option<PathBuf>,
    pub boost: VocabularyBoost,
    /// Times real speed to feed the stream; high enough overfeeds it.
    pub speed: f64,
}

/// Replays the clips in the JSON array at `manifest`, one line each.
pub fn run(manifest: &Path, options: Options) -> Result<()> {
    let clips: Vec<Clip> = read_json(manifest)?;
    let entries: Vec<VocabularyEntry> = match &options.vocabulary {
        Some(path) => read_json(path)?,
        None => Vec::new(),
    };
    vocabulary::apply(&entries, options.boost);
    crate::labs::apply(&LabsSettings {
        vocabulary_biasing: true,
        ..Default::default()
    });

    let selection = TranscriptionSelection::default();
    let engine = TranscriptionEngine::start(WarmTranscriber::serving(Transcriber::Gguf(
        Box::new(Parakeet::load_streaming(&options.model, &selection)?),
    )))?;
    let offline = || -> Offline {
        let selection = selection.clone();
        Box::new(move |model, samples| {
            model
                .activate(&selection)
                .and_then(|model| model.transcribe(samples))
                .map_err(|error| error.to_string())
        })
    };
    for clip in clips {
        let samples = read_wav(&clip.audio)?;
        let mut stream = engine
            .begin()
            .ok_or_else(|| eyre!("the model can't stream"))?;
        let started = Instant::now();
        let held = samples.len().saturating_sub(HELD_SAMPLES);
        for (index, block) in samples[..held].chunks(BLOCK_SAMPLES).enumerate() {
            let due = Duration::from_secs_f64(index as f64 * BLOCK_SECONDS / options.speed);
            thread::sleep(due.saturating_sub(started.elapsed()));
            stream.feed(block);
        }
        thread::sleep(Duration::from_secs_f64(0.2 / options.speed));
        let result = engine
            .finish_with(Some(stream), samples, offline())?
            .wait()
            .map_err(|reason| eyre!(reason))?;
        println!(
            "{}",
            serde_json::json!({
                "id": clip.id,
                "text": result.text,
                "source": result.source.label(),
                "finish_ms": result.finish_ms,
            })
        );
    }
    Ok(())
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let bytes =
        std::fs::read(path).wrap_err_with(|| format!("could not read {}", path.display()))?;
    serde_json::from_slice(&bytes).wrap_err_with(|| format!("could not parse {}", path.display()))
}

/// A 16 kHz mono 16-bit WAV, as Keep recordings writes them.
fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let mut reader = hound::WavReader::open(path)
        .wrap_err_with(|| format!("could not open {}", path.display()))?;
    reader
        .samples::<i16>()
        .map(|sample| Ok(f32::from(sample?) / 32_768.0))
        .collect()
}
