# Changelog

## [Unreleased]

### Added

- Android: hold volume-down to dictate into any keyboard's text field, Samsung
  Keyboard included, with a small status pill while it works.
- Android: double-tap volume-down to dictate hands-free; press it once more to finish.
- LAN: `POST /dictate` transcribes and cleans up in one request. Android uses it,
  connects while you speak, and falls back to two requests on an older Mac.
- Android streams a dictation to the Mac while you speak (`/dictate/stream`) and shows
  the live text in the volume-key pill; the whole clip goes to `/dictate` when
  streaming can't run.
- Pair and unpair phones from Labs › LAN dictation's drawer, which shows the QR code.
- The Mac advertises LAN dictation over Bonjour, and Android finds it again by its
  pinned certificate when its address changes, so a new address needs no new pairing.
- Labs › Keep recordings also sends each recording, with its History entry as JSON,
  to the Storage Box (`tiro-box:recordings/YYYY/MM/`), queueing it while the box is
  unreachable.

- Labs › Save corrections, on by default: after a dictation pastes on macOS, Tiro
  follows only the text it pasted and saves what you changed as `corrected_text`,
  beside `raw_text` and `final_text`. Password fields are never read.
- A settled correction also reaches the Storage Box, in that recording's JSON.
- History has a Correct button for typing a dictation's right text by hand, on every
  platform; what you type outranks anything read back from a field.
- Smart spacing now works in T3 Code and other Electron apps, because Tiro asks them
  for their accessibility tree while following a paste.

### Changed

- Keep recordings holds at most 64 MiB of audio on the Mac (was 512 MiB), as a
  cache for Transcribe again; the Storage Box keeps every recording.

- A streamed Android dictation cleans up each sentence while you're still speaking,
  so the text lands about twice as fast after you let go (~280 ms instead of ~560 ms
  on an 11 s dictation).
- Android now uses the desktop app’s silver hexagon icon.
