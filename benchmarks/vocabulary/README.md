# Vocabulary on real dictations

Replays Keep recordings through the path a dictation takes, with and without a
vocabulary, and reports which terms helped. The recordings, the term list, and every
result name private text, so **none of them belong in this public repository**: keep
them outside it. Only these scripts and the summary below are committed.

```sh
# Pull the recordings (the box allows rsync and sftp only).
rsync -a -e "ssh -o BatchMode=yes" tiro-box:recordings/ box/

# Replay; use a folder of your own with `models` linked in, never the real one.
HEX_APPLICATION_SUPPORT_DIR=root hex replay-dictations manifest.json \
  --model parakeet-unified-en-0.6b-Q8_0.gguf --boost off --speed 6 > baseline.jsonl
HEX_APPLICATION_SUPPORT_DIR=root hex replay-dictations manifest.json \
  --model parakeet-unified-en-0.6b-Q8_0.gguf --vocabulary list.json > seeded.jsonl

python3 join_sent.py box corpus.sqlite labels.json   # references from sent messages
python3 compare.py baseline.jsonl seeded.jsonl labels.json list.json
```

`manifest.json` is a JSON array of `{"id", "audio"}`. Parakeet reads the first 64 terms.

## Measured 2026-11-10

73 recordings, 64 minutes, all dictated into T3 Code, Parakeet Unified English, Normal
boost, 6x stream feeding. 47 recordings had a reference from the message actually sent.

- About half were sent word for word (23 of 47); 19 had a word changed.
- WER against the sent text went from 7.01% to 6.82% (7 fewer errors in 3,854 words),
  with 7 clips changed and no term written where it wasn't said. A vocabulary fixes a
  few rare words, and they barely move an average.
- Most gains came from words seen wrong in these same clips, with their misheard forms
  as "sounds like" spellings, so that result is optimistic.
- Names that were never said change nothing, so a clean run says nothing about them.
- A guard of 32 synthetic sentences full of sound-alikes changed in no place.
- Replays vary: 3 of 73 clips switched between stream and whole-clip decoding from
  timing alone, so read term counts, not small WER differences.
