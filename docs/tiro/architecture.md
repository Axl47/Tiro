# Tiro: evaluating Hex, and what changed

Written before and during the fork, against `anomalyco/hex` at `131e9ccd`
(2026-11-05). Facts here come from reading that source and running it. The Mac is
an M1 Pro with 16 GB.

## Which Hex

Two projects carry the name. `kitlangton/Hex` is the small Swift app most people
picture, and it is archived. `anomalyco/hex` is its successor: a 1,473-file Rust
codebase with its own UI toolkit (Rune), Windows, Linux, and iOS targets, Meetings,
Annotate, and a virtual microphone. Tiro forks the second. Expect its upstream to
move daily, and to take no outside pull requests.

## How a dictation flows

```text
hotkey ─▶ suppression/recognition ─▶ dictation/audio (mic timeline, pre-roll)
       ─▶ transcription/engine ─ live stream, on one thread that owns the model
       ─▶ parakeet.rs worker ─▶ dictation/processor ─▶ paste
```

`Profiles::process_cancellable` is the seam: every OS's dictation passes through it
as text, off the UI thread, already cancellable, with a raw copy kept for History.

## What Hex already gets right for this

- **Speech is already fast.** Parakeet Unified English streams during capture, so
  stopping finishes a stream rather than transcribing a clip from scratch.
- **Fallback discipline.** The OpenCode rewrite is deadline-bounded and falls back
  to the corrected transcript. Cleanup copies that shape.
- **A local API.** `/transcriptions`, `/streams`, and `/control` are authenticated
  and bounded. Most of the Android work is already here (`android-lan.md`).
- **Real-path tooling.** `HEX_AUDIO_FIXTURE` replaces the microphone and
  `hex dictation stop --print` returns text without pasting. That is what makes an
  honest end-to-end benchmark possible without recording anyone.

## What it costs

- **Size.** Most of the binary is unrelated to dictation. Those features are off
  until enabled, so they cost binary size more than runtime; the app idled around
  190 MB in testing.
- **Merge pressure.** A broad rename or deletion would conflict with a very active
  upstream indefinitely. "Hex" appears in about 1,150 Rust string literals.
- **Existing cleanup doesn't fit.** The OpenCode rewrite needs OpenCode running and
  usually a cloud model. Labs' filler removal is a word list that can't handle false
  starts or self-corrections.

## Decisions

### Run Scribe in a separate process

| | Separate Swift process (chosen) | MLX linked into the Rust binary | Convert to GGUF, use llama.cpp |
| --- | --- | --- | --- |
| Memory when idle | Exit returns all of it | Stays mapped | Could free it |
| A Metal fault | Costs one cleanup | Takes down the app | Takes down the app |
| Build impact | Own SwiftPM package | Swift and Metal in Cargo's build | New native dependency |
| Model fidelity | The published weights | The published weights | Lossy: mixed-quantized weights re-quantized, invalidating the card's evaluation |
| Added latency | A pipe write | None | None |

The pipe costs well under a millisecond against hundreds spent generating, so the
isolation is close to free. `tiro-scribe` holds no policy: the Rust side decides what
to trust.

### The model doesn't get the last word

The benchmark drove this design. On real transcripts Scribe mangled words it should
have copied (`dictation` → `dictionation`), dropped a path segment, and kept "but
yeah". So the stage became:

```text
transcript ─▶ spoken paths ─▶ vocabulary ─▶ Scribe ─▶ faithfulness ─▶ residue ─▶ text
                                              └─ unavailable, late, or unfaithful ─┘
```

- **Spoken paths** (`cleanup/spoken_paths.rs`): Scribe drops segments from "src slash
  dictation slash processor dot rs" but copies `src/dictation/processor.rs` exactly,
  so the path is written out first.
- **Vocabulary** (`cleanup/vocab.rs`): the model's `[vocab]` header is what gets
  technical terms right (5 of 14 without it, 13 of 14 with it, on real Parakeet text).
  It sends the person's terms, then a built-in developer list, pre-matched to what was
  said.
- **Faithfulness** (`cleanup/faithful.rs`): repairs near copies, and rejects output
  that lost code-shaped tokens or most of the speech, grew, or is largely new words.
- **Residue** (`cleanup/residue.rs`): removes hesitations and discourse markers, and
  is also the whole of cleanup whenever the model can't run.

Limit of this design: the guard catches mangled and missing *code-shaped* text, not a
dropped ordinary word, because a dropped word looks exactly like a correct retraction.
History keeps the raw transcript for that case. On held-out rows the guards improved
the average only slightly (`benchmarks/cleanup/README.md`); they exist for rare,
expensive failures rather than for the average.

### Failures the real path found

Unit tests passed through all of these; running the real pipeline caught them:

- Killing the helper on any missed deadline was an endless loop: a short dictation's
  deadline was shorter than the model's load, so it never finished loading. A helper
  that's still loading is now left alone, and only two inference misses in a row restart it.
- `swift-transformers` prints to stdout, which corrupted the protocol. The helper now
  gives the protocol a private descriptor and points fd 1 at stderr.
- Warming up once didn't remove the first-dictation penalty; the second inference is
  slow too. A longer warm-up prompt didn't help, and a second pass did.

### Keep the fork thin

Tiro changes identity (name, bundle ID, data folder), adds the stage, and narrows the
model list. It does not rename internal `hex_` symbols, the `hex` binary, or `HEX_*`
variables, does not rewrite all UI copy, and does not delete Meetings or Annotate.
Each of those would trade a little tidiness for permanent conflicts. Its everyday
build is a local build, which never checks Hex's update channel.

Cleanup lives under Labs because that gives it a switch, a CLI, and a settings key for
a few lines, not because it is experimental. It is the one feature on by default.

## Keeping up with upstream

```sh
git fetch upstream && git merge upstream/dev
```

Conflicts should stay in `src/labs.rs`, `scripts/build-app.sh`, and the few wiring
lines in `processor.rs`. The stage itself is in new files.
