# Android over LAN

The phone records; the Mac does everything else. The phone runs no model.

```text
Tiro keyboard (android/) ─TLS─▶ :47811 on the Mac (src/lan.rs)
  hold the mic, speak        ├─ allowlist: GET /health, POST /transcriptions, POST /cleanup
  let go                     ├─ paired device token (only its SHA-256 is stored)
                             └─▶ 127.0.0.1 local API: Parakeet, then the cleanup stage
  ◀──────────── clean text, typed into the focused field ───────────────┘
```

## Setting it up

On the Mac:

```sh
hex labs on lan-dictation
hex lan pair --name Pixel        # prints a QR code and a tiro://pair link
```

On the phone: install `android/` (`./gradlew :app:installDebug`), scan the QR code
with the camera, confirm the pairing in Tiro, allow the microphone, and turn on the
"Tiro voice" keyboard. In any text field, switch to it and hold the mic.

`hex lan devices` lists paired phones; `hex lan revoke Pixel` unpairs one. Turning the
Labs switch off closes the port within a second.

For the Android emulator, pair with `--host 10.0.2.2`, the emulator's address for the
Mac.

## Design

- **The loopback API is never exposed.** `/control` can change settings, so the LAN
  listener is a separate TLS front that forwards three routes, one request per
  connection, after replacing the phone's token with the loopback one.
- **TLS with a pinned, self-signed certificate.** Made once by the system `openssl`
  in `<support>/lan/`. The QR code carries its SHA-256; the phone trusts exactly that
  certificate, so nothing else on the network can stand in for the Mac.
- **Pairing asks first.** Any Android app can open a `tiro://` link, so the phone
  confirms before it saves one. Otherwise a silent re-pair could send your speech to
  someone else's computer.
- **The Mac picks the model.** `/transcriptions` always uses the model dictation holds,
  so a phone never makes the Mac load a second one.
- **Whole clips, not streams.** The phone sends the recording when you let go, then the
  transcript to `/cleanup`. `/streams` could start transcribing while you speak, but it
  needs many small requests; measure first.

## What has been verified

- Over real TLS from the Mac: pinned fingerprint matches what the listener serves;
  unpaired tokens and non-allowlisted routes (`/control`) are refused; audio and
  cleanup round-trip; early errors from the API reach the client intact.
- The Android app builds (debug and a 64 KB release), passes its unit tests (pairing
  links, WAV headers, fingerprints, spacing), and passes lint with no errors.
- **Not yet run on a device or emulator.** The emulator was in use by another session.
  Debug builds can play `files/fixture.wav` instead of the microphone, so the full path
  can be tested there without recording anyone:
  `adb shell run-as dev.axl47.tiro cp /data/local/tmp/clip.wav files/fixture.wav`.
- Not measured: latency over real Wi-Fi, and the macOS firewall prompt, which appears
  when an app first listens on the network if the firewall is on.

## Next

- Stream audio while speaking (`/streams`) if stop-to-text over Wi-Fi feels slow.
- mDNS discovery, so a changed Mac address doesn't need a new pairing.
- Pairing from Tiro's window, not only the CLI.
