#![no_main]

//! Decode arbitrary fuzz-supplied bytes through the full Lagarith decode
//! chain at the public [`decode_frame`] entry point.
//!
//! The one-byte frame-type selector at `payload[0]` chooses between the
//! Uncompressed (type 1) literal body, the SOLID grey / RGB / RGBA fills
//! (types 5 / 6 / 9), the modern arithmetic-coded RGB24 / RGB32 / RGBA /
//! YV12 / YUY2 / reduced-res families (types 2 / 4 / 8 / 10 / 3 / 11),
//! and the legacy pre-1.1.0 adaptive-CDF RGB path (type 7). Every
//! non-literal path walks a channel-offset table, decodes a Fibonacci
//! probability prefix, runs a range coder, expands a zero-run RLE
//! escape, and applies a spatial-predictor + cross-plane decorrelation
//! inverse — all driven by lengths and offsets read straight off the
//! wire.
//!
//! The contract under test is purely that every call *returns*: a
//! malformed stream yields `Err(lagarith::Error::…)`, a well-formed one
//! yields `Ok(DecodedFrame)`, and neither path may panic, abort,
//! integer-overflow (in a debug/ASAN build), or index out of bounds —
//! regardless of how hostile the bytes are.
//!
//! # Input framing
//!
//! The decoder takes the frame dimensions and host pixel format from the
//! caller (a real integration gets them from the AVI `strf`
//! `BITMAPINFOHEADER`), not from the codec payload. So the fuzz buffer
//! carries a tiny header that a single libFuzzer mutation can perturb
//! independently of the codec body:
//!
//! ```text
//! [0]       u8     width  selector  (mapped to a small even width)
//! [1]       u8     height selector  (mapped to a small even height)
//! [2..]            Lagarith frame payload (byte 0 is the frame type)
//! ```
//!
//! Each payload is driven against **all four** [`PixelKind`] host
//! formats, so one input exercises every wire-type → host-format decode
//! dispatch (RGB / RGBA pack the RGB-family types; YV12 / YUY2 pack the
//! YUV-family types). A type/host mismatch is a defined `Err`, not a
//! panic — which is exactly what this harness asserts.
//!
//! # Why the dimension mapping
//!
//! The decoder allocates `buffer_len(width, height)` for the output
//! raster, and the arithmetic paths allocate per-plane scratch of the
//! same order. Dimensions taken raw off two fuzz bytes could still
//! request a multi-gigabyte raster (a *resource* request, not a decoder
//! bug); worse, libFuzzer would waste its budget on allocation churn
//! rather than the parse/build/predictor logic this harness targets. We
//! therefore map the two selector bytes onto small dimensions in
//! `1..=64`, keeping the raster tiny while still reaching every code
//! path. The library itself is deliberately left free of an arbitrary
//! built-in size policy.
//!
//! **Odd dimensions are in scope.** Earlier revisions of this harness
//! mapped selectors onto *even* dimensions only ("YV12 / YUY2 want even
//! width and height"). That left the decoder's documented odd-dimension
//! branches unfuzzed: the YV12 `floor(W·H/4) != (W/2)·(H/2)` SPECGAP
//! fallback (`spec/03` §6.1.1 — a single-row chroma placeholder
//! geometry) and the YUY2 odd-width tail that emits the trailing luma
//! column with a `0x80` neutral chroma slot (`spec/03` §6.2). Those
//! paths run different predictor-geometry and packing arithmetic than
//! the even path, so panic-freedom there must be exercised independently
//! — which means the harness has to be able to *produce* odd widths and
//! heights. The chroma math is not "exact" at odd dims (it is a
//! host-integration placeholder per the spec), but the contract under
//! test is panic-freedom, not chroma exactness, so odd dims belong in
//! the corpus.

use libfuzzer_sys::fuzz_target;
use oxideav_lagarith::{decode_frame, DecodedFrame, PixelKind};

/// Map a fuzz selector byte onto a small dimension in `1..=64`,
/// **including odd values**. Odd widths and heights reach the decoder's
/// YV12 odd-dimension SPECGAP fallback (`spec/03` §6.1.1) and the YUY2
/// odd-width tail (`spec/03` §6.2) — paths the prior even-only mapping
/// never exercised. The small cap keeps the output raster tiny so
/// libFuzzer spends its budget on logic paths rather than allocation.
fn dim(selector: u8) -> u32 {
    // 0..=63 → 1,2,3,…,64 (both parities so the odd-dimension branches
    // are reachable; the `% 64` keeps the raster bounded).
    1 + u32::from(selector % 64)
}

fn drive(payload: &[u8], width: u32, height: u32) {
    // The whole point: decode must never panic / overflow / OOB on a
    // payload of arbitrary bytes, for any of the four host pixel
    // formats. Return values intentionally discarded — a debug-build
    // round-trip oracle would need a trusted encoder of the *same*
    // arbitrary stream, which doesn't exist for a clean-room codec.
    for kind in PixelKind::all() {
        let _: Result<DecodedFrame, _> = decode_frame(payload, width, height, kind);
    }
}

fuzz_target!(|data: &[u8]| {
    if data.len() < 2 {
        // Need at least the two dimension selectors; an empty payload is
        // the NULL-frame ("JUMP") case, which `decode_frame` rejects as
        // `Err(NullFrame)` — exercised here with a zero-length body.
        return;
    }
    let width = dim(data[0]);
    let height = dim(data[1]);
    drive(&data[2..], width, height);
});
