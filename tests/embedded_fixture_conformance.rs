//! CI-run conformance guard over the embedded 64x64 wire fixtures
//! shared by `benches/decode.rs` and `examples/profile_decode.rs`
//! (single source: `benches/fixtures_64.rs`).
//!
//! Motivation (round 407): the bench and the profiling example only
//! sanity-assert their fixtures when *they* run — which is never in
//! CI — so the pre-407 `FRAME_RGB24_64` / `FRAME_RGBA_64` captures
//! (pre-RLE channels with non-power-of-two totals coded against the
//! raw table, non-conformant under the `0x180001050`-normalized
//! model of `provenance/52`) went stale unnoticed by the test suite.
//! This test decodes every embedded fixture under `cargo test`, so a
//! wire-semantics change that invalidates a capture fails CI instead
//! of the next profiling session.

use oxideav_lagarith::{decode_frame, PixelKind};

#[allow(dead_code)]
mod fixtures {
    include!("../benches/fixtures_64.rs");
}

const W: u32 = 64;
const H: u32 = 64;

#[test]
fn every_embedded_fixture_decodes_conformantly() {
    let cases: &[(&str, &[u8], PixelKind)] = &[
        ("rgb24", fixtures::FRAME_RGB24_64, PixelKind::Bgr24),
        ("rgba", fixtures::FRAME_RGBA_64, PixelKind::Bgra32),
        ("yv12", fixtures::FRAME_YV12_64, PixelKind::Yv12),
        ("yuy2", fixtures::FRAME_YUY2_64, PixelKind::Yuy2),
        ("legacy", fixtures::FRAME_LEGACY_64, PixelKind::Bgr24),
        (
            "uncompressed",
            fixtures::FRAME_UNCOMP_RGB24_64,
            PixelKind::Bgr24,
        ),
        ("solid", fixtures::FRAME_SOLID_RGB, PixelKind::Bgr24),
    ];
    for &(name, frame, kind) in cases {
        let dec = decode_frame(frame, W, H, kind)
            .unwrap_or_else(|e| panic!("embedded fixture {name} no longer decodes: {e}"));
        assert_eq!(
            dec.pixels.len(),
            kind.buffer_len(W, H),
            "embedded fixture {name}: unexpected decoded length"
        );
    }
}
