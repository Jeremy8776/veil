# Contributing to Veil

Thank you for helping. Veil is a small project, so a short issue before a large change saves
everyone time.

## Before you start

- **Security holes:** do not open an issue. See [SECURITY.md](SECURITY.md).
- **Bugs and ideas:** open an issue with the template. Say what you expected and what happened.
- **Large changes:** open an issue first, so we can agree the approach before you build it.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and
[docs/HANDOFF.md](docs/HANDOFF.md) for what is done and what is next.

## Build and test

Veil builds on Windows, Linux and macOS with Rust 1.98.1 (the version CI uses). Media uses
Media Foundation and WASAPI on Windows, Video4Linux2 and ALSA on Linux, and AVFoundation and
Core Audio on macOS. Linux also needs the development packages listed in
`.github/workflows/ci.yml`; released Linux binaries need glibc 2.39 or later.

```bash
cargo build -p veil-app --release
cargo test -p veil-security -p veil-topology
cargo clippy --workspace --all-targets --locked
python tools/licences.py --check
```

The full test suite is large. Run the tests for the crates you changed. CI runs everything.
Tests marked `#[ignore]` need a camera, microphone or open ports.

## What a good pull request has

- **One change.** A fix or a feature, not both.
- **Tests with the code.** New logic gets tests in the same pull request, above all pure
  functions and anything security relevant.
- **Clippy clean.** CI builds with `-D warnings`.
- **Docs kept true.** If your change alters what Veil protects, what it depends on, or what other
  parties can see, update [docs/TRANSPARENCY.md](docs/TRANSPARENCY.md) in the same pull request.
- **No new `unsafe`** outside `veil-media`'s documented FFI modules, and no new dependency for a few
  lines of code. A new dependency must pass `tools/licences.py`.
- **Small files.** Aim for under 500 lines per file. Split by responsibility.

## Writing style

Docs and interface text use plain English: short sentences, one idea each, UK spelling. Say
what something does, then why. Interface labels are short; detail goes in tooltips.

## Licence

Unless you say otherwise, anything you submit is dual licensed under
[Apache-2.0](LICENSE-APACHE) and [MIT](LICENSE-MIT), as the rest of Veil.
