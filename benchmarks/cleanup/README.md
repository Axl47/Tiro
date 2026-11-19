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

## Sentence by sentence

A streamed LAN dictation (`src/lan/stream.rs`) cleans each sentence while the phone still
records, so the release waits only on the last one. `bench.py --api` scores that as
`tiro-sentences`: each sentence through `POST /cleanup` alone, except that a sentence
opening with a correction ("No actually five…", "I mean…") stays with the one before.
Inputs are the corpus clips streamed through `/streams` at real-time pace
([`streamed-transcripts.json`](results-2026-11-05/streamed-transcripts.json), usable as `--inputs`).

| Input | Error | Fillers | Kept | Dropped | Vocabulary |
| --- | ---: | ---: | ---: | ---: | ---: |
| Held-out, whole transcript | 8.7% | 2 | 23/25 | 7/7 | 3/3 |
| Held-out, by sentence | 8.7% | 2 | 23/25 | 7/7 | 3/3 |
| Main, whole transcript | 9.6% | 8 | 42/43 | 12/12 | 9/14 |
| Main, by sentence | 9.9% | 8 | 42/43 | 12/12 | 8/14 |

Without the correction rule, by sentence scored 10.3% and 12.2%, and lost one dropped
disfluency in each set: "three attempts? No actually five attempts" kept both counts.
The rule was written after seeing that row, so the held-out set no longer tests it. The
one remaining difference is context: alone, "Zot" stayed "Zot"; in the whole transcript
it became "Zod".

## Prompt lookup doesn't pay here

A cleaned dictation is mostly the transcript again, so drafting tokens by copying from
the transcript and verifying them in one pass (prompt-lookup decoding) looked like a
lossless speedup. It produced the same text in 12 passes instead of 43, and took just
as long. One forward pass, warm, M1 Pro:

| Tokens per pass | 1 | 2 | 4 | 8 | 16 | 32 |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Time | 9 ms | 12 ms | 19 ms | 33 ms | 44 ms | 44 ms |

Below about 16 rows MLX's quantized matmul runs as a matrix-vector product per row, so a
pass costs nearly as much as decoding its tokens one at a time, and even a wide pass costs
five single steps. Cleanup's edits (casing, punctuation, fillers) end a guess every few
tokens, and each miss costs another pass to restore the recurrent layers' state.

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

## Phonon's refiner, and running off the Mac

Measured 2026-11-19 with [`compare.py`](compare.py). The models compared:

- **Tiro**: the full stage through `POST /cleanup` (Vedu Scribe 0.8B on MLX, plus its guards).
- **Phonon**: [Phonon](https://github.com/infatoshi/phonon)'s refiner, an LFM3-1.7B fine-tune,
  `phonon-refiner-1.7b-Q4_K_S-e4.gguf` at `f5e269a`. It runs with Phonon's prompt, writing
  mode, output budget, length guard, and term restoration, and gets your vocabulary and
  Tiro's built-in developer terms.
- **Vedu GGUF**: Tiro's own pinned model (`7dced584`) converted to Q8_0 GGUF
  (`sumitsah35/vedu-scribe-0.8b-gguf`), as the bare model.

Both GGUFs ran on stock llama.cpp `b12132`, the release Phonon ships: Metal on the M1 Pro,
and the Ubuntu Vulkan build on Omarchy with an RTX 4060.

### Synthetic sets (hand-written references)

| | Held-out error | Fillers | Main error | Fillers | Dropped |
| --- | ---: | ---: | ---: | ---: | ---: |
| Raw | 22.6% | 9 | 27.9% | 21 | 10/19 |
| Tiro | 8.7% | 2 | **9.6%** | 8 | **19/19** |
| Phonon | **6.3%** | **1** | 9.9% | **5** | 17/19 |
| Vedu GGUF, bare | 10.0% | 3 | 17.8% | 11 | 16/19 |

Phonon kept the retraction in "every keystroke. No, not every keystroke, it's when the
debounce fires" as spoken. Tiro applied it, but broke the sentence.

### Real dictations (History, 303 rows)

The edits you made after a paste don't separate the models. Most of them fix misheard
words or add new content, which no cleanup model can recover from the transcript: of 91
short fixes, raw text already had 4, Phonon 3, and Vedu 1. The reference also starts as
Tiro's output, so it scores closeness to Tiro.

Instead, a blind A/B judge (Claude Sonnet, randomized order, faithfulness first) compared
Tiro with Phonon on every row. Phonon ran in `prompt` mode for T3 Code and Annex, and in
`minimal` for Phone. `casual`, its chat mode, writes lowercase and lost most Phone rows on
casing alone.

| | Phonon better | Tiro better | Tie or identical |
| --- | ---: | ---: | ---: |
| Desktop (195) | 38 (10 major) | 22 (6 major) | 135 |
| Phone (108) | 26 (9 major) | 20 (5 major) | 62 |
| **All (303)** | **64 (19 major)** | **42 (11 major)** | 197 |

"Major" means a dropped clause or wrong word, not a cleanup or punctuation difference.
Tiro's major losses include dropping the last two sentences of a long dictation and
writing "Adobe Premiere" for "Eclipse". Phonon's include "3,999" heard as "3,990", "two
threads with Opus" turned into "Opus 2, Threads", and kept retractions. Phonon left fewer
fillers: 248 across History, against Tiro's 352.

### Off the Mac

The Mac dependency is MLX, not the model. Both GGUFs ran on Linux and wrote the same
text as on the Mac (Phonon 293/303 rows identical, Vedu 296/303). The Vedu GGUF matched
Tiro's MLX helper on 272/303 rows, and most of the rest differ by a comma. `sidecar::clean`
off macOS is where a llama.cpp engine would plug in; the guards around it are Rust already.

| Per dictation, History | p50 | p95 | Memory |
| --- | ---: | ---: | ---: |
| Tiro, M1 Pro (MLX + guards) | 575 ms | 1,599 ms | ~0.7 GB |
| Phonon, M1 Pro (llama.cpp Metal) | 1,161 ms | 2,741 ms | ~1 GB |
| Vedu GGUF, M1 Pro (llama.cpp Metal) | 824 ms | 2,350 ms | ~0.9 GB |
| Phonon, RTX 4060 (Vulkan) | 455 ms | 1,199 ms | 1.1 GB VRAM |
| Vedu GGUF, RTX 4060 (Vulkan) | 383 ms | 1,048 ms | 0.9 GB VRAM |

The 4060 rows were timed from the Mac over an SSH tunnel on the LAN. None of these runs
uses Phonon's own speedups, which refine while you speak and use its Metal and CUDA
kernels. The refiner is under the LFM Open License v1.0, free below $10M in yearly revenue.

```sh
# llama.cpp b12132 release; any build that knows the lfm3 architecture works
llama-server -m phonon-refiner-1.7b-Q4_K_S-e4.gguf --port 18731 -ngl 99 -c 8192 --jinja &
llama-server -m vedu-scribe-0.8b-Q8_0.gguf --port 18732 -ngl 99 -c 8192 --jinja &
benchmarks/cleanup/compare.py --set history --out "$(mktemp -d)" \
  --tiro "$HOME/Library/Application Support/tiro" \
  --phonon http://127.0.0.1:18731 --scribe http://127.0.0.1:18732
```

### In Tiro, cleaning while you speak

Measured 2026-11-19 after Phonon moved into Tiro (Labs › Clean up with Phonon,
`cleanup/llama.rs`), with Tiro's faithfulness check and residue after it, and with
cleanup while recording (`cleanup/live.rs`) on for both models.

Through `POST /cleanup` on the synthetic sets, Phonon in Tiro keeps what it scored
alone: 6.3% held-out error against Vedu's 8.7%, and 9.7% against 9.6% on the main set,
with 5 fillers left against 10. On 315 History rows it fell back twice, both on a
spoken path it lost ("slash"), and whole-dictation latency was 1,161 ms p50 and
2,935 ms p95 against Vedu's 575 and 1,599.

`e2e.py` on the 10 spoken clips, through the real app: the time cleanup adds after
release, once sentences cleaned while recording are reused.

| Added after release | Median | Max | 67 s dictation |
| --- | ---: | ---: | ---: |
| Vedu Scribe | 307 ms | 849 ms | 322 ms (6 sentences reused, 20 words left) |
| Phonon | 933 ms | 1,524 ms | 799 ms (6 reused, 20 left) |

Without cleaning while recording, the 67 s dictation's whole cleanup takes about
3 s with Phonon. A second Phonon run, on a heavily loaded Mac, had a median of
1,120 ms; one clip's stream fell back to the whole clip, so nothing could be reused
and its 300 words took 4.8 s. Vedu stays the default on the Mac: Phonon's gains are
real but modest, and it triples the wait. On Linux there is no MLX, so both run
through llama.cpp.

## Run it

```sh
TIRO_CODESIGN_IDENTITY="…" scripts/build-app.sh
CLIPS=$(mktemp -d); MODELS="$HOME/Library/Application Support/tiro/models"
benchmarks/cleanup/make_clips.py "$CLIPS"

# Scribe through the helper; add --api <data-dir> with `hex service` running for the full stage
benchmarks/cleanup/bench.py --helper target/app/Tiro.app/Contents/Helpers/tiro-scribe \
  --model "$MODELS/vedu-scribe-7dced584"

# Only the full stage, whole and by sentence, against the running app
benchmarks/cleanup/bench.py --variants "" --api "$HOME/Library/Application Support/tiro" \
  --inputs benchmarks/cleanup/results-2026-11-05/streamed-transcripts.json --repeats 1

# Through the real app, in its own data folder (--phonon for Phonon's refiner)
benchmarks/cleanup/e2e.py --app target/app/Tiro.app --clips "$CLIPS" --models "$MODELS" \
  --work "$(mktemp -d)" --vocabulary
```

`holdout.json` is for scoring only. Once you tune against it, it stops being held out;
write new rows instead.
