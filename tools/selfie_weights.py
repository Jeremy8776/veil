"""Converts the MediaPipe selfie segmenter into the files Veil's `segment` module embeds.

Purpose: Veil runs person segmentation (Background blur) with a hand-written safe-Rust network,
so the build needs no Python and no inference runtime. This script is the one-off step that
turns the published TFLite model into Rust-friendly data. The generated files are committed;
run this only to regenerate them (see crates/veil-media/src/segment/MODEL.md).

Run from the repo root, with `pip install tflite numpy onnx onnxruntime`:
    python tools/selfie_weights.py path/to/selfie_segmenter_landscape.tflite

It refuses any file whose SHA-256 is not the one below. It writes, in
crates/veil-media/src/segment/:
    weights.bin          every weight as little-endian fp16 (the model's own precision)
    netdef.rs            the op list and activation buffer sizes, as Rust constants
    parity_logits.bin    reference output for the Rust parity test (see below)

weights.bin layout (the network runs NHWC, output channel innermost):
    CONV_2D 1x1       w[cout/8][cin][8] (8-wide output tiles), b[cout]
    CONV_2D 3x3       w[ky][kx][cin][cout], b[cout]
    DEPTHWISE_CONV    w[ky][kx][c], b[c]
    transposed conv   w[a*2+b][c], b[1]

Parity reference: the tool converts the same model to ONNX in memory (final sigmoid removed),
runs onnxruntime on a fixed synthetic picture, and keeps every third row and column of the
pre-sigmoid output as little-endian f32 (about 16 KB). The picture is a flat-coloured "head and
body" scene (gradient background, skin-coloured ellipse, dark ellipse, integer arithmetic only)
plus xorshift32 noise (seed 0x12345678; x ^= x << 13; x ^= x >> 17; x ^= x << 5; byte x >> 24,
in height, width, channel order, scaled to -32..31). Pure noise drives this model to
one-signed logits, so it makes a weak test; this scene gives both signs. No photographs are
involved. The ONNX conversion was checked against MediaPipe itself to 2e-5 on the final mask.
"""

import hashlib
import pathlib
import struct
import sys

import numpy as np
import onnx
import onnxruntime
import tflite
from onnx import TensorProto, helper, numpy_helper

MODEL_SHA256 = "490e9ea734313e0de10fa0cd9e3c6133e36ea4db2b7a49bde9ef019f72796b8e"
WEIGHTS_SHA256 = "aafbc18409206308925f21703011617ce407067dd195bb69e4594c271b409228"
OUT = pathlib.Path(__file__).resolve().parent.parent / "crates/veil-media/src/segment"
H, W = 144, 256
GRID_STEP = 3
SEED = 0x12345678

OPS = {v: k for k, v in tflite.BuiltinOperator.__dict__.items() if not k.startswith("_")}
NP = {tflite.TensorType.FLOAT32: np.float32, tflite.TensorType.FLOAT16: np.float16,
      tflite.TensorType.INT32: np.int32}
ACT = {"HARD_SWISH": "Hswish", "RELU": "Relu", "LOGISTIC": "Sigmoid"}


class Model:
    """Read access to one TFLite graph: tensors, constants and operators."""

    def __init__(self, data):
        self.m = tflite.Model.GetRootAsModel(data, 0)
        self.g = self.m.Subgraphs(0)
        self.folded = {}  # DEQUANTIZE outputs, as fp32

    def shape(self, i):
        return self.g.Tensors(i).ShapeAsNumpy().tolist()

    def const(self, i):
        """Constant data of tensor i as fp32 (or int32), or None for an activation."""
        if i in self.folded:
            return self.folded[i]
        t = self.g.Tensors(i)
        d = self.m.Buffers(t.Buffer()).DataAsNumpy()
        if isinstance(d, int) or d.size == 0:
            return None
        a = np.frombuffer(d.tobytes(), NP[t.Type()]).reshape(self.shape(i))
        return a.astype(np.float32) if a.dtype == np.float16 else a

    def operators(self):
        """Yields (name, op, inputs, output, code) with DEQUANTIZE folded away."""
        for k in range(self.g.OperatorsLength()):
            o = self.g.Operators(k)
            code = self.m.OperatorCodes(o.OpcodeIndex())
            name = OPS[max(code.BuiltinCode(), code.DeprecatedBuiltinCode())]
            ins = o.InputsAsNumpy().tolist()
            out = int(o.OutputsAsNumpy()[0])
            if name == "DEQUANTIZE":
                self.folded[out] = self.const(ins[0]).astype(np.float32)
                continue
            yield name, o, ins, out, code


def options(o, cls):
    t = o.BuiltinOptions()
    x = cls()
    x.Init(t.Bytes, t.Pos)
    return x


def same_pads(in_hw, k, s):
    """TF SAME padding: (top, left); the odd extra pixel goes bottom and right."""
    pads = [max((-(-n // s) - 1) * s + k - n, 0) for n in in_hw]
    return pads[0] // 2, pads[1] // 2


def tconv_params(o, code, w, b):
    assert code.CustomCode() == b"Convolution2DTransposeBias"
    padding, sw, sh = struct.unpack("<3i", o.CustomOptionsAsNumpy().tobytes()[:12])
    assert padding == 1 and (sw, sh) == (2, 2) and w.shape == (1, 2, 2, 16) and b.shape == (1,)


def test_picture():
    """The parity picture: H x W x 3 bytes. Integer arithmetic only, so Rust can repeat it."""
    s, noise = SEED, []
    for _ in range(H * W * 3):
        s ^= (s << 13) & 0xFFFFFFFF
        s ^= s >> 17
        s ^= (s << 5) & 0xFFFFFFFF
        noise.append(s >> 24)
    img = np.zeros((H, W, 3), np.uint8)
    for y in range(H):
        for x in range(W):
            px = [60 + x * 40 // W, 90 + y * 40 // H, 120 - x * 30 // W]  # background
            if ((x - 128) * 70) ** 2 + ((y - 150) * 80) ** 2 < (80 * 70) ** 2:
                px = [40, 60, 110]  # body
            if ((x - 128) * 42) ** 2 + ((y - 55) * 34) ** 2 < (34 * 42) ** 2:
                px = [205, 150, 120]  # head
            for c in range(3):
                n = (noise[(y * W + x) * 3 + c] >> 2) - 32  # xorshift byte, -32..31
                img[y, x, c] = min(max(px[c] + n, 0), 255)
    return img


def reference_logits(model):
    """Runs the model under onnxruntime with the final sigmoid removed; returns H x W logits."""
    nodes, inits = [], []

    def init(name, arr):
        inits.append(numpy_helper.from_array(np.ascontiguousarray(arr), name))
        return name

    final = f"t{model.g.Outputs(0)}"
    for op, o, ins, out, code in model.operators():
        x, y = f"t{ins[0]}", f"t{out}"
        if op == "CONV_2D" or op == "DEPTHWISE_CONV_2D":
            w, b = model.const(ins[1]), model.const(ins[2])
            conv = op == "CONV_2D"
            p = options(o, tflite.Conv2DOptions if conv else tflite.DepthwiseConv2DOptions)
            s = [p.StrideH(), p.StrideW()]
            if p.Padding() == tflite.Padding.SAME:
                pads = same_onnx_pads(model.shape(ins[0])[1:3], w.shape[1], s[0])
            else:
                pads = [0, 0, 0, 0]
            wo = w.transpose(0, 3, 1, 2) if conv else w.transpose(3, 0, 1, 2)
            nodes.append(helper.make_node(
                "Conv", [x, init(y + "_w", wo), init(y + "_b", b)], [y], strides=s, pads=pads,
                group=1 if conv else w.shape[3], kernel_shape=list(w.shape[1:3])))
        elif op == "HARD_SWISH":
            nodes.append(helper.make_node("HardSwish", [x], [y]))
        elif op == "RELU":
            nodes.append(helper.make_node("Relu", [x], [y]))
        elif op == "LOGISTIC":
            if y == final:
                final = x  # the last sigmoid is removed: the parity test compares logits
            else:
                nodes.append(helper.make_node("Sigmoid", [x], [y]))
        elif op in ("ADD", "MUL"):
            nodes.append(helper.make_node(op.capitalize(), [x, f"t{ins[1]}"], [y]))
        elif op == "MEAN":
            nodes.append(helper.make_node("GlobalAveragePool", [x], [y]))
        elif op == "RESIZE_BILINEAR":
            size = np.array([1, model.shape(out)[3], *model.shape(out)[1:3]], np.int64)
            nodes.append(helper.make_node(
                "Resize", [x, "", "", init(y + "_size", size)], [y], mode="linear",
                coordinate_transformation_mode="half_pixel"))
        elif op == "CUSTOM":
            w, b = model.const(ins[1]), model.const(ins[2])
            tconv_params(o, code, w, b)
            nodes.append(helper.make_node(
                "ConvTranspose", [x, init(y + "_w", w.transpose(3, 0, 1, 2)), init(y + "_b", b)],
                [y], strides=[2, 2], kernel_shape=[2, 2]))
        else:
            raise SystemExit(f"unhandled op {op}")

    i_in = model.g.Inputs(0)
    graph = helper.make_graph(
        nodes, "selfie_segmenter",
        [helper.make_tensor_value_info(f"t{i_in}", TensorProto.FLOAT, [1, 3, H, W])],
        [helper.make_tensor_value_info(final, TensorProto.FLOAT, [1, 1, H, W])], inits)
    onnx_model = helper.make_model(graph, opset_imports=[helper.make_opsetid("", 17)])
    onnx_model.ir_version = 8
    onnx.checker.check_model(onnx_model, full_check=True)
    sess = onnxruntime.InferenceSession(onnx_model.SerializeToString())
    x = (test_picture().astype(np.float32) / np.float32(255)).transpose(2, 0, 1)[None]
    return sess.run(None, {sess.get_inputs()[0].name: x})[0][0, 0]


def same_onnx_pads(in_hw, k, s):
    t, l = same_pads(in_hw, k, s)
    pads = [max((-(-n // s) - 1) * s + k - n, 0) for n in in_hw]
    return [t, l, pads[0] - t, pads[1] - l]


def build_ops(model):
    """Reads the graph into a list of dicts, fusing each activation into its producer."""
    ops = []
    for name, o, ins, out, code in model.operators():
        d = dict(op=name, ins=[ins[0]], out=out, ish=model.shape(ins[0]), osh=model.shape(out),
                 act="None")
        if name in ("CONV_2D", "DEPTHWISE_CONV_2D"):
            w, b = model.const(ins[1]), model.const(ins[2])
            conv = name == "CONV_2D"
            p = options(o, tflite.Conv2DOptions if conv else tflite.DepthwiseConv2DOptions)
            assert p.FusedActivationFunction() == 0
            assert p.DilationHFactor() == 1 and p.DilationWFactor() == 1
            assert p.StrideH() == p.StrideW() and w.shape[1] == w.shape[2]
            d.update(k=w.shape[1], stride=p.StrideH(), b=b)
            same = p.Padding() == tflite.Padding.SAME
            d["pad"] = same_pads(d["ish"][1:3], d["k"], d["stride"]) if same else (0, 0)
            if conv:
                assert same or d["k"] == 1
                d["w"] = np.ascontiguousarray(w.transpose(1, 2, 3, 0))  # OHWI -> HWIO
            else:
                assert p.DepthMultiplier() == 1
                d["w"] = np.ascontiguousarray(w[0])  # H W C
        elif name in ACT:
            pass
        elif name in ("ADD", "MUL"):
            d["ins"] = ins[:2]
            assert all(model.const(i) is None for i in ins[:2])
        elif name == "MEAN":
            assert sorted(model.const(ins[1]).tolist()) == [1, 2]
            assert options(o, tflite.ReducerOptions).KeepDims()
        elif name == "RESIZE_BILINEAR":
            p = options(o, tflite.ResizeBilinearOptions)
            assert p.HalfPixelCenters() and not p.AlignCorners()
            assert d["osh"][1:3] == [2 * d["ish"][1], 2 * d["ish"][2]]
        elif name == "CUSTOM":
            w, b = model.const(ins[1]), model.const(ins[2])
            tconv_params(o, code, w, b)
            d.update(op="TCONV", w=np.ascontiguousarray(w[0].reshape(4, 16)), b=b)
        else:
            raise SystemExit(f"unhandled op {name}")
        ops.append(d)

    uses = {}
    for d in ops:
        for t in d["ins"]:
            uses.setdefault(t, []).append(d)
    dead = []
    for d in ops:
        if d["op"] in ACT:
            t = d["ins"][0]
            prod = next(p for p in ops if p["out"] == t)
            assert prod["op"] in ("CONV_2D", "DEPTHWISE_CONV_2D", "TCONV") and len(uses[t]) == 1
            prod["act"], prod["out"] = ACT[d["op"]], d["out"]
            dead.append(d)
    ops = [d for d in ops if not any(d is x for x in dead)]
    # The Rust network stops at the logits: the sigmoid is applied by the caller.
    assert ops[-1]["op"] == "TCONV" and ops[-1]["act"] == "Sigmoid"
    ops[-1]["act"] = "None"
    return ops


def allocate(model, ops):
    """Gives each op's input and output an activation slot by liveness; returns slot sizes."""
    size = lambda sh: int(np.prod(sh))
    t_in = model.g.Inputs(0)
    last = {}
    for i, d in enumerate(ops):
        for t in d["ins"]:
            last[t] = i
    slot_of, slot_size, free = {t_in: 0}, [size(model.shape(t_in))], []

    def alloc(n):
        fits = [s for s in free if slot_size[s] >= n]
        if fits:
            s = min(fits, key=lambda s: slot_size[s])
        elif free:
            s = max(free, key=lambda s: slot_size[s])
        else:
            slot_size.append(n)
            return len(slot_size) - 1
        free.remove(s)
        slot_size[s] = max(slot_size[s], n)
        return s

    for i, d in enumerate(ops):
        n = size(d["osh"])
        if d["op"] in ("ADD", "MUL"):
            a, b = d["ins"]
            cand = [a, b] if d["op"] == "ADD" else [a]  # MUL: the first operand is the big one
            dying = [t for t in cand if last[t] == i and size(model.shape(t)) == n]
            if dying:  # in place into an operand that dies here
                slot_of[d["out"]] = slot_of[dying[0]]
                free.extend(slot_of[t] for t in (a, b) if t != dying[0] and last[t] == i)
            else:
                slot_of[d["out"]] = alloc(n)
                free.extend(slot_of[t] for t in (a, b) if last[t] == i)
        elif d["op"] == "TCONV":
            slot_of[d["out"]] = -1  # the caller's logits buffer
            free.extend(slot_of[t] for t in d["ins"] if last[t] == i)
        else:
            slot_of[d["out"]] = alloc(n)  # allocated before freeing inputs: no aliasing
            free.extend(slot_of[t] for t in d["ins"] if last[t] == i)
        d["src"] = [slot_of[t] for t in d["ins"]]
        d["dst"] = slot_of[d["out"]]
    return slot_size


def emit(ops):
    """Packs the weights and renders one Rust `Op` per line. Returns (lines, weights, count)."""
    chunks, count = [], 0

    def pack(a):
        nonlocal count
        a = np.ascontiguousarray(a, np.float32).ravel()
        h = a.astype(np.float16)
        assert (h.astype(np.float32) == a).all(), "weight is not exactly fp16"
        chunks.append(h)
        start, count = count, count + a.size
        return start

    lines = []
    for d in ops:
        o, s, dst, act = d["op"], d["src"], d["dst"], f"Act::{d['act']}"
        ih, iw, ic = d["ish"][1:]
        oh, ow, oc = d["osh"][1:]
        if o == "CONV_2D":
            assert oc % 8 == 0
            w = d["w"]
            if d["k"] == 1:
                assert d["stride"] == 1
                w = w[0, 0].reshape(ic, oc // 8, 8).transpose(1, 0, 2)
            wo, bo = pack(w), pack(d["b"])
            if d["k"] == 1:
                lines.append(f"Op::Conv1x1 {{ src: {s[0]}, dst: {dst}, px: {ih * iw}, cin: {ic}, "
                             f"cout: {oc}, w: {wo}, b: {bo}, act: {act} }}")
            else:
                assert ic == 3 and oc == 16 and d["k"] == 3
                lines.append(f"Op::ConvDense {{ src: {s[0]}, dst: {dst}, ih: {ih}, iw: {iw}, "
                             f"oh: {oh}, ow: {ow}, cin: {ic}, k: {d['k']}, "
                             f"stride: {d['stride']}, pt: {d['pad'][0]}, pl: {d['pad'][1]}, "
                             f"w: {wo}, b: {bo}, act: {act} }}")
        elif o == "DEPTHWISE_CONV_2D":
            wo, bo = pack(d["w"]), pack(d["b"])
            assert ic % 8 == 0
            lines.append(f"Op::Depthwise {{ src: {s[0]}, dst: {dst}, ih: {ih}, iw: {iw}, "
                         f"oh: {oh}, ow: {ow}, c: {ic}, k: {d['k']}, stride: {d['stride']}, "
                         f"pt: {d['pad'][0]}, pl: {d['pad'][1]}, w: {wo}, b: {bo}, act: {act} }}")
        elif o == "MEAN":
            lines.append(f"Op::Mean {{ src: {s[0]}, dst: {dst}, px: {ih * iw}, c: {ic} }}")
        elif o == "MUL":
            lines.append(f"Op::Mul {{ dst: {dst}, x: {s[0]}, s: {s[1]}, px: {ih * iw}, c: {ic} }}")
        elif o == "ADD":
            n = int(np.prod(d["osh"]))
            lines.append(f"Op::Add {{ dst: {dst}, a: {s[0]}, b: {s[1]}, len: {n} }}")
        elif o == "RESIZE_BILINEAR":
            lines.append(f"Op::Resize {{ src: {s[0]}, dst: {dst}, ih: {ih}, iw: {iw}, c: {ic} }}")
        elif o == "TCONV":
            wo, bo = pack(d["w"]), pack(d["b"])
            lines.append(f"Op::TConv {{ src: {s[0]}, px: {ih * iw}, w: {wo}, b: {bo}, ow: {iw} }}")
    return lines, np.concatenate(chunks).astype("<f2").tobytes(), count


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    data = pathlib.Path(sys.argv[1]).read_bytes()
    digest = hashlib.sha256(data).hexdigest()
    if digest != MODEL_SHA256:
        sys.exit(f"refusing {sys.argv[1]}: SHA-256 is {digest}, expected {MODEL_SHA256}")
    model = Model(data)
    ops = build_ops(model)
    slots = allocate(model, ops)
    lines, weights, count = emit(ops)
    digest = hashlib.sha256(weights).hexdigest()
    if digest != WEIGHTS_SHA256:
        sys.exit(f"weights.bin SHA-256 is {digest}, expected {WEIGHTS_SHA256}: converter changed?")
    (OUT / "weights.bin").write_bytes(weights)
    src = [
        "// Generated by tools/selfie_weights.py; do not edit.",
        "// Op list and buffer sizes of the selfie segmenter; weights live in weights.bin.",
        "",
        "use super::net::{Act, Op};",
        "",
        f"pub(super) const N_WEIGHTS: usize = {count};",
        f"pub(super) const SLOTS: [usize; {len(slots)}] = {slots};",
        f"pub(super) const OPS: [Op; {len(lines)}] = [",
        *[f"    {line}," for line in lines],
        "];",
        "",
    ]
    (OUT / "netdef.rs").write_text("\n".join(src), encoding="utf-8", newline="\n")

    logits = reference_logits(Model(data))
    grid = np.ascontiguousarray(logits[::GRID_STEP, ::GRID_STEP], "<f4")
    (OUT / "parity_logits.bin").write_bytes(grid.tobytes())
    print(f"{len(lines)} ops, {len(slots)} slots, {count} weights ({len(weights)} bytes)")
    print(f"logits: min {logits.min():.3f} max {logits.max():.3f} "
          f"mean {logits.mean():.3f} std {logits.std():.3f}; "
          f"positive {(logits > 0).mean():.1%}, beyond +-8 {(abs(logits) > 8).mean():.1%}")
    print(f"parity_logits.bin: {grid.shape[0]} x {grid.shape[1]} grid, {grid.nbytes} bytes")


if __name__ == "__main__":
    main()
