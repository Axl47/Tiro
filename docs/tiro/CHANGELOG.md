# Changelog

## [Unreleased]

### Added

- Android: hold volume-down to dictate into any keyboard's text field, Samsung
  Keyboard included, with a small status pill while it works.
- LAN: `POST /dictate` transcribes and cleans up in one request. Android uses it,
  connects while you speak, and falls back to two requests on an older Mac.
- Android streams a dictation to the Mac while you speak (`/dictate/stream`) and shows
  the live text in the volume-key pill; the whole clip goes to `/dictate` when
  streaming can't run.

### Changed

- Android now uses the desktop app’s silver hexagon icon.
