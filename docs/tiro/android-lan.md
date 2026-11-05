# Android over LAN: the plan, not built yet

Android records; the Mac does everything else. The phone runs no model.

## What already exists

| Need | Route | State |
| --- | --- | --- |
| Whole clip to text | `POST /transcriptions` | Exists |
| Live partials while speaking | `POST /streams` and friends (`docs/streams.md`) | Exists |
| Text to clean text | `POST /cleanup` | Added by Tiro |
| Auth | Bearer token | Exists, but delivered by a local file |

So the phone's request flow needs no new engine work:

```text
Android mic ─▶ /streams (16 kHz PCM) ─▶ partials on screen
            ─▶ finish ─▶ transcript ─▶ /cleanup ─▶ clean text ─▶ phone's text field
```

## What is missing

1. **A LAN listener.** `local_api` binds `127.0.0.1` on purpose. Add a second, opt-in
   listener rather than widening this one: the loopback API also carries `/control`,
   which can change settings and must never reach the network. Expose an allowlist
   of `/streams`, `/transcriptions`, and `/cleanup`.
2. **Pairing and transport.** The discovery file can't reach a phone, and bearer tokens
   must not cross Wi-Fi in clear text. QR pairing can carry a per-device token and the
   Mac's certificate fingerprint for pinned TLS.
3. **Discovery.** mDNS, falling back to the address in the QR code.
4. **Concurrency.** `/streams` serves one stream at a time and yields to local
   dictation. That is fine for one person, and worth saying out loud in the phone's UI.
5. **The Android app.** Most likely an input method (IME), so it works in any text field.

## Will it feel fast?

Probably, and it can be measured before writing an Android app. On home Wi-Fi the
network should add tens of milliseconds at most, small next to cleanup's hundreds, and
16 kHz PCM is only 32 KB/s. Those are estimates, not measurements. The real risks are
Wi-Fi power saving on the phone, and the Mac sleeping, since `IDLE_LIMIT` also means a
first request can wait for a model reload. A small script streaming a WAV across the LAN
would answer this for an afternoon's work, and is the right first step.

Off-LAN use (a tunnel such as Tailscale) should work with the same API, since it only
sees a reachable address.
