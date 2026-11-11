# Android over LAN

The Android launcher uses the desktop app’s silver hexagon artwork, inset for adaptive icon masks.

The phone records; the Mac does everything else. The phone runs no model.

```text
volume key ─────────────────TLS─▶ :47811 on the Mac (src/lan.rs)
  hold, speak                ├─ allowlist: GET /health, POST /dictate/stream, POST /dictate,
  │             POST /transcriptions, POST /cleanup
  let go                     ├─ paired device token (only its SHA-256 is stored)
                             └─▶ 127.0.0.1 local API: Parakeet, then the cleanup stage
  ◀──────────── clean text, typed into the focused field ───────────────┘
```

## Setting it up

On the Mac, turn on Labs › LAN dictation, open its drawer, and press **Pair a
phone**: it shows the QR code. Or from a terminal:

```sh
hex labs on lan-dictation
hex lan pair --name Pixel        # prints a QR code and a tiro://pair link
```

On the phone: install `android/` (`./gradlew :app:installDebug`), scan the QR code
with the camera, confirm the pairing in Tiro, and allow the microphone. Then Tiro › Turn on
volume-key dictation, and turn on *Tiro volume-key dictation* in Accessibility ›
Installed apps. Keep your own keyboard; with a text field and the keyboard open, hold
volume-down to talk and let go to finish, or double-tap it to talk hands-free and
press it again to finish. A single quick press still changes the volume.

Android turns an accessibility service off when its app is force-stopped, so a force
stop (or `agent-device open --relaunch`) means turning it on again.

The drawer lists paired phones, each with Unpair; so do `hex lan devices` and
`hex lan revoke Pixel`. Turning the
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
- **Streamed while you speak (`POST /dictate/stream`).** One TLS connection per
  dictation, opened when recording starts. The phone sends framed 16 kHz s16le audio as
  it records (`'A'`, then `'F'` to finish or `'C'` to cancel; a type byte and a
  big-endian length), and the LAN front feeds the loopback `/streams` API with it, so
  Parakeet transcribes during the dictation and only the tail and cleanup are left at
  release (`src/lan/stream.rs`). It sends back newline-delimited JSON: live text from
  the stream's events, which the pill shows and nothing types, then the cleaned final.
  The phone keeps the whole recording, and when the stream can't deliver (an older Mac,
  `409 stream-busy`, `503 engine-unavailable`, a dropped connection) it sends the clip
  to `/dictate` instead (`LiveDictation`).
- **Whole clips in one request, as the fallback.** The phone sends the recording to
  `POST /dictate` when you let go; the LAN front forwards it to the loopback
  `/transcriptions`, sends the transcript through the loopback `/cleanup`, and answers
  `{"text", "transcript"}` (`src/lan/dictate.rs`). A failed cleanup answers the raw
  transcript. `/streams` could start transcribing while you speak, but it needs many
  small requests; measure first.
- **Connect while you speak.** When a dictation starts, the phone sends an empty
  `POST /dictate` on a background thread: it does the TLS handshake while you talk (one
  TLS context per pinned Mac, so the dictation resumes that session) and learns, every
  time, whether the Mac has `/dictate`. An older Mac answers `404 not-found`, and the phone then uses
  `/transcriptions` followed by `/cleanup`. The probe is empty because an older Mac
  refuses unknown routes before reading the body, and a refused upload reaches the
  phone as a reset, not as the 404.

- **Found again by Bonjour.** While it listens, the Mac advertises `_tiro._tcp` on the
  LAN port under the computer's name, with the certificate's fingerprint in the TXT
  record (`src/lan/bonjour.rs`, through mDNSResponder). When the saved address doesn't
  answer, the phone browses for that fingerprint, saves the address it finds, and
  tries once more (`MacFinder`). The advert only says where to look; the pin still
  decides whether to trust what answers. The first dictation after a move waits for
  the dead address's connect timeout and a search of up to 4 s.
- **Volume key over a second keyboard.** Samsung Keyboard's mic key only offers Samsung
  or Google voice input and takes no plug-ins, so dictation that keeps your keyboard
  needs its own trigger. `VolumeKeyDictation` is an accessibility service that takes
  volume-down only while an input-method window and an editable, non-password field
  are on screen; elsewhere it never consumes the key. A press shorter than 300 ms
  waits 300 ms for a second press: one makes a double tap, which records hands-free
  until the next press of volume-down (the keyboard need not still show), and
  without one the first press is handed back as a volume change, so a lone volume
  tap now lands 300 ms late. Each press within 1 s of a volume change goes straight
  to the system, so tapping the volume down never turns into a dictation. While
  recording, Tiro holds exclusive transient audio focus: media apps pause and resume
  when it stops (apps that ignore audio focus keep playing). A hold starts recording at the threshold, with a haptic
  tick, so a tap never turns the microphone on. The text is set into the field at its
  selection (`ACTION_SET_TEXT`), else pasted; if neither works it stays on the
  clipboard.
- **What became of it (`POST /corrections`).** Each dictation the Mac answers is
  saved to its History as application "Phone" (with its audio under Keep recordings),
  and the answer carries `entry` and `corrections` (Labs › Save corrections). With
  corrections on, the volume-key service follows the span it typed from the field's
  text-change events (`FieldSpan`): an edit before it moves it, one inside resizes it,
  and text typed at its end joins only when it continues the last word. The watch ends
  when the span goes away (sent), focus leaves the field, the keyboard closes, another
  dictation starts, or after 15 minutes; the last reading that still resembles the
  dictation is posted as `{"entry","status","text"}`, and only the span, never the
  rest of the field. Unsent corrections wait in the phone's `CorrectionOutbox` (newest
  50) for the next dictation. The Mac only accepts them for Phone entries, and a
  correction typed in History still wins.
- **Placeholders aren't text.** WhatsApp reports its "Message" placeholder as the
  field's text without marking it as a hint, so a dictation was typed after it. Text
  equal to the field's hint, or in a short list of known placeholders, counts as empty.
  WhatsApp exposes no hint at all (measured 2026-11-11), so only the list catches it.
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
- **On the Android 16 emulator, end to end** (2026-11-05, through the Tiro voice
  keyboard, removed 2026-11-11 in favour of the volume key), with a debug build playing a
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
- **One request, against a Mac without `/dictate` (2026-11-05, same phone):** the probe
  learned the route is missing and three fixture dictations typed through the two
  requests: 2.6 s (cleanup helper cold), then 0.49 s and 0.50 s, down from 0.72 s
  before the shared TLS context and the early connection.
- **`/dictate` against two requests, same installed Mac build (2026-11-05):** in the
  app, warm fixture dictations typed in a median 692 ms (15 runs) with `/dictate` and
  758 ms (5 runs, a temporary build forced onto two requests), about 65 ms saved. Over
  Wi-Fi with `curl` on the phone, a full handshake per request, `/dictate` took a median
  0.68 s against 0.82 s for the pair. The Mac's own work is most of it: about 0.3 s to
  transcribe and 0.23 s to clean up a 3.4 s clip. The same dictation measured 477 ms
  against the Mac build before, so compare runs only on one build in one sitting.
- **Streaming's fallback, same phone (2026-11-05):** with the Mac's listener waiting for
  setup, `/dictate/stream` was refused `503 engine-unavailable` (once read as a reset,
  because the Mac closed while the phone was already sending audio), and three fixture
  dictations typed through `/dictate`: 3.4 s cold, then 406 and 380 ms.
- **Streaming against the whole clip, same phone and Mac build (2026-11-05),** an 11 s
  fixture clip played at real-time pace, held through the clip: streamed dictations typed
  in a median 571 ms after release (565–573 ms warm, 6 runs), against 760 ms for the whole
  clip through `/dictate` (738–1033 ms, 6 runs, a temporary build with streaming off).
  The Mac's log (`LAN dictation stream finished`) puts the streamed time at ~170 ms to
  finish the stream's tail and ~390 ms of cleanup, with each audio frame relayed in about
  1 ms. Live text reached the pill about a second behind the audio. A person speaking
  into the phone hasn't been tried.
- After `adb install -r`, One UI can leave the accessibility service stuck in "Binding"
  for minutes; turning it off and on again binds it at once.
- **Not verified on the volume key:** a person speaking into the phone. The Mac's
  `say` from across the desk recorded as audio the Mac heard no words in, while the
  same sentence as a clean file transcribed exactly. Also untested: the physical
  button itself, apps beyond those two, and fields that refuse `ACTION_SET_TEXT`.
- Accessibility services are unbound while a UiAutomation client (agent-device's
  snapshot helper, `uiautomator`) is connected, so drive the volume-key test with
  `adb` and close agent-device first.
- **Cleaning finished sentences during a streamed dictation (2026-11-05, M1 Pro, no
  phone):** a Python client speaking the phone's protocol over pinned TLS to
  `192.168.0.3:47811` streamed the same 11 s clip at real-time pace. Release to final
  text went from a median 556 ms (542–564 ms, 10 runs, the build before) to 277 ms
  (250–338 ms, 10 runs), back to back in one sitting. At release, cleanup now waits
  only on the last 8 words: ~111 ms instead of ~384 ms. The stream's tail (~160 ms)
  and the live text's cadence were unchanged. Clips that correct themselves across a
  sentence ("three attempts? No actually five…") typed the same text as cleaning the
  whole transcript. Not tried on the phone or with a person speaking.

- **Bonjour and pairing from the window (2026-11-05):** `dns-sd` found the installed
  app's advert 1 s after launch, resolving to port 47811 with the fingerprint. In the
  window, Pair a phone showed a QR code that macOS Vision decoded to the right
  `tiro://pair` link, `hex lan devices` listed the new phone, and Unpair removed it. On
  the S24+, pairing to a dead address (192.168.0.250) and pressing Test found the Mac at
  192.168.0.3, saved it, and connected in 2.5 s. Not tried: a dictation (not just Test)
  after a move, a real DHCP change, and a network with more than one Mac.

## Next

- Phase 3 polish: Material You, a history screen, per-app on/off for the volume key.
