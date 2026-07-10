//! Profiling driver for the Lagarith decode hot path.
//!
//! Round 335 (depth-mode profiling): oxideav-lagarith is
//! decode-saturated — every wire frame type decodes, the test-side
//! reference encoder self-roundtrips bit-exactly, a Criterion bench
//! (`benches/decode.rs`, r301) tracks per-format pixel-rate, and a
//! libFuzzer harness (`fuzz/`, r291) asserts panic-freedom. The one
//! depth-mode capability not yet present is a *standalone runnable*
//! that loops the decode hot path long enough for an external
//! sampling/instrumenting profiler — `perf record`, `valgrind
//! --tool=callgrind`, macOS Instruments, `samply`, `dtrace` — to
//! attach and produce a flame graph or call-count profile. Criterion
//! wraps every iteration in its own measurement harness (warmup,
//! statistics, black-box fences) which pollutes a profiler's symbol
//! attribution; libFuzzer mutates the input so each decode walks a
//! different (often error) path. This example does neither: it
//! decodes one fixed, valid frame in a tight, harness-free loop so
//! the profiler sees only the decoder's own work.
//!
//! The benched / profiled entry point is the public
//! [`decode_frame`]. The fixture is a real compressed type-4 modern
//! arithmetic RGB24 frame — the most representative real-world path,
//! exercising the full pipeline: the 3-channel offset table
//! (`spec/01` §2.3), the per-channel header dispatcher (`spec/06`
//! §1), the Fibonacci probability prefix (`spec/04`), the modern
//! range coder (`spec/02`), the residual zero-run RLE escape
//! (`spec/05`), the JPEG-LS clamped-median spatial predictor
//! (`spec/03` §3), and the RGB cross-plane decorrelation (`spec/03`
//! §4). The frame bytes were captured once from the crate's own
//! reference encoder (the same self-contained, no-committed-fixture,
//! no-`docs/`-reads pattern the in-tree bench and cross-decoder pin
//! set use) and embedded inline below.
//!
//! Usage:
//!
//! ```text
//! # Default 200_000 decode iterations:
//! cargo run --release --example profile_decode
//!
//! # Override the iteration count (first positional arg):
//! cargo run --release --example profile_decode -- 1000000
//!
//! # Under an external profiler, e.g.:
//! perf record -g -- target/release/examples/profile_decode 2000000
//! valgrind --tool=callgrind target/release/examples/profile_decode 50000
//! samply record -- target/release/examples/profile_decode 2000000
//! ```
//!
//! It prints the iteration count, total decoded-byte volume, and a
//! checksum (so the optimiser cannot elide the decode and so a
//! regression that silently changes output is visible) to stderr,
//! then exits 0. The checksum is folded over every decoded frame's
//! pixel buffer.

use std::hint::black_box;

use oxideav_lagarith::{decode_frame, PixelKind};

/// 64x64 host surface — the decoded frame dimensions for the
/// embedded fixture below. Matches the bench's `W`/`H` so the two
/// harnesses profile the same workload size.
const W: u32 = 64;
const H: u32 = 64;

/// Default decode-loop iteration count. ~200k decodes of a 64x64
/// RGB24 frame is a few seconds of wall time at release-build speed —
/// enough sampling resolution for `perf`/`samply` without making a
/// quick interactive run feel unbounded. Override with the first CLI
/// argument.
const DEFAULT_ITERS: u64 = 200_000;

fn main() {
    let iters: u64 = std::env::args()
        .nth(1)
        .map(|s| {
            s.parse().unwrap_or_else(|_| {
                eprintln!("profile_decode: could not parse iteration count {s:?}; using default");
                DEFAULT_ITERS
            })
        })
        .unwrap_or(DEFAULT_ITERS);

    // Sanity-decode once before the timed loop so a corrupt fixture
    // surfaces as a clear error rather than a silent zero-work run.
    let first = decode_frame(FRAME_RGB24_64, W, H, PixelKind::Bgr24)
        .expect("embedded profiling fixture failed to decode");
    let decoded_len = first.pixels.len() as u64;
    assert_eq!(
        decoded_len,
        (W as u64) * (H as u64) * 3,
        "unexpected decoded RGB24 buffer length"
    );

    // Tight, harness-free decode loop. `black_box` on the input
    // arguments and on the folded checksum prevents the optimiser
    // from hoisting the decode out of the loop or eliding it entirely.
    // The fold *accumulates* (rather than XORs) each frame's content
    // hash mixed with the iteration index, so the running total stays
    // observably non-trivial regardless of iteration parity and the
    // optimiser cannot recognise the per-iteration result as
    // loop-invariant and lift it out.
    let mut checksum: u64 = 0;
    for i in 0..iters {
        let frame = decode_frame(
            black_box(FRAME_RGB24_64),
            black_box(W),
            black_box(H),
            PixelKind::Bgr24,
        )
        .expect("profiling fixture decode failed mid-loop");
        // Fold the decoded bytes into a rolling content hash so the
        // decode's output is observable — the optimiser must keep the
        // work.
        let mut acc: u64 = i;
        for &b in &frame.pixels {
            acc = acc.wrapping_mul(31).wrapping_add(b as u64);
        }
        checksum = checksum.wrapping_add(black_box(acc));
    }

    let total_bytes = decoded_len.saturating_mul(iters);
    eprintln!(
        "profile_decode: {iters} iterations, {total_bytes} decoded bytes, checksum={checksum:#018x}"
    );
}

// ───────────────────────── Embedded fixture ─────────────────────────
// Shared with the bench + the CI conformance guard — see
// `benches/fixtures_64.rs` for provenance. Only `FRAME_RGB24_64` is
// used here; the sibling fixtures come along via the include and are
// allowed to be dead code in this binary.
#[allow(dead_code)]
mod fixtures {
    include!("../benches/fixtures_64.rs");
}
use fixtures::FRAME_RGB24_64;
