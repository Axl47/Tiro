#!/usr/bin/env python3
"""Compares cleanup models on the same transcripts: Tiro's full stage against GGUF
models served by llama.cpp's `llama-server`, which also runs on Linux.

    benchmarks/cleanup/compare.py --set history --out <dir> \
        --tiro "$HOME/Library/Application Support/tiro" \
        --phonon http://127.0.0.1:18731 --scribe http://127.0.0.1:18732

Engines (each optional):
- `tiro`: POST /cleanup on a running Tiro, the path a dictation takes.
- `phonon`: Phonon's refiner (LFM3-1.7B fine-tune) with Phonon's own prompt, output
  budget, length guard and term restoration, ported from its `sidecar/polish_server.py`
  and `sidecar/refiner_prompt.py`. It gets the vocabulary Tiro can draw on (the person's
  terms, then Tiro's built-in developer terms) under Phonon's list cap, and the writing
  mode Phonon would pick for the app.
- `scribe`: Vedu Scribe as GGUF, the bare model with Tiro's prompt contract (lowercased
  transcript, no vocabulary, no guards). With `--helper`, the same input also goes through
  the MLX `tiro-scribe` helper, to check that the GGUF writes the same text.

Sets:
- `corpus`, `holdout`: the synthetic rows with hand-written references, scored like
  `bench.py`, on the recognizer's streamed transcripts.
- `history`: Tiro's own History. Rows whose paste the person then edited
  (`correction.status == "edited"`) are scored against what they left in the field. That
  reference started as Tiro's output, so it leans toward Tiro.

Outputs hold personal dictation text when `--set history`; keep `--out` outside the repo.
"""

import argparse
import json
import re
import statistics
import time
import urllib.request
from difflib import SequenceMatcher
from pathlib import Path

from bench import Helper, Pipeline, filler_count, score, summarize, wer, words

ROOT = Path(__file__).parent
REPO = ROOT.parent.parent
STREAMED = ROOT / "results-2026-11-05" / "streamed-transcripts.json"

# --- Phonon's refiner contract (polish_server.py, refiner_prompt.py) ---------------------

PHONON_INSTRUCTION = (
    "Rewrite the raw dictation transcript into the exact text the speaker intended. "
    "Fix recognition errors and technical terms, keep the speaker's wording and casing style, "
    "do not add or drop content."
)
PHONON_MODES = {
    "minimal": "Writing mode: minimal (notes and documents).",
    "casual": "Writing mode: casual (a chat message).",
    "formal": "Writing mode: formal (an email or document).",
    "prompt": "Writing mode: prompt (an instruction to an AI assistant).",
}
# Phonon maps code editors, terminals and AI apps to `prompt`, anything it can't place to
# `minimal`. Phone rows come from any Android app, so they get `minimal`: `casual` writes
# chat-style lowercase, which lost most of them on casing alone.
MODE_BY_APP = {"T3 Code (Nightly)": "prompt", "Annex (Nightly)": "prompt"}
LEAKED_MARKER = re.compile(r"<\|?/?(?:channel|turn|think)\|?>?")
TERM_SHAPE = re.compile(r"[A-Z].*[A-Z0-9]|.*\d.*|.+-.+")
WORD_PARTS = re.compile(r"^(\W*)(.*?)(\W*)$", re.DOTALL)


def phonon_vocab_cap(transcript):
    return min(80, max(30, 3 * len(transcript.split())))


def phonon_guard(spoken, output):
    """The spoken text instead of a runaway (over 2x) or collapsed (under 0.4x) output."""
    s, o = len(spoken.split()), len(output.split())
    if s and (o > 2.0 * s or (s > 8 and o < 0.4 * s)):
        return spoken, True
    return output, False


def _norm(word):
    return "".join(c for c in word.lower() if c.isalnum())


def phonon_restore_terms(spoken, output, vocab):
    """Phonon's one-for-one swap back to a one-word term the speaker said."""
    terms = {_norm(t): t.strip() for t in vocab if t.strip() and " " not in t.strip()}
    if not terms:
        return output
    said = [_norm(w) for w in spoken.split()]
    said_set = set(said)
    spans = [m.span() for m in re.finditer(r"\S+", output)]
    out_words = [output[a:b] for a, b in spans]
    normed = [_norm(w) for w in out_words]
    fixes = {}
    for op, i1, i2, j1, j2 in SequenceMatcher(a=said, b=normed, autojunk=False).get_opcodes():
        if op != "replace" or i2 - i1 != j2 - j1:
            continue
        for i, j in zip(range(i1, i2), range(j1, j2)):
            term = terms.get(said[i])
            lead, core, trail = WORD_PARTS.match(out_words[j]).groups()
            if term and normed[j] not in said_set and normed[j] != _norm(term) and TERM_SHAPE.fullmatch(core):
                fixes[j] = lead + term + trail
    pieces, last = [], 0
    for j, (a, b) in enumerate(spans):
        if j in fixes:
            pieces += [output[last:a], fixes[j]]
            last = b
    return "".join(pieces) + output[last:]


# --- llama-server -------------------------------------------------------------------------


class LlamaServer:
    """Renders the chat template with thinking off, then decodes greedily from the raw
    prompt, so the text is exactly what the model wrote."""

    def __init__(self, url):
        self.url = url.rstrip("/")

    def post(self, path, body):
        request = urllib.request.Request(
            self.url + path, data=json.dumps(body).encode(), headers={"content-type": "application/json"}
        )
        with urllib.request.urlopen(request, timeout=120) as response:
            return json.load(response)

    def tokens(self, text):
        return len(self.post("/tokenize", {"content": text})["tokens"])

    def chat(self, system, user, max_tokens):
        prompt = self.post(
            "/apply-template",
            {"messages": [{"role": "system", "content": system}, {"role": "user", "content": user}],
             "chat_template_kwargs": {"enable_thinking": False}},
        )["prompt"]
        started = time.perf_counter()
        reply = self.post(
            "/completion",
            {"prompt": prompt, "n_predict": max_tokens, "temperature": 0, "top_k": 1,
             "cache_prompt": True, "special": False},
        )
        return reply["content"], (time.perf_counter() - started) * 1000, reply.get("timings", {})


class Phonon:
    def __init__(self, url, vocabulary):
        self.server = LlamaServer(url)
        self.vocabulary = vocabulary

    def clean(self, transcript, app=None):
        spoken = transcript.strip()
        vocab = self.vocabulary[: phonon_vocab_cap(spoken)]
        system = PHONON_INSTRUCTION + "\n" + PHONON_MODES[MODE_BY_APP.get(app, "prompt" if app is None else "minimal")]
        if vocab:
            system += "\nVocabulary the speaker may use: " + ", ".join(vocab)
        budget = max(8, min(2048, int(1.5 * self.server.tokens(spoken)) + 32))
        text, ms, timings = self.server.chat(system, spoken, budget)
        output, guarded = phonon_guard(spoken, LEAKED_MARKER.sub("", text).strip())
        output = phonon_restore_terms(spoken, output, vocab)
        return dict(text=output, ms=ms, timings=timings, fallback="guard" if guarded else None)


class ScribeGguf:
    """The bare model, as `tiro-scribe` prompts it (`Contract` in Scribe.swift); one piece,
    since every history row fits the helper's 400-token chunk budget or is reported."""

    def __init__(self, url):
        self.server = LlamaServer(url)

    def clean(self, transcript, app=None):
        text = transcript.lower()
        cap = int(-(-self.server.tokens(text) * 1.4 // 1)) + 48
        out, ms, timings = self.server.chat("Clean up this dictation.", text, cap)
        out = out.replace("<think>", "").replace("</think>", "").strip()
        return dict(text=out, ms=ms, timings=timings, fallback=None)


class Tiro:
    def __init__(self, data_dir):
        self.pipeline = Pipeline(data_dir)

    def clean(self, transcript, app=None):
        reply = self.pipeline.clean(transcript)
        return dict(text=reply["text"], ms=reply["roundTripMs"], timings={}, fallback=reply.get("fallback"))


def tiro_vocabulary(data_dir):
    """The person's terms first, then Tiro's built-in developer terms, as Tiro ranks them."""
    entries = json.loads((Path(data_dir) / "settings.json").read_text()).get("vocabulary", [])
    own = [e if isinstance(e, str) else e["term"] for e in entries]
    source = (REPO / "src/dictation/cleanup/vocab.rs").read_text()
    block = re.search(r"DEVELOPER_TERMS: &\[&str\] = &\[(.*?)\];", source, re.DOTALL).group(1)
    built_in = re.findall(r'"([^"]+)"', block)
    return list(dict.fromkeys(own + built_in))


# --- sets ---------------------------------------------------------------------------------


def load_rows(name, data_dir):
    if name in ("corpus", "holdout"):
        rows = json.loads((ROOT / f"{name}.json").read_text())["rows"]
        streamed = json.loads(STREAMED.read_text())
        return [dict(r, raw=streamed.get(r["id"], r["raw"]), app=None) for r in rows]
    entries = json.loads((Path(data_dir) / "history.json").read_text())["entries"]
    rows = []
    for e in entries:
        status = (e.get("correction") or {}).get("status")
        rows.append(dict(
            id=e["id"], raw=e["raw_text"], app=e["application"], then=e["final_text"],
            reference=e.get("corrected_text") if status == "edited" else None,
        ))
    return rows


def history_summary(rows, outputs):
    scored = [r for r in rows if r["reference"]]
    out = dict(
        rows=len(rows),
        fillers=sum(filler_count(outputs[r["id"]]["text"]) for r in rows),
        words=sum(len(words(outputs[r["id"]]["text"])) for r in rows),
        changed_from_raw=statistics.mean(wer(outputs[r["id"]]["text"], r["raw"]) for r in rows),
        fallbacks=sum(bool(outputs[r["id"]]["fallback"]) for r in rows),
        p50=statistics.median(outputs[r["id"]]["ms"] for r in rows),
        p95=sorted(outputs[r["id"]]["ms"] for r in rows)[int(len(rows) * 0.95)],
    )
    if scored:
        out["edited_rows"] = len(scored)
        out["edited_wer"] = statistics.mean(wer(outputs[r["id"]]["text"], r["reference"]) for r in scored)
    return out


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--set", choices=["corpus", "holdout", "history"], required=True)
    parser.add_argument("--tiro", help="data folder of a running Tiro (also the vocabulary and History)")
    parser.add_argument("--phonon", help="llama-server URL serving the Phonon refiner")
    parser.add_argument("--scribe", help="llama-server URL serving Vedu Scribe GGUF")
    parser.add_argument("--helper", help="tiro-scribe binary, to check the GGUF against MLX")
    parser.add_argument("--model", help="Vedu Scribe MLX folder, with --helper")
    parser.add_argument("--limit", type=int)
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    data_dir = args.tiro or str(Path.home() / "Library/Application Support/tiro")
    rows = load_rows(args.set, data_dir)[: args.limit]
    engines = {}
    if args.tiro:
        engines["tiro"] = Tiro(args.tiro)
    if args.phonon:
        engines["phonon"] = Phonon(args.phonon, tiro_vocabulary(data_dir))
    if args.scribe:
        engines["scribe-gguf"] = ScribeGguf(args.scribe)

    results = {"set": args.set, "engines": {}}
    for name, engine in engines.items():
        engine.clean("um run the tests")  # warm-up, kept out of the numbers
        outputs = {}
        for i, row in enumerate(rows, 1):
            outputs[row["id"]] = engine.clean(row["raw"], row["app"])
            print(f"\r{name}: {i}/{len(rows)}", end="", flush=True)
        print()
        results["engines"][name] = outputs

    if args.helper and args.scribe:
        helper = Helper(args.helper, args.model)
        helper.clean("um run the tests")
        gguf = results["engines"]["scribe-gguf"]
        same = 0
        mismatches = []
        for row in rows:
            mlx = helper.clean(row["raw"].lower())["text"]
            if mlx == gguf[row["id"]]["text"]:
                same += 1
            else:
                mismatches.append(dict(id=row["id"], mlx=mlx, gguf=gguf[row["id"]]["text"],
                                       wer=wer(gguf[row["id"]]["text"], mlx)))
        helper.close()
        results["mlx_vs_gguf"] = dict(identical=same, of=len(rows), mismatches=mismatches)
        print(f"Vedu Scribe GGUF vs MLX: {same}/{len(rows)} identical")

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / f"{args.set}.json").write_text(json.dumps(dict(results, rows=rows), indent=2))

    print(f"{'engine':<14}{'WER':>7}{'fillers':>9}{'words':>7}  extra")
    if args.set != "history":
        raw = summarize([score(r, r["raw"]) for r in rows])
        print(f"{'raw':<14}{raw['wer']:>7.1f}{raw['fillers']:>9}{raw['words']:>7}  kept {raw['kept']} dropped {raw['dropped']} vocab {raw['vocab']}")
        for name, outputs in results["engines"].items():
            s = summarize([score(r, outputs[r["id"]]["text"]) for r in rows])
            ms = [o["ms"] for o in outputs.values()]
            print(f"{name:<14}{s['wer']:>7.1f}{s['fillers']:>9}{s['words']:>7}  kept {s['kept']} dropped {s['dropped']}"
                  f" vocab {s['vocab']} p50 {statistics.median(ms):.0f} ms fallbacks {sum(bool(o['fallback']) for o in outputs.values())}")
        return
    raw = {r["id"]: dict(text=r["raw"], ms=0, fallback=None) for r in rows}
    then = {r["id"]: dict(text=r["then"], ms=0, fallback=None) for r in rows}
    for name, outputs in [("raw", raw), ("tiro-then", then)] + list(results["engines"].items()):
        s = history_summary(rows, outputs)
        print(f"{name:<14}{s.get('edited_wer', 0):>7.1f}{s['fillers']:>9}{s['words']:>7}  changed {s['changed_from_raw']:.1f}%"
              f" p50 {s['p50']:.0f} ms p95 {s['p95']:.0f} ms fallbacks {s['fallbacks']} (edited rows {s.get('edited_rows')})")


if __name__ == "__main__":
    main()
