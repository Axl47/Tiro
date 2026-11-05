# Tiro

Tiro is English-only local dictation for macOS, built for dictating prompts to coding
agents. Hold a key and speak the way you actually talk; what pastes reads the way you
meant it, with your technical terms intact.

It is a fork of [Hex](https://github.com/anomalyco/hex) that adds
[Vedu Scribe 0.8B](https://huggingface.co/heyvedu/vedu-scribe-0.8b), a small on-device
model, as a cleanup stage. The name is Marcus Tullius Tiro's, the secretary who took
down Cicero's speech and invented a shorthand to keep up.

- Speech recognition and cleanup both run on your Mac; requires Apple silicon
- Removes fillers, repetitions, and false starts, and applies self-corrections
- Guards code: mangled words are repaired, and output that loses a path, flag, or
  `snake_case` name is thrown away in favor of a deterministic cleanup
- Add your own terms with `hex vocabulary add`; the raw transcript stays in History

## Build

Needs Xcode with its Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`),
Rust, and cmake.

```sh
TIRO_CODESIGN_IDENTITY="Your Identity" scripts/build-app.sh
open target/app/Tiro.app
```

A stable signing identity keeps macOS permissions across rebuilds; without one the
build is ad-hoc signed and asks again each time. The first launch downloads Parakeet
(731 MB) and Vedu Scribe (561 MB), both verified against pinned SHA-256 hashes.

## Using it

Hold the shortcut, speak, let go. Turn cleanup off with `hex labs off scribe-cleanup`.
The command-line tool is still called `hex`, on purpose: see the architecture notes.

## More

- [Architecture evaluation and decisions](docs/tiro/architecture.md)
- [Raw vs cleaned benchmark](benchmarks/cleanup/README.md)
- [Android over LAN plan](docs/tiro/android-lan.md)
- [Hex's own docs](docs/README.md), which still apply to everything underneath

## License

MIT, as Hex is. Vedu Scribe's weights are Apache-2.0. The HeyVedu app is GPLv3, so
none of its code is used here; the integration follows the model card.
