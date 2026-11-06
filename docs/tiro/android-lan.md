# Android over LAN

The Android launcher uses the desktop app’s silver hexagon artwork, inset for adaptive icon masks.

The phone records; the Mac does everything else. The phone runs no model.

```text
volume key or Tiro keyboard ─TLS─▶ :47811 on the Mac (src/lan.rs)
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
with the camera, confirm the pairing in Tiro, and allow the microphone. Then either:

- **Volume key (recommended):** Tiro › Turn on volume-key dictation, then turn on
  *Tiro volume-key dictation* in Accessibility › Installed apps. Keep your own
  keyboard; with a text field and the keyboard open, hold volume-down to talk and let
  go to finish. A quick press still changes the volume.
- **Keyboard:** turn on the "Tiro voice" keyboard, switch to it in a text field, and
  hold the mic.

Android turns an accessibility service off when its app is force-stopped, so a force
stop (or `agent-device open --relaunch`) means turning it on again.

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

- **Volume key over a second keyboard.** Samsung Keyboard's mic key only offers Samsung
  or Google voice input and takes no plug-ins, so dictation that keeps your keyboard
  needs its own trigger. `VolumeKeyDictation` is an accessibility service that takes
  volume-down only while an input-method window and an editable, non-password field
  are on screen; elsewhere it never consumes the key. A press shorter than 300 ms is
  handed back as a volume change. Recording starts at the threshold, with a haptic
  tick, so a tap never turns the microphone on. The text is set into the field at its
  selection (`ACTION_SET_TEXT`), else pasted; if neither works it stays on the
  clipboard.
- **The pill is the HUD.** While a volume-key dictation runs, a pill under the status
  bar shows Listening (its dot follows the mic level), Transcribing, then the result.
  It is an accessibility overlay that is not touchable or focusable, and it is removed
  two seconds after the result.

## What has been verified

- Over real TLS from the Mac: pinned fingerprint matches what the listener serves;
  unpaired tokens and non-allowlisted routes (`/control`) are refused; audio and
  cleanup round-trip; early errors from the API reach the client intact.
- The Android app builds (debug and a 64 KB release), passes its unit tests (pairing
  links, WAV headers, fingerprints, spacing), and passes lint with no errors.
- **On the Android 16 emulator, end to end** (2026-11-05), with a debug build playing a
  15 s clip instead of the microphone (`files/fixture.wav`):
  - the `tiro://pair` link opens the confirmation, and pairing then connects over pinned
    TLS (279 ms for the first request, handshake included);
  - the microphone permission, turning the keyboard on in Settings, and choosing it;
  - tap to start and tap to finish typed the cleaned text into the field, 1.1-1.2 s
    after the second tap, three times; a second dictation is spaced after the first;
  - with Tiro quit on the Mac, the keyboard says it can't reach the Mac.
- Fixed on the emulator: Android 15+ drew the system's navigation buttons over the
  bottom keys; the keyboard now pads itself above the navigation bar.
- Not verified: Backspace and Enter (the field's text was hidden from the checks), a real
  microphone, and holding rather than tapping the mic.
- Not measured: latency over real Wi-Fi (the emulator reaches the Mac through loopback), and the macOS firewall prompt, which appears
  when an app first listens on the network if the firewall is on.

- **Volume key on a Samsung S24+ (Android 17, One UI 9), 2026-11-05**, with Samsung
  Keyboard, the volume-down press made by a `uinput` virtual key (it goes through the
  accessibility filter like the real button; `input keyevent` and `sendevent` don't):
  - the service binds and receives the key; a held press records from the real
    microphone (Android doesn't silence it: peak 5,300–7,700 of 32,768);
  - with a debug fixture clip in place of the microphone, the cleaned text was typed
    into Tiro's own field (2.6 s, with the Mac's cleanup helper starting cold) and
    into a web page's search box in Firefox (0.7 s warm);
  - a 120 ms tap lowered the volume (the volume panel showed) and started nothing;
  - the pill appeared under the status bar, clear of the keyboard.
- **Not verified on the volume key:** a person speaking into the phone. The Mac's
  `say` from across the desk recorded as audio the Mac heard no words in, while the
  same sentence as a clean file transcribed exactly. Also untested: the physical
  button itself, apps beyond those two, and fields that refuse `ACTION_SET_TEXT`.
- Accessibility services are unbound while a UiAutomation client (agent-device's
  snapshot helper, `uiautomator`) is connected, so drive the volume-key test with
  `adb` and close agent-device first.

## Next

- One `/dictate` route that transcribes and cleans up in one request, so a dictation
  costs one TLS handshake instead of two.
- Stream audio while speaking (`/streams`) if stop-to-text over Wi-Fi feels slow.
- mDNS discovery, so a changed Mac address doesn't need a new pairing.
- Pairing from Tiro's window, not only the CLI.
