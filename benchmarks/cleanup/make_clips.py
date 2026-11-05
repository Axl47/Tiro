#!/usr/bin/env python3
"""Speaks each corpus row with macOS `say` and writes a transcription manifest.

Text-to-speech carries the words but none of a person's timing or hesitation
sound, so this exercises the speech model's path, not its handling of a real
voice. Real recordings can be listed in a manifest of the same shape.

    benchmarks/cleanup/make_clips.py <out-dir> [--voice Samantha]
"""

import argparse
import json
import subprocess
import wave
from pathlib import Path

SILENCE_SECONDS = 3  # the fixture loops; recordings stop inside this silence

parser = argparse.ArgumentParser()
parser.add_argument("out")
parser.add_argument("--voice", default="Samantha")
parser.add_argument("--rate", default="170")
parser.add_argument("--corpus", default=str(Path(__file__).parent / "corpus.json"))
args = parser.parse_args()

out = Path(args.out).resolve()
out.mkdir(parents=True, exist_ok=True)
rows = json.loads(Path(args.corpus).read_text())["rows"]
clips = []
for row in rows:
    audio = out / f"{row['id']}.wav"
    subprocess.run(
        ["say", "-v", args.voice, "-r", args.rate, "-o", str(audio),
         "--data-format=LEI16@16000", row["raw"]],
        check=True,
    )
    with wave.open(str(audio)) as clip:
        params, frames = clip.getparams(), clip.readframes(clip.getnframes())
    with wave.open(str(audio), "wb") as clip:
        clip.setparams(params)
        clip.writeframes(frames + b"\0" * params.sampwidth * params.framerate * SILENCE_SECONDS)
    clips.append({"id": row["id"], "audio": str(audio), "expected": row["raw"]})
(out / "manifest.json").write_text(json.dumps({"clips": clips}, indent=2))
print(out / "manifest.json")
