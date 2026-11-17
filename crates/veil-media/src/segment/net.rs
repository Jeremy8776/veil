//! The network: op definitions, weight loading and the op-by-op executor.
//!
//! Purpose: [`Net`] holds the expanded f32 weights and the activation buffers, and walks the
//! generated op list in `netdef.rs`. The converter has already assigned each op's input and
//! output to a buffer slot by liveness, so only seven buffers exist.
//!
//! Invariants:
//! - [`Net::new`] allocates everything; [`Net::run`] allocates nothing. (Taking a slot out of
//!   its `Vec` and putting it back moves three words and does not allocate.)
//! - Slot 0 holds the NHWC picture first. The last op writes the logits to the caller's buffer,
//!   so its destination slot is -1 in the converter and is never used here.
//! - Weights are exactly fp16 in `weights.bin`; widening them to f32 loses nothing.

use super::kernels::{conv_dense, conv1x1, depthwise};
use super::netdef::{N_WEIGHTS, OPS, SLOTS};
use super::ops;
use super::{MODEL_H, MODEL_W};

pub(super) static WEIGHTS: &[u8] = include_bytes!("weights.bin");

/// Fused activation of a convolution.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Act {
    None,
    Relu,
    Hswish,
    Sigmoid,
}

/// One network step. Slots index [`SLOTS`]; `w` and `b` are offsets into the weight table.
#[derive(Clone, Copy)]
pub(super) enum Op {
    ConvDense {
        src: usize,
        dst: usize,
        ih: usize,
        iw: usize,
        oh: usize,
        ow: usize,
        cin: usize,
        k: usize,
        stride: usize,
        pt: usize,
        pl: usize,
        w: usize,
        b: usize,
        act: Act,
    },
    Conv1x1 {
        src: usize,
        dst: usize,
        px: usize,
        cin: usize,
        cout: usize,
        w: usize,
        b: usize,
        act: Act,
    },
    Depthwise {
        src: usize,
        dst: usize,
        ih: usize,
        iw: usize,
        oh: usize,
        ow: usize,
        c: usize,
        k: usize,
        stride: usize,
        pt: usize,
        pl: usize,
        w: usize,
        b: usize,
        act: Act,
    },
    Mean {
        src: usize,
        dst: usize,
        px: usize,
        c: usize,
    },
    Mul {
        dst: usize,
        x: usize,
        s: usize,
        px: usize,
        c: usize,
    },
    Add {
        dst: usize,
        a: usize,
        b: usize,
        len: usize,
    },
    Resize {
        src: usize,
        dst: usize,
        ih: usize,
        iw: usize,
        c: usize,
    },
    TConv {
        src: usize,
        px: usize,
        w: usize,
        b: usize,
        ow: usize,
    },
}

/// Half-precision float to f32, including zero, subnormals, infinities and NaN.
fn half_to_f32(h: u16) -> f32 {
    let sign = (u32::from(h) & 0x8000) << 16;
    let exp = u32::from((h >> 10) & 0x1f);
    let man = u32::from(h & 0x3ff);
    let bits = if exp == 0 {
        if man == 0 {
            sign
        } else {
            // Subnormal: shift the top set bit to bit 10 and fix the exponent.
            let shift = man.leading_zeros() - 21;
            sign | ((113 - shift) << 23) | (((man << shift) & 0x3ff) << 13)
        }
    } else if exp == 31 {
        sign | 0x7f80_0000 | (man << 13)
    } else {
        sign | ((exp + 112) << 23) | (man << 13)
    };
    f32::from_bits(bits)
}

/// Runs a convolution kernel with the activation chosen at compile time.
macro_rules! by_act {
    ($act:expr, $f:ident($($a:expr),*)) => {
        match $act {
            Act::None => $f::<0>($($a),*),
            Act::Relu => $f::<1>($($a),*),
            Act::Hswish => $f::<2>($($a),*),
            Act::Sigmoid => $f::<3>($($a),*),
        }
    };
}

/// Weights and activation buffers.
pub(super) struct Net {
    w: Vec<f32>,
    slots: Vec<Vec<f32>>,
}

impl Net {
    pub(super) fn new() -> Net {
        debug_assert_eq!(WEIGHTS.len(), N_WEIGHTS * 2);
        let w = WEIGHTS
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| half_to_f32(u16::from_le_bytes(c)))
            .collect();
        Net {
            w,
            slots: SLOTS.iter().map(|&n| vec![0.0; n]).collect(),
        }
    }

    /// `input` is planar CHW RGB (3 x H x W) and `logits` is H x W, both exactly that size.
    pub(super) fn run(&mut self, input: &[f32], logits: &mut [f32]) {
        let plane = MODEL_H * MODEL_W;
        for (c, chan) in input[..3 * plane].chunks_exact(plane).enumerate() {
            for (px, &v) in self.slots[0].as_chunks_mut::<3>().0.iter_mut().zip(chan) {
                px[c] = v;
            }
        }
        for op in OPS.iter() {
            match *op {
                Op::ConvDense {
                    src,
                    dst,
                    ih,
                    iw,
                    oh,
                    ow,
                    cin,
                    k,
                    stride,
                    pt,
                    pl,
                    w,
                    b,
                    act,
                    ..
                } => {
                    let mut d = std::mem::take(&mut self.slots[dst]);
                    let (wt, bs) = (&self.w[w..], &self.w[b..]);
                    by_act!(
                        act,
                        conv_dense(
                            &self.slots[src],
                            wt,
                            bs,
                            &mut d,
                            ih,
                            iw,
                            oh,
                            ow,
                            cin,
                            k,
                            stride,
                            pt,
                            pl
                        )
                    );
                    self.slots[dst] = d;
                }
                Op::Conv1x1 {
                    src,
                    dst,
                    px,
                    cin,
                    cout,
                    w,
                    b,
                    act,
                } => {
                    let mut d = std::mem::take(&mut self.slots[dst]);
                    let (wt, bs) = (&self.w[w..], &self.w[b..]);
                    by_act!(
                        act,
                        conv1x1(&self.slots[src], wt, bs, &mut d, px, cin, cout)
                    );
                    self.slots[dst] = d;
                }
                Op::Depthwise {
                    src,
                    dst,
                    ih,
                    iw,
                    oh,
                    ow,
                    c,
                    k,
                    stride,
                    pt,
                    pl,
                    w,
                    b,
                    act,
                } => {
                    let mut d = std::mem::take(&mut self.slots[dst]);
                    let (wt, bs) = (&self.w[w..], &self.w[b..]);
                    by_act!(
                        act,
                        depthwise(
                            &self.slots[src],
                            wt,
                            bs,
                            &mut d,
                            ih,
                            iw,
                            oh,
                            ow,
                            c,
                            k,
                            stride,
                            pt,
                            pl
                        )
                    );
                    self.slots[dst] = d;
                }
                Op::Mean { src, dst, px, c } => {
                    let mut d = std::mem::take(&mut self.slots[dst]);
                    ops::mean(&self.slots[src], &mut d, px, c);
                    self.slots[dst] = d;
                }
                Op::Mul { dst, x, s, px, c } => {
                    let mut d = std::mem::take(&mut self.slots[dst]);
                    if dst == x {
                        ops::scale_in_place(&mut d, &self.slots[s], px, c);
                    } else {
                        ops::scale(&self.slots[x], &self.slots[s], &mut d, px, c);
                    }
                    self.slots[dst] = d;
                }
                Op::Add { dst, a, b, len } => {
                    let mut d = std::mem::take(&mut self.slots[dst]);
                    if dst == a {
                        ops::add_in_place(&mut d, &self.slots[b], len);
                    } else if dst == b {
                        ops::add_in_place(&mut d, &self.slots[a], len);
                    } else {
                        ops::add(&self.slots[a], &self.slots[b], &mut d, len);
                    }
                    self.slots[dst] = d;
                }
                Op::Resize {
                    src,
                    dst,
                    ih,
                    iw,
                    c,
                } => {
                    let mut d = std::mem::take(&mut self.slots[dst]);
                    ops::resize2x(&self.slots[src], &mut d, ih, iw, c);
                    self.slots[dst] = d;
                }
                Op::TConv { src, px, w, b, ow } => {
                    ops::tconv(
                        &self.slots[src],
                        &self.w[w..][..64],
                        self.w[b],
                        logits,
                        px,
                        ow,
                    );
                }
            }
        }
    }
}
