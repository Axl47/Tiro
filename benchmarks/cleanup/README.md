# Raw transcription vs cleaned output

What Vedu Scribe does to real dictation, and what Tiro's stage adds around it. Measured
2026-11-05 on an M1 Pro with 16 GB, with the model pinned at `7dced584`. Full outputs are
in [`results-2026-11-05/`](results-2026-11-05/).

## Read this first

- **No real voice was involved.** Speech came from macOS `say`, reading Axel's two ChatGPT
  transcripts plus synthetic developer prompts, then through Parakeet. TTS doesn't
  hesitate like a person. It tests the path and the words, not how you actually sound.
- **Word error rate here is distance from a reference I wrote by hand**, so it partly
  scores my taste in clean prompts. Compare rows; don't read it as absolute accuracy. It
  is not comparable to the error rates on the model card.
- **The corpus is small and was used for tuning.** The guards were built looking at it,
  which is why the held-out set exists. Trust that set more.
- **The machine was shared.** A Java build, a Python job, and a VM ran during many runs;
  swap was 90%+ full. Latencies are representative of a busy day, not best case.

## Quality

Lower error and fillers are better. "Kept" counts required terms that survived; "dropped"
counts disfluencies removed.

Held-out set: 6 synthetic rows, written after the code froze, scored once.

| Input | Error | Fillers | Kept | Dropped | Vocabulary |
| --- | ---: | ---: | ---: | ---: | ---: |
| Raw Parakeet | 23.0% | 10 | 23/25 | 2/7 | 1/3 |
| Scribe, recognizer's casing | 10.0% | 3 | 23/25 | 6/7 | 1/3 |
| Scribe, lowercase + vocabulary | 9.4% | 3 | 23/25 | 6/7 | 3/3 |
| **Tiro's full stage** | **8.7%** | **2** | 23/25 | **7/7** | **3/3** |

Main corpus through Parakeet: 10 rows, including both real samples. Tuned against.

| Input | Error | Fillers | Kept | Dropped | Vocabulary |
| --- | ---: | ---: | ---: | ---: | ---: |
| Raw Parakeet | 28.3% | 21 | 41/43 | 6/12 | 2/14 |
| Scribe, recognizer's casing | 20.1% | 10 | 41/43 | 9/12 | 6/14 |
| Scribe, lowercase + vocabulary | 15.7% | 10 | 42/43 | 10/12 | 13/14 |
| **Tiro's full stage** | **8.5%** | **7** | **43/43** | **12/12** | 11/14 |

The Scribe rows are given each row's ideal vocabulary. Tiro's row selects its own from a
built-in list, so it can't know `OpenSuperWhisper`, hence 11/14. In the real app with a
small personal vocabulary, all of those terms came out right.

The same corpus as ChatGPT's already-punctuated text: raw 31.6%, Tiro 10.8%, fillers 27 to 7.

### What this says

Most of the gain is Scribe and its vocabulary header. The guards add a little on average,
and on the tuned corpus that gain is partly overfit; their real job is rare, expensive
failures that an average hides, such as a mangled path segment. Their value shows up in
what doesn't happen, not in the error rate.

Lowercasing is not a free choice: on already-clean text it makes no difference, but on
real Parakeet output it beat the recognizer's casing (20.1% to 16.1% without vocabulary).
Testing only on the ChatGPT samples would have given the wrong answer.

The speech model does much of the work you'd credit to cleanup. Parakeet alone removes
some fillers and punctuates.

### What still goes wrong

- **Retractions are the weak spot.** "Every like every keystroke, no wait, not every
  keystroke, it's when the debounce fires" became "rendering on every debounce fires":
  right idea, broken sentence. The ChatGPT-text version kept "no wait" outright.
- **Ordinary words can vanish.** Without vocabulary, "Open Super Whisper" became "Open
  Whisper". The guard can't see this, since a dropped word looks like a retraction.
- **Conservative on rambling.** It leaves much of "kind of", "I guess", "or something",
  and "just look up". It was trained to preserve wording, so this is closer to a feature
  than a bug: your style stays, but it will not tighten your prose.
- **The model can change a word's sense.** Scribe turned a leading "Yeah" into "Yes", and
  "users table" into "user's table".
- **Recognition errors pass through.** `--locked` heard as `--lock`, "Jest" as "just".
  Cleanup can only fix these for terms in your vocabulary.

## Speed and memory

Real app, one Parakeet stream and one helper, from `dictation stop` to text:

| | Median | Range |
| --- | ---: | ---: |
| Raw, no cleanup | 409 ms | 330-589 ms |
| Cleaned, first dictation after launch, all 10 clips | 1,266 ms | 668-2,910 ms |
| Cleaned, later dictations of a 24-word sentence | ~820 ms | 665-1,039 ms |

So cleanup adds about 300-800 ms on a typical prompt, and about 2.4 s on a 219-word,
67-second monologue.

Scribe alone, warm, through the helper, on a quiet moment:

| Words | Time |
| ---: | ---: |
| 5 | 90 ms |
| 24 | 238 ms |
| 44 | ~375 ms |
| 143 | ~1,250 ms |
| 219 | ~1,930 ms |

| Memory | |
| --- | ---: |
| Helper, warm (`phys_footprint`) | 690-750 MB |
| App, with Parakeet loaded | ~220 MB |
| Helper load, spawn to warm | ~1.6 s, up to ~3.8 s under load |

The helper exits after 15 idle minutes and with the app. Hard-killing its parent also
ended it, since stdin closes.

**Unexplained:** inside the app the helper took 273-662 ms on a sentence that takes
238 ms in isolation, alternating slow and fast. The extra time is inside the helper, not
in Rust (under 10 ms) and not idle wake-up or first-call cost, both of which were tested.
Forcing its QoS class didn't help. GPU contention from the HUD or Parakeet is the likely
suspect, but it hasn't been shown.

## Run it

```sh
TIRO_CODESIGN_IDENTITY="…" scripts/build-app.sh
CLIPS=$(mktemp -d); MODELS="$HOME/Library/Application Support/tiro/models"
benchmarks/cleanup/make_clips.py "$CLIPS"

# Scribe through the helper; add --api <data-dir> with `hex service` running for the full stage
benchmarks/cleanup/bench.py --helper target/app/Tiro.app/Contents/Helpers/tiro-scribe \
  --model "$MODELS/vedu-scribe-7dced584"

# Through the real app, in its own data folder
benchmarks/cleanup/e2e.py --app target/app/Tiro.app --clips "$CLIPS" --models "$MODELS" \
  --work "$(mktemp -d)" --vocabulary
```

`holdout.json` is for scoring only. Once you tune against it, it stops being held out;
write new rows instead.
