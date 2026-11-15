# Vendored libopus

libopus 1.6.1, BSD-3-Clause (see `COPYING`). Built by `crates/veil-media/build.rs` with the
`cc` crate. Veil does not change these files.

## Where it came from

The release sources bundled in the `opusic-sys` 0.7.7 crate from crates.io (checksum verified
by cargo). That crate patches only `CMakeLists.txt`, which is not kept here. The C sources
have not yet been compared against the tarball from downloads.xiph.org.

## What was left out

Everything the float build without SIMD does not compile:

- `dnn/` (deep PLC, DRED, OSCE and their model weights, 22 MB)
- `tests/`, `celt/tests/`, `silk/tests/`, `celt/dump_modes/`
- `src/opus_demo.c`, `src/opus_compare.c`, `src/qext_compare.c`, `src/repacketizer_demo.c`
- `doc/`, and the autotools, cmake and meson build files (all `meson.build` files removed)

The `celt/x86`, `celt/arm`, `silk/x86`, `silk/arm` and `silk/fixed` folders are kept so SIMD
or fixed point can be turned on later without vendoring again.

## Updating

1. Replace this folder with the new release, leaving out the same files.
2. Keep this file and update the version above.
3. `cargo test -j 8 -p veil-media --test opus` (it checks the version string).
