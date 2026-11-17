# Selfie segmentation model

- **Model:** MediaPipe selfie segmenter, landscape variant, float16.
- **Source:** https://storage.googleapis.com/mediapipe-models/image_segmenter/selfie_segmenter_landscape/float16/latest/selfie_segmenter_landscape.tflite
- **SHA-256 of the .tflite:** `490e9ea734313e0de10fa0cd9e3c6133e36ea4db2b7a49bde9ef019f72796b8e`
- **Licence:** Apache-2.0, per the model card:
  https://storage.googleapis.com/mediapipe-assets/Model%20Card%20MediaPipe%20Selfie%20Segmentation.pdf
- **Authors:** Google (2021), as on the model card.

## What is committed

- `weights.bin`: the model's weights, repacked for the Rust network, fp16.
  SHA-256 `aafbc18409206308925f21703011617ce407067dd195bb69e4594c271b409228`.
- `netdef.rs`: the op list and buffer sizes. Generated; do not edit.
- `parity_logits.bin`: onnxruntime's pre-sigmoid output on a fixed synthetic picture, every third
  row and column, f32. The test in `tests.rs` compares against it (limit 1e-3).

The Veil build does not need Python. Only these generated files are used.

## Regenerate

Download the .tflite, then from the repo root:

    pip install tflite numpy onnx onnxruntime
    python tools/selfie_weights.py path/to/selfie_segmenter_landscape.tflite

The tool refuses a file with a different SHA-256, and a result whose weights hash differs from
the one above. Run `cargo test -p veil-media segment` afterwards.
