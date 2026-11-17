# Background blur runs its own segmentation network, not an ML crate

Status: accepted (2026-11-17)

Background blur needs to find the person in each camera picture. Veil does this with its own
inference code, in safe Rust, for one fixed model: the MediaPipe selfie segmenter, landscape
variant (Apache-2.0). The weights are embedded as a 213 KB fp16 file. We chose this because it
was the only option that met both limits we set: at most 5 ms added to the camera thread per
picture, and at most 2 MB added to the binary. It also keeps `unsafe` and native libraries out.

## Considered options

Measured on a Ryzen 5 5600X, 1280×720 NV12, one thread, 600 pictures (see issue #23).

- **tract** (pure-Rust inference crate). Rejected. The model alone took 4.1 ms, and the whole
  blur 8.3 ms (p95 9.2 ms); more threads didn't help. It added 17 MB to a stripped release build
  (24 MB with its ONNX loader), against Veil's 29 MB. It also can't load the model as published:
  one MediaPipe custom operation (`Convolution2DTransposeBias`) needs an offline conversion first.
- **ONNX Runtime (`ort`)**. Rejected without measuring. It's a native C++ library per platform:
  larger than tract, and a new supply-chain surface.
- **The operating systems' own camera effects** (Windows Studio Effects, macOS Portrait).
  Rejected. They differ per platform, Linux has none, Windows needs an NPU, and Veil wouldn't
  control what reaches the encoder.
- **Our own network** (chosen). The model takes 2.65 ms (p95 3.1 ms) on its worker thread. The
  camera thread adds 1.0 ms (p95 1.2 ms), and the binary grows by 0.32 MB. On a real photo, the
  output matches MediaPipe's own runtime to 7e-6.

## Consequences

- Veil owns about 90 fused operations (convolutions, depthwise convolutions, hard-swish,
  squeeze-and-excitation, bilinear resize, one transposed convolution) and their speed. They're
  written so the compiler vectorises them for the baseline target, with no runtime CPU detection.
- A new or retrained model means regenerating the weights and network description with
  `tools/selfie_weights.py`. A parity test against a reference catches a wrong conversion.
- Segmentation runs on its own worker thread, which exists only while blur is on. The camera
  thread never waits for it and uses the newest mask. If the mask falls more than three pictures
  behind, blur turns off and says so.
