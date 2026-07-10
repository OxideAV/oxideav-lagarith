//! Criterion benchmarks for the Lagarith decoder hot paths.
//!
//! Round 301 (depth-mode benchmarks): oxideav-lagarith is decode-
//! saturated — every wire frame type (1 / 2 / 3 / 4 / 5 / 6 / 7 / 8 /
//! 10 / 11 + NULL) decodes, the test-side reference encoder
//! self-roundtrips bit-exactly, the modern RGB(A) path is
//! cross-decoder black-box confirmed, and a cargo-fuzz harness
//! landed in r291. Per the workspace
//! "saturated -> fuzz / bench / profile"
//! memo this round wires up `criterion` so future optimisation
//! rounds can A/B-test decode changes against a stable baseline.
//!
//! The benched entry point is the public [`decode_frame`]. Each
//! fixture is a real compressed Lagarith frame produced by the
//! crate's own reference encoder (captured once, embedded inline as
//! a `const` byte array — same self-contained, no-committed-fixture,
//! no-`docs/`-reads pattern the in-tree cross-decoder pin set uses).
//! The fixtures cover one representative
//! 64x64 frame of each major decode pipeline:
//!
//!   - **rgb24**: type-4 modern arithmetic RGB24 — three planes,
//!     modern range coder, JPEG-LS median predictor, RGB cross-plane
//!     decorrelation, BGR pack.
//!   - **rgba**: type-8 modern arithmetic RGBA — adds the fourth
//!     (alpha) plane decode on top of rgb24.
//!   - **yv12**: type-10 4:2:0 planar — Y + half-res V/U planes, no
//!     cross-plane decorrelation.
//!   - **yuy2**: type-3 4:2:2 packed-at-output — Y + half-width U/V
//!     planes, macropixel repack.
//!   - **legacy**: type-7 pre-1.1.0 adaptive-CDF legacy range coder
//!     (`spec/07`) — the most expensive per-channel entropy decode.
//!   - **solid**: type-6 solid-RGB fill — the constant-time fast
//!     path.
//!   - **uncompressed**: type-1 raw memcpy — the I/O-bound floor.
//!
//! Run with:
//!     cargo bench -p oxideav-lagarith --bench decode

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_lagarith::{decode_frame, PixelKind};

const W: u32 = 64;
const H: u32 = 64;

fn bench_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("decode_64x64");
    // Throughput is reported per decoded pixel so the per-format
    // numbers are directly comparable despite differing wire sizes.
    group.throughput(Throughput::Elements((W * H) as u64));

    let cases: &[(&str, &[u8], PixelKind)] = &[
        ("rgb24", FRAME_RGB24_64, PixelKind::Bgr24),
        ("rgba", FRAME_RGBA_64, PixelKind::Bgra32),
        ("yv12", FRAME_YV12_64, PixelKind::Yv12),
        ("yuy2", FRAME_YUY2_64, PixelKind::Yuy2),
        ("legacy", FRAME_LEGACY_64, PixelKind::Bgr24),
        ("uncompressed", FRAME_UNCOMP_RGB24_64, PixelKind::Bgr24),
    ];

    for (name, frame, kind) in cases {
        // Sanity: confirm the embedded fixture decodes before timing
        // it (a malformed fixture would otherwise silently bench the
        // error path).
        assert!(
            decode_frame(frame, W, H, *kind).is_ok(),
            "bench fixture {name} failed to decode"
        );
        group.bench_with_input(BenchmarkId::from_parameter(name), frame, |b, frame| {
            b.iter(|| decode_frame(black_box(frame), black_box(W), black_box(H), *kind).unwrap());
        });
    }

    // Solid fill is a 1x1-independent constant-time path; bench it at
    // the same 64x64 surface for an apples-to-apples pixel-rate.
    assert!(decode_frame(FRAME_SOLID_RGB, W, H, PixelKind::Bgr24).is_ok());
    group.bench_function("solid", |b| {
        b.iter(|| {
            decode_frame(
                black_box(FRAME_SOLID_RGB),
                black_box(W),
                black_box(H),
                PixelKind::Bgr24,
            )
            .unwrap()
        });
    });

    group.finish();
}

criterion_group!(benches, bench_decode);
criterion_main!(benches);

include!("fixtures_64.rs");
