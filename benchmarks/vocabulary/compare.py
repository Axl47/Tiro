#!/usr/bin/env python3
"""Which vocabulary terms a replay gained or lost against a baseline.

usage: compare.py BASELINE.jsonl SEEDED.jsonl LABELS.json LIST.json
Its output names private text: don't commit it.
"""
import json
import re
import sys


def load(path):
    return {row["id"]: row for row in map(json.loads, open(path))}


def main(baseline, seeded, labels_path, vocabulary):
    base, out = load(baseline), load(seeded)
    labels = json.load(open(labels_path))
    print("clips changed", sum(base[i]["text"] != out[i]["text"] for i in base))
    print("stream/whole-clip switches", sum(base[i]["source"] != out[i]["source"] for i in base))
    for item in json.load(open(vocabulary)):
        term = item if isinstance(item, str) else item["term"]
        any_case = re.compile(rf"\b{re.escape(term)}(?:'s|s)?\b", re.I)
        exact = re.compile(rf"\b{re.escape(term)}\b")
        said = kept_before = kept_after = 0
        for label in labels:
            count = len(any_case.findall(label["corrected"]))
            said += count
            if count:
                kept_before += min(count, len(exact.findall(base[label["id"]]["text"])))
                kept_after += min(count, len(exact.findall(out[label["id"]]["text"])))
        gained = sum(max(0, len(any_case.findall(out[i]["text"])) - len(any_case.findall(base[i]["text"]))) for i in base)
        lost = sum(max(0, len(any_case.findall(base[i]["text"])) - len(any_case.findall(out[i]["text"]))) for i in base)
        if said or gained or lost:
            print(f"{term:18} said {said:3}  before {kept_before:3}  after {kept_after:3}  gained {gained}  lost {lost}")


if __name__ == "__main__":
    main(*sys.argv[1:5])
