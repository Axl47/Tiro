#!/usr/bin/env python3
"""Runs corpus clips through the real Tiro.app, from microphone stream to text.

Each clip is a fresh app whose microphone is replaced by the clip
(HEX_AUDIO_FIXTURE), in its own data folder, so no one is recorded and nothing
touches your settings. Each clip is dictated twice: without processing (`--raw`),
then normally, so the latency difference is cleanup's real cost. `dictation stop
--print` returns the text instead of pasting it, so paste is NOT exercised here.

    benchmarks/cleanup/e2e.py --app target/app/Tiro.app --clips <dir> --models <dir> --work <dir>
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import time
import wave
from pathlib import Path


# What someone would add with `hex vocabulary add` after a few dictations.
VOCABULARY = [
    "OpenSuperWhisper",
    {"term": "Hex", "sounds_like": ["hacks"]},
    "Gaia",
    {"term": "Qwen", "sounds_like": ["quen"]},
    {"term": "ArkType", "sounds_like": ["archetype"]},
]


def run(hex_binary, env, *args):
    return subprocess.run([hex_binary, *args], env=env, capture_output=True, text=True)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--app", required=True)
    parser.add_argument("--clips", required=True)
    parser.add_argument("--models", required=True)
    parser.add_argument("--work", required=True)
    parser.add_argument("--only", help="comma-separated clip ids")
    parser.add_argument("--vocabulary", action="store_true", help="dictate with VOCABULARY set")
    args = parser.parse_args()

    hex_binary = str(Path(args.app).resolve() / "Contents/MacOS/hex")
    manifest = json.loads((Path(args.clips) / "manifest.json").read_text())["clips"]
    wanted = set(args.only.split(",")) if args.only else None
    results = []
    for clip in manifest:
        if wanted and clip["id"] not in wanted:
            continue
        data = Path(args.work) / "data" / clip["id"]
        shutil.rmtree(data, ignore_errors=True)
        data.mkdir(parents=True)
        (data / "models").symlink_to(Path(args.models).resolve())
        # The release-microphone mode opens the fixture at its start on each
        # press, so the recording lines up with the clip instead of its loop.
        (data / "settings.json").write_text(json.dumps(
            {"release_microphone_while_idle": True, "vocabulary": VOCABULARY if args.vocabulary else []}))
        env = dict(os.environ, HEX_APPLICATION_SUPPORT_DIR=str(data), HEX_AUDIO_FIXTURE=clip["audio"])
        app = subprocess.Popen([hex_binary, "app"], env=env,
                               stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        try:
            for _ in range(240):
                if run(hex_binary, env, "status").returncode == 0:
                    break
                time.sleep(0.25)
            # The speech model warms in the background; wait for both models.
            log = data / "logs/process.log"
            for _ in range(240):
                text = log.read_text() if log.exists() else ""
                if "loaded transcription model" in text and subprocess.run(
                    ["pgrep", "-f", "Helpers/tiro-scribe"], capture_output=True).returncode == 0:
                    break
                time.sleep(0.25)
            time.sleep(4)  # the helper warms itself after it spawns
            with wave.open(clip["audio"]) as audio:
                # Stop a second into the clip's trailing silence (make_clips.py).
                seconds = audio.getnframes() / audio.getframerate() - 2

            def dictate(*options):
                run(hex_binary, env, "dictation", "start", *options)
                time.sleep(seconds)
                released = time.perf_counter()
                stop = run(hex_binary, env, "dictation", "stop", "--print")
                return stop.stdout.strip(), round((time.perf_counter() - released) * 1000)

            raw, raw_ms = dictate("--raw")
            text, cleaned_ms = dictate()
            stripped = re.sub(r"\x1b\[[0-9;]*m", "", log.read_text())
            cleanup = re.findall(r"dictation cleanup finished.*?latency_ms=(\d+) fallback=(\S+)", stripped)
            result = dict(id=clip["id"], speechSeconds=round(seconds - 1, 1),
                          rawStopToTextMs=raw_ms, cleanedStopToTextMs=cleaned_ms,
                          cleanupMs=int(cleanup[-1][0]) if cleanup else None,
                          fallback=cleanup[-1][1] if cleanup else None, raw=raw, text=text)
            results.append(result)
            print(json.dumps(result), flush=True)
        finally:
            run(hex_binary, env, "quit")
            try:
                app.wait(timeout=10)
            except subprocess.TimeoutExpired:
                app.kill()
    (Path(args.work) / "e2e.json").write_text(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
