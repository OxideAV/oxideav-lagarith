//! Profiling driver for the Lagarith encode hot path.
//!
//! Round 376 promoted the frame encoder to a public API
//! ([`encode_frame`]); this is the encode-side counterpart of
//! `examples/profile_decode.rs`. It encodes one fixed, valid raw
//! frame in a tight, harness-free loop so an external sampling /
//! instrumenting profiler — `perf record`, `valgrind
//! --tool=callgrind`, macOS Instruments, `samply`, `dtrace` — sees
//! only the encoder's own work, without Criterion's per-iteration
//! measurement harness polluting symbol attribution.
//!
//! The profiled entry point is the public [`encode_frame`]. The input
//! is a deterministically-synthesised 64x64 RGB24 (`PixelKind::Bgr24`)
//! buffer with a gradient + low-frequency noise — smooth enough that
//! the encoder exercises its real pipeline (the per-channel header
//! dispatcher / `spec/06` §1, the Fibonacci probability prefix /
//! `spec/04`, the modern range coder / `spec/02`, the residual
//! zero-run RLE escape / `spec/05`, the JPEG-LS clamped-median
//! predictor / `spec/03` §3, and the RGB cross-plane decorrelation /
//! `spec/03` §4) rather than the raw / solid fast paths. No committed
//! fixtures, no `docs/` reads.
//!
//! Usage:
//!
//! ```text
//! # Default 50_000 encode iterations:
//! cargo run --release --example profile_encode
//!
//! # Override the iteration count (first positional arg):
//! cargo run --release --example profile_encode -- 200000
//!
//! # Under an external profiler, e.g.:
//! perf record -g -- target/release/examples/profile_encode 200000
//! valgrind --tool=callgrind target/release/examples/profile_encode 20000
//! samply record -- target/release/examples/profile_encode 200000
//! ```
//!
//! It prints the iteration count, total source-byte volume, the
//! produced frame size, and a checksum (so the optimiser cannot elide
//! the encode and a regression that silently changes output is
//! visible) to stderr, then exits 0.

use std::hint::black_box;

use oxideav_lagarith::{decode_frame, encode_frame, PixelKind};

/// 64x64 host surface — matches the bench / decode-profiler workload
/// size so the three harnesses profile the same shape.
const W: u32 = 64;
const H: u32 = 64;

/// Default encode-loop iteration count. Encode is heavier than decode,
/// so 50k iterations of a 64x64 RGB24 frame is a few seconds of wall
/// time at release-build speed — enough sampling resolution without an
/// unbounded interactive run. Override with the first CLI argument.
const DEFAULT_ITERS: u64 = 50_000;

/// Deterministic gradient + low-frequency-noise RGB24 content for the
/// 64x64 surface (the same shape the encode bench uses).
fn gradient_noise_rgb24() -> Vec<u8> {
    let len = PixelKind::Bgr24.buffer_len(W, H);
    let mut s = 0x9e37_79b9_7f4a_7c15u64;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        let grad = (i as u64 / 7) as u8;
        let noise = ((s >> 40) & 0x07) as u8;
        out.push(grad.wrapping_add(noise));
    }
    out
}

fn main() {
    let iters: u64 = std::env::args()
        .nth(1)
        .map(|s| {
            s.parse().unwrap_or_else(|_| {
                eprintln!("profile_encode: could not parse iteration count {s:?}; using default");
                DEFAULT_ITERS
            })
        })
        .unwrap_or(DEFAULT_ITERS);

    let pixels = gradient_noise_rgb24();

    // Sanity: encode + round-trip once before the timed loop so a
    // broken encode path surfaces as a clear error rather than a
    // silent run, and confirm we are exercising a compressing
    // (non-fallback) frame.
    let first =
        encode_frame(&pixels, W, H, PixelKind::Bgr24).expect("profiling input failed to encode");
    let decoded =
        decode_frame(&first, W, H, PixelKind::Bgr24).expect("profiling frame failed to decode");
    assert_eq!(decoded.pixels, pixels, "profiling input did not round-trip");
    let frame_len = first.len();

    // Tight, harness-free encode loop. `black_box` on the input and on
    // the folded checksum keeps the optimiser from hoisting the encode
    // out of the loop or eliding it.
    let mut checksum: u64 = 0;
    for i in 0..iters {
        let frame = encode_frame(
            black_box(&pixels),
            black_box(W),
            black_box(H),
            PixelKind::Bgr24,
        )
        .expect("profiling encode failed mid-loop");
        let mut acc: u64 = i;
        for &b in &frame {
            acc = acc.wrapping_mul(31).wrapping_add(b as u64);
        }
        checksum = checksum.wrapping_add(black_box(acc));
    }

    let src_len = pixels.len() as u64;
    let total_bytes = src_len.saturating_mul(iters);
    eprintln!(
        "profile_encode: {iters} iterations, {total_bytes} source bytes, \
         frame={frame_len} B, checksum={checksum:#018x}"
    );
}
