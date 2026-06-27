//! Criterion benchmarks for the Lagarith encoder hot paths.
//!
//! Round 376 promoted the frame encoder from a `#[cfg(test)]`-only
//! helper to a public API ([`encode_frame`]). This bench times the
//! public encode entry across every host pixel family so future
//! optimisation rounds can A/B-test encode changes against a stable
//! baseline, mirroring the decode bench in `benches/decode.rs`.
//!
//! Inputs are synthesised deterministically from a small LCG (no
//! committed fixtures, no `docs/` reads) at a representative 64x64
//! surface. The content is a gentle gradient + low-frequency noise so
//! the encoder exercises its real arithmetic / predictor / RLE path
//! (a pure-random buffer would route most planes to the raw-memcpy or
//! type-1 fallback, benching the wrong thing; a constant buffer would
//! hit the solid fast path). One bench covers each pipeline:
//!
//!   - **rgb24**: type-2/4 modern arithmetic RGB24 (3 planes, range
//!     coder, JPEG-LS median predictor, RGB cross-plane decorrelation).
//!   - **rgba**: type-8 modern arithmetic RGBA (adds the alpha plane).
//!   - **yv12**: type-10 4:2:0 planar (Y + half-res V/U).
//!   - **yuy2**: type-3 4:2:2 packed (Y + half-width U/V macropixels).
//!
//! Run with:
//!     cargo bench -p oxideav-lagarith --bench encode

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};

use oxideav_lagarith::{decode_frame, encode_frame, PixelKind};

const W: u32 = 64;
const H: u32 = 64;

/// Deterministic gradient + low-frequency noise content of `len`
/// bytes. Smooth enough that the predictor + range coder actually
/// compress (so the bench times the arithmetic path, not the raw /
/// solid fast paths).
fn gradient_noise(seed: u64, len: usize) -> Vec<u8> {
    let mut s = seed | 1;
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        // Gradient term dominates; a few bits of noise on top.
        let grad = (i as u64 / 7) as u8;
        let noise = ((s >> 40) & 0x07) as u8;
        out.push(grad.wrapping_add(noise));
    }
    out
}

/// Build a YUY2 buffer whose odd-tail chroma slot (if any) carries the
/// neutral 0x80 fill — here W is even, so this is a no-op, but it keeps
/// the helper honest for non-64 surfaces.
fn yuy2_input(seed: u64) -> Vec<u8> {
    gradient_noise(seed, PixelKind::Yuy2.buffer_len(W, H))
}

fn bench_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("encode_64x64");
    // Throughput per source pixel so per-format numbers are comparable.
    group.throughput(Throughput::Elements((W * H) as u64));

    let rgb24 = gradient_noise(1, PixelKind::Bgr24.buffer_len(W, H));
    let rgba = gradient_noise(2, PixelKind::Bgra32.buffer_len(W, H));
    let yv12 = gradient_noise(3, PixelKind::Yv12.buffer_len(W, H));
    let yuy2 = yuy2_input(4);

    let cases: &[(&str, &[u8], PixelKind)] = &[
        ("rgb24", &rgb24, PixelKind::Bgr24),
        ("rgba", &rgba, PixelKind::Bgra32),
        ("yv12", &yv12, PixelKind::Yv12),
        ("yuy2", &yuy2, PixelKind::Yuy2),
    ];

    for (name, pixels, kind) in cases {
        // Sanity: confirm the input encodes and round-trips before
        // timing it, so a regression that breaks the encode path can't
        // silently bench the error branch.
        let frame = encode_frame(pixels, W, H, *kind).expect("bench input encodes");
        let decoded = decode_frame(&frame, W, H, *kind).expect("bench frame decodes");
        assert_eq!(
            decoded.pixels, *pixels,
            "bench input {name} did not round-trip"
        );
        group.bench_with_input(BenchmarkId::from_parameter(name), pixels, |b, pixels| {
            b.iter(|| encode_frame(black_box(pixels), black_box(W), black_box(H), *kind).unwrap());
        });
    }

    group.finish();
}

criterion_group!(benches, bench_encode);
criterion_main!(benches);
