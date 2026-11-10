#!/usr/bin/env python3
"""Finds the message each saved dictation ended up in, as a reference.

usage: join_sent.py BOX CORPUS.sqlite OUT.json
Writes private text: keep OUT outside the repository.
"""
import datetime as dt
import difflib
import glob
import json
import re
import sqlite3
import sys

MIN_COVERAGE = 0.6
WINDOW_MS = (-120_000, 45 * 60_000)


def norm(word):
    return re.sub(r"[^\w']", "", word.lower())


def to_ms(stamp):
    return int(dt.datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp() * 1000)


def main(box, corpus, out):
    db = sqlite3.connect(f"file:{corpus}?mode=ro", uri=True)
    turns = [
        (to_ms(started), text)
        for started, text in db.execute(
            "select started_at, user_text from turns where user_origin = 'human' "
            "and client in ('T3 Code', 't3-code') and user_text is not null"
        )
    ]
    labels = []
    for path in sorted(glob.glob(f"{box}/**/*.json", recursive=True)):
        entry = json.load(open(path))
        final = entry["final_text"].split()
        key = [norm(word) for word in final]
        best = None
        for started, text in turns:
            if not WINDOW_MS[0] <= started - entry["timestamp_ms"] <= WINDOW_MS[1]:
                continue
            words = text.split()
            matcher = difflib.SequenceMatcher(None, key, [norm(w) for w in words], autojunk=False)
            blocks = [block for block in matcher.get_matching_blocks() if block.size]
            coverage = sum(block.size for block in blocks) / max(1, len(key))
            if blocks and (best is None or coverage > best[0]):
                start = max(0, blocks[0].b - blocks[0].a)
                end = min(len(words), blocks[-1].b + blocks[-1].size + len(key) - blocks[-1].a - blocks[-1].size)
                best = (coverage, " ".join(words[start:end]))
        if best and best[0] >= MIN_COVERAGE:
            stem = path.rsplit("/", 1)[1][:-5]
            labels.append({"id": stem, "corrected": best[1], "final": entry["final_text"]})
    json.dump(labels, open(out, "w"))
    print(f"{len(labels)} references")


if __name__ == "__main__":
    main(*sys.argv[1:4])
