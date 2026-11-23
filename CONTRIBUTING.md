# Contributing to Veil

Thank you for helping. Veil is a small project, so a short issue before a large change saves
everyone time.

## Before you start

Every Veil session opens the live roadmap and updates its shared work state. Follow
[AGENTS.md](AGENTS.md) and start with `python tools/roadmap.py start --session <id> --title "<task>"`.
The highlighted next action, work states and completion evidence are shared across sessions;
the generated HTML also remains available as an offline snapshot. See
[Roadmap tracking](docs/ROADMAP_TRACKING.md) for commands and live-preview behaviour.

- **Security holes:** do not open an issue. See [SECURITY.md](SECURITY.md).
- **Bugs and ideas:** open an issue with the template. Say what you expected and what happened.
- **Large changes:** open an issue first, so we can agree the approach before you build it.

Read [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the design and
[docs/HANDOFF.md](docs/HANDOFF.md) for what is done and what is next.
The handoff also contains historical notes; its current snapshot and the code take
precedence over old next-step lists. User setup is in
[Getting started](docs/GETTING_STARTED.md), node setup in
[Self-hosting](docs/SELF_HOSTING.md), and release maintenance in
[Releasing](docs/RELEASING.md).

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

The synthetic call suite uses generated video and silent speaker playback, with no camera
or microphone capture. On Windows, run it explicitly with:

```bash
cargo test -p veil-app engine::loopback -- --ignored --test-threads=1
```

This exercises admission, raised hands, chat/reactions, screen changes and call shutdown on
localhost. It does not replace a call between separate devices or networks. Do not run all
ignored tests casually: other tests can capture the camera or microphone or map router ports.

For a desktop UI-only demo, build with `--features dev-ui`, set `VEIL_START=call` and
`VEIL_DEV_NO_CAPTURE=1`. The latter keeps real camera/microphone capture off while inspecting
layouts and simulated joins. Both variables are development features, ignored by normal builds.

CI runs the full workspace tests on Windows, Clippy on all three operating systems, and
debug builds on Linux/macOS. The release workflow builds and packages optimised binaries.
Normal CI does not run the ignored hardware or real-network tests.

## What a good pull request has

- **One change.** A fix or a feature, not both.
- **Tests with the code.** New logic gets tests in the same pull request, above all pure
  functions and anything security relevant.
- **Clippy clean.** CI builds with `-D warnings`.
- **Docs kept true.** If your change alters what Veil protects, what it depends on, or what other
  parties can see, update [docs/TRANSPARENCY.md](docs/TRANSPARENCY.md) in the same pull request.
- **No new `unsafe`** outside `veil-media`'s documented FFI modules and the Android launcher's
  single exported entry-point attribute (see Transparency), and no new dependency for a few
  lines of code. A new dependency must pass `tools/licences.py`.
- **Small files.** Aim for under 500 lines per file. Split by responsibility.

## Writing style

Docs and interface text use plain English: short sentences, one idea each, UK spelling. Say
what something does, then why. Interface labels are short; detail goes in tooltips.

## Licence

Unless you say otherwise, anything you submit is dual licensed under
[Apache-2.0](LICENSE-APACHE) and [MIT](LICENSE-MIT), as the rest of Veil.
