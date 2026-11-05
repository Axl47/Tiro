#!/usr/bin/env python3
"""Benchmarks Vedu Scribe through the real tiro-scribe helper.

Runs every corpus row through each input variant and reports raw vs cleaned
quality (word error rate against the hand-written reference, kept terms, dropped
disfluencies) plus warm latency and memory. Standard library only.

    benchmarks/cleanup/bench.py --helper <tiro-scribe> --model <dir> [--out dir]
    benchmarks/cleanup/bench.py ... --inputs transcripts.json   # e.g. real ASR output
    benchmarks/cleanup/bench.py ... --api <data-dir>            # add Tiro's full stage

`--api` reads a running Tiro's local-api.json and adds a `tiro` row: POST /cleanup,
the path dictation takes, with its own vocabulary selection, guard, and residue pass.
"""

import argparse
import json
import re
import statistics
import subprocess
import sys
import time
import urllib.request
from pathlib import Path

ROOT = Path(__file__).parent
FILLERS = {"um", "uh", "umm", "uhh", "er", "erm"}
SOFT_FILLERS = ["like", "you know", "basically", "yeah", "i mean", "kind of", "sort of"]
# Lowercase is what the model trained on; the others test what ASR casing and
# the dictionary do to a coding prompt.
VARIANTS = {
    "as-is": dict(lower=False, vocab=False),
    "lower": dict(lower=True, vocab=False),
    "as-is+vocab": dict(lower=False, vocab=True),
    "lower+vocab": dict(lower=True, vocab=True),
}


class Helper:
    def __init__(self, binary, model):
        started = time.perf_counter()
        self.process = subprocess.Popen(
            [binary, "--model", model],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.DEVNULL,
            text=True,
            bufsize=1,
        )
        self.ready = json.loads(self.process.stdout.readline())
        self.ready["spawnToReadyMs"] = round((time.perf_counter() - started) * 1000)
        self.next_id = 0

    def clean(self, text, vocab=()):
        self.next_id += 1
        started = time.perf_counter()
        request = {"id": self.next_id, "text": text, "vocab": list(vocab)}
        self.process.stdin.write(json.dumps(request) + "\n")
        self.process.stdin.flush()
        reply = json.loads(self.process.stdout.readline())
        reply["roundTripMs"] = (time.perf_counter() - started) * 1000
        if reply.get("error"):
            raise RuntimeError(reply["error"])
        return reply

    def close(self):
        self.process.stdin.close()
        self.process.wait(timeout=10)


class Pipeline:
    """POST /cleanup on a running Tiro."""

    def __init__(self, data_dir):
        api = json.loads((Path(data_dir) / "local-api.json").read_text())
        self.url = f"http://127.0.0.1:{api['port']}/cleanup"
        self.token = api["token"]

    def clean(self, text):
        request = urllib.request.Request(
            self.url,
            data=json.dumps({"text": text}).encode(),
            headers={"authorization": f"Bearer {self.token}", "content-type": "application/json"},
        )
        started = time.perf_counter()
        with urllib.request.urlopen(request, timeout=30) as response:
            reply = json.load(response)
        reply["roundTripMs"] = (time.perf_counter() - started) * 1000
        return reply


def words(text):
    return re.findall(r"[a-z0-9']+", text.lower().replace("-", " ").replace("/", " "))


def wer(hypothesis, reference):
    """Word error rate of hypothesis against reference, in percent."""
    hyp, ref = words(hypothesis), words(reference)
    row = list(range(len(hyp) + 1))
    for i, r in enumerate(ref, 1):
        previous, row[0] = row[0], i
        for j, h in enumerate(hyp, 1):
            previous, row[j] = row[j], min(
                row[j] + 1, row[j - 1] + 1, previous + (r != h)
            )
    return 100 * row[-1] / max(len(ref), 1)


def contains(text, term):
    # A term with internal capitals must survive exactly; others ignore case.
    if any(c.isupper() for c in term[1:]):
        return term in text
    return term.lower() in text.lower()


def filler_count(text):
    tokens = words(text)
    hard = sum(token in FILLERS for token in tokens)
    joined = " ".join(tokens)
    soft = sum(len(re.findall(rf"\b{re.escape(f)}\b", joined)) for f in SOFT_FILLERS)
    repeats = sum(a == b for a, b in zip(tokens, tokens[1:]))
    return hard + soft + repeats


def score(row, output):
    keep = row["must_keep"]
    drop = row["must_drop"]
    return dict(
        wer=wer(output, row["reference"]),
        fillers=filler_count(output),
        words=len(words(output)),
        kept=sum(contains(output, term) for term in keep),
        kept_of=len(keep),
        lost=[term for term in keep if not contains(output, term)],
        dropped=sum(not contains(output, term.strip()) for term in drop),
        dropped_of=len(drop),
        vocab_hit=sum(term in output for term in row["vocab"]),
        vocab_of=len(row["vocab"]),
    )


def percentile(values, fraction):
    ordered = sorted(values)
    return ordered[min(int(len(ordered) * fraction), len(ordered) - 1)]


def summarize(scores):
    total = lambda key: sum(s[key] for s in scores)
    return dict(
        wer=statistics.mean(s["wer"] for s in scores),
        fillers=total("fillers"),
        words=total("words"),
        kept=f"{total('kept')}/{total('kept_of')}",
        dropped=f"{total('dropped')}/{total('dropped_of')}",
        vocab=f"{total('vocab_hit')}/{total('vocab_of')}",
    )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--helper", required=True)
    parser.add_argument("--model", required=True)
    parser.add_argument("--corpus", default=str(ROOT / "corpus.json"))
    parser.add_argument("--inputs", help="JSON map of row id to transcript, replacing `raw`")
    parser.add_argument("--variants", default=",".join(VARIANTS))
    parser.add_argument("--repeats", type=int, default=5)
    parser.add_argument("--api", help="data folder of a running Tiro, to add its full stage")
    parser.add_argument("--out", default=str(ROOT / "results"))
    args = parser.parse_args()

    rows = json.loads(Path(args.corpus).read_text())["rows"]
    if args.inputs:
        inputs = json.loads(Path(args.inputs).read_text())
        rows = [dict(row, raw=inputs[row["id"]]) for row in rows if row["id"] in inputs]

    helper = Helper(args.helper, args.model)
    print(f"ready: {helper.ready}", file=sys.stderr)
    # The first inference compiles Metal pipelines; report it, then keep it out
    # of the warm numbers.
    first = helper.clean("um run the tests")
    cold = dict(firstInferenceMs=first["roundTripMs"], footprintBytes=first["footprintBytes"])

    results = {"ready": helper.ready, "cold": cold, "variants": {}}
    results["raw"] = {r["id"]: score(r, r["raw"]) for r in rows}
    print_table = [("raw (no cleanup)", summarize(results["raw"].values()), None)]
    peak = first["footprintBytes"]

    for name in args.variants.split(","):
        config = VARIANTS[name]
        outputs, latencies, scores = {}, [], {}
        for row in rows:
            text = row["raw"].lower() if config["lower"] else row["raw"]
            vocab = row["vocab"] if config["vocab"] else []
            samples = []
            for _ in range(args.repeats):
                reply = helper.clean(text, vocab)
                samples.append(reply["roundTripMs"])
                peak = max(peak, reply["footprintBytes"])
            latencies.append(
                dict(id=row["id"], words=len(words(text)), median=statistics.median(samples),
                     max=max(samples), firstTokenMs=reply["firstTokenMs"],
                     inputTokens=reply["inputTokens"], outputTokens=reply["outputTokens"])
            )
            outputs[row["id"]] = reply["text"]
            scores[row["id"]] = score(row, reply["text"])
        medians = [l["median"] for l in latencies]
        summary = summarize(scores.values())
        summary.update(p50=statistics.median(medians), p95=percentile(medians, 0.95))
        results["variants"][name] = dict(
            summary=summary, outputs=outputs, scores=scores, latency=latencies
        )
        print_table.append((name, summary, latencies))

    results["peakFootprintBytes"] = peak
    helper.close()

    if args.api:
        pipeline = Pipeline(args.api)
        pipeline.clean("um run the tests")
        outputs, latencies, scores, fallbacks = {}, [], {}, {}
        for row in rows:
            samples = [pipeline.clean(row["raw"]) for _ in range(args.repeats)]
            reply = samples[-1]
            outputs[row["id"]] = reply["text"]
            scores[row["id"]] = score(row, reply["text"])
            fallbacks[row["id"]] = reply.get("fallback")
            latencies.append(dict(id=row["id"], words=len(words(row["raw"])),
                                  median=statistics.median(s["roundTripMs"] for s in samples),
                                  max=max(s["roundTripMs"] for s in samples)))
        medians = [l["median"] for l in latencies]
        summary = summarize(scores.values())
        summary.update(p50=statistics.median(medians), p95=percentile(medians, 0.95))
        results["variants"]["tiro"] = dict(summary=summary, outputs=outputs, scores=scores,
                                           latency=latencies, fallbacks=fallbacks)
        print_table.append(("tiro", summary, latencies))

    out = Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    (out / "latest.json").write_text(json.dumps(results, indent=2))

    print(f"\nload {helper.ready['loadMs']} ms · first inference {cold['firstInferenceMs']:.0f} ms"
          f" · peak footprint {peak / 2**20:.0f} MB")
    print(f"{'variant':<18}{'WER':>7}{'fillers':>9}{'words':>7}{'kept':>8}{'dropped':>9}{'vocab':>7}{'p50 ms':>8}{'p95 ms':>8}")
    for name, s, _ in print_table:
        print(f"{name:<18}{s['wer']:>7.1f}{s['fillers']:>9}{s['words']:>7}{s['kept']:>8}{s['dropped']:>9}"
              f"{s['vocab']:>7}{s.get('p50', 0):>8.0f}{s.get('p95', 0):>8.0f}")


if __name__ == "__main__":
    main()
