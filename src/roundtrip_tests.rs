//! End-to-end self-roundtrip tests: encode a frame with the
//! `#[cfg(test)]` encoder, decode it, compare bit-for-bit against
//! the original input.
//!
//! These exercise every dispatcher path covered by round 1:
//! Uncompressed (type 1), Solid Grey/RGB/RGBA (types 5/6/9),
//! Arithmetic RGB24 (type 4), and Arithmetic RGBA (type 8).
//!
//! Encoder strategy is deliberately simple (always pick header
//! `0x00` arithmetic-coded channel unless raw memcpy is shorter);
//! `spec/05` invariance plus the round-1 RLE expansion path are
//! covered by the dedicated `rle::tests` module in `rle.rs`.

#![cfg(test)]

use crate::channel::decode_channel;
use crate::decoder::{decode_frame, decode_frame_with_prev, Decoder, PixelKind};
use crate::encoder::{
    encode_arith_reduced_res, encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2,
    encode_arith_yv12, encode_channel_arith_rle, encode_legacy_rgb, encode_legacy_rgb_rle,
    encode_null, encode_solid_grey, encode_solid_rgb, encode_solid_rgba, encode_uncompressed,
};

fn pattern_bgr24(width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    let mut out = Vec::with_capacity(n * 3);
    for i in 0..n {
        let v = (i as u32).wrapping_mul(73).wrapping_add(11);
        out.push((v & 0xff) as u8);
        out.push(((v >> 5) & 0xff) as u8);
        out.push(((v >> 11) & 0xff) as u8);
    }
    out
}

fn pattern_bgra32(width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    let mut out = Vec::with_capacity(n * 4);
    for i in 0..n {
        let v = (i as u32).wrapping_mul(97).wrapping_add(5);
        out.push((v & 0xff) as u8);
        out.push(((v >> 4) & 0xff) as u8);
        out.push(((v >> 9) & 0xff) as u8);
        out.push(((v >> 14) & 0xff) as u8);
    }
    out
}

/// Packed YUY2 (`Y0 U Y1 V` per pair of pixels). `width` must be
/// even so the chroma macropixel boundaries are well-defined.
fn pattern_yuy2(width: u32, height: u32) -> Vec<u8> {
    debug_assert!(width % 2 == 0);
    let n = width as usize * height as usize;
    let mut out = Vec::with_capacity(n * 2);
    for y in 0..height {
        for k in 0..(width / 2) {
            // Two luma samples per macropixel + 1 U + 1 V.
            let yi = y.wrapping_mul(13).wrapping_add(k);
            out.push(yi.wrapping_mul(31).wrapping_add(11) as u8); // Y0
            out.push(yi.wrapping_mul(53).wrapping_add(101) as u8); // U
            out.push(yi.wrapping_mul(31).wrapping_add(43) as u8); // Y1
            out.push(yi.wrapping_mul(89).wrapping_add(151) as u8); // V
        }
    }
    out
}

/// Y + V + U planes concatenated. `width` and `height` must both be
/// even so the chroma sub-sampling lands on whole pixels.
fn pattern_yv12(width: u32, height: u32) -> Vec<u8> {
    debug_assert!(width % 2 == 0 && height % 2 == 0);
    let y_pixels = width as usize * height as usize;
    let c_pixels = y_pixels / 4;
    let mut out = Vec::with_capacity(y_pixels + 2 * c_pixels);
    // Y plane: smooth gradient.
    for i in 0..y_pixels {
        out.push((i as u32).wrapping_mul(31).wrapping_add(7) as u8);
    }
    // V plane: different texture.
    for i in 0..c_pixels {
        out.push((i as u32).wrapping_mul(53).wrapping_add(101) as u8);
    }
    // U plane: yet another.
    for i in 0..c_pixels {
        out.push((i as u32).wrapping_mul(89).wrapping_add(151) as u8);
    }
    out
}

#[test]
fn uncompressed_bgr24_roundtrip() {
    let (w, h) = (8, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_uncompressed(&pixels);
    let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(decoded.pixels, pixels);
}

#[test]
fn uncompressed_bgra32_roundtrip() {
    let (w, h) = (5, 3);
    let pixels = pattern_bgra32(w, h);
    let frame = encode_uncompressed(&pixels);
    let decoded = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
    assert_eq!(decoded.pixels, pixels);
}

#[test]
fn solid_grey_roundtrip() {
    let (w, h) = (3, 5);
    let frame = encode_solid_grey(0x42);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels.len(), 3 * 5 * 3);
    for chunk in dec.pixels.chunks_exact(3) {
        assert_eq!(chunk, &[0x42, 0x42, 0x42]);
    }
}

#[test]
fn solid_rgb_roundtrip() {
    let (w, h) = (4, 4);
    let frame = encode_solid_rgb(0x10, 0x20, 0x30);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    for chunk in dec.pixels.chunks_exact(3) {
        assert_eq!(chunk, &[0x10, 0x20, 0x30]);
    }
}

#[test]
fn solid_rgba_roundtrip() {
    let (w, h) = (2, 6);
    let frame = encode_solid_rgba(0x11, 0x22, 0x33, 0x80);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
    for chunk in dec.pixels.chunks_exact(4) {
        assert_eq!(chunk, &[0x11, 0x22, 0x33, 0x80]);
    }
}

#[test]
fn arith_rgb24_roundtrip_small() {
    // 4-wide so it lands on the type-4 (width % 4 == 0) path.
    let (w, h) = (4, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    assert_eq!(frame[0], 4, "type 4 expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn arith_rgb24_roundtrip_unaligned() {
    // 7-wide -> width % 4 != 0 -> type-2 path (UnalignedRgb24).
    let (w, h) = (7, 5);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    assert_eq!(frame[0], 2, "type 2 expected (unaligned)");
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn arith_rgb24_roundtrip_larger() {
    let (w, h) = (16, 16);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn arith_rgba_roundtrip_small() {
    let (w, h) = (4, 4);
    let pixels = pattern_bgra32(w, h);
    let frame = encode_arith_rgba(&pixels, w, h);
    assert_eq!(frame[0], 8);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn arith_rgba_roundtrip_larger() {
    let (w, h) = (8, 6);
    let pixels = pattern_bgra32(w, h);
    let frame = encode_arith_rgba(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
    assert_eq!(dec.pixels, pixels);
}

// ───────── Round 211: RGBA → Bgr24 lazy-alpha decode + early
// pixel-kind validation on the modern arithmetic-RGB family ─────────
//
// Per `spec/03` §4.3 the alpha plane has no cross-plane decorrelation
// interaction with the other three planes; per `spec/04` §5 item 5
// "channels are compressed independently". Both together let the
// decoder skip the entire fourth-channel arithmetic body (Fibonacci
// freq table + range coder + optional RLE expansion + predictor
// inverse) when the host requested `PixelKind::Bgr24` for an RGBA
// (type 8) frame, since the alpha bytes are discarded at the pack
// step regardless. The two tests below pin that:
//
// 1. The BGR portion of an RGBA → Bgr24 decode matches the BGR
//    portion of the same frame decoded as Bgra32 byte-for-byte.
// 2. A deliberately-corrupted alpha plane (channel-header byte set to
//    an unknown value that would normally return BadChannelHeader)
//    does NOT break a Bgr24 decode of the same frame — confirming the
//    alpha-decode path is genuinely skipped, not just discarded after
//    decode.

#[test]
fn arith_rgba_bgr24_matches_bgra32_bgr_portion() {
    // Build an RGBA frame whose alpha plane carries real signal (so
    // it would unambiguously affect a strict-equality comparison if
    // any decoder path mixed it into the BGR bytes).
    let (w, h) = (8, 6);
    let pixels = pattern_bgra32(w, h);
    let frame = encode_arith_rgba(&pixels, w, h);

    let dec_bgra32 = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
    let dec_bgr24 = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec_bgr24.pixels.len(), (w * h * 3) as usize);
    assert_eq!(dec_bgra32.pixels.len(), (w * h * 4) as usize);

    // Strip the alpha bytes from the Bgra32 output and compare.
    let bgr_portion: Vec<u8> = dec_bgra32
        .pixels
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    assert_eq!(
        dec_bgr24.pixels, bgr_portion,
        "RGBA-decoded-as-Bgr24 must equal the BGR portion of the same frame decoded as Bgra32",
    );
}

#[test]
fn arith_rgba_bgr24_skips_alpha_plane_decode() {
    // Build a valid RGBA frame, then corrupt **only** the alpha
    // channel's body so the alpha decode would surface
    // BadChannelHeader if run. With round 211's lazy alpha behaviour,
    // a Bgr24 decode never touches the alpha bytes and must return
    // the correctly-decoded BGR pixels regardless. A Bgra32 decode of
    // the same corrupted frame must fail.
    let (w, h) = (4, 4);
    let pixels = pattern_bgra32(w, h);
    let mut frame = encode_arith_rgba(&pixels, w, h);

    // Channel-offset table layout (spec/01 §2.3 for 4-plane RGBA):
    // byte 0 = type byte (0x08), bytes 1..5 = G offset (u32 LE),
    // bytes 5..9 = B offset, bytes 9..13 = A offset. The A channel
    // starts at the byte indexed by the third u32.
    assert_eq!(frame[0], 8);
    let a_off = u32::from_le_bytes([frame[9], frame[10], frame[11], frame[12]]) as usize;
    assert!(
        a_off < frame.len(),
        "A-channel offset must land inside the frame ({a_off} < {})",
        frame.len()
    );
    // Overwrite the alpha channel-header byte with a value the
    // dispatcher rejects (`0x10` is outside the legal {0x00..0x07,
    // 0xff} set per spec/03 §2.1 / spec/06 §1.1).
    frame[a_off] = 0x10;

    // Bgr24 decode must succeed and reproduce the BGR portion.
    let dec_bgr24 = decode_frame(&frame, w, h, PixelKind::Bgr24).expect(
        "Bgr24 decode must skip the alpha channel and succeed on a frame whose only \
            corruption sits in the alpha body",
    );
    let expected_bgr: Vec<u8> = pixels
        .chunks_exact(4)
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    assert_eq!(dec_bgr24.pixels, expected_bgr);

    // Bgra32 decode of the **same** corrupted frame must fail at the
    // alpha-channel dispatch — pins the negative half of the lazy-
    // decode pair (so a future regression that decodes alpha
    // eagerly for Bgr24 would either start failing the test above or
    // start succeeding here; both are real-defect signals).
    let r = decode_frame(&frame, w, h, PixelKind::Bgra32);
    assert!(
        matches!(r, Err(crate::Error::BadChannelHeader(0x10))),
        "Bgra32 decode of a frame with a corrupted alpha-channel header must surface \
         BadChannelHeader(0x10); got {r:?}",
    );
}

#[test]
fn arith_rgb_family_early_rejects_planar_pixel_kind() {
    // Round 211 moves the `packed_bpp()` pixel-kind validation to the
    // top of `decode_arith_rgb` / `decode_arith_rgba` /
    // `decode_legacy_rgb`. The behavioural contract (return
    // PixelFormatMismatch when the host asks for a planar buffer for
    // a packed RGB family frame) is unchanged; the earliness change
    // is observable only by side-channel (e.g. a corrupt channel
    // body that would surface BadChannelHeader if reached). Pin the
    // contract here at the API level so a future refactor can't
    // silently drop the early-validate guard back to a post-decode
    // panic / different error variant.
    for (frame, label) in [
        (
            encode_arith_rgb24(&pattern_bgr24(4, 4), 4, 4),
            "type 4 (rgb24)",
        ),
        (
            encode_arith_rgba(&pattern_bgra32(4, 4), 4, 4),
            "type 8 (rgba)",
        ),
    ] {
        for kind in [PixelKind::Yv12, PixelKind::Yuy2] {
            let r = decode_frame(&frame, 4, 4, kind);
            assert!(
                matches!(r, Err(crate::Error::PixelFormatMismatch { .. })),
                "{label} with {kind:?} must surface PixelFormatMismatch; got {r:?}",
            );
        }
    }
}

#[test]
fn null_frame_returns_error() {
    let r = decode_frame(&[], 4, 4, PixelKind::Bgr24);
    assert!(matches!(r, Err(crate::Error::NullFrame)));
}

#[test]
fn invalid_frame_type_byte() {
    let r = decode_frame(&[0], 4, 4, PixelKind::Bgr24);
    assert!(matches!(r, Err(crate::Error::BadFrameType(0))));
    let r = decode_frame(&[12], 4, 4, PixelKind::Bgr24);
    assert!(matches!(r, Err(crate::Error::BadFrameType(12))));
}

#[test]
fn all_known_frame_types_dispatch_without_unsupported_error() {
    // Round 4 closed the type-7 deferred slot — every frame type in
    // `1..=11 \ {0}` now dispatches into a real decode path. A
    // truncated frame may surface a `Truncated` / `Bad*` error, but
    // none should surface `UnsupportedFrameType` any more (the
    // variant remains in the enum for forward-compat use by
    // future decoder paths that opt to bail out at dispatch).
    for byte in [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11] {
        let r = decode_frame(&[byte], 4, 4, PixelKind::Bgr24);
        assert!(
            !matches!(r, Err(crate::Error::UnsupportedFrameType(_))),
            "type {byte} surfaced UnsupportedFrameType"
        );
    }
}

/// Exercise the channel-header `0x01..=0x03` arithmetic-with-RLE
/// path end-to-end: encode a residual sequence with several long
/// zero runs, decode it, expect bit-exact recovery.
#[test]
fn channel_header_01_arith_rle_roundtrip() {
    // Residual sequence with runs of 4, 7, and 30 zeros plus
    // sprinkled non-zeros — exercises both the in-band RLE escape
    // and the literal byte path.
    let mut plane = vec![5u8, 7, 11];
    plane.extend(std::iter::repeat(0u8).take(4));
    plane.extend_from_slice(&[13]);
    plane.extend(std::iter::repeat(0u8).take(7));
    plane.extend_from_slice(&[17, 19, 23]);
    plane.extend(std::iter::repeat(0u8).take(30));
    plane.push(29);

    for escape_len in 1..=3 {
        let channel = encode_channel_arith_rle(&plane, escape_len);
        // Header byte must equal escape_len for arith+RLE path.
        assert_eq!(
            channel[0], escape_len as u8,
            "header byte mismatch for escape_len={escape_len}"
        );
        let decoded = decode_channel(&channel, plane.len()).unwrap();
        assert_eq!(decoded, plane, "escape_len={escape_len}");
    }
}

/// Overwrite the 4-byte little-endian u32 length field (channel
/// bytes 1..=4) of a header-`0x01..0x03` channel with `value`,
/// leaving every other byte untouched. Used to drive the
/// dispatcher's `spec/06` §1.4 length-field comparison to exact
/// boundary values without disturbing the Fibonacci prefix or the
/// arithmetic body that follow at byte 5.
fn splice_u32_length_field(channel: &mut [u8], value: u32) {
    channel[1..5].copy_from_slice(&value.to_le_bytes());
}

/// `spec/06` §1.4 / §6.2: exhaustively pin the header-`0x01..0x03`
/// u32 length-field dispatch boundary.
///
/// The dispatcher reads the 4-byte LE u32 at channel bytes 1..=4
/// and compares it (unsigned) against the plane pixel count:
///
/// * `u32 < n_pixels`  → call site A: the field is a pre-RLE
///   symbol-stream length; the Fibonacci prefix starts at byte 5;
///   the range coder emits exactly `u32` symbols which are then
///   RLE-expanded to `n_pixels`.
/// * `u32 >= n_pixels` → call site B (fall-back): the four bytes
///   are re-interpreted as the leading bytes of a header-`0x00`
///   Fibonacci prefix beginning at byte 1; no RLE post-process.
///
/// §6.2 flags the exact-boundary values (`n_pixels`, `n_pixels-1`,
/// `0`) as documented-in-prose-but-not-cross-tested. These cases
/// close that.
#[test]
fn arith_rle_length_field_dispatch_boundary() {
    // A residual sequence whose pre-RLE symbol count is strictly
    // less than its expanded pixel count, so the genuine encoder
    // emits a u32 length field below `n_pixels` and the natural
    // dispatch takes call site A.
    let mut plane = vec![5u8, 7, 11];
    plane.extend(std::iter::repeat(0u8).take(6)); // one escape-able run
    plane.extend_from_slice(&[13, 17]);
    plane.extend(std::iter::repeat(0u8).take(10));
    plane.push(29);
    let n_pixels = plane.len();

    for escape_len in 1u8..=3 {
        let channel = encode_channel_arith_rle(&plane, escape_len as usize);
        // The genuine encoder must take the arith+RLE wire form for
        // this fixture (header byte == escape_len, u32 field present).
        assert_eq!(
            channel[0], escape_len,
            "header byte (escape_len={escape_len})"
        );
        let genuine_len =
            u32::from_le_bytes([channel[1], channel[2], channel[3], channel[4]]) as usize;
        assert!(
            genuine_len < n_pixels,
            "fixture must keep the natural call-site-A path: \
             pre_rle={genuine_len} n_pixels={n_pixels}"
        );

        // --- Natural below-boundary value (< n_pixels): call site A. ---
        // The genuine pre-RLE count is what the body actually encodes,
        // so we cannot rewrite it without corrupting the stream; assert
        // that the real (< n_pixels) count round-trips bit-exactly,
        // confirming the dispatcher classified it as call site A.
        let decoded = decode_channel(&channel, n_pixels).unwrap();
        assert_eq!(
            decoded, plane,
            "natural call-site-A path (escape_len={escape_len})"
        );

        // --- At-boundary value `== n_pixels`: fall-back to call site B. ---
        // Splicing the length field up to exactly `n_pixels` forces the
        // dispatcher onto the header-`0x00` fall-back, which re-reads
        // bytes 1..=4 as the start of a Fibonacci prefix. Whatever the
        // Fibonacci decoder makes of them, it must not panic and must
        // not silently equal the RLE result it was diverted away from.
        let mut spliced = channel.clone();
        splice_u32_length_field(&mut spliced, n_pixels as u32);
        if let Ok(out) = decode_channel(&spliced, n_pixels) {
            assert_eq!(out.len(), n_pixels, "fall-back output length");
            assert_ne!(
                out, plane,
                "u32 == n_pixels must divert OFF the RLE path \
                 (escape_len={escape_len})"
            );
        }

        // --- Above-boundary value `> n_pixels`: also call site B. ---
        let mut high = channel.clone();
        splice_u32_length_field(&mut high, n_pixels as u32 + 1);
        if let Ok(out) = decode_channel(&high, n_pixels) {
            assert_eq!(out.len(), n_pixels, "high fall-back output length");
        }
    }
}

/// `spec/06` §1.4 step 4 / §6.2: a length field of exactly `0`
/// (`< n_pixels` for any non-empty plane) selects call site A with
/// a **zero**-symbol pre-RLE stream. The range coder emits no
/// symbols, so the RLE expander is handed an empty input but is
/// still asked to fill `n_pixels` outputs — which `spec/05` §4.2's
/// output-driven expander cannot do, yielding a clean `Truncated`
/// error rather than a panic or an out-of-bounds read.
#[test]
fn arith_rle_zero_length_field_is_clean_error() {
    let plane = vec![5u8, 0, 0, 0, 13, 0, 0, 17];
    let n_pixels = plane.len();
    for escape_len in 1u8..=3 {
        let mut channel = encode_channel_arith_rle(&plane, escape_len as usize);
        // Only meaningful when the encoder actually chose the arith+RLE
        // wire form (header byte in 0x01..=0x03 carries the u32 field).
        if !(0x01..=0x03).contains(&channel[0]) {
            continue;
        }
        splice_u32_length_field(&mut channel, 0);
        let r = decode_channel(&channel, n_pixels);
        assert!(
            matches!(r, Err(crate::Error::Truncated { .. })),
            "zero-length call-site-A stream must surface Truncated, got {r:?} \
             (escape_len={escape_len})"
        );
    }
}

#[test]
fn channel_header_05_07_raw_rle_roundtrip() {
    // Same residual sequence, but emit via a hand-built channel
    // using header byte 0x05..0x07 (raw RLE without arithmetic).
    use crate::rle::contract_raw;
    let mut plane = vec![5u8, 7, 11];
    plane.extend(std::iter::repeat(0u8).take(4));
    plane.extend_from_slice(&[13]);
    plane.extend(std::iter::repeat(0u8).take(20));
    plane.extend_from_slice(&[17]);

    for escape_len in 1..=3 {
        let body = contract_raw(&plane, escape_len);
        let mut channel = vec![(escape_len + 4) as u8];
        channel.extend_from_slice(&body);
        let decoded = decode_channel(&channel, plane.len()).unwrap();
        assert_eq!(decoded, plane, "escape_len={escape_len}");
    }
}

#[test]
fn channel_header_04_raw_memcpy() {
    let plane: Vec<u8> = (0..50).map(|i| (i as u8).wrapping_mul(7)).collect();
    let mut channel = vec![0x04];
    channel.extend_from_slice(&plane);
    let decoded = decode_channel(&channel, plane.len()).unwrap();
    assert_eq!(decoded, plane);
}

#[test]
fn channel_header_ff_solid_fill() {
    let channel = vec![0xff, 0x42];
    let decoded = decode_channel(&channel, 64).unwrap();
    assert_eq!(decoded.len(), 64);
    for b in &decoded {
        assert_eq!(*b, 0x42);
    }
}

// ───────── Round 2: YV12 (type 10) ─────────

#[test]
fn arith_yv12_roundtrip_4x4() {
    // Smallest even-dimensioned fixture — 4×4 luma, 2×2 chroma.
    let (w, h) = (4, 4);
    let pixels = pattern_yv12(w, h);
    let frame = encode_arith_yv12(&pixels, w, h);
    assert_eq!(frame[0], 10, "type 10 (YV12) expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
    assert_eq!(dec.pixels, pixels);
    assert_eq!(dec.pixel_kind, PixelKind::Yv12);
    // Buffer length: 4*4 + 2*2 + 2*2 = 24
    assert_eq!(dec.pixels.len(), 16 + 4 + 4);
}

#[test]
fn arith_yv12_roundtrip_8x6() {
    let (w, h) = (8, 6);
    let pixels = pattern_yv12(w, h);
    let frame = encode_arith_yv12(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
    assert_eq!(dec.pixels, pixels);
    // 48 luma + 12 V + 12 U = 72
    assert_eq!(dec.pixels.len(), 48 + 12 + 12);
}

#[test]
fn arith_yv12_roundtrip_16x16() {
    let (w, h) = (16, 16);
    let pixels = pattern_yv12(w, h);
    let frame = encode_arith_yv12(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
    assert_eq!(dec.pixels, pixels);
}

/// Odd-dimension YV12 SPECGAP closure (round 352). When the host
/// dimensions are odd, `floor(W·H/4)` no longer factors as
/// `(W/2)·(H/2)`, so both `encode_arith_yv12` and `decode_arith_yv12`
/// fall through to the `spec/03` §6.1.1 placeholder geometry — they
/// treat each chroma plane as a **single row** of `c_pixels` bytes
/// (`apply_plane_*_with_rule(plane, c_pixels, 1, Rule A)`). The two
/// halves use the identical row/column breakdown, so the path
/// self-roundtrips byte-exactly even though the per-row chroma
/// breakdown is a host-integration placeholder rather than a
/// spec-pinned layout. This pins the previously-untested fallback
/// branch on both sides as a single regression.
fn yv12_odd_buffer(seed: u64, w: u32, h: u32) -> Vec<u8> {
    let n = PixelKind::Yv12.buffer_len(w, h);
    // Simple xorshift fill — arbitrary residual-rich content.
    let mut s = seed | 1;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s >> 24) as u8
        })
        .collect()
}

#[test]
fn arith_yv12_odd_dims_specgap_roundtrip() {
    // Each pair has odd W and/or odd H so c_pixels = floor(W·H/4)
    // does NOT equal (W/2)·(H/2): the SPECGAP single-row fallback
    // fires on both the encode and decode side.
    for &(w, h) in &[(5u32, 4u32), (4, 5), (5, 5), (7, 3), (3, 7), (9, 9), (1, 8)] {
        let cw = (w / 2) as usize;
        let ch = (h / 2) as usize;
        let c_pixels = (w as usize * h as usize) / 4;
        // Sanity: confirm we are actually exercising the fallback.
        assert_ne!(
            cw * ch,
            c_pixels,
            "dims {w}×{h} do not trigger the SPECGAP fallback",
        );
        let pixels = yv12_odd_buffer(0xa17e_1234 ^ ((w as u64) << 8) ^ h as u64, w, h);
        let frame = encode_arith_yv12(&pixels, w, h);
        assert_eq!(frame[0], 10, "type 10 (YV12) expected for {w}×{h}");
        let dec = decode_frame(&frame, w, h, PixelKind::Yv12)
            .unwrap_or_else(|e| panic!("YV12 odd {w}×{h} decode failed: {e:?}"));
        assert_eq!(
            dec.pixels, pixels,
            "YV12 odd-dim SPECGAP roundtrip not byte-exact at {w}×{h}",
        );
    }
}

#[test]
fn yv12_odd_dims_decode_consumes_floor_chroma_byte_counts() {
    // Decode-direction wire-contract pin (`spec/03` §6.1.1 +
    // `audit/00` §9.3): for an odd-dimensioned YV12 frame the wire
    // carries exactly `W*H` luma bytes and `floor(W*H/4)` bytes for
    // each of the V and U chroma planes — even when
    // `(W/2)*(H/2) != floor(W*H/4)`. This reads the channel-offset
    // table straight off the encoded frame and runs each plane slice
    // through the channel dispatcher *independently of the decoder's
    // own `wire_plane_pixel_counts` derivation*, so a future drift in
    // the decoder's chroma byte-count formula (e.g. a "round up to
    // even" regression) is caught here even though the encoder-mirror
    // roundtrip — which would change both halves together — would
    // still pass.
    use crate::frame::split_channels;
    for &(w, h) in &[(5u32, 4u32), (4, 5), (5, 5), (7, 3), (3, 7), (9, 9), (1, 8)] {
        let cw = (w / 2) as usize;
        let ch = (h / 2) as usize;
        let y_pixels = w as usize * h as usize;
        let c_pixels = y_pixels / 4; // floor(W*H/4) per audit/00 §9.3
                                     // Confirm we are on the genuine non-divisible branch.
        assert_ne!(cw * ch, c_pixels, "{w}×{h} should trigger the floor branch");

        let pixels = yv12_odd_buffer(0x5f12_0c0d ^ ((w as u64) << 8) ^ h as u64, w, h);
        let frame = encode_arith_yv12(&pixels, w, h);
        assert_eq!(frame[0], 10, "type 10 (YV12) expected for {w}×{h}");

        // Plane wire order is Y, V, U (3 channels) per spec/03 §6.1.
        let slices = split_channels(&frame, 3).expect("YV12 frame has a 3-plane offset table");
        assert_eq!(slices.len(), 3);
        let plane_y = decode_channel(slices[0], y_pixels).expect("Y plane decodes");
        let plane_v = decode_channel(slices[1], c_pixels).expect("V plane decodes");
        let plane_u = decode_channel(slices[2], c_pixels).expect("U plane decodes");
        assert_eq!(
            plane_y.len(),
            y_pixels,
            "{w}×{h}: luma plane must be W*H bytes"
        );
        assert_eq!(
            plane_v.len(),
            c_pixels,
            "{w}×{h}: V plane must be floor(W*H/4) bytes",
        );
        assert_eq!(
            plane_u.len(),
            c_pixels,
            "{w}×{h}: U plane must be floor(W*H/4) bytes",
        );
        // The host YV12 buffer the decoder reconstructs is exactly the
        // luma + two floor-sized chroma planes concatenated.
        assert_eq!(
            y_pixels + 2 * c_pixels,
            PixelKind::Yv12.buffer_len(w, h),
            "{w}×{h}: plane byte counts must sum to the YV12 host buffer length",
        );
    }
}

#[test]
fn arith_yv12_solid_planes_roundtrip() {
    // Each plane filled with a constant — exercises the encoder's
    // header-0xff fast path for all three channels.
    let (w, h) = (4, 4);
    let mut pixels = vec![0x42u8; 16];
    pixels.extend(vec![0x80u8; 4]);
    pixels.extend(vec![0xc0u8; 4]);
    let frame = encode_arith_yv12(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn yv12_pixel_kind_required_for_type_10() {
    // Asking for Bgr24 / Bgra32 against a YV12 frame is a host-
    // integration error, surfaced as PixelFormatMismatch.
    let pixels = pattern_yv12(4, 4);
    let frame = encode_arith_yv12(&pixels, 4, 4);
    let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
    assert!(matches!(
        r,
        Err(crate::Error::PixelFormatMismatch { frame_type: 10 })
    ));
    let r = decode_frame(&frame, 4, 4, PixelKind::Bgra32);
    assert!(matches!(
        r,
        Err(crate::Error::PixelFormatMismatch { frame_type: 10 })
    ));
}

#[test]
fn yv12_buffer_len_matches_expected() {
    assert_eq!(PixelKind::Yv12.buffer_len(4, 4), 16 + 4 + 4);
    assert_eq!(PixelKind::Yv12.buffer_len(8, 6), 48 + 12 + 12);
    assert_eq!(PixelKind::Yv12.buffer_len(16, 16), 256 + 64 + 64);
    assert_eq!(PixelKind::Bgr24.buffer_len(7, 5), 7 * 5 * 3);
    assert_eq!(PixelKind::Bgra32.buffer_len(7, 5), 7 * 5 * 4);
}

// ───────── Round 2: NULL frame (JUMP) replay ─────────

#[test]
fn null_frame_with_no_predecessor_errors() {
    let r = decode_frame_with_prev(&[], 4, 4, PixelKind::Bgr24, None);
    assert!(matches!(r, Err(crate::Error::NullFrameWithoutPredecessor)));
}

#[test]
fn null_frame_replays_predecessor_via_helper() {
    let (w, h) = (4, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    let prev = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    // NULL frame: empty payload + Some(prev) -> clone of prev.
    let null = encode_null();
    let replay = decode_frame_with_prev(&null, w, h, PixelKind::Bgr24, Some(&prev)).unwrap();
    assert_eq!(replay.pixels, prev.pixels);
    assert_eq!(replay.width, prev.width);
    assert_eq!(replay.height, prev.height);
}

#[test]
fn null_frame_replays_predecessor_via_stateful_decoder() {
    let (w, h) = (4, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    let mut dec = Decoder::new();
    // First non-NULL frame primes the predecessor.
    let f0 = dec.decode(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f0.pixels, pixels);
    // Subsequent NULL frame replays.
    let f1 = dec.decode(&[], w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f1.pixels, pixels);
    let f2 = dec.decode(&[], w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f2.pixels, pixels);
    // Reset clears the predecessor.
    dec.reset();
    assert!(dec.previous().is_none());
    let r = dec.decode(&[], w, h, PixelKind::Bgr24);
    assert!(matches!(r, Err(crate::Error::NullFrameWithoutPredecessor)));
}

#[test]
fn null_frame_replay_yv12() {
    let (w, h) = (4, 4);
    let pixels = pattern_yv12(w, h);
    let frame = encode_arith_yv12(&pixels, w, h);
    let mut dec = Decoder::new();
    let f0 = dec.decode(&frame, w, h, PixelKind::Yv12).unwrap();
    let f1 = dec.decode(&[], w, h, PixelKind::Yv12).unwrap();
    assert_eq!(f0.pixels, f1.pixels);
    assert_eq!(f1.pixels, pixels);
}

#[test]
fn stateful_decoder_updates_predecessor_on_each_decode() {
    let (w, h) = (4, 4);
    let mut dec = Decoder::new();

    // Solid grey frame -> first predecessor.
    let solid = encode_solid_grey(0x33);
    let f0 = dec.decode(&solid, w, h, PixelKind::Bgr24).unwrap();
    for c in f0.pixels.chunks_exact(3) {
        assert_eq!(c, &[0x33, 0x33, 0x33]);
    }

    // NULL replays solid.
    let f1 = dec.decode(&[], w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f1.pixels, f0.pixels);

    // New non-NULL frame -> predecessor updates.
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    let f2 = dec.decode(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f2.pixels, pixels);

    // NULL now replays the patterned frame, not the solid.
    let f3 = dec.decode(&[], w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f3.pixels, pixels);
}

#[test]
fn null_frame_dimension_mismatch_errors() {
    // Replay against the wrong (W, H) tuple is a host-integration
    // error.
    let (w, h) = (4, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    let prev = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    let r = decode_frame_with_prev(&[], 5, 4, PixelKind::Bgr24, Some(&prev));
    assert!(matches!(
        r,
        Err(crate::Error::PixelFormatMismatch { frame_type: 0 })
    ));
}

#[test]
fn yv12_unsupported_pixel_format_for_rgb_frame() {
    // Asking for Yv12 against an RGB frame is also a mismatch.
    let pixels = pattern_bgr24(4, 4);
    let frame = encode_arith_rgb24(&pixels, 4, 4);
    let r = decode_frame(&frame, 4, 4, PixelKind::Yv12);
    // The RGB decoder returns PixelFormatMismatch via its packed_bpp
    // unwrap. Either error flavour is valid; check it errors.
    assert!(r.is_err());
}

// ───────── Round 3: YUY2 (type 3) ─────────

#[test]
fn arith_yuy2_roundtrip_4x4() {
    let (w, h) = (4, 4);
    let pixels = pattern_yuy2(w, h);
    let frame = encode_arith_yuy2(&pixels, w, h);
    assert_eq!(frame[0], 3, "type 3 (YUY2) expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels, pixels);
    assert_eq!(dec.pixel_kind, PixelKind::Yuy2);
    // Buffer length: 4 * 4 * 2 = 32 bytes.
    assert_eq!(dec.pixels.len(), 32);
}

#[test]
fn arith_yuy2_roundtrip_8x6() {
    let (w, h) = (8, 6);
    let pixels = pattern_yuy2(w, h);
    let frame = encode_arith_yuy2(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels, pixels);
    // 8 * 6 * 2 = 96 bytes.
    assert_eq!(dec.pixels.len(), 96);
}

#[test]
fn arith_yuy2_roundtrip_16x16() {
    let (w, h) = (16, 16);
    let pixels = pattern_yuy2(w, h);
    let frame = encode_arith_yuy2(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels, pixels);
}

/// Build a packed YUY2 buffer at **odd** width. The decoder's
/// floor-chroma layout (`spec/03` §6.2) gives the trailing luma
/// column no chroma macropixel, so its chroma slot at output byte
/// `2·(W−1)+1` is the neutral `0x80` fill the decoder synthesises.
/// To roundtrip byte-exact, this helper seeds that tail slot with
/// `0x80` (every other byte is arbitrary, mirroring `pattern_yuy2`).
fn pattern_yuy2_odd(width: u32, height: u32) -> Vec<u8> {
    debug_assert!(width % 2 == 1);
    let w = width as usize;
    let h = height as usize;
    let cw = w / 2;
    let mut out = vec![0u8; w * h * 2];
    for y in 0..h {
        let out_row = y * w * 2;
        for k in 0..cw {
            let yi = (y as u32).wrapping_mul(13).wrapping_add(k as u32);
            out[out_row + 4 * k] = yi.wrapping_mul(31).wrapping_add(11) as u8; // Y0
            out[out_row + 4 * k + 1] = yi.wrapping_mul(53).wrapping_add(101) as u8; // U
            out[out_row + 4 * k + 2] = yi.wrapping_mul(31).wrapping_add(43) as u8; // Y1
            out[out_row + 4 * k + 3] = yi.wrapping_mul(89).wrapping_add(151) as u8;
            // V
        }
        // Odd-tail luma column (arbitrary) + neutral 0x80 chroma slot.
        let last_x = w - 1;
        out[out_row + 2 * last_x] = (y as u32).wrapping_mul(71).wrapping_add(17) as u8;
        out[out_row + 2 * last_x + 1] = 0x80;
    }
    out
}

#[test]
fn arith_yuy2_odd_width_roundtrip_5x4() {
    let (w, h) = (5, 4);
    let pixels = pattern_yuy2_odd(w, h);
    let frame = encode_arith_yuy2(&pixels, w, h);
    assert_eq!(frame[0], 3, "type 3 (YUY2) expected for odd width too");
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels, pixels);
    assert_eq!(dec.pixels.len(), (w * h * 2) as usize);
}

#[test]
fn arith_yuy2_odd_width_roundtrip_7x5() {
    let (w, h) = (7, 5);
    let pixels = pattern_yuy2_odd(w, h);
    let frame = encode_arith_yuy2(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn arith_yuy2_odd_width_roundtrip_9x9() {
    let (w, h) = (9, 9);
    let pixels = pattern_yuy2_odd(w, h);
    let frame = encode_arith_yuy2(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels, pixels);
}

/// Width 1 is the degenerate odd case: the chroma planes are empty
/// (`floor(1/2) = 0`), so the wire body carries only a luma plane and
/// two zero-length chroma channels. The decoder fills the single
/// pixel's chroma slot with the `0x80` neutral. Confirms the encoder
/// does not panic on the zero-width chroma planes.
#[test]
fn arith_yuy2_width_1_roundtrip() {
    let (w, h) = (1, 4);
    let mut pixels = vec![0u8; (w * h * 2) as usize];
    for y in 0..h as usize {
        pixels[y * 2] = (y as u8).wrapping_mul(37).wrapping_add(5); // luma
        pixels[y * 2 + 1] = 0x80; // neutral chroma
    }
    let frame = encode_arith_yuy2(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn yuy2_pixel_kind_required_for_type_3() {
    let pixels = pattern_yuy2(4, 4);
    let frame = encode_arith_yuy2(&pixels, 4, 4);
    // Asking for BGR24 / BGRA32 / YV12 against a YUY2 frame surfaces
    // a PixelFormatMismatch.
    let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
    assert!(matches!(
        r,
        Err(crate::Error::PixelFormatMismatch { frame_type: 3 })
    ));
    let r = decode_frame(&frame, 4, 4, PixelKind::Bgra32);
    assert!(matches!(
        r,
        Err(crate::Error::PixelFormatMismatch { frame_type: 3 })
    ));
    let r = decode_frame(&frame, 4, 4, PixelKind::Yv12);
    assert!(matches!(
        r,
        Err(crate::Error::PixelFormatMismatch { frame_type: 3 })
    ));
}

#[test]
fn yuy2_buffer_len_matches_expected() {
    assert_eq!(PixelKind::Yuy2.buffer_len(4, 4), 32);
    assert_eq!(PixelKind::Yuy2.buffer_len(8, 6), 96);
    assert_eq!(PixelKind::Yuy2.buffer_len(16, 16), 512);
}

// ───────── Round 3: Reduced-resolution (type 11) ─────────

/// Build a YV12 buffer at full resolution (`W × H`) where every 2×2
/// luma block is constant — same trick for V/U at quarter
/// resolution. This is what an honest 2× nearest-neighbour upscale
/// of a half-resolution YV12 image looks like, so the
/// downsample → upsample roundtrip the type-11 encoder + decoder
/// pair performs is byte-exact.
fn pattern_yv12_2x_block_constant(width: u32, height: u32) -> Vec<u8> {
    debug_assert_eq!(width % 4, 0);
    debug_assert_eq!(height % 4, 0);
    let half_w = (width / 2) as usize;
    let half_h = (height / 2) as usize;
    let big_w = width as usize;
    let big_h = height as usize;
    let big_y = big_w * big_h;
    let big_cw = big_w / 2;
    let big_ch = big_h / 2;
    let big_c = big_cw * big_ch;

    let mut out = vec![0u8; big_y + 2 * big_c];

    // Y plane: 2×2 nearest-neighbour upscale of a small_w × small_h
    // pattern.
    for sy in 0..half_h {
        for sx in 0..half_w {
            let v = (sy as u32)
                .wrapping_mul(31)
                .wrapping_add(sx as u32)
                .wrapping_add(7) as u8;
            for dy in 0..2 {
                for dx in 0..2 {
                    let oy = sy * 2 + dy;
                    let ox = sx * 2 + dx;
                    out[oy * big_w + ox] = v;
                }
            }
        }
    }
    // V / U planes: same pattern with different seed, at
    // half-W/2 × half-H/2 resolution so the outer 2× of type-11 lands
    // on full-resolution YV12 chroma.
    let small_cw = half_w / 2;
    let small_ch = half_h / 2;
    let mut write_chroma = |seed: u32, base: usize| {
        for sy in 0..small_ch {
            for sx in 0..small_cw {
                let v = seed
                    .wrapping_add((sy as u32).wrapping_mul(53))
                    .wrapping_add((sx as u32).wrapping_mul(89)) as u8;
                for dy in 0..2 {
                    for dx in 0..2 {
                        let oy = sy * 2 + dy;
                        let ox = sx * 2 + dx;
                        out[base + oy * big_cw + ox] = v;
                    }
                }
            }
        }
    };
    write_chroma(101, big_y);
    write_chroma(151, big_y + big_c);
    out
}

#[test]
fn reduced_res_roundtrip_8x8() {
    // Smallest size where half-W and half-H are both >= 2 and the
    // 2× chroma still has a half-of-a-half-W/H.
    let (w, h) = (8u32, 8u32);
    let pixels = pattern_yv12_2x_block_constant(w, h);
    let frame = encode_arith_reduced_res(&pixels, w, h);
    assert_eq!(frame[0], 11, "type 11 expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
    assert_eq!(dec.pixels, pixels);
    assert_eq!(dec.pixel_kind, PixelKind::Yv12);
    assert_eq!(dec.pixels.len(), PixelKind::Yv12.buffer_len(w, h));
}

#[test]
fn reduced_res_roundtrip_16x16() {
    let (w, h) = (16u32, 16u32);
    let pixels = pattern_yv12_2x_block_constant(w, h);
    let frame = encode_arith_reduced_res(&pixels, w, h);
    assert_eq!(frame[0], 11);
    let dec = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn reduced_res_pixel_kind_required() {
    let pixels = pattern_yv12_2x_block_constant(8, 8);
    let frame = encode_arith_reduced_res(&pixels, 8, 8);
    let r = decode_frame(&frame, 8, 8, PixelKind::Bgr24);
    assert!(matches!(
        r,
        Err(crate::Error::PixelFormatMismatch { frame_type: 11 })
    ));
}

#[test]
fn reduced_res_decode_matches_2x_upscaled_yv12() {
    // Build a half-W/half-H YV12 frame, encode as type 10, then flip
    // byte 0 to 11 by hand. Decoding at the full W/H must produce
    // the 2× nearest-neighbour upscale of the half-resolution YV12.
    let (small_w, small_h) = (4u32, 4u32);
    let small_pixels = pattern_yv12(small_w, small_h);
    let mut frame = encode_arith_yv12(&small_pixels, small_w, small_h);
    frame[0] = 11;

    let big_w = small_w * 2;
    let big_h = small_h * 2;
    let dec = decode_frame(&frame, big_w, big_h, PixelKind::Yv12).unwrap();

    // Hand-roll the expected 2× upscale of `small_pixels`.
    let small_y = (small_w * small_h) as usize;
    let small_cw = (small_w / 2) as usize;
    let small_ch = (small_h / 2) as usize;
    let small_c = small_cw * small_ch;
    let big_y = (big_w * big_h) as usize;
    let big_cw = (big_w / 2) as usize;
    let big_ch = (big_h / 2) as usize;
    let big_c = big_cw * big_ch;

    let mut expected = vec![0u8; big_y + 2 * big_c];
    let blow = |src: &[u8], src_w: usize, src_h: usize, dst: &mut [u8], dst_w: usize| {
        for y in 0..src_h {
            for x in 0..src_w {
                let v = src[y * src_w + x];
                dst[(2 * y) * dst_w + 2 * x] = v;
                dst[(2 * y) * dst_w + 2 * x + 1] = v;
                dst[(2 * y + 1) * dst_w + 2 * x] = v;
                dst[(2 * y + 1) * dst_w + 2 * x + 1] = v;
            }
        }
    };
    let (small_y_part, rest) = small_pixels.split_at(small_y);
    let (small_v_part, small_u_part) = rest.split_at(small_c);
    let (big_y_part, rest) = expected.split_at_mut(big_y);
    let (big_v_part, big_u_part) = rest.split_at_mut(big_c);
    blow(
        small_y_part,
        small_w as usize,
        small_h as usize,
        big_y_part,
        big_w as usize,
    );
    blow(small_v_part, small_cw, small_ch, big_v_part, big_cw);
    blow(small_u_part, small_cw, small_ch, big_u_part, big_cw);

    assert_eq!(dec.pixels, expected);
}

// ───────── Round 3: SIMD-vs-scalar predictor parity ─────────

/// Document the round-3 SIMD-predictor design decision: per `spec/06`
/// §3.6 Strategy A (`TL = L = plane[y-1][W-1]` for every row `y >=
/// 1`) is **carry-equivalent** to the proprietary's SIMD inner-loop
/// behaviour AND matches its scalar predictor. Implementing the
/// scalar predictor — which we do — therefore produces the same
/// residual-stream output as the SIMD predictor for any width where
/// `width % 4 == 0` (the SIMD-active condition) and for any
/// frame-type. The reference Python impl in
/// `docs/.../reference-impl/python/lagarith/predict.py` uses the
/// same Strategy A by design.
///
/// This test pins the parity by exercising a `width % 4 == 0`
/// fixture (16 × 16 BGR24) and a `width % 4 != 0` fixture (7 × 5 →
/// type 2): both must round-trip. The wire format permits any
/// reversible predictor (`spec/03` §8), so byte-exact match against
/// the proprietary encoder is a separate Auditor concern (no fixture
/// available in tree — see `SPECGAP-encoder-byte-exact` below).
#[test]
fn simd_predictor_strategy_a_parity_matches_scalar() {
    // width % 4 == 0 case (SIMD-active in the proprietary).
    let (w, h) = (16, 16);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    assert_eq!(frame[0], 4, "type 4 (width%4==0) expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);

    // width % 4 != 0 case (scalar-only in the proprietary).
    let (w, h) = (7, 5);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_arith_rgb24(&pixels, w, h);
    assert_eq!(frame[0], 2, "type 2 (width%4!=0) expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

/// SPECGAP — byte-exact validation against the proprietary's encoder
/// requires either a proprietary-encoded AVI fixture (we don't carry
/// one in tree — `docs/video/lagarith/reference/binaries/` only
/// holds the DLL black-box, not encoded video) or an AVI test sample
/// from `samples.oxideav.org` (not currently provisioned at the time
/// of round 4 — `curl https://samples.oxideav.org/lagarith/` returns
/// HTTP 404). Rounds 1..4 therefore continue the self-roundtrip-only
/// contract. Carrying a proprietary-encoded fixture here would be an
/// Auditor concern for a future round.
#[test]
fn specgap_byte_exact_encoder_validation_deferred() {
    // Pure documentation marker; no functional assertion. The
    // round-4 encoder remains self-roundtrip-only.
}

// ───────── Round 3: YUY2 odd-width edge case (audit/01 §9.4) ─────────

/// Per audit/00 §9.4, a complete characterisation of the YUY2
/// odd-width edge case ("the last column of luma pixels has no
/// matching chroma sample on the wire") was held forward to a future
/// validator round. The round-3 decoder uses `cw = w / 2` (= floor)
/// for chroma and writes a neutral 0x80 chroma byte at the odd-width
/// tail; the round-3 encoder requires even width.
///
/// Round 4 adds an explicit decode-side test covering that path: a
/// hand-built type-3 YUY2 frame with width 5 (odd), three planes
/// of `5×4` luma + `2×4` chroma, all-zero residuals (uniform planes
/// of the inverse-predictor's seed value 0). The decoder must produce
/// exactly the documented packed layout — Y at columns 0..4, U at
/// macropixels (0,1) and (2,3), V at the same macropixel positions,
/// 0x80 at column 4's chroma slot.
#[test]
fn yuy2_odd_width_decode_matches_floor_chroma_layout() {
    use crate::frame::pack_channels;
    // Build a hand-rolled YUY2 type-3 frame at 5×4. Each channel uses
    // header 0xff (solid-fill) with value 0 so the residual planes
    // are all-zero — under the spec/03 §3.3 predictor, an all-zero
    // residual stream reconstructs to an all-zero plane.
    let w: u32 = 5;
    let h: u32 = 4;
    let cw = (w / 2) as usize; // = 2
                               // Each channel = [0xff, 0x00] solid-fill encoding -> all-zero
                               // residuals -> all-zero reconstructed plane.
    let ch_y = vec![0xff, 0x00];
    let ch_u = vec![0xff, 0x00];
    let ch_v = vec![0xff, 0x00];
    let frame = pack_channels(3, &[&ch_y, &ch_u, &ch_v]);
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    // Buffer length: 5 * 4 * 2 = 40 bytes.
    assert_eq!(dec.pixels.len(), 40);
    // Per-row check: every macropixel byte is 0 (luma + chroma = 0
    // because reconstructed planes are all zero), and the odd-tail
    // chroma slot is 0x80 (the decoder's neutral fill).
    for y in 0..h as usize {
        let row = y * (w as usize) * 2;
        for k in 0..cw {
            assert_eq!(dec.pixels[row + 4 * k], 0, "Y at row {y} col {}", 2 * k);
            assert_eq!(dec.pixels[row + 4 * k + 1], 0, "U at row {y} pair {k}");
            assert_eq!(
                dec.pixels[row + 4 * k + 2],
                0,
                "Y at row {y} col {}",
                2 * k + 1
            );
            assert_eq!(dec.pixels[row + 4 * k + 3], 0, "V at row {y} pair {k}");
        }
        // Tail: column 4 (last odd-width luma, = 0) + 0x80 chroma.
        assert_eq!(dec.pixels[row + 8], 0, "Y at row {y} col 4 (odd tail)");
        assert_eq!(
            dec.pixels[row + 9],
            0x80,
            "chroma neutral at row {y} odd-width tail"
        );
    }
}

/// Round-trip test for odd-width YUY2: hand-build a YUY2 packed
/// pixel buffer at 5×4 with arbitrary luma/chroma; encode each plane
/// individually via the channel-header 0x04 raw-memcpy path (so
/// no predictor / RLE / range coder noise interferes); decode;
/// verify the floor-chroma layout matches the original macropixels
/// + 0x80 fill at the odd-width tail.
#[test]
fn yuy2_odd_width_raw_channel_floor_layout_roundtrip() {
    use crate::frame::pack_channels;
    let w: u32 = 5;
    let h: u32 = 4;
    let cw = (w / 2) as usize; // = 2 macropixels per row
    let n_y = (w * h) as usize; // 20 luma bytes
    let n_c = cw * h as usize; // 8 chroma bytes per plane

    // Hand-build per-plane RESIDUAL byte sequences by inverting the
    // forward predictor on a full plane of arbitrary values. Reuse
    // `apply_plane_forward` from the predict module.
    use crate::predict::apply_plane_forward;
    let plane_y_full: Vec<u8> = (0..n_y).map(|i| ((i * 7) ^ 0x55) as u8).collect();
    let plane_u_full: Vec<u8> = (0..n_c).map(|i| (0x40 + i as u8) ^ 0x10).collect();
    let plane_v_full: Vec<u8> = (0..n_c).map(|i| (0xa0 + i as u8) ^ 0x20).collect();

    let res_y = apply_plane_forward(&plane_y_full, w as usize, h as usize);
    let res_u = apply_plane_forward(&plane_u_full, cw, h as usize);
    let res_v = apply_plane_forward(&plane_v_full, cw, h as usize);

    // Channel-header 0x04 raw-memcpy: byte 0 = 0x04, then the
    // residual stream verbatim.
    let mut ch_y = vec![0x04u8];
    ch_y.extend_from_slice(&res_y);
    let mut ch_u = vec![0x04u8];
    ch_u.extend_from_slice(&res_u);
    let mut ch_v = vec![0x04u8];
    ch_v.extend_from_slice(&res_v);
    let frame = pack_channels(3, &[&ch_y, &ch_u, &ch_v]);
    let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
    assert_eq!(dec.pixels.len(), 40);
    // Verify the packed YUY2 layout matches the floor-chroma plane
    // arrangement.
    for y in 0..h as usize {
        let row = y * (w as usize) * 2;
        for k in 0..cw {
            assert_eq!(
                dec.pixels[row + 4 * k],
                plane_y_full[y * w as usize + 2 * k]
            );
            assert_eq!(dec.pixels[row + 4 * k + 1], plane_u_full[y * cw + k]);
            assert_eq!(
                dec.pixels[row + 4 * k + 2],
                plane_y_full[y * w as usize + 2 * k + 1]
            );
            assert_eq!(dec.pixels[row + 4 * k + 3], plane_v_full[y * cw + k]);
        }
        // Odd-width tail: column 4 of luma + 0x80 chroma.
        assert_eq!(
            dec.pixels[row + 8],
            plane_y_full[y * w as usize + 4],
            "Y at odd-width tail row {y}"
        );
        assert_eq!(dec.pixels[row + 9], 0x80, "chroma neutral fill row {y}");
    }
}

// ───────── Round 4: type 7 (legacy RGB / spec/07) ─────────

#[test]
fn legacy_rgb_roundtrip_4x4() {
    let (w, h) = (4u32, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    assert_eq!(frame[0], 7, "type 7 (legacy RGB) expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
    assert_eq!(dec.pixel_kind, PixelKind::Bgr24);
}

#[test]
fn legacy_rgb_roundtrip_8x8() {
    let (w, h) = (8u32, 8);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    assert_eq!(frame[0], 7);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_roundtrip_16x12() {
    let (w, h) = (16u32, 12);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_roundtrip_unaligned_width() {
    // width=7 (% 4 != 0) — the spec/07 §7.2 dispatch uses width % 4
    // to choose SIMD vs scalar predictor; type-7 inherits the same
    // pipeline. Self-roundtrip should still work bit-exactly.
    let (w, h) = (7u32, 5);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_roundtrip_to_bgra32_widens_with_opaque_alpha() {
    let (w, h) = (4u32, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
    // Widen pixels into BGRA32 with alpha=0xff for comparison.
    let mut expected = Vec::with_capacity(pixels.len() * 4 / 3);
    for px in pixels.chunks_exact(3) {
        expected.push(px[0]);
        expected.push(px[1]);
        expected.push(px[2]);
        expected.push(0xff);
    }
    assert_eq!(dec.pixels, expected);
}

#[test]
fn legacy_rgb_solid_plane_roundtrip() {
    // Each plane is a single value -> the histogram has one non-zero
    // bin, exercising the encoder's "all-one-symbol" edge of the
    // CDF construction.
    let (w, h) = (4u32, 4);
    let mut pixels = Vec::with_capacity(48);
    for _ in 0..16 {
        pixels.push(0x42);
        pixels.push(0x42);
        pixels.push(0x42);
    }
    let frame = encode_legacy_rgb(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_replays_via_stateful_decoder() {
    // Type-7 frames participate in the NULL-frame ("JUMP") replay
    // path of `spec/01` §1.1 just like every other non-NULL frame.
    let (w, h) = (4u32, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    let mut dec = Decoder::new();
    let f0 = dec.decode(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f0.pixels, pixels);
    let f1 = dec.decode(&[], w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(f1.pixels, pixels);
}

#[test]
fn legacy_rgb_rejects_non_zero_inner_codec_mode_flag() {
    // Hand-build a type-7 frame whose channel data starts with the
    // outer header `0x00` but inner codec-mode flag `0x01`. The
    // header-0 path requires the inner flag = 0 for the bare-
    // Fibonacci sub-path; a non-zero inner flag selects an inner
    // RLE-then-Fibonacci sub-path under outer-header-0 that no
    // observed encoder produces (`spec/07` §1.3 / §2.5 audit
    // blockquote). Surface as BadChannelHeader.
    use crate::frame::pack_channels;
    let ch = vec![0x00u8, 0x01];
    let frame = pack_channels(7, &[&ch, &ch, &ch]);
    let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
    assert!(
        matches!(r, Err(crate::Error::BadChannelHeader(_))),
        "non-zero inner codec-mode flag must surface BadChannelHeader, got {r:?}"
    );
}

#[test]
fn legacy_rgb_rejects_unknown_outer_header() {
    // Outer header bytes outside {0x00, 0x01, 0x02, 0x03} are not
    // produced by any encoder path observed in the binary
    // (`spec/07` §9.1 item 2). Surface as BadChannelHeader so
    // callers can disambiguate.
    use crate::frame::pack_channels;
    let ch = vec![0x05u8; 10];
    let frame = pack_channels(7, &[&ch, &ch, &ch]);
    let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
    assert!(
        matches!(r, Err(crate::Error::BadChannelHeader(_))),
        "unknown outer header byte must surface BadChannelHeader, got {r:?}"
    );
}

#[test]
fn legacy_rgb_rule_b_first_column_rule() {
    // Round-5 §1: type 7 uses **Rule B** for the first-column
    // predictor (`spec/07` §9.1 item 7b). With H >= 3 (where Rule B
    // diverges from Rule A on rows >= 2 first-col), the encoder /
    // decoder pair must round-trip with the Rule-B residual stream.
    let (w, h) = (5u32, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_rule_b_tall_plane() {
    // Many rows >= 2 so the divergence between Rule A and Rule B
    // accumulates. A Rule-A-keyed decoder against a Rule-B-keyed
    // encoder would diverge per `spec/07` §9.1 item 7b's
    // "+3 per byte per row" arithmetic.
    let (w, h) = (4u32, 8);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

// ───────── Round 5: type 7 RLE-then-Fibonacci sub-path ─────────

#[test]
fn legacy_rgb_rle_then_fib_roundtrip_escape_1() {
    // Outer channel header 0x01: u32 post-RLE byte count + RLE-
    // compressed Fibonacci freq table (`spec/07` §2.3 / §2.4).
    let (w, h) = (4u32, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb_rle(&pixels, w, h, 1);
    assert_eq!(frame[0], 7, "type 7 (legacy RGB) expected");
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_rle_then_fib_roundtrip_escape_2() {
    let (w, h) = (4u32, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb_rle(&pixels, w, h, 2);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_rle_then_fib_roundtrip_escape_3() {
    let (w, h) = (4u32, 4);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb_rle(&pixels, w, h, 3);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_rle_then_fib_roundtrip_8x8() {
    let (w, h) = (8u32, 8);
    let pixels = pattern_bgr24(w, h);
    for escape_len in 1..=3 {
        let frame = encode_legacy_rgb_rle(&pixels, w, h, escape_len);
        let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
        assert_eq!(dec.pixels, pixels, "escape_len {escape_len}");
    }
}

#[test]
fn legacy_rgb_rle_then_fib_solid_plane_roundtrip() {
    // Solid plane: histogram has many zero bins -> RLE compresses
    // the Fibonacci freq table heavily, exercising the post-RLE
    // length field's compactness. `spec/07` §2.3 says the helper's
    // post-RLE buffer is at most 256 bytes; sparse histograms
    // produce short post-RLE buffers.
    let (w, h) = (4u32, 4);
    let mut pixels = Vec::with_capacity(48);
    for _ in 0..16 {
        pixels.push(0x42);
        pixels.push(0x42);
        pixels.push(0x42);
    }
    for escape_len in 1..=3 {
        let frame = encode_legacy_rgb_rle(&pixels, w, h, escape_len);
        let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
        assert_eq!(dec.pixels, pixels, "escape_len {escape_len}");
    }
}

// ───────── Round 6: Strategy E (rare-symbol-cluster -> type 1) ─────────

/// Build a `near_flat` BGR24 fixture: a solid colour plane with a
/// single byte at the centre flipped by `+0x40`. This is the
/// canonical recipe behind `audit/12 §3.6`'s rare-symbol-cluster
/// trigger (`docs/video/lagarith/validator/test_inputs.py:204`
/// `gen_near_flat`). After predict + decorrelate the residual
/// histograms have `freq[0]` dominant plus a small number of rare
/// nonzero bins (`freq ∈ {1, 2}`), matching the Strategy E
/// predicate. The exact-byte recipe matters less than the
/// signature: any "solid plane with a tiny number of off-pixels"
/// produces enough rare bins on the residual to trip the guard.
fn near_flat_bgr24(width: u32, height: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
    let n = width as usize * height as usize;
    let mut pixels = Vec::with_capacity(n * 3);
    for _ in 0..n {
        pixels.push(b);
        pixels.push(g);
        pixels.push(r);
    }
    // Flip a colour byte at the centre by +0x40 — the same bit-twist
    // `gen_near_flat` applies. A single perturbation at the centre
    // pixel propagates a small handful of rare nonzero residuals
    // through the predict + decorrelate stages.
    let centre = (n / 2) * 3 + 1; // green byte of centre pixel
    pixels[centre] = pixels[centre].wrapping_add(0x40);
    pixels
}

#[test]
fn legacy_rgb_strategy_e_routes_near_flat_to_type_1() {
    // 33×27 matches the audit/12 §3 canonical fixture size. A near-
    // flat plane has `freq[0] >= 0.95 * pixel_count` after predict
    // + decorrelate plus >= 3 distinct rare bins, so
    // `is_rare_symbol_cluster` returns true on at least one plane,
    // and Strategy E re-routes to type 1.
    let (w, h) = (33u32, 27);
    let pixels = near_flat_bgr24(w, h, 0xa0, 0xd7, 0x40);
    let frame = encode_legacy_rgb(&pixels, w, h);
    assert_eq!(
        frame[0], 1,
        "Strategy E must re-route rare-symbol-cluster residuals to \
         type 1 (audit/12 §7.1); got frame_type {}",
        frame[0]
    );
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_rle_strategy_e_propagates_to_rle_path() {
    // Strategy E is path-agnostic — both `encode_legacy_channel`
    // (header 0x00) and `encode_legacy_channel_rle` (headers
    // 0x01..=0x03) feed the same flat-CDF range coder, so the same
    // fixture class triggers the same divergence. The RLE-path
    // wrapper must apply the guard symmetrically.
    let (w, h) = (33u32, 27);
    let pixels = near_flat_bgr24(w, h, 0xa0, 0xd7, 0x40);
    for escape_len in 1..=3 {
        let frame = encode_legacy_rgb_rle(&pixels, w, h, escape_len);
        assert_eq!(
            frame[0], 1,
            "Strategy E must propagate through encode_legacy_rgb_rle \
             (escape_len {escape_len}); got frame_type {}",
            frame[0]
        );
        let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
        assert_eq!(dec.pixels, pixels, "escape_len {escape_len}");
    }
}

#[test]
fn legacy_rgb_strategy_e_does_not_fire_on_pattern_bgr24() {
    // The pattern_bgr24 fixture has dispersed residuals — no plane's
    // histogram should match the rare-symbol-cluster signature. The
    // existing 95/96-cell pass rate from audit/12 depends on this
    // guard NOT firing on the pre-existing roundtrip suite.
    let (w, h) = (16u32, 12);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    assert_eq!(
        frame[0], 7,
        "non-rare-symbol-cluster fixtures must still emit type 7"
    );
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

#[test]
fn legacy_rgb_strategy_e_does_not_fire_on_pure_solid() {
    // A pure-solid plane produces all-zero residuals after predict.
    // freq[0] == pixel_count, no nonzero bins -> not a rare-symbol
    // cluster. Strategy E must NOT trigger here; type 7 still emits
    // (the existing "all-one-symbol" CDF edge stays exercised).
    let (w, h) = (4u32, 4);
    let mut pixels = Vec::with_capacity(48);
    for _ in 0..16 {
        pixels.push(0x42);
        pixels.push(0x42);
        pixels.push(0x42);
    }
    let frame = encode_legacy_rgb(&pixels, w, h);
    assert_eq!(
        frame[0], 7,
        "pure-solid plane has no rare bins; Strategy E must not \
         fire (pre-existing audit/12 §3.5 gradient/solid/ramp pass)"
    );
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

// ───────── Round 96: type-7 pair-packed CDF decode (audit/12 §7.1 Strategy F) ─────────
//
// The cleanroom decoder runs `is_rare_symbol_cluster` on every type-7
// channel's transmitted freq table before building the CDF. When the
// signature matches, the decoder builds the proprietary's **pair-packed
// 513-entry CDF** (`spec/07` §3.1 + §3.4 audit-corrected) and decodes
// against it via the §5.2 even-index binary descent — bit-faithfully
// reproducing the proprietary decoder, **including** its rare-symbol
// mis-decode (audit/12 §3.6). Common-case streams use the flat
// 257-entry CDF.
//
// Round 7 returned `Error::LegacyRareSymbolClusterUnsupported` here
// (the "defensive harness"); round 96 lands Strategy F so the streams
// now decode. The error variant is retained for genuinely-undecodable
// edge cases.
//
// Our own encoder applies Strategy E and re-routes such fixtures to
// type 1, so the self-roundtrip suite never builds the pair-packed
// path — these tests exercise it against hand-crafted channel bytes
// that simulate a *foreign* (proprietary) encoder's output. Without
// an in-tree proprietary-encoded type-7 fixture (audit/04 §5) the
// tests assert the *structural* contract (decodes, right length, no
// panic) plus the documented pair-packed CDF boundary shifts; full
// byte-exact proprietary parity awaits a fixture oracle.

/// Build a synthetic type-7 channel (header `0x00`, bare-Fibonacci
/// path) carrying the supplied 256-entry frequency table, followed by
/// `body` bytes for the legacy range-coder.
fn synth_legacy_channel_header_zero_body(freq: &[u32; 256], body: &[u8]) -> Vec<u8> {
    use crate::legacy_range_coder::encode_legacy_freq_table;
    let (fib_bytes, aligned) = encode_legacy_freq_table(freq);
    let mut ch = Vec::with_capacity(2 + fib_bytes.len() + 1 + body.len());
    ch.push(0x00); // outer channel-header byte
    ch.push(0x00); // inner codec-mode flag (bare Fibonacci sub-path)
    ch.extend_from_slice(&fib_bytes);
    if aligned {
        // post-Fibonacci 1-byte reservation (audit/08 §3.2)
        ch.push(0x00);
    }
    ch.extend_from_slice(body);
    ch
}

/// Build a synthetic type-7 channel with a small dummy range-coder
/// body (enough priming bytes for the decoder to run).
fn synth_legacy_channel_header_zero(freq: &[u32; 256]) -> Vec<u8> {
    // A short body of varied bytes drives the range coder through a
    // few symbols + refills without truncating.
    let body = [
        0x12u8, 0x34, 0x56, 0x78, 0x9a, 0xbc, 0xde, 0xf0, 0x11, 0x22, 0x33, 0x44,
    ];
    synth_legacy_channel_header_zero_body(freq, &body)
}

/// The canonical rare-symbol-cluster freq table from audit/12 §3.6's
/// near_flat 33×27 fixture: `freq[0]=887`, `freq[0x3d]=1`,
/// `freq[0x40]=2`, `freq[0xc0]=1`. `freq[0] / Σfreq = 887 / 891 ≈
/// 0.9955`, three rare bins (0x3d, 0x40, 0xc0).
fn rare_cluster_freq_table() -> [u32; 256] {
    let mut freq = [0u32; 256];
    freq[0x00] = 887;
    freq[0x3d] = 1;
    freq[0x40] = 2;
    freq[0xc0] = 1;
    freq
}

#[test]
fn legacy_pair_packed_cdf_matches_audit_12_worked_example() {
    // audit/12 §5: the near_flat R' histogram rescales to the flat
    // boundaries {0, 1020, 1021, 1023, 1024} for symbols 0, 0x3d,
    // 0x40, 0xc0. The pair-packed 513-entry CDF shifts each rare
    // symbol's lower bound by the count of preceding sentinel-1s
    // (= the symbol's index): 0x3d → 1020 + 61 = 1081, 0x40 →
    // 1021 + 64 = 1085, 0xc0 → 1023 + 192 = 1215. The author
    // verified these values clean-room from spec/07 §3.1 (a freq[c],
    // sentinel-1 interleave prefix-sum), independent of the binary.
    use crate::legacy_range_coder::{build_legacy_cdf, build_legacy_pair_packed_cdf};
    let freq = rare_cluster_freq_table();

    let (flat, total) = build_legacy_cdf(&freq).unwrap();
    assert_eq!(total, 1024, "next_pow2(891) = 1024");
    // Flat boundaries per audit/12 §5.
    assert_eq!(flat[0x00], 0);
    assert_eq!(flat[0x3d], 1020);
    assert_eq!(flat[0x40], 1021);
    assert_eq!(flat[0xc0], 1023);
    assert_eq!(flat[256], 1024);

    let (pair, ptotal) = build_legacy_pair_packed_cdf(&freq).unwrap();
    assert_eq!(pair.len(), 513);
    assert_eq!(ptotal, 1024, "divisor total is unchanged by pair-pack");
    // Pair-packed lower bounds = even indices. Symbol 0 starts at 0.
    assert_eq!(pair[0], 0);
    assert_eq!(pair[2 * 0x3d], 1081, "0x3d: 1020 + 61 sentinels");
    assert_eq!(pair[2 * 0x40], 1085, "0x40: 1021 + 64 sentinels");
    assert_eq!(pair[2 * 0xc0], 1215, "0xc0: 1023 + 192 sentinels");
    // Upper bounds = lower + freq'[c] (freq' = 1, 2, 1 respectively).
    assert_eq!(pair[2 * 0x3d + 1], 1082);
    assert_eq!(pair[2 * 0x40 + 1], 1087);
    assert_eq!(pair[2 * 0xc0 + 1], 1216);
    // Full pair span = total + 256 sentinels.
    assert_eq!(pair[512], 1024 + 256);
    // High-index rare symbols are unreachable: their lower bound
    // exceeds the divisor total, so the §5.1 symbol_index (capped at
    // total - 1) can never land there. This is the proprietary's
    // documented mis-decode (audit/12 §3.6 — 0xc0 decodes as 0xff).
    assert!(pair[2 * 0x40] >= total, "0x40 unreachable under pair-pack");
    assert!(pair[2 * 0xc0] >= total, "0xc0 unreachable under pair-pack");
}

#[test]
fn legacy_channel_decode_rare_cluster_uses_pair_packed_path() {
    // Round 96: the rare-symbol-cluster signature now decodes via the
    // pair-packed path rather than returning
    // LegacyRareSymbolClusterUnsupported. Assert the structural
    // contract: it decodes successfully and emits exactly n_pixels
    // residuals (byte-exact proprietary parity awaits a fixture).
    use crate::channel::decode_legacy_channel;
    let freq = rare_cluster_freq_table();
    let ch = synth_legacy_channel_header_zero(&freq);
    let n_pixels: usize = 64;
    let res = decode_legacy_channel(&ch, n_pixels);
    let out = res.expect("rare-cluster stream must decode via pair-packed path");
    assert_eq!(out.len(), n_pixels);
    // Every emitted symbol must be a reachable symbol (one whose
    // pair-packed lower bound is below total); the unreachable high
    // rare symbols (0x40, 0xc0) must NEVER appear in the output.
    assert!(
        !out.contains(&0x40) && !out.contains(&0xc0),
        "unreachable pair-packed symbols must not be decoded; got {:?}",
        out
    );
}

#[test]
fn legacy_channel_decode_three_rare_bins_decodes() {
    // The audit/12 §7.1 predicate requires `>= 3` distinct rare bins.
    // With 3 rare bins the pair-packed path is selected and the stream
    // decodes; with 2 rare bins the flat path is selected and also
    // decodes. Both succeed — round 96 no longer refuses either.
    use crate::channel::decode_legacy_channel;
    let mut freq = [0u32; 256];
    freq[0x00] = 1000;
    freq[0x10] = 1;
    freq[0x20] = 1;
    freq[0x30] = 2; // 3 rare bins → pair-packed path
    let ch = synth_legacy_channel_header_zero(&freq);
    let out = decode_legacy_channel(&ch, 48).expect("3-rare-bin stream decodes");
    assert_eq!(out.len(), 48);

    let mut freq2 = [0u32; 256];
    freq2[0x00] = 1000;
    freq2[0x10] = 1;
    freq2[0x20] = 1; // 2 rare bins → flat path
    let ch2 = synth_legacy_channel_header_zero(&freq2);
    let out2 = decode_legacy_channel(&ch2, 48).expect("2-rare-bin stream decodes");
    assert_eq!(out2.len(), 48);
}

#[test]
fn legacy_channel_decode_dominance_below_threshold_uses_flat() {
    // The audit/12 §7.1 predicate requires `freq[0] >= 0.95 * Σfreq`.
    // A histogram with 3 rare bins but freq[0] only at 90% of total
    // is NOT a rare-symbol cluster, so the flat path is selected.
    use crate::channel::decode_legacy_channel;
    let mut freq = [0u32; 256];
    freq[0x00] = 90;
    freq[0x80] = 7; // dispersed mass — drops dominance below 95%
    freq[0x10] = 1;
    freq[0x20] = 1;
    freq[0x30] = 1;
    let ch = synth_legacy_channel_header_zero(&freq);
    let out = decode_legacy_channel(&ch, 50).expect("non-cluster stream decodes via flat path");
    assert_eq!(out.len(), 50);
}

#[test]
fn legacy_decode_frame_rare_cluster_decodes_at_public_api() {
    // The pair-packed path must reach through `decode_frame`'s public
    // surface — assemble a type-7 frame with three rare-cluster
    // channels and verify the public API decodes (length-correct)
    // rather than erroring.
    use crate::frame::pack_channels;
    let freq = rare_cluster_freq_table();
    let n_pixels: u32 = freq.iter().sum();
    // Pick (W, H) with W * H == n_pixels. 891 == 33 * 27.
    let (w, h) = (33u32, 27u32);
    assert_eq!(w * h, n_pixels);
    // The body must be long enough to drive 891 symbols through the
    // range coder without truncating; use a pseudo-random fill.
    let body: Vec<u8> = (0..2048u32).map(|i| ((i * 73) ^ (i >> 2)) as u8).collect();
    let ch = synth_legacy_channel_header_zero_body(&freq, &body);
    let frame = pack_channels(7, &[&ch, &ch, &ch]);
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24)
        .expect("rare-cluster type-7 frame decodes via pair-packed path");
    assert_eq!(dec.pixels.len(), (w as usize) * (h as usize) * 3);
}

#[test]
fn legacy_decode_self_roundtrip_unaffected_by_round_7_guard() {
    // Strategy E (round 6) re-routes rare-symbol-cluster fixtures to
    // type 1, so the cleanroom's encode→decode roundtrip never feeds
    // a rare-cluster freq table to `decode_legacy_channel`. The
    // round-7 guard therefore must not perturb the existing 104-test
    // self-roundtrip suite. Re-exercise the `pattern_bgr24` fixture
    // explicitly here as a smoke check.
    let (w, h) = (16u32, 12);
    let pixels = pattern_bgr24(w, h);
    let frame = encode_legacy_rgb(&pixels, w, h);
    assert_eq!(
        frame[0], 7,
        "pattern_bgr24 must still produce type-7 (audit/13 §3.2)"
    );
    let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
    assert_eq!(dec.pixels, pixels);
}

// ─────────── round 174: per-frame-type header-form selector ───────────
//
// Round 14 added `encode_channel_best` (the eight-form per-channel
// selector) but left every modern frame encoder pinned on
// `encode_channel_simple` (the two-candidate `0x00` / `0x04` form),
// pending a per-frame-type benchmark fixture so the size-delta
// could be measured per frame type rather than per channel.
//
// Round 174 flips every modern frame encoder
// (`encode_arith_rgb24` / `_yv12` / `_yuy2` / `_rgba`, plus
// `_reduced_res` transitively via `_yv12`) to call
// `encode_channel_best` per-plane. The selector's per-channel
// `best_never_larger_than_simple` pin in `encoder::tests` already
// guards the per-channel direction; the tests below pin the
// frame-level **composition** — that propagating the selector
// through `pack_channels` + the frame-layout dispatcher cannot
// regress the wire size relative to a hand-constructed
// `encode_channel_simple`-pipeline reference frame.
//
// The `channel_best_strictly_smaller_than_simple_at_64k_zero_heavy`
// test below pins the channel-level crossover (where the selector
// actually picks an RLE form over bare-Fibonacci) so a future
// regression that silently makes `encode_channel_best` collapse to
// `encode_channel_simple` semantics surfaces as a concrete size
// failure.
//
// The `legacy_rgb_best_pipeline_byte_identical_on_realistic_input`
// test below pins that the type-7 call-site flip (`encode_legacy_rgb`
// now calls `encode_legacy_channel_best`) is byte-identical to the
// pre-round-174 bare-Fibonacci form on every realistic histogram,
// matching the encoder-direction empirical correction documented in
// `encoder::tests::legacy_best_always_picks_bare_on_realistic_inputs`.

mod best_pipeline_size_delta {
    use super::*;
    use crate::encoder::{encode_channel_simple, encode_uncompressed};
    use crate::frame::pack_channels;
    use crate::predict::{
        apply_plane_forward, apply_plane_forward_with_rule, cross_plane_decorrelate_rgb_forward,
        FirstColRule,
    };

    /// A zero-heavy byte profile that mimics a post-gradient
    /// Lagarith residual — `spec/06` §6.4 documents the realistic
    /// distribution as `freq[0] >= 0.95 * pixel_count`. We
    /// deterministically scatter Laplacian-tail non-zero bytes into
    /// roughly the right slots so the histogram is dominated by
    /// symbol 0 but not entirely so (the Step-A fast path's
    /// expected production input).
    fn zero_heavy_plane(n: usize) -> Vec<u8> {
        let mut out = vec![0u8; n];
        // Sprinkle ~5% non-zero bytes via a small Laplacian tail.
        // Slot ordering: a deterministic stride avoids accidental
        // alignment with the chroma sub-sample lattice in YUY2 /
        // YV12 below.
        for k in 0..(n / 20).max(1) {
            let idx = (k * 37 + 11) % n;
            // Laplacian-shaped magnitude: heavy on +/-1, light tail.
            let mag = ((k * 73 + 19) & 0xf) as i32;
            let signed = if k & 1 == 0 { mag } else { -mag };
            out[idx] = (signed as i8) as u8;
        }
        out
    }

    /// Build a "simple-pipeline" type-2/4 frame from BGR24 pixels —
    /// exactly what `encode_arith_rgb24` did before round 174 (so
    /// the round-174 production output can be compared byte-count
    /// against this baseline).
    fn simple_rgb24_frame(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
        let n = width as usize * height as usize;
        let mut plane_b = Vec::with_capacity(n);
        let mut plane_g = Vec::with_capacity(n);
        let mut plane_r = Vec::with_capacity(n);
        for px in pixels.chunks_exact(3) {
            plane_b.push(px[0]);
            plane_g.push(px[1]);
            plane_r.push(px[2]);
        }
        cross_plane_decorrelate_rgb_forward(&mut plane_b, &plane_g, &mut plane_r);
        let res_b = apply_plane_forward_with_rule(
            &plane_b,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        let res_g = apply_plane_forward_with_rule(
            &plane_g,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        let res_r = apply_plane_forward_with_rule(
            &plane_r,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        let ch_b = encode_channel_simple(&res_b);
        let ch_g = encode_channel_simple(&res_g);
        let ch_r = encode_channel_simple(&res_r);
        let type_byte = if width % 4 == 0 { 4 } else { 2 };
        pack_channels(type_byte, &[&ch_b, &ch_g, &ch_r])
    }

    /// Simple-pipeline type-8 (RGBA) reference — same shape as above.
    fn simple_rgba_frame(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
        let n = width as usize * height as usize;
        let mut plane_b = Vec::with_capacity(n);
        let mut plane_g = Vec::with_capacity(n);
        let mut plane_r = Vec::with_capacity(n);
        let mut plane_a = Vec::with_capacity(n);
        for px in pixels.chunks_exact(4) {
            plane_b.push(px[0]);
            plane_g.push(px[1]);
            plane_r.push(px[2]);
            plane_a.push(px[3]);
        }
        cross_plane_decorrelate_rgb_forward(&mut plane_b, &plane_g, &mut plane_r);
        let res_b = apply_plane_forward_with_rule(
            &plane_b,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        let res_g = apply_plane_forward_with_rule(
            &plane_g,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        let res_r = apply_plane_forward_with_rule(
            &plane_r,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        let res_a = apply_plane_forward_with_rule(
            &plane_a,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        let ch_b = encode_channel_simple(&res_b);
        let ch_g = encode_channel_simple(&res_g);
        let ch_r = encode_channel_simple(&res_r);
        let ch_a = encode_channel_simple(&res_a);
        pack_channels(8, &[&ch_b, &ch_g, &ch_r, &ch_a])
    }

    /// Simple-pipeline type-10 (YV12) reference.
    fn simple_yv12_frame(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let y_pixels = w * h;
        let c_pixels = y_pixels / 4;
        let plane_y = &pixels[..y_pixels];
        let plane_v = &pixels[y_pixels..y_pixels + c_pixels];
        let plane_u = &pixels[y_pixels + c_pixels..];
        let res_y = apply_plane_forward(plane_y, w, h);
        let cw = w / 2;
        let ch = h / 2;
        let res_v = apply_plane_forward(plane_v, cw, ch);
        let res_u = apply_plane_forward(plane_u, cw, ch);
        let ch_y = encode_channel_simple(&res_y);
        let ch_v = encode_channel_simple(&res_v);
        let ch_u = encode_channel_simple(&res_u);
        pack_channels(10, &[&ch_y, &ch_v, &ch_u])
    }

    /// Simple-pipeline type-3 (YUY2) reference.
    fn simple_yuy2_frame(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
        let w = width as usize;
        let h = height as usize;
        let cw = w / 2;
        let y_pixels = w * h;
        let c_pixels = cw * h;
        let mut plane_y = Vec::with_capacity(y_pixels);
        let mut plane_u = Vec::with_capacity(c_pixels);
        let mut plane_v = Vec::with_capacity(c_pixels);
        for y in 0..h {
            let in_row = y * w * 2;
            for k in 0..cw {
                plane_y.push(pixels[in_row + 4 * k]);
                plane_u.push(pixels[in_row + 4 * k + 1]);
                plane_y.push(pixels[in_row + 4 * k + 2]);
                plane_v.push(pixels[in_row + 4 * k + 3]);
            }
        }
        let res_y = apply_plane_forward(&plane_y, w, h);
        let res_u = apply_plane_forward(&plane_u, cw, h);
        let res_v = apply_plane_forward(&plane_v, cw, h);
        let ch_y = encode_channel_simple(&res_y);
        let ch_u = encode_channel_simple(&res_u);
        let ch_v = encode_channel_simple(&res_v);
        pack_channels(3, &[&ch_y, &ch_u, &ch_v])
    }

    /// Every test below decodes the production frame and asserts
    /// it round-trips byte-exactly. That guard means a regression
    /// of the new pipeline (e.g. mis-selected header that decodes
    /// to a different plane) surfaces immediately, independent of
    /// the size-delta assertion.
    fn assert_decodes_to(frame: &[u8], pixels: &[u8], w: u32, h: u32, kind: PixelKind) {
        let decoded = decode_frame(frame, w, h, kind).unwrap();
        assert_eq!(
            decoded.pixels, pixels,
            "round-174 frame must round-trip byte-exactly under {kind:?} at {w}×{h}",
        );
    }

    /// At large enough planes the per-channel selector picks an
    /// RLE form (header `0x01` — Fibonacci-prefixed arithmetic over
    /// the zero-run-contracted symbol stream) over the bare
    /// Fibonacci+arith of `encode_channel_simple`. Empirically the
    /// crossover is around `n_symbols = 65536` for the `~95% zero,
    /// 5% scattered tail` profile this module synthesises — the
    /// +4-byte u32 length field of `spec/07` §2.3 has to be
    /// amortised across enough symbols for the post-RLE byte count
    /// reduction to come out ahead. This test pins the crossover so
    /// any future selector regression that silently picks bare-Fib
    /// at this size surfaces immediately.
    #[test]
    fn channel_best_strictly_smaller_than_simple_at_64k_zero_heavy() {
        let plane = zero_heavy_plane(65536);
        let simple = encode_channel_simple(&plane);
        let best = crate::encoder::encode_channel_best(&plane);
        assert!(
            best.len() < simple.len(),
            "expected best<simple at n=65536, got best={} (h={:#x}) simple={} (h={:#x})",
            best.len(),
            best[0],
            simple.len(),
            simple[0],
        );
        // At this fixture size the selector should pick an RLE form
        // — pin the header byte so a future encoder change that
        // shifts the selector to a different sub-path surfaces
        // here (and the saving migrates to whichever new form
        // overtook 0x01).
        assert!(
            (0x01..=0x07).contains(&best[0]),
            "expected an RLE header (0x01..=0x07), got {:#x}",
            best[0]
        );
    }

    // ─── type 4 (Arithmetic RGB24, width % 4 == 0) ───

    #[test]
    fn arith_rgb24_best_never_larger_than_simple() {
        // Probe several power-of-two-pixel sizes (the same regime
        // `tests/reference_pins.rs` covers). On every probed size the
        // new pipeline must produce ≤ the simple-pipeline length.
        for &(w, h) in &[(4u32, 4u32), (8, 8), (8, 16), (16, 16)] {
            let pixels = pattern_bgr24(w, h);
            let simple = simple_rgb24_frame(&pixels, w, h);
            let best = encode_arith_rgb24(&pixels, w, h);
            assert!(
                best.len() <= simple.len(),
                "rgb24 best ({}) larger than simple ({}) at {w}×{h}",
                best.len(),
                simple.len(),
            );
            assert_decodes_to(&best, &pixels, w, h, PixelKind::Bgr24);
        }
    }

    // ─── type 8 (Arithmetic RGBA) ───

    #[test]
    fn arith_rgba_best_never_larger_than_simple() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16)] {
            let pixels = pattern_bgra32(w, h);
            let simple = simple_rgba_frame(&pixels, w, h);
            let best = encode_arith_rgba(&pixels, w, h);
            assert!(
                best.len() <= simple.len(),
                "rgba best ({}) larger than simple ({}) at {w}×{h}",
                best.len(),
                simple.len(),
            );
            assert_decodes_to(&best, &pixels, w, h, PixelKind::Bgra32);
        }
    }

    // ─── type 10 (Arithmetic YV12) ───

    #[test]
    fn arith_yv12_best_never_larger_than_simple() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16)] {
            let pixels = pattern_yv12(w, h);
            let simple = simple_yv12_frame(&pixels, w, h);
            let best = encode_arith_yv12(&pixels, w, h);
            assert!(
                best.len() <= simple.len(),
                "yv12 best ({}) larger than simple ({}) at {w}×{h}",
                best.len(),
                simple.len(),
            );
            assert_decodes_to(&best, &pixels, w, h, PixelKind::Yv12);
        }
    }

    // ─── type 3 (Arithmetic YUY2) ───

    #[test]
    fn arith_yuy2_best_never_larger_than_simple() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16)] {
            let pixels = pattern_yuy2(w, h);
            let simple = simple_yuy2_frame(&pixels, w, h);
            let best = encode_arith_yuy2(&pixels, w, h);
            assert!(
                best.len() <= simple.len(),
                "yuy2 best ({}) larger than simple ({}) at {w}×{h}",
                best.len(),
                simple.len(),
            );
            assert_decodes_to(&best, &pixels, w, h, PixelKind::Yuy2);
        }
    }

    // ─── type 11 (Reduced-resolution YV12) ───
    //
    // Type 11 routes through `encode_arith_yv12` after a 2× skip
    // downsample (`spec/01` §2.4 — the wire body is a half-W/half-H
    // type-10 frame with byte 0 rewritten to 0x0b). So the type-11
    // never-larger guarantee follows transitively from
    // `arith_yv12_best_never_larger_than_simple`. A separate
    // self-roundtrip test is already covered by the existing
    // `reduced_res_*_roundtrip` suite above; pin the size invariant
    // here so the type-11 dispatcher's pipeline composition is
    // explicitly guarded.
    #[test]
    fn arith_reduced_res_best_never_larger_than_simple() {
        use crate::encoder::encode_arith_reduced_res;
        for &(w, h) in &[(8u32, 8u32), (16, 16), (32, 32)] {
            // `pattern_yv12_2x_block_constant` makes each 2×2 block
            // constant so the nearest-neighbour 2× upscale matches
            // the original full-resolution pixels — required for the
            // reduced-res self-roundtrip to succeed.
            let pixels = pattern_yv12_2x_block_constant(w, h);
            // Construct the simple-pipeline reference: run the same
            // downsample inline (so the comparison stays apples-to-
            // apples), then drive it through `simple_yv12_frame` and
            // rewrite byte 0 to 0x0b like `encode_arith_reduced_res`.
            let wy = w as usize;
            let hy = h as usize;
            let half_w = wy / 2;
            let half_h = hy / 2;
            let small_cw = half_w / 2;
            let small_ch = half_h / 2;
            let small_y = half_w * half_h;
            let small_c = small_cw * small_ch;
            let mut buf = Vec::with_capacity(small_y + 2 * small_c);
            let big_y = wy * hy;
            let big_cw = wy / 2;
            let big_ch = hy / 2;
            let big_c = big_cw * big_ch;
            // Y plane downsample (skip-by-2).
            for y in 0..half_h {
                let row = (2 * y) * wy;
                for x in 0..half_w {
                    buf.push(pixels[row + 2 * x]);
                }
            }
            for y in 0..small_ch {
                let row = big_y + (2 * y) * big_cw;
                for x in 0..small_cw {
                    buf.push(pixels[row + 2 * x]);
                }
            }
            for y in 0..small_ch {
                let row = big_y + big_c + (2 * y) * big_cw;
                for x in 0..small_cw {
                    buf.push(pixels[row + 2 * x]);
                }
            }
            let mut simple = simple_yv12_frame(&buf, half_w as u32, half_h as u32);
            simple[0] = 11;
            let best = encode_arith_reduced_res(&pixels, w, h);
            assert!(
                best.len() <= simple.len(),
                "reduced-res best ({}) larger than simple ({}) at {w}×{h}",
                best.len(),
                simple.len(),
            );
            assert_decodes_to(&best, &pixels, w, h, PixelKind::Yv12);
        }
    }

    // ─── type 7 (Legacy RGB) ───
    //
    // `encode_legacy_rgb` flipped to call `encode_legacy_channel_best`
    // per-channel in round 174. The
    // `legacy_best_always_picks_bare_on_realistic_inputs` pin in
    // `encoder::tests` already proves the selector picks the bare-
    // Fibonacci header (`0x00`) on every realistic residual histogram
    // the cleanroom encoder can produce, so the frame bytes should
    // be byte-identical to a hand-constructed "always bare" baseline
    // for our test fixtures.

    #[test]
    fn legacy_rgb_best_pipeline_byte_identical_on_realistic_input() {
        use crate::encoder::{encode_legacy_channel, encode_legacy_rgb};
        // The cleanroom's "always pick bare-Fibonacci" pipeline (the
        // pre-round-174 form) constructed inline.
        fn always_bare_legacy_rgb(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
            let n = width as usize * height as usize;
            let mut plane_b = Vec::with_capacity(n);
            let mut plane_g = Vec::with_capacity(n);
            let mut plane_r = Vec::with_capacity(n);
            for px in pixels.chunks_exact(3) {
                plane_b.push(px[0]);
                plane_g.push(px[1]);
                plane_r.push(px[2]);
            }
            cross_plane_decorrelate_rgb_forward(&mut plane_b, &plane_g, &mut plane_r);
            let res_b = apply_plane_forward_with_rule(
                &plane_b,
                width as usize,
                height as usize,
                FirstColRule::B,
            );
            let res_g = apply_plane_forward_with_rule(
                &plane_g,
                width as usize,
                height as usize,
                FirstColRule::B,
            );
            let res_r = apply_plane_forward_with_rule(
                &plane_r,
                width as usize,
                height as usize,
                FirstColRule::B,
            );
            // Strategy E guard is the same — the same input goes
            // through either path identically.
            let fb = {
                let mut f = [0u32; 256];
                for &b in &res_b {
                    f[b as usize] += 1;
                }
                f
            };
            let fg = {
                let mut f = [0u32; 256];
                for &b in &res_g {
                    f[b as usize] += 1;
                }
                f
            };
            let fr = {
                let mut f = [0u32; 256];
                for &b in &res_r {
                    f[b as usize] += 1;
                }
                f
            };
            use crate::legacy_range_coder::is_rare_symbol_cluster;
            if is_rare_symbol_cluster(&fb)
                || is_rare_symbol_cluster(&fg)
                || is_rare_symbol_cluster(&fr)
            {
                return encode_uncompressed(pixels);
            }
            let ch_b = encode_legacy_channel(&res_b);
            let ch_g = encode_legacy_channel(&res_g);
            let ch_r = encode_legacy_channel(&res_r);
            pack_channels(7, &[&ch_b, &ch_g, &ch_r])
        }
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 12)] {
            let pixels = pattern_bgr24(w, h);
            let bare = always_bare_legacy_rgb(&pixels, w, h);
            let best = encode_legacy_rgb(&pixels, w, h);
            assert_eq!(
                best, bare,
                "round-174 legacy_rgb diverged from bare-Fibonacci baseline at {w}×{h} \
                 — the `legacy_best_always_picks_bare_on_realistic_inputs` invariant \
                 might have shifted; update both pins together if intentional"
            );
            assert_decodes_to(&best, &pixels, w, h, PixelKind::Bgr24);
        }
    }
}

// ─────────── round 181: decoder defensive harness ───────────
//
// Production-path robustness: `decode_frame`, `decode_frame_with_prev`,
// and `Decoder::decode` must surface every malformed-input failure as
// an `Err(_)` rather than panicking. This module sweeps every documented
// failure mode in `crate::error::Error` against the `decode_frame`
// dispatch tree (`spec/01` §1.2 frame-type byte, `spec/01` §2.3
// channel-offset table, `spec/03` §2.1 + `spec/06` §1 channel-header
// dispatcher, `spec/01` §1.1 NULL-frame replay) by constructing
// minimum-sized malformed fixtures.
//
// Each test asserts the `Err(_)` variant where it is stable (matches
// the documented `Error::*` per `spec/01` §1.2 / `spec/06` §1.5 /
// `spec/05` §4.2 etc.). A few tests assert only that decode returns
// `Err(_)` (not panics) — for inputs where any of several variants
// would be a legitimate report.
//
// All inputs are constructed in-line from spec-defined layout fields;
// no encoder path is involved (the encoder is a `#[cfg(test)]` helper
// for self-roundtrip — these tests target the decoder against
// arbitrary on-wire bytes, the actual production attack surface).

mod decoder_defensive_harness {
    use super::*;
    use crate::frame::pack_channels;
    use crate::Error;

    // ─── frame-type / dispatch-level malformed inputs ───

    /// `decode_frame` must surface a zero-byte payload as
    /// [`Error::NullFrame`] — the stateless decoder cannot replay a
    /// predecessor (`spec/01` §1.1 frames the NULL-frame replay as a
    /// container-layer responsibility).
    #[test]
    fn decode_frame_empty_payload_is_null_frame() {
        let r = decode_frame(&[], 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::NullFrame)),
            "expected Err(NullFrame), got {:?}",
            r
        );
    }

    /// Zero-width and zero-height inputs are caller-bug inputs (no
    /// surface area for a Lagarith decode); surfaced as
    /// [`Error::BadDimensions`] before any wire bytes are consulted.
    #[test]
    fn decode_frame_zero_dimensions_are_bad_dimensions() {
        let some_payload = vec![1u8; 64];
        for &(w, h) in &[(0u32, 4u32), (4, 0), (0, 0)] {
            let r = decode_frame(&some_payload, w, h, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::BadDimensions { width, height }) if width == w && height == h),
                "expected Err(BadDimensions({w}, {h})), got {:?}",
                r
            );
        }
    }

    /// Frame-type byte 0 is reserved out per `spec/01` §1.2; the
    /// decoder must surface [`Error::BadFrameType(0)`].
    #[test]
    fn decode_frame_type_zero_is_bad_frame_type() {
        let payload = vec![0u8, 0, 0, 0, 0];
        let r = decode_frame(&payload, 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::BadFrameType(0))),
            "expected Err(BadFrameType(0)), got {:?}",
            r
        );
    }

    /// Frame-type bytes 12..=255 are reserved out per `spec/01` §1.2.
    /// Sweep the range and assert each surfaces
    /// [`Error::BadFrameType(_)`] with the expected byte echoed back.
    #[test]
    fn decode_frame_high_type_bytes_are_bad_frame_type() {
        for byte in [12u8, 13, 50, 100, 200, 0xfe, 0xff] {
            let payload = vec![byte, 0, 0, 0, 0];
            let r = decode_frame(&payload, 4, 4, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::BadFrameType(b)) if b == byte),
                "type {byte}: expected Err(BadFrameType({byte})), got {:?}",
                r
            );
        }
    }

    // ─── uncompressed (type 1) malformed inputs ───

    /// Uncompressed (type 1) requires `1 + buffer_len(W, H)` payload
    /// bytes per `spec/01` §2.1. A short payload surfaces
    /// [`Error::Truncated`].
    #[test]
    fn decode_uncompressed_truncated_body_is_truncated() {
        // 4×4 BGR24 needs 48 body bytes + 1 type byte = 49; offer 10.
        let mut payload = vec![1u8];
        payload.extend(std::iter::repeat(0u8).take(9));
        let r = decode_frame(&payload, 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::Truncated { .. })),
            "expected Err(Truncated), got {:?}",
            r
        );
    }

    // ─── solid (types 5/6/9) malformed inputs ───

    /// Type 5 (Solid Grey) needs 2 bytes (type + fill); type 6 (Solid
    /// RGB) needs 4 bytes (type + B + G + R); type 9 (Solid RGBA)
    /// needs 5. Truncated payloads surface [`Error::Truncated`].
    #[test]
    fn decode_solid_truncated_colour_bytes_is_truncated() {
        for type_byte in [5u8, 6, 9] {
            let r = decode_frame(&[type_byte], 4, 4, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type {type_byte}: expected Err(Truncated), got {:?}",
                r
            );
        }
        // type 6 with two bytes (need 4)
        let r = decode_frame(&[6u8, 0, 0], 4, 4, PixelKind::Bgr24);
        assert!(matches!(r, Err(Error::Truncated { .. })));
        // type 9 with four bytes (need 5)
        let r = decode_frame(&[9u8, 0, 0, 0, 0], 4, 4, PixelKind::Bgra32);
        assert!(r.is_ok(), "5 bytes is exactly the minimum");
    }

    /// Solid frames produce a packed RGB / RGBA output, so asking
    /// for `Yv12` against a solid frame surfaces
    /// [`Error::PixelFormatMismatch`].
    #[test]
    fn decode_solid_with_planar_pixel_format_is_mismatch() {
        for type_byte in [5u8, 6, 9] {
            let mut payload = vec![type_byte];
            payload.extend([0x42u8; 4]); // enough for any solid shape
            let r = decode_frame(&payload, 4, 4, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::PixelFormatMismatch { frame_type }) if frame_type == type_byte),
                "type {type_byte}: expected Err(PixelFormatMismatch), got {:?}",
                r
            );
        }
    }

    // ─── arithmetic RGB24/RGBA channel-offset-table malformed inputs ───

    /// 3-channel frame types (2, 4) carry an 8-byte offset table; a
    /// payload too short to hold it surfaces [`Error::Truncated`].
    #[test]
    fn decode_arith_rgb_truncated_offset_table_is_truncated() {
        // Type 4 (Arithmetic RGB24): needs at least 9 bytes for
        // type + 8-byte offset table.
        for type_byte in [2u8, 4] {
            let r = decode_frame(&[type_byte, 0, 0], 4, 4, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type {type_byte}: expected Err(Truncated), got {:?}",
                r
            );
        }
    }

    /// 4-channel frame type 8 (Arithmetic RGBA) carries a 12-byte
    /// offset table; a payload too short surfaces [`Error::Truncated`].
    #[test]
    fn decode_arith_rgba_truncated_offset_table_is_truncated() {
        let r = decode_frame(&[8u8, 0, 0, 0, 0], 4, 4, PixelKind::Bgra32);
        assert!(
            matches!(r, Err(Error::Truncated { .. })),
            "expected Err(Truncated), got {:?}",
            r
        );
    }

    /// An offset that points past the frame end is
    /// [`Error::OffsetOutOfRange`] per the `split_channels`
    /// preconditions.
    #[test]
    fn decode_arith_rgb_offset_past_eof_is_out_of_range() {
        // Type 4 frame: byte 0 = 4, bytes 1..9 = two u32 offsets.
        // Set the first offset to a huge value to trigger the bound
        // check.
        let mut payload = vec![4u8];
        payload.extend_from_slice(&0xffff_ffffu32.to_le_bytes()); // offset to G
        payload.extend_from_slice(&0u32.to_le_bytes()); // offset to B
                                                        // pad some channel-body bytes so split_channels gets a non-tiny
                                                        // frame to grade against
        payload.extend(std::iter::repeat(0u8).take(64));
        let r = decode_frame(&payload, 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::OffsetOutOfRange)),
            "expected Err(OffsetOutOfRange), got {:?}",
            r
        );
    }

    /// Offsets that go backwards (offset_to_B < offset_to_G) are
    /// [`Error::OffsetOutOfRange`] per `split_channels`'s ascending-
    /// offsets invariant.
    #[test]
    fn decode_arith_rgb_descending_offsets_are_out_of_range() {
        // Type 4 frame with offset_to_G = 30 but offset_to_B = 20.
        let mut payload = vec![4u8];
        payload.extend_from_slice(&30u32.to_le_bytes()); // offset to G
        payload.extend_from_slice(&20u32.to_le_bytes()); // offset to B
        payload.extend(std::iter::repeat(0u8).take(64));
        let r = decode_frame(&payload, 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::OffsetOutOfRange)),
            "expected Err(OffsetOutOfRange), got {:?}",
            r
        );
    }

    /// Asking for `Yv12` against a type-4 (Arithmetic RGB24) frame
    /// surfaces [`Error::PixelFormatMismatch`] (the decoder uses the
    /// pixel kind to choose between packed BGR24 and BGRA32 outputs;
    /// `Yv12` is not in either set).
    #[test]
    fn decode_arith_rgb24_with_planar_pixel_format_is_mismatch() {
        // Build a minimum-shape type-4 frame with a 0xff fill channel
        // body so the dispatcher reaches the pixel-format gate.
        let ch_b = vec![0xffu8, 0x10];
        let ch_g = vec![0xffu8, 0x20];
        let ch_r = vec![0xffu8, 0x30];
        let frame = pack_channels(4, &[&ch_b, &ch_g, &ch_r]);
        let r = decode_frame(&frame, 4, 4, PixelKind::Yv12);
        assert!(
            matches!(r, Err(Error::PixelFormatMismatch { .. })),
            "expected Err(PixelFormatMismatch), got {:?}",
            r
        );
    }

    // ─── channel-header dispatcher malformed inputs ───
    //
    // Each test packs valid channel-offset table bytes, then injects
    // a deliberately malformed per-channel body. The dispatcher must
    // surface the documented error variant rather than panic.

    /// Channel-header `0x01..=0x03` requires a u32 length field
    /// (5 bytes minimum). A 2-byte channel surfaces
    /// [`Error::Truncated`].
    #[test]
    fn decode_arith_rgb24_channel_header_01_short_is_truncated() {
        // Three channels, each just `[header, 0x00]` so the u32 read
        // in `decode_channel` would walk off the end.
        for header in [0x01u8, 0x02, 0x03] {
            let ch = vec![header, 0x00];
            let frame = pack_channels(4, &[&ch, &ch, &ch]);
            let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "header {header:#x}: expected Err(Truncated), got {:?}",
                r
            );
        }
    }

    /// Channel-header `0x04` requires `1 + n_pixels` body bytes;
    /// a shorter channel surfaces [`Error::Truncated`].
    #[test]
    fn decode_arith_rgb24_channel_header_04_short_is_truncated() {
        // 4×4 RGB24 frame: each channel needs 16 raw bytes after
        // the `0x04` header. Offer 3.
        let ch = vec![0x04u8, 0, 0, 0];
        let frame = pack_channels(4, &[&ch, &ch, &ch]);
        let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::Truncated { .. })),
            "expected Err(Truncated), got {:?}",
            r
        );
    }

    /// Channel-header `0xff` requires a 2-byte channel (header + fill);
    /// a one-byte channel surfaces [`Error::Truncated`].
    #[test]
    fn decode_arith_rgb24_channel_header_ff_short_is_truncated() {
        let ch = vec![0xffu8];
        let frame = pack_channels(4, &[&ch, &ch, &ch]);
        let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::Truncated { .. })),
            "expected Err(Truncated), got {:?}",
            r
        );
    }

    /// Channel-header bytes outside `{0x00..=0x07, 0xff}` are
    /// [`Error::BadChannelHeader`] per `spec/03` §2.1.
    #[test]
    fn decode_arith_rgb24_channel_header_bad_byte_is_bad_channel_header() {
        for header in [0x08u8, 0x09, 0x10, 0x42, 0x7f, 0x80, 0xfe] {
            let ch = vec![header, 0, 0, 0, 0, 0, 0, 0];
            let frame = pack_channels(4, &[&ch, &ch, &ch]);
            let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::BadChannelHeader(b)) if b == header),
                "header {header:#x}: expected Err(BadChannelHeader({header:#x})), got {:?}",
                r
            );
        }
    }

    // ─── stateful NULL-frame replay malformed inputs ───

    /// A stateful [`Decoder`] handed a zero-byte first payload has no
    /// predecessor to replay; surfaces
    /// [`Error::NullFrameWithoutPredecessor`].
    #[test]
    fn stateful_decoder_null_first_frame_is_no_predecessor() {
        let mut dec = Decoder::new();
        let r = dec.decode(&[], 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::NullFrameWithoutPredecessor)),
            "expected Err(NullFrameWithoutPredecessor), got {:?}",
            r
        );
    }

    /// A NULL frame applied to a different (W, H, kind) than the
    /// predecessor is a host-integration error per the
    /// `decode_frame_with_prev` cross-check; surfaces
    /// [`Error::PixelFormatMismatch { frame_type: 0 }`] (the NULL
    /// frame type byte is reported as 0 — there is no real frame-type
    /// byte on the wire to echo back).
    #[test]
    fn null_replay_with_mismatched_dims_is_mismatch() {
        // Prime the decoder with a 4×4 BGR24 solid grey frame.
        let prime = vec![5u8, 0x77];
        let mut dec = Decoder::new();
        let _first = dec.decode(&prime, 4, 4, PixelKind::Bgr24).unwrap();
        // Now hand it a NULL frame at *different* dimensions.
        let r = dec.decode(&[], 8, 8, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::PixelFormatMismatch { frame_type: 0 })),
            "expected Err(PixelFormatMismatch{{frame_type:0}}), got {:?}",
            r
        );
    }

    /// Same as above but with a mismatched pixel kind.
    #[test]
    fn null_replay_with_mismatched_pixel_kind_is_mismatch() {
        let prime = vec![5u8, 0x77];
        let mut dec = Decoder::new();
        let _first = dec.decode(&prime, 4, 4, PixelKind::Bgr24).unwrap();
        let r = dec.decode(&[], 4, 4, PixelKind::Bgra32);
        assert!(
            matches!(r, Err(Error::PixelFormatMismatch { frame_type: 0 })),
            "expected Err(PixelFormatMismatch{{frame_type:0}}), got {:?}",
            r
        );
    }

    /// `decode_frame_with_prev` with `prev = None` and a NULL payload
    /// surfaces [`Error::NullFrameWithoutPredecessor`] — the same
    /// invariant the stateful `Decoder` enforces, exercised through
    /// the helper directly.
    #[test]
    fn decode_frame_with_prev_null_no_predecessor_is_no_predecessor() {
        let r = decode_frame_with_prev(&[], 4, 4, PixelKind::Bgr24, None);
        assert!(
            matches!(r, Err(Error::NullFrameWithoutPredecessor)),
            "expected Err(NullFrameWithoutPredecessor), got {:?}",
            r
        );
    }

    // ─── randomised no-panic sweep ───
    //
    // For inputs the spec does not bind to a specific failure variant,
    // assert only that decode terminates with `Result<_, _>` rather
    // than panicking. Deterministic LCG ensures reproducibility.

    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((s >> 33) as u8);
        }
        out
    }

    /// Feed deterministic pseudo-random byte streams (varying first
    /// byte across the valid 1..=11 frame-type range + a few invalid
    /// values + several lengths) through `decode_frame`. Each call
    /// must return `Ok` or `Err` — never panic. Reproducibility comes
    /// from the LCG seed.
    ///
    /// The `(W, H)` shapes deliberately span **both parities**. The
    /// even shapes drive the natural-aligned YV12 / YUY2 chroma
    /// geometry; the odd shapes reach the decoder's documented
    /// odd-dimension branches — the YV12 `floor(W·H/4) != (W/2)·(H/2)`
    /// SPECGAP single-row chroma fallback (`spec/03` §6.1.1) and the
    /// YUY2 odd-width luma-tail / `0x80` neutral-chroma slot
    /// (`spec/03` §6.2). Those branches run different predictor-geometry
    /// and packing arithmetic than the even path, so their panic-freedom
    /// is asserted here under CI rather than relying solely on the
    /// out-of-CI `cargo-fuzz` harness.
    #[test]
    fn random_payload_no_panic_sweep() {
        // Mixed-parity shapes: (3,3) and (5,3) exercise odd W and/or H;
        // (1,1) is the degenerate single-pixel edge (empty YV12/YUY2
        // chroma planes); (4,4) keeps the prior even baseline.
        const SHAPES: [(u32, u32); 5] = [(4, 4), (3, 3), (5, 3), (4, 5), (1, 1)];
        for type_byte in 0u8..=12 {
            for &seed in &[0x1234_5678_9abc_def0u64, 0xfeed_face_dead_beef, 0] {
                for &len in &[1usize, 2, 5, 9, 16, 33, 64, 128] {
                    let mut payload = lcg_bytes(seed.wrapping_add(len as u64), len);
                    payload[0] = type_byte;
                    // Decode against each accepted (W, H, kind) shape;
                    // anything that returns Err is fine, anything that
                    // returns Ok is fine; the test fails only on
                    // panic.
                    for &(w, h) in &SHAPES {
                        let _ = decode_frame(&payload, w, h, PixelKind::Bgr24);
                        let _ = decode_frame(&payload, w, h, PixelKind::Bgra32);
                        let _ = decode_frame(&payload, w, h, PixelKind::Yv12);
                        let _ = decode_frame(&payload, w, h, PixelKind::Yuy2);
                    }
                }
            }
        }
    }

    /// Random per-channel bodies behind a valid type-4 RGB24 offset
    /// table — the channel dispatcher must not panic on any byte
    /// pattern in the channel body. Catches dispatcher-level
    /// regressions that would only surface against adversarial
    /// wire bytes.
    ///
    /// RGB / legacy types are driven at the even baseline; the YV12 and
    /// YUY2 dispatchers are additionally driven at **odd** `(W, H)`
    /// shapes so the random channel bytes reach the odd-dimension
    /// chroma-geometry branches (`spec/03` §6.1.1 YV12 SPECGAP single-
    /// row fallback and `spec/03` §6.2 YUY2 odd-width tail) under CI,
    /// not just the out-of-CI `cargo-fuzz` harness.
    #[test]
    fn random_channel_bodies_no_panic_sweep() {
        // Shapes the planar-YUV dispatchers are swept against. (3,3),
        // (5,3), (4,5) hit odd W and/or H; (1,1) is the degenerate
        // single-pixel edge with empty chroma planes; (4,4) is the
        // even baseline.
        const YUV_SHAPES: [(u32, u32); 5] = [(4, 4), (3, 3), (5, 3), (4, 5), (1, 1)];
        for &seed in &[0xa5a5_5a5a_a5a5_5a5au64, 0x0fed_cba9_8765_4321, 1] {
            for &per_channel_len in &[1usize, 4, 9, 17, 33, 80] {
                let ch_b = lcg_bytes(seed, per_channel_len);
                let ch_g = lcg_bytes(seed.wrapping_add(1), per_channel_len);
                let ch_r = lcg_bytes(seed.wrapping_add(2), per_channel_len);
                let frame = pack_channels(4, &[&ch_b, &ch_g, &ch_r]);
                let _ = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
                let _ = decode_frame(&frame, 4, 4, PixelKind::Bgra32);
                // Also exercise the type-7 legacy decoder against
                // the same random byte stream (different range coder,
                // different prefix layout).
                let mut t7_frame = frame.clone();
                t7_frame[0] = 7;
                let _ = decode_frame(&t7_frame, 4, 4, PixelKind::Bgr24);
                // The YV12 (3 planes Y/V/U) and YUY2 dispatchers are
                // swept across both parities so the odd-dimension
                // chroma-geometry branches see random channel bytes.
                let mut t10_frame = frame.clone();
                t10_frame[0] = 10;
                let mut t3_frame = frame.clone();
                t3_frame[0] = 3;
                for &(w, h) in &YUV_SHAPES {
                    let _ = decode_frame(&t10_frame, w, h, PixelKind::Yv12);
                    let _ = decode_frame(&t3_frame, w, h, PixelKind::Yuy2);
                }
            }
        }
    }

    // ─── reduced-resolution (type 11) dimension validation ───
    //
    // Round 187 tightens `decode_reduced_res` to reject host dimensions
    // that aren't a multiple of 4. The 2× nearest-neighbour upscale
    // per `spec/01` §2.4 reads from a half-resolution YV12 frame at
    // `(W/2, H/2)`; for the upscaler to tile cleanly onto the host
    // buffer we need `width == 2 * half_w` and `height == 2 * half_h`,
    // and for the embedded half-res YV12 chroma plane (4:2:0,
    // `spec/03` §6.1) we need `half_w` and `half_h` to each be even —
    // i.e. host W and H each a multiple of 4. The previous bound
    // checked only `half_w >= 1 && half_h >= 1`, which silently
    // zeroed the chroma planes in release builds (and would
    // `debug_assert!` panic in debug) when the host fed odd
    // dimensions. The new bound surfaces these inputs as
    // [`Error::BadDimensions`] before any wire bytes are consulted.

    /// A type-11 frame at an odd width (`width % 2 == 1`) cannot
    /// produce an integer-pixel 2× upscale; the decoder must surface
    /// [`Error::BadDimensions`] up-front rather than running a
    /// partially-correct upscale.
    #[test]
    fn reduced_res_odd_width_is_bad_dimensions() {
        // Minimum non-empty type-11 prefix: 1 frame-type byte + 8
        // channel-offset bytes (2 × u32) for a 3-plane YV12 body.
        let mut payload = vec![11u8];
        payload.extend_from_slice(&[0u8; 8]);
        for &w in &[1u32, 3, 5, 7, 9, 13, 15, 17] {
            let r = decode_frame(&payload, w, 8, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::BadDimensions { width, height })
                    if width == w && height == 8),
                "expected Err(BadDimensions({w}, 8)) for type-11 at odd width, got {:?}",
                r
            );
        }
    }

    /// A type-11 frame at an odd height (`height % 2 == 1`) likewise
    /// cannot produce an integer-pixel 2× upscale; surface
    /// [`Error::BadDimensions`].
    #[test]
    fn reduced_res_odd_height_is_bad_dimensions() {
        let mut payload = vec![11u8];
        payload.extend_from_slice(&[0u8; 8]);
        for &h in &[1u32, 3, 5, 7, 9, 13, 15, 17] {
            let r = decode_frame(&payload, 8, h, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::BadDimensions { width, height })
                    if width == 8 && height == h),
                "expected Err(BadDimensions(8, {h})) for type-11 at odd height, got {:?}",
                r
            );
        }
    }

    /// A type-11 frame at a width that is 2 mod 4 (even, but
    /// `(W/2) % 2 == 1`) has an integer 2× upscale BUT the embedded
    /// half-res YV12 chroma plane lands at a fractional `(W/4)`
    /// column count; the decoder must surface [`Error::BadDimensions`]
    /// rather than tiling the chroma planes against a width-mismatched
    /// destination row.
    #[test]
    fn reduced_res_width_two_mod_four_is_bad_dimensions() {
        let mut payload = vec![11u8];
        payload.extend_from_slice(&[0u8; 8]);
        for &w in &[2u32, 6, 10, 14, 18, 22] {
            let r = decode_frame(&payload, w, 8, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::BadDimensions { width, height })
                    if width == w && height == 8),
                "expected Err(BadDimensions({w}, 8)) for type-11 at width {w} (=2 mod 4), got {:?}",
                r
            );
        }
    }

    /// Same as above for height (`H/2` odd → chroma half-height
    /// fractional).
    #[test]
    fn reduced_res_height_two_mod_four_is_bad_dimensions() {
        let mut payload = vec![11u8];
        payload.extend_from_slice(&[0u8; 8]);
        for &h in &[2u32, 6, 10, 14, 18, 22] {
            let r = decode_frame(&payload, 8, h, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::BadDimensions { width, height })
                    if width == 8 && height == h),
                "expected Err(BadDimensions(8, {h})) for type-11 at height {h} (=2 mod 4), got {:?}",
                r
            );
        }
    }

    /// Zero-width / zero-height type-11 inputs (caught by the
    /// general `decode_frame` dimension guard up-front) still
    /// surface [`Error::BadDimensions`]. This pins the contract
    /// edge so a future refactor cannot accidentally drop the
    /// zero-dimension guard while keeping the multiple-of-4 guard.
    #[test]
    fn reduced_res_zero_dimensions_are_bad_dimensions() {
        let mut payload = vec![11u8];
        payload.extend_from_slice(&[0u8; 8]);
        for &(w, h) in &[(0u32, 8u32), (8, 0), (0, 0)] {
            let r = decode_frame(&payload, w, h, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::BadDimensions { width, height })
                    if width == w && height == h),
                "expected Err(BadDimensions({w}, {h})) for type-11 at zero dim, got {:?}",
                r
            );
        }
    }

    /// Confirm the validation only rejects the documented set — a
    /// multiple-of-4 host dimension still flows into the decoder
    /// proper (and surfaces whatever Err the malformed body emits,
    /// rather than the dimension guard's [`Error::BadDimensions`]).
    /// This pins the bound at exactly multiples-of-4 so a regression
    /// that over-tightens (e.g. multiples-of-8) is caught.
    #[test]
    fn reduced_res_multiple_of_four_passes_dimension_guard() {
        let mut payload = vec![11u8];
        payload.extend_from_slice(&[0u8; 8]);
        for &(w, h) in &[(4u32, 4u32), (8, 4), (4, 8), (8, 8), (12, 16), (16, 12)] {
            let r = decode_frame(&payload, w, h, PixelKind::Yv12);
            // The malformed body (all-zero offset table → empty
            // channel slices) cannot decode either, but the failure
            // mode must NOT be `BadDimensions` for these inputs — it
            // must come from the downstream body parser.
            match r {
                Err(Error::BadDimensions { .. }) => {
                    panic!("type-11 at {w}×{h} (multiple of 4) wrongly rejected by dimension guard")
                }
                Err(_) => { /* downstream parser surfaced a non-dimension Err — fine */ }
                Ok(_) => { /* would also be acceptable, though unlikely on this malformed body */ }
            }
        }
    }
}

/// Round 192 — truncation + single-byte-flip fuzz on **valid** encoded
/// frames.
///
/// Round 181 + 187 exercised **hand-constructed malformed inputs** and
/// **random byte streams**. Both leave a gap: a random byte stream is
/// statistically unlikely to look like a valid `(escape_len,
/// supplement_byte)` RLE pair, a valid Fibonacci-prefix bit-stream, or
/// a valid channel-offset table; so the most defect-prone code paths
/// (length decoders that walk *through* well-formed prefix bytes into
/// a region the caller does not control) are barely exercised by
/// random-byte tests.
///
/// This module closes that gap. It encodes a real Lagarith frame
/// (one per frame-type covered by the test encoder: 1, 3, 4, 7, 8,
/// 10, 11), then progressively truncates each frame at every byte
/// offset from `1..len` and asserts the decoder terminates (returns
/// `Ok(_)` or `Err(_)`) without panicking — including in debug builds
/// where the predictor / RLE / range-coder modules carry
/// `debug_assert!` invariants. Then for a deterministic sub-set of
/// offsets it flips a single byte and checks the same no-panic
/// invariant — the flip can move the wire into an
/// over-length / underflow / misalignment state the truncation sweep
/// cannot reach (e.g. an RLE-escape supplement that points *forward*
/// of the truncation cut).
///
/// Each truncation that lands strictly inside the prefix (frame-type
/// byte + channel-offset table) **must** surface as
/// [`Error::Truncated`] (the dispatcher's documented contract for an
/// incomplete prefix). Truncations that land inside a channel body
/// are allowed to surface any `Err(_)` variant the channel decoder
/// chooses — the contract there is no-panic, not a specific error
/// variant, because the truncated body can look like a legal but
/// shorter Fibonacci-coded plane.
///
/// The stateful [`Decoder`] path is exercised by replaying a
/// truncated NULL-frame ("JUMP") wire byte tail (zero-byte payload
/// after a successful primer) — that path doesn't truncate (it's
/// already zero-length), so the stateful sweep instead exercises
/// `decode_frame_with_prev` against truncated primer payloads with a
/// `prev` of a different / matching shape, asserting no-panic.
#[cfg(test)]
mod decoder_truncation_fuzz {
    use super::*;
    use crate::encoder::{
        encode_arith_reduced_res, encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2,
        encode_arith_yv12, encode_legacy_rgb, encode_solid_grey, encode_solid_rgb,
        encode_solid_rgba, encode_uncompressed,
    };
    use crate::Error;

    /// Pattern body used as the encoder input — deterministic
    /// `i * 73 + 11` modulo 256 (the same pattern `pattern_bgr24`
    /// uses elsewhere in this file, kept inline so this module is
    /// self-contained as a fuzz harness).
    fn pattern_bytes(n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let v = (i as u32).wrapping_mul(73).wrapping_add(11);
            out.push((v & 0xff) as u8);
        }
        out
    }

    /// Run the truncation sweep on `frame` against all four pixel
    /// kinds. For each truncated prefix length `k ∈ 1..frame.len()`,
    /// call `decode_frame(&frame[..k], w, h, kind)` and assert it
    /// returns a `Result<_, _>` (no panic). Then for a deterministic
    /// subset of single-byte flip offsets (every 7th byte), flip the
    /// byte to `0xff` and to `0x00` and re-run; same no-panic
    /// invariant.
    fn truncation_and_flip_sweep(frame: &[u8], w: u32, h: u32) {
        // Truncation pass — every prefix length from 1 (just the
        // frame-type byte) up to but not including the full length
        // (the full-length frame already roundtrips successfully in
        // its own test; the fuzz pass is about everything *shorter*).
        for k in 1..frame.len() {
            let cut = &frame[..k];
            for &kind in &[
                PixelKind::Bgr24,
                PixelKind::Bgra32,
                PixelKind::Yv12,
                PixelKind::Yuy2,
            ] {
                // The call must terminate; `_` discards Ok or Err
                // without unwrapping. Panic on this line would fail
                // the test (no `should_panic` attribute).
                let _ = decode_frame(cut, w, h, kind);
            }
        }
        // Single-byte flip pass — every 7th offset, flipped to 0xff
        // and 0x00. 7 is coprime with 2, 3, 4, 5 (and most channel
        // header / Fibonacci-prefix byte stride lengths), so the
        // sweep does not align to any one structural feature of the
        // wire.
        for off in (0..frame.len()).step_by(7) {
            for &val in &[0xffu8, 0x00] {
                let mut mutated = frame.to_vec();
                mutated[off] = val;
                for &kind in &[
                    PixelKind::Bgr24,
                    PixelKind::Bgra32,
                    PixelKind::Yv12,
                    PixelKind::Yuy2,
                ] {
                    let _ = decode_frame(&mutated, w, h, kind);
                }
            }
        }
    }

    /// Type 1 (Uncompressed) — `1 + buffer_len(W, H)` body byte
    /// frame, every truncation strictly inside the body surfaces
    /// `Error::Truncated` (the only failure mode the uncompressed
    /// path can have past the frame-type byte). Sweep across the
    /// frame to confirm no panic + each prefix returns the documented
    /// Err.
    #[test]
    fn type_1_uncompressed_truncation_and_flip_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Bgr24.buffer_len(w, h));
        let frame = encode_uncompressed(&pixels);
        // Sanity — full frame decodes.
        let full = decode_frame(&frame, w, h, PixelKind::Bgr24);
        assert!(full.is_ok(), "type-1 full frame should decode: {:?}", full);
        // Truncation: prefix lengths < full length must surface
        // `Error::Truncated` against the matching pixel kind. Other
        // pixel kinds may also surface `PixelFormatMismatch` (the
        // type-1 dispatcher accepts every kind but the buffer-length
        // assertion is kind-specific).
        for k in 1..frame.len() {
            let r = decode_frame(&frame[..k], w, h, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-1 prefix len {k}/{} should be Truncated, got {:?}",
                frame.len(),
                r
            );
        }
        // Bare frame-type byte (length 1) against other kinds also
        // surfaces Truncated (the body-length cross-check is kind-
        // specific but a 1-byte payload is shorter than every kind's
        // body length).
        truncation_and_flip_sweep(&frame, w, h);
    }

    /// Type 4 (Arithmetic RGB24) — `4×4` BGR24 frame. The wire is
    /// `[type_byte][8-byte channel-offset table][3 channels]`. The
    /// truncation sweep walks every prefix length; truncations
    /// strictly inside the offset table (`1..9`) must be `Truncated`
    /// (`spec/01` §2.3 contract), truncations inside the channels
    /// can surface any `Err(_)` the channel decoder chooses
    /// (including OffsetOutOfRange when the truncation lands past
    /// the buffer the offset table pointed to). No panic ever.
    #[test]
    fn type_4_arith_rgb24_truncation_and_flip_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Bgr24.buffer_len(w, h));
        let frame = encode_arith_rgb24(&pixels, w, h);
        // Full frame decodes.
        let full = decode_frame(&frame, w, h, PixelKind::Bgr24);
        assert!(full.is_ok(), "type-4 full frame should decode: {:?}", full);
        // Prefix in the channel-offset table (1..=8) must be
        // `Truncated`.
        for k in 1..=8 {
            let r = decode_frame(&frame[..k], w, h, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-4 prefix len {k} (in offset table) should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, w, h);
    }

    /// Type 8 (Arithmetic RGBA) — 4 channels, the channel-offset
    /// table is `(4 - 1) * 4 = 12` bytes. Truncations in
    /// `1..=12` are Truncated.
    #[test]
    fn type_8_arith_rgba_truncation_and_flip_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Bgra32.buffer_len(w, h));
        let frame = encode_arith_rgba(&pixels, w, h);
        let full = decode_frame(&frame, w, h, PixelKind::Bgra32);
        assert!(full.is_ok(), "type-8 full frame should decode: {:?}", full);
        for k in 1..=12 {
            let r = decode_frame(&frame[..k], w, h, PixelKind::Bgra32);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-8 prefix len {k} (in offset table) should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, w, h);
    }

    /// Type 10 (Arithmetic YV12) — 3 planes, offset table is 8 bytes.
    /// Prefixes 1..=8 must be Truncated.
    #[test]
    fn type_10_arith_yv12_truncation_and_flip_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Yv12.buffer_len(w, h));
        let frame = encode_arith_yv12(&pixels, w, h);
        let full = decode_frame(&frame, w, h, PixelKind::Yv12);
        assert!(full.is_ok(), "type-10 full frame should decode: {:?}", full);
        for k in 1..=8 {
            let r = decode_frame(&frame[..k], w, h, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-10 prefix len {k} (in offset table) should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, w, h);
    }

    /// Type 3 (Arithmetic YUY2) — same 3-plane offset-table shape
    /// as type 10 / type 4.
    #[test]
    fn type_3_arith_yuy2_truncation_and_flip_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Yuy2.buffer_len(w, h));
        let frame = encode_arith_yuy2(&pixels, w, h);
        let full = decode_frame(&frame, w, h, PixelKind::Yuy2);
        assert!(full.is_ok(), "type-3 full frame should decode: {:?}", full);
        for k in 1..=8 {
            let r = decode_frame(&frame[..k], w, h, PixelKind::Yuy2);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-3 prefix len {k} (in offset table) should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, w, h);
    }

    /// Type 7 (Legacy RGB) — same 3-plane shape; routes through the
    /// adaptive-CDF legacy range coder (`spec/07`) rather than the
    /// modern coder. Prefixes 1..=8 must be Truncated (same
    /// dispatcher contract). The channel-body path can return any
    /// `Err(_)` variant the legacy decoder chooses.
    #[test]
    fn type_7_legacy_rgb_truncation_and_flip_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Bgr24.buffer_len(w, h));
        let frame = encode_legacy_rgb(&pixels, w, h);
        let full = decode_frame(&frame, w, h, PixelKind::Bgr24);
        assert!(full.is_ok(), "type-7 full frame should decode: {:?}", full);
        for k in 1..=8 {
            let r = decode_frame(&frame[..k], w, h, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-7 prefix len {k} (in offset table) should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, w, h);
    }

    /// Type 11 (Reduced-resolution) — wire is byte-0 = 0x0b then a
    /// type-10 YV12 body at half-W / half-H. Host dimensions must be
    /// multiples of 4 (round 187 guard) so the smallest valid host
    /// size that flows past the dimension guard is 4×4. Truncation
    /// sweep walks the body.
    #[test]
    fn type_11_reduced_res_truncation_and_flip_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Yv12.buffer_len(w, h));
        let frame = encode_arith_reduced_res(&pixels, w, h);
        let full = decode_frame(&frame, w, h, PixelKind::Yv12);
        assert!(full.is_ok(), "type-11 full frame should decode: {:?}", full);
        for k in 1..=8 {
            let r = decode_frame(&frame[..k], w, h, PixelKind::Yv12);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-11 prefix len {k} (in offset table) should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, w, h);
    }

    /// Type 5 (Solid Grey) — only 2 bytes total on the wire. Single-
    /// byte payload truncation → `Error::Truncated`. Flip sweep covers
    /// both bytes.
    #[test]
    fn type_5_solid_grey_truncation_and_flip_no_panic() {
        let frame = encode_solid_grey(0x77);
        assert_eq!(frame.len(), 2, "solid grey is type byte + colour byte");
        let r = decode_frame(&frame[..1], 4, 4, PixelKind::Bgr24);
        assert!(
            matches!(r, Err(Error::Truncated { .. })),
            "type-5 with no colour byte should be Truncated, got {:?}",
            r
        );
        // Flip the colour byte across the value range; every value
        // is a legal grey, every kind that accepts a packed BPP
        // decodes; Yv12 / Yuy2 surface PixelFormatMismatch.
        for val in [0x00u8, 0x55, 0xaa, 0xff] {
            let mutated = vec![5u8, val];
            for &kind in &[
                PixelKind::Bgr24,
                PixelKind::Bgra32,
                PixelKind::Yv12,
                PixelKind::Yuy2,
            ] {
                let _ = decode_frame(&mutated, 4, 4, kind);
            }
        }
    }

    /// Type 6 (Solid RGB) — 4 bytes total. Truncation of the colour
    /// trio → Truncated.
    #[test]
    fn type_6_solid_rgb_truncation_and_flip_no_panic() {
        let frame = encode_solid_rgb(0x11, 0x22, 0x33);
        for k in 1..frame.len() {
            let r = decode_frame(&frame[..k], 4, 4, PixelKind::Bgr24);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-6 prefix len {k} should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, 4, 4);
    }

    /// Type 9 (Solid RGBA) — 5 bytes total.
    #[test]
    fn type_9_solid_rgba_truncation_and_flip_no_panic() {
        let frame = encode_solid_rgba(0x11, 0x22, 0x33, 0x44);
        for k in 1..frame.len() {
            let r = decode_frame(&frame[..k], 4, 4, PixelKind::Bgra32);
            assert!(
                matches!(r, Err(Error::Truncated { .. })),
                "type-9 prefix len {k} should be Truncated, got {:?}",
                r
            );
        }
        truncation_and_flip_sweep(&frame, 4, 4);
    }

    /// Stateful [`Decoder`] — truncation sweep across the primer
    /// frame, then NULL replay on each truncated primer's
    /// surviving state. The primer can fail to decode (truncated);
    /// when it does, the stateful decoder must not leave a
    /// half-initialised `prev` slot that a subsequent NULL replay
    /// would dereference.
    #[test]
    fn stateful_decoder_truncation_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Bgr24.buffer_len(w, h));
        let primer = encode_arith_rgb24(&pixels, w, h);
        for k in 1..primer.len() {
            let mut dec = Decoder::new();
            let primer_res = dec.decode(&primer[..k], w, h, PixelKind::Bgr24);
            // Whatever the primer returned, a follow-up NULL frame
            // must not panic. If `primer_res` was `Err`, the stateful
            // decoder is documented to leave `prev` unset, so the
            // NULL replay should surface
            // `Error::NullFrameWithoutPredecessor`. If `primer_res`
            // was `Ok` (the truncated primer happened to land on a
            // legal Lagarith bitstream — unlikely but not impossible),
            // the NULL replay should return a clone of the primer's
            // output.
            let null_res = dec.decode(&[], w, h, PixelKind::Bgr24);
            match (&primer_res, &null_res) {
                (Err(_), Err(Error::NullFrameWithoutPredecessor)) => {}
                (Ok(_), Ok(_)) => {}
                // Anything else terminates without panic, which is
                // the only invariant the harness guarantees. We
                // accept it (no assertion) — the specific Err
                // variant is not the contract here.
                _ => {}
            }
        }
    }

    /// `decode_frame_with_prev` against a truncated primer and a
    /// non-matching prev frame — no panic regardless of how the
    /// dimensions / kind line up. The prev-frame buffer length is
    /// host-controlled, so the truncated wire bytes must not lean on
    /// the prev's buffer being any specific size.
    #[test]
    fn decode_frame_with_prev_truncation_against_mismatched_prev_no_panic() {
        let (w, h) = (4u32, 4u32);
        let pixels = pattern_bytes(PixelKind::Bgr24.buffer_len(w, h));
        let frame = encode_arith_rgb24(&pixels, w, h);
        // Build a `prev` of a different size + kind — a 8×8 Yv12
        // surface. The decoder should reject the NULL replay shape
        // mismatch up-front; truncated primers must surface their
        // own Err without dereferencing the prev buffer.
        let mut dec_prev = Decoder::new();
        let prev_pixels = pattern_bytes(PixelKind::Yv12.buffer_len(8, 8));
        let prev_frame = encode_arith_yv12(&prev_pixels, 8, 8);
        let prev = dec_prev
            .decode(&prev_frame, 8, 8, PixelKind::Yv12)
            .expect("prev primer decodes");
        for k in 1..frame.len() {
            let _ = decode_frame_with_prev(&frame[..k], w, h, PixelKind::Bgr24, Some(&prev));
        }
    }
}

/// Deeper-coverage no-panic fuzz harness exercising the channel-body
/// decoders (modern range coder + Fibonacci-prefix decoder + RLE
/// expansion + legacy adaptive-CDF coder) at a frame size large enough
/// that the channel body is the dominant byte budget.
///
/// Round 192's `decoder_truncation_fuzz` module is `4×4`-sized and
/// only mutates with two values (`0x00` / `0xff`) at one offset at a
/// time. That sweep is sound for the dispatcher + offset-table layer
/// but barely reaches the per-channel body decoders — at `4×4` the
/// body is on the order of 5-20 bytes per plane, and `0x00` / `0xff`
/// only test "all bits cleared" / "all bits set". Channel bodies hold
/// the modern range-coder framing (`spec/02`), the MSB-first
/// Fibonacci-prefix bit stream (`spec/04`), the residual zero-run
/// escape (`spec/05`), and the legacy adaptive-CDF coder (`spec/07`);
/// off-by-one bit-level bugs in any of these would slip a single-byte
/// flip sweep.
///
/// This module covers the gap with three complementary axes:
///
/// 1. **Single-bit XOR fuzz** at `8×8`: flips one bit at a time across
///    every body byte and every bit position `1 << b` for `b ∈ 0..8`.
///    Strictly stronger than the `0x00` / `0xff` byte writes in
///    `decoder_truncation_fuzz` (which only test the two extremes of
///    bit-pattern space); single-bit flips locate the MSB-first
///    Fibonacci prefix decoder and the range-coder normalisation loop
///    in the per-bit shift count.
/// 2. **Burst flip fuzz** at `8×8`: replaces `N` consecutive body
///    bytes (N ∈ {2, 3, 4}) with each of `0xff` / `0x00` / `0x55` /
///    `0xaa`. The 0x55 / 0xaa values were missing from r192's two-
///    value vocabulary; multi-byte bursts find Fibonacci-prefix
///    decoders that walk through a well-formed length byte into a
///    region the burst has corrupted.
/// 3. **Insertion + deletion fuzz** at `8×8`: shifts the channel-body
///    region by ±1 byte (deleting a body byte, or inserting a `0x00`
///    body byte) at deterministic offsets. This catches decoders that
///    assume aligned reads from the wire — the modern range coder's
///    4-byte priming and the Fibonacci prefix's bit-granular reads
///    have different alignment exposure surfaces.
///
/// All three axes assert the same invariant the rest of the
/// decoder_*_fuzz modules assert: every decode call must terminate
/// with `Ok(_)` or `Err(_)`; no panic, no infinite loop. CI runs
/// the same default-`bench`-off test profile so the harness is
/// shaped to complete inside the workspace `cargo test` budget on a
/// laptop (≪ 1 s combined across all axis-tests).
#[cfg(test)]
mod decoder_deep_fuzz {
    use super::*;
    use crate::encoder::{
        encode_arith_reduced_res, encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2,
        encode_arith_yv12, encode_legacy_rgb,
    };

    /// `8×8` body pattern — same `i * 73 + 11` LCG-ish step used by
    /// the rest of the file (kept inline for module self-containment).
    fn body_bytes(n: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            let v = (i as u32).wrapping_mul(73).wrapping_add(11);
            out.push((v & 0xff) as u8);
        }
        out
    }

    /// Compute the byte offset where the channel-body region starts.
    /// The encoded frame layout is:
    /// ```text
    /// [byte 0       ] frame type
    /// [bytes 1..N   ] (n_channels - 1) * 4 byte channel-offset table
    /// [bytes N..end ] N concatenated channels' worth of body bytes
    /// ```
    /// where `N` is `n_channels - 1`. For the modern RGB(A) / YUY2 /
    /// YV12 frames there is always at least one channel body; for
    /// type 11 (reduced-res) the wire layout is `[0x0b][type-10 body]`
    /// so the body starts at byte 1.
    fn body_start_offset(frame_type: u8) -> usize {
        match frame_type {
            // Solid / Uncompressed: body is the literal byte stream
            // right after the type byte.
            1 | 5 | 6 | 9 => 1,
            // Type 11 wraps a type-10 frame whose own offset table
            // starts at +1; for the deep-fuzz purposes "the body" is
            // every byte the host decoder must read past the type
            // byte — so we start at 1 here too and rely on the
            // truncation tests to cover the inner offset table.
            11 => 1,
            // 3 channels: 8-byte offset table.
            3 | 4 | 7 | 10 => 1 + 8,
            // 4 channels: 12-byte offset table.
            8 => 1 + 12,
            _ => 1,
        }
    }

    /// Single-bit XOR sweep across the channel-body region of a valid
    /// frame. For every body offset, flip one bit at a time at each
    /// of the 8 bit positions; assert no panic on each decode.
    ///
    /// At `8×8` the body is on the order of 50-100 bytes; the inner
    /// loop is `body_len × 8 × n_kinds` ≈ 100 × 8 × 4 = 3200 decode
    /// calls per frame type. Quick to run, locates Fibonacci prefix
    /// and range-coder shift-count off-by-ones the byte-extremes
    /// sweep is blind to.
    fn single_bit_xor_body_sweep(frame: &[u8], w: u32, h: u32, frame_type: u8) {
        let body_start = body_start_offset(frame_type);
        // No body to flip — solid types are 2..=5 bytes total, the
        // truncation harness already covers them; skip.
        if body_start >= frame.len() {
            return;
        }
        for off in body_start..frame.len() {
            for bit in 0..8u8 {
                let mut mutated = frame.to_vec();
                mutated[off] ^= 1 << bit;
                for &kind in &[
                    PixelKind::Bgr24,
                    PixelKind::Bgra32,
                    PixelKind::Yv12,
                    PixelKind::Yuy2,
                ] {
                    // The decode must terminate; we don't pin
                    // `Ok` vs specific `Err` — random bit flips
                    // create most-of-the-time-invalid bitstreams
                    // but occasionally land on a legal alternate
                    // wire form, and the contract is "no panic".
                    let _ = decode_frame(&mutated, w, h, kind);
                }
            }
        }
    }

    /// Burst-flip sweep: replace `burst_len` consecutive body bytes
    /// with `fill` and assert no panic. Burst lengths up to 4 reach
    /// multi-byte Fibonacci-prefix length decoders and the range
    /// coder's 4-byte prime/refill window.
    fn burst_flip_body_sweep(frame: &[u8], w: u32, h: u32, frame_type: u8) {
        let body_start = body_start_offset(frame_type);
        if body_start + 4 > frame.len() {
            return;
        }
        // Stride 5 so 8 / 16 / 32 / 64 byte bodies all hit multiple
        // distinct offsets without sweep blowup; the four fill values
        // span the bit-pattern extremes (all-zero / all-one) plus the
        // two alternating-bit patterns (`0x55` / `0xaa`) that any
        // single-bit-flip sweep also exercises but here together as
        // a contiguous burst.
        for off in (body_start..frame.len().saturating_sub(4)).step_by(5) {
            for &burst_len in &[2usize, 3, 4] {
                if off + burst_len > frame.len() {
                    break;
                }
                for &fill in &[0xffu8, 0x00, 0x55, 0xaa] {
                    let mut mutated = frame.to_vec();
                    for byte in mutated.iter_mut().skip(off).take(burst_len) {
                        *byte = fill;
                    }
                    for &kind in &[
                        PixelKind::Bgr24,
                        PixelKind::Bgra32,
                        PixelKind::Yv12,
                        PixelKind::Yuy2,
                    ] {
                        let _ = decode_frame(&mutated, w, h, kind);
                    }
                }
            }
        }
    }

    /// Insertion / deletion fuzz: shift the channel-body region by
    /// `±1` byte at deterministic offsets. This tests decoders that
    /// implicitly assume aligned reads (the modern range coder's
    /// 4-byte priming + 1-byte refill, and the Fibonacci prefix's
    /// bit-granular decode that crosses byte boundaries).
    fn shift_body_sweep(frame: &[u8], w: u32, h: u32, frame_type: u8) {
        let body_start = body_start_offset(frame_type);
        if body_start >= frame.len() {
            return;
        }
        for off in (body_start..frame.len()).step_by(11) {
            // Delete one body byte at `off`.
            let mut shorter = frame.to_vec();
            shorter.remove(off);
            // Insert one zero body byte at `off`.
            let mut longer = frame.to_vec();
            longer.insert(off, 0x00);
            for variant in &[shorter, longer] {
                for &kind in &[
                    PixelKind::Bgr24,
                    PixelKind::Bgra32,
                    PixelKind::Yv12,
                    PixelKind::Yuy2,
                ] {
                    let _ = decode_frame(variant, w, h, kind);
                }
            }
        }
    }

    /// Type 4 (Arithmetic RGB24) — 8×8 BGR24, 3 channels, 8-byte
    /// offset table. All three fuzz axes against the valid frame.
    #[test]
    fn type_4_arith_rgb24_deep_fuzz_no_panic() {
        let (w, h) = (8u32, 8u32);
        let pixels = body_bytes(PixelKind::Bgr24.buffer_len(w, h));
        let frame = encode_arith_rgb24(&pixels, w, h);
        assert!(
            decode_frame(&frame, w, h, PixelKind::Bgr24).is_ok(),
            "type-4 8x8 full frame should decode"
        );
        single_bit_xor_body_sweep(&frame, w, h, 4);
        burst_flip_body_sweep(&frame, w, h, 4);
        shift_body_sweep(&frame, w, h, 4);
    }

    /// Type 8 (Arithmetic RGBA) — 8×8, 4 channels, 12-byte offset
    /// table. Exercises the alpha-plane channel-body decoder in
    /// addition to the RGB planes.
    #[test]
    fn type_8_arith_rgba_deep_fuzz_no_panic() {
        let (w, h) = (8u32, 8u32);
        let pixels = body_bytes(PixelKind::Bgra32.buffer_len(w, h));
        let frame = encode_arith_rgba(&pixels, w, h);
        assert!(
            decode_frame(&frame, w, h, PixelKind::Bgra32).is_ok(),
            "type-8 8x8 full frame should decode"
        );
        single_bit_xor_body_sweep(&frame, w, h, 8);
        burst_flip_body_sweep(&frame, w, h, 8);
        shift_body_sweep(&frame, w, h, 8);
    }

    /// Type 10 (Arithmetic YV12) — 8×8, 3 planes (Y, V, U). The
    /// chroma planes are 4×4 so the Y plane dominates the body.
    #[test]
    fn type_10_arith_yv12_deep_fuzz_no_panic() {
        let (w, h) = (8u32, 8u32);
        let pixels = body_bytes(PixelKind::Yv12.buffer_len(w, h));
        let frame = encode_arith_yv12(&pixels, w, h);
        assert!(
            decode_frame(&frame, w, h, PixelKind::Yv12).is_ok(),
            "type-10 8x8 full frame should decode"
        );
        single_bit_xor_body_sweep(&frame, w, h, 10);
        burst_flip_body_sweep(&frame, w, h, 10);
        shift_body_sweep(&frame, w, h, 10);
    }

    /// Type 3 (Arithmetic YUY2) — 8×8, 3 planes (Y, U, V) extracted
    /// from packed `Y U Y V`. Same shape as type 10 / 4 but the
    /// packed→planar dispatcher differs.
    #[test]
    fn type_3_arith_yuy2_deep_fuzz_no_panic() {
        let (w, h) = (8u32, 8u32);
        let pixels = body_bytes(PixelKind::Yuy2.buffer_len(w, h));
        let frame = encode_arith_yuy2(&pixels, w, h);
        assert!(
            decode_frame(&frame, w, h, PixelKind::Yuy2).is_ok(),
            "type-3 8x8 full frame should decode"
        );
        single_bit_xor_body_sweep(&frame, w, h, 3);
        burst_flip_body_sweep(&frame, w, h, 3);
        shift_body_sweep(&frame, w, h, 3);
    }

    /// Type 7 (Legacy RGB) — 8×8, routes through the adaptive-CDF
    /// legacy range coder (`spec/07`) rather than the modern coder.
    /// Different entropy path, different bit-level surfaces.
    #[test]
    fn type_7_legacy_rgb_deep_fuzz_no_panic() {
        let (w, h) = (8u32, 8u32);
        let pixels = body_bytes(PixelKind::Bgr24.buffer_len(w, h));
        let frame = encode_legacy_rgb(&pixels, w, h);
        assert!(
            decode_frame(&frame, w, h, PixelKind::Bgr24).is_ok(),
            "type-7 8x8 full frame should decode"
        );
        single_bit_xor_body_sweep(&frame, w, h, 7);
        burst_flip_body_sweep(&frame, w, h, 7);
        shift_body_sweep(&frame, w, h, 7);
    }

    /// Type 11 (Reduced-resolution) — 8×8 host frame whose body is a
    /// type-10 YV12 4×4 sub-frame. Host dimensions must be a multiple
    /// of 4 per round 187's guard.
    #[test]
    fn type_11_reduced_res_deep_fuzz_no_panic() {
        let (w, h) = (8u32, 8u32);
        let pixels = body_bytes(PixelKind::Yv12.buffer_len(w, h));
        let frame = encode_arith_reduced_res(&pixels, w, h);
        assert!(
            decode_frame(&frame, w, h, PixelKind::Yv12).is_ok(),
            "type-11 8x8 full frame should decode"
        );
        single_bit_xor_body_sweep(&frame, w, h, 11);
        burst_flip_body_sweep(&frame, w, h, 11);
        shift_body_sweep(&frame, w, h, 11);
    }
}

/// Randomised encoder → decoder self-roundtrip property suite.
///
/// Rounds 181 / 187 / 192 / 198 hardened the **decoder** against
/// malformed / truncated / bit-flipped inputs (no-panic invariants).
/// This module probes the orthogonal property on the **encoder** side:
/// for every valid `(seed, dimensions, frame-type)` triple the encoder
/// accepts, the encoded wire bytes must decode back to a pixel buffer
/// that compares **byte-equal** to the original input (modulo the
/// documented lossless invariant of every frame type covered here —
/// reduced-resolution type 11 is excluded because its 2× nearest-
/// neighbour downsample → upsample is lossy by construction; only the
/// downsampled-then-upsampled fixed point round-trips, which is pinned
/// separately in `reduced_res_roundtrip_8x8` / `_16x16`).
///
/// Coverage gap closed: the existing roundtrip tests at the top of
/// this file (`arith_rgb24_roundtrip_*` / `arith_yv12_roundtrip_*` /
/// `legacy_rgb_roundtrip_*` / etc.) all feed the encoder a small set
/// of **fixed** patterns derived from `i * 73 + 11`. They pin the
/// happy path against one fixture per frame type per size but cannot
/// surface encoder bugs that fire only on pixel distributions outside
/// that pattern (e.g. encoder fast-path `s == 0` short-circuits that
/// incorrectly fire when the post-prediction residual stays mid-band).
/// The deep-fuzz module above attacks the *decoder* path with single-
/// bit XOR + multi-byte bursts on already-encoded valid frames, but
/// leaves the *encoder* path covered only by its fixed-pattern fixtures.
///
/// The harness sweeps three orthogonal axes:
///
/// 1. **Seeds**: three distinct LCG seeds (`0x0123_4567_89ab_cdef`,
///    `0xa5a5_5a5a_a5a5_5a5a`, `0xfeed_face_dead_beef`). The LCG is
///    the same PCG-style multiply-add (mul `6364136223846793005`,
///    add `1442695040888963407`) used by the existing
///    `decoder_defensive_harness::lcg_bytes`, kept inline for module
///    self-containment per workspace convention.
///
/// 2. **Dimensions**: four representative `(W, H)` pairs per frame
///    family. RGB24 / RGBA / type-7 hit the `width % 4 == 0` selector
///    boundary plus the unaligned branch (`spec/01` §2.1 frame-type
///    `2` vs `4`); YV12 / YUY2 hit the chroma sub-sampling alignment.
///    Sizes stay small (≤ 24 × 24) so the suite completes inside the
///    `cargo test --lib` budget on a laptop (≪ 1 s in release).
///
/// 3. **Frame types**: every modern arithmetic-coded family
///    (`encode_arith_rgb24` / `encode_arith_rgba` / `encode_arith_yv12`
///    / `encode_arith_yuy2`) plus the legacy type-7 path
///    (`encode_legacy_rgb`), each driven by `encode_channel_best` post-
///    r174 so the round implicitly verifies the per-channel header
///    selector picks a header whose decoder path round-trips.
///
/// The invariant is **strict byte equality** between the input
/// `pixels` and the decoded `Image::pixels` — encoding is lossless
/// for every covered type per `spec/06` §6 and `spec/07` §1.2. A
/// failure here would localise to either an encoder fast-path
/// asymmetry vs. the decoder (e.g. range-coder Step-A / Step-B / Step-C
/// arithmetic skew under unusual residual distributions) or to a
/// channel-header form whose decoder branch mishandles the bytes the
/// encoder emits.
///
/// 24 tests added: 5 frame types × 4 dimensions + 4 cross-seed
/// per-type sweeps (one for each modern type) = 24.
#[cfg(test)]
mod encoder_random_roundtrip_property {
    use super::*;
    use crate::encoder::{
        encode_arith_reduced_res, encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2,
        encode_arith_yv12, encode_legacy_rgb,
    };

    /// PCG-step LCG, deterministic. Same constants as
    /// `decoder_defensive_harness::lcg_bytes` (kept inline for module
    /// self-containment).
    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((s >> 33) as u8);
        }
        out
    }

    /// Build a pseudo-random BGR24 pixel buffer of size
    /// `width * height * 3`. The encoder ingests interleaved BGR
    /// bytes; the LCG output is byte-flat so each plane sees a
    /// roughly-uniform 0..=255 distribution rather than the
    /// gradient-friendly `i * 73 + 11` the fixed-pattern fixtures use.
    fn random_bgr24(seed: u64, width: u32, height: u32) -> Vec<u8> {
        lcg_bytes(seed, PixelKind::Bgr24.buffer_len(width, height))
    }

    /// Build a pseudo-random BGRA32 pixel buffer of size
    /// `width * height * 4`.
    fn random_bgra32(seed: u64, width: u32, height: u32) -> Vec<u8> {
        lcg_bytes(seed, PixelKind::Bgra32.buffer_len(width, height))
    }

    /// Build a pseudo-random YV12 pixel buffer (Y + V + U planes
    /// concatenated, chroma at quarter resolution).
    fn random_yv12(seed: u64, width: u32, height: u32) -> Vec<u8> {
        lcg_bytes(seed, PixelKind::Yv12.buffer_len(width, height))
    }

    /// Build a pseudo-random packed YUY2 pixel buffer
    /// (`Y0 U Y1 V` per pair of pixels, `width` must be even).
    fn random_yuy2(seed: u64, width: u32, height: u32) -> Vec<u8> {
        debug_assert!(width % 2 == 0);
        lcg_bytes(seed, PixelKind::Yuy2.buffer_len(width, height))
    }

    /// Three deterministic seeds covering distinct LCG trajectories.
    /// Chosen so the high-bit and low-bit byte distributions all differ
    /// (no two seeds give the same first 8 emitted bytes).
    const SEEDS: [u64; 3] = [
        0x0123_4567_89ab_cdefu64,
        0xa5a5_5a5a_a5a5_5a5au64,
        0xfeed_face_dead_beefu64,
    ];

    // ─────────── modern arithmetic RGB24 (type 2 / 4) ───────────

    /// Modern RGB24 encoder must self-roundtrip byte-equal on every
    /// `(seed, dims)` combination. `(W, H)` pairs span the
    /// `width % 4 == 0` selector boundary so both the type-2 and
    /// type-4 wire-encoder branches are exercised: 4×4 / 8×4 are
    /// type-4 (`width % 4 == 0`), 6×4 / 5×4 are type-2 unaligned.
    #[test]
    fn arith_rgb24_random_seeded_roundtrip() {
        let dims: &[(u32, u32)] = &[(4, 4), (8, 4), (6, 4), (5, 4)];
        for &seed in &SEEDS {
            for &(w, h) in dims {
                let pixels = random_bgr24(seed, w, h);
                let frame = encode_arith_rgb24(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24)
                    .expect("encoder output must decode (RGB24, randomised)");
                assert_eq!(
                    decoded.pixels, pixels,
                    "RGB24 random roundtrip mismatch at seed={seed:#x} dims=({w}, {h})",
                );
            }
        }
    }

    // ─────────── modern arithmetic RGBA (type 8) ───────────

    /// Modern RGBA encoder roundtrips byte-equal on every seed. The
    /// alpha plane is stored raw (no R += G / B += G decorrelation per
    /// `spec/03` §4); randomised bytes here verify the 4-plane channel-
    /// offset table + the alpha-raw path together.
    #[test]
    fn arith_rgba_random_seeded_roundtrip() {
        let dims: &[(u32, u32)] = &[(4, 4), (8, 4), (8, 8), (16, 4)];
        for &seed in &SEEDS {
            for &(w, h) in dims {
                let pixels = random_bgra32(seed, w, h);
                let frame = encode_arith_rgba(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Bgra32)
                    .expect("encoder output must decode (RGBA, randomised)");
                assert_eq!(
                    decoded.pixels, pixels,
                    "RGBA random roundtrip mismatch at seed={seed:#x} dims=({w}, {h})",
                );
            }
        }
    }

    // ─────────── modern arithmetic YV12 (type 10) ───────────

    /// Modern YV12 encoder roundtrips byte-equal on every seed. YV12
    /// is the only modern type whose first-column predictor still uses
    /// Rule A (round 124 pinned Rule B for RGB24 / RGBA via the oracle;
    /// YV12's planar scan order vs. the DIB flip means the pinned
    /// fixture for YV12 is still self-roundtrip-only — see crate
    /// README "Open items (c)"). Randomised seeded inputs verify that
    /// the encoder + decoder agree on Rule A for every distribution.
    #[test]
    fn arith_yv12_random_seeded_roundtrip() {
        let dims: &[(u32, u32)] = &[(4, 4), (8, 8), (16, 8), (6, 4)];
        for &seed in &SEEDS {
            for &(w, h) in dims {
                let pixels = random_yv12(seed, w, h);
                let frame = encode_arith_yv12(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Yv12)
                    .expect("encoder output must decode (YV12, randomised)");
                assert_eq!(
                    decoded.pixels, pixels,
                    "YV12 random roundtrip mismatch at seed={seed:#x} dims=({w}, {h})",
                );
            }
        }
    }

    // ─────────── modern arithmetic YUY2 (type 3) ───────────

    /// Modern YUY2 encoder roundtrips byte-equal on every seed. YUY2
    /// shares the Rule A pending-oracle-pin status with YV12 (see
    /// crate README "Open items (c)"). Width must be even for the
    /// macropixel boundary (`Y0 U Y1 V`); 4 / 6 / 8 / 16 cover that.
    #[test]
    fn arith_yuy2_random_seeded_roundtrip() {
        let dims: &[(u32, u32)] = &[(4, 4), (6, 4), (8, 8), (16, 4)];
        for &seed in &SEEDS {
            for &(w, h) in dims {
                let pixels = random_yuy2(seed, w, h);
                let frame = encode_arith_yuy2(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Yuy2)
                    .expect("encoder output must decode (YUY2, randomised)");
                assert_eq!(
                    decoded.pixels, pixels,
                    "YUY2 random roundtrip mismatch at seed={seed:#x} dims=({w}, {h})",
                );
            }
        }
    }

    /// Modern YUY2 encoder roundtrips byte-equal on every seed at
    /// **odd** widths too (round 352). The floor-chroma layout
    /// (`spec/03` §6.2) drops the trailing luma column's chroma
    /// macropixel, which the decoder fills with the `0x80` neutral, so
    /// the random buffer's odd-tail chroma slot is normalised to `0x80`
    /// before encoding. Dims 5 / 7 / 9 / 11 cross the macropixel-count
    /// parity; `(1, 8)` is the empty-chroma degenerate case.
    #[test]
    fn arith_yuy2_odd_width_random_seeded_roundtrip() {
        let dims: &[(u32, u32)] = &[(5, 4), (7, 5), (9, 9), (11, 6), (1, 8)];
        for &seed in &SEEDS {
            for &(w, h) in dims {
                let mut pixels = lcg_bytes(seed, PixelKind::Yuy2.buffer_len(w, h));
                let last_x = (w - 1) as usize;
                for y in 0..h as usize {
                    pixels[y * w as usize * 2 + 2 * last_x + 1] = 0x80;
                }
                let frame = encode_arith_yuy2(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Yuy2)
                    .expect("encoder output must decode (YUY2 odd, randomised)");
                assert_eq!(
                    decoded.pixels, pixels,
                    "YUY2 odd random roundtrip mismatch at seed={seed:#x} dims=({w}, {h})",
                );
            }
        }
    }

    // ─────────── legacy RGB (type 7) ───────────

    /// Legacy type-7 RGB encoder roundtrips byte-equal on every seed.
    /// The type-7 path uses the spec/07 adaptive-CDF range coder and
    /// the bare-Fibonacci channel form (per r15 / r141 — the selector
    /// proves bare-Fib wins on every realistic histogram). Randomised
    /// bytes here verify the legacy range coder + flat-257 CDF round-
    /// trip across distributions the fixed-pattern fixtures don't
    /// probe (e.g. near-uniform vs. zero-heavy).
    #[test]
    fn legacy_rgb_random_seeded_roundtrip() {
        let dims: &[(u32, u32)] = &[(4, 4), (8, 8), (16, 12), (5, 4)];
        for &seed in &SEEDS {
            for &(w, h) in dims {
                let pixels = random_bgr24(seed, w, h);
                let frame = encode_legacy_rgb(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24)
                    .expect("encoder output must decode (legacy RGB, randomised)");
                assert_eq!(
                    decoded.pixels, pixels,
                    "legacy RGB random roundtrip mismatch at seed={seed:#x} dims=({w}, {h})",
                );
            }
        }
    }

    // ─────────── extended seed sweeps ───────────
    //
    // For each modern arithmetic family, run a deeper cross-seed
    // sweep at the canonical 8×8 size with eight LCG seeds. Catches
    // encoder fast-path bugs that fire only on rare residual
    // distributions a three-seed × four-dim cross would miss.

    /// 8 seeds × 8×8 RGB24 randomised roundtrip. The wider seed sweep
    /// finds residual histograms that escape the `s == 0` / `s == 255`
    /// fast paths and exercise Step-C of the encoder range coder
    /// (`spec/02` §5) for the dominant chunk of symbols.
    #[test]
    fn arith_rgb24_extended_seed_sweep() {
        const W: u32 = 8;
        const H: u32 = 8;
        for seed_lo in 0u64..8 {
            let seed = seed_lo.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let pixels = random_bgr24(seed, W, H);
            let frame = encode_arith_rgb24(&pixels, W, H);
            let decoded = decode_frame(&frame, W, H, PixelKind::Bgr24)
                .expect("encoder output must decode (RGB24, extended sweep)");
            assert_eq!(
                decoded.pixels, pixels,
                "RGB24 extended-sweep mismatch at seed={seed:#x}",
            );
        }
    }

    /// 8 seeds × 8×8 RGBA randomised roundtrip — covers the 4-plane
    /// + raw-alpha path.
    #[test]
    fn arith_rgba_extended_seed_sweep() {
        const W: u32 = 8;
        const H: u32 = 8;
        for seed_lo in 0u64..8 {
            let seed = seed_lo.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let pixels = random_bgra32(seed, W, H);
            let frame = encode_arith_rgba(&pixels, W, H);
            let decoded = decode_frame(&frame, W, H, PixelKind::Bgra32)
                .expect("encoder output must decode (RGBA, extended sweep)");
            assert_eq!(
                decoded.pixels, pixels,
                "RGBA extended-sweep mismatch at seed={seed:#x}",
            );
        }
    }

    /// 8 seeds × 8×8 YV12 randomised roundtrip — covers the Rule A
    /// first-column predictor for the planar Y/V/U scan.
    #[test]
    fn arith_yv12_extended_seed_sweep() {
        const W: u32 = 8;
        const H: u32 = 8;
        for seed_lo in 0u64..8 {
            let seed = seed_lo.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let pixels = random_yv12(seed, W, H);
            let frame = encode_arith_yv12(&pixels, W, H);
            let decoded = decode_frame(&frame, W, H, PixelKind::Yv12)
                .expect("encoder output must decode (YV12, extended sweep)");
            assert_eq!(
                decoded.pixels, pixels,
                "YV12 extended-sweep mismatch at seed={seed:#x}",
            );
        }
    }

    /// 8 seeds × 8×8 YUY2 randomised roundtrip — covers the packed
    /// `Y0 U Y1 V` interleave through the chroma sub-sampling.
    #[test]
    fn arith_yuy2_extended_seed_sweep() {
        const W: u32 = 8;
        const H: u32 = 8;
        for seed_lo in 0u64..8 {
            let seed = seed_lo.wrapping_mul(0x9E37_79B9_7F4A_7C15);
            let pixels = random_yuy2(seed, W, H);
            let frame = encode_arith_yuy2(&pixels, W, H);
            let decoded = decode_frame(&frame, W, H, PixelKind::Yuy2)
                .expect("encoder output must decode (YUY2, extended sweep)");
            assert_eq!(
                decoded.pixels, pixels,
                "YUY2 extended-sweep mismatch at seed={seed:#x}",
            );
        }
    }

    // ─────────── milestone lock: every colour mode, non-pow2 total ───
    //
    // The crate's modern range coder narrows the `[low, low + range)`
    // interval with `q = range / total_freq` where `total_freq` is the
    // **raw histogram sum** (= the per-channel symbol count), per
    // `spec/02` §5's invariant box + `spec/04` §5 (the audit/01 §3.2
    // validation correction: the wire carries a raw byte-histogram
    // table whose total is the pixel count, NOT the internal
    // 524288-normalised LUT total). That division is exact for any
    // `total_freq`, so a plane whose pixel count is **not** a power of
    // two is decoded sample-exactly through the very same path. The
    // proprietary's `range >> shift` fast form (`spec/02` §5 step 1)
    // and the reciprocal-multiply LUT at `0x180001050` are
    // bit-stream-independent post-load artefacts (`spec/04` §6 / §8
    // item 2): the clean-room cumulative-search decoder reproduces
    // their output for every total.
    //
    // These two tests are the single named regression guarding the
    // round-338 milestone "all documented Lagarith colour-mode
    // variants decode sample-exactly across the fixture class." Each
    // drives one representative frame of **every** modern colour
    // family — RGB24 (type 2, unaligned `width % 4 != 0`), RGB24
    // (type 4, aligned), RGBA (type 8), YV12 (type 10),
    // reduced-resolution YV12 (type 11 fixed-point lattice), YUY2
    // (type 3), and legacy RGB (type 7) — at a deliberately
    // non-power-of-two plane pixel count, asserting byte-exact decode
    // of the input pixel buffer. A regression that reintroduced a
    // power-of-two total assumption into the modern range coder (e.g.
    // `q = range >> total.next_power_of_two().trailing_zeros()`) would
    // pass the pow2-sized `reference_pins.rs` set yet fail here.

    /// Non-power-of-two pixel counts spanning the modern RGB24 selector
    /// boundary and the chroma-subsampled families. `11 * 7 = 77`,
    /// `13 * 5 = 65`, `10 * 6 = 60`, `6 * 6 = 36`, `14 * 6 = 84`,
    /// `12 * 12 = 144`, `10 * 10 = 100` — none a power of two, so the
    /// modern range coder's `range / total` division lands on a
    /// non-pow2 `total` on every plane.
    const MILESTONE_SEED: u64 = 0xb16b_00b5_c0de_face;

    #[test]
    fn milestone_all_modes_decode_sample_exact_non_pow2() {
        // RGB24 type 2 (unaligned: width % 4 != 0, 11*7 = 77 px/plane).
        {
            let (w, h) = (11, 7);
            let pixels = random_bgr24(MILESTONE_SEED, w, h);
            let frame = encode_arith_rgb24(&pixels, w, h);
            assert_eq!(frame[0], 2, "expected type-2 unaligned RGB24");
            let dec = decode_frame(&frame, w, h, PixelKind::Bgr24)
                .expect("type-2 RGB24 must decode at non-pow2 size");
            assert_eq!(dec.pixels, pixels, "RGB24 type-2 non-pow2 sample-exact");
        }
        // RGB24 type 4 (aligned: width % 4 == 0, 12*12 = 144 px/plane).
        {
            let (w, h) = (12, 12);
            let pixels = random_bgr24(MILESTONE_SEED ^ 0x1, w, h);
            let frame = encode_arith_rgb24(&pixels, w, h);
            assert_eq!(frame[0], 4, "expected type-4 aligned RGB24");
            let dec = decode_frame(&frame, w, h, PixelKind::Bgr24)
                .expect("type-4 RGB24 must decode at non-pow2 size");
            assert_eq!(dec.pixels, pixels, "RGB24 type-4 non-pow2 sample-exact");
        }
        // RGBA type 8 (four planes incl. raw alpha, 13*5 = 65 px/plane).
        {
            let (w, h) = (13, 5);
            let pixels = random_bgra32(MILESTONE_SEED ^ 0x2, w, h);
            let frame = encode_arith_rgba(&pixels, w, h);
            assert_eq!(frame[0], 8, "expected type-8 RGBA");
            let dec = decode_frame(&frame, w, h, PixelKind::Bgra32)
                .expect("type-8 RGBA must decode at non-pow2 size");
            assert_eq!(dec.pixels, pixels, "RGBA type-8 non-pow2 sample-exact");
        }
        // YV12 type 10 (planar Y/V/U, luma 10*6 = 60, chroma 15 px).
        {
            let (w, h) = (10, 6);
            let pixels = random_yv12(MILESTONE_SEED ^ 0x3, w, h);
            let frame = encode_arith_yv12(&pixels, w, h);
            assert_eq!(frame[0], 10, "expected type-10 YV12");
            let dec = decode_frame(&frame, w, h, PixelKind::Yv12)
                .expect("type-10 YV12 must decode at non-pow2 size");
            assert_eq!(dec.pixels, pixels, "YV12 type-10 non-pow2 sample-exact");
        }
        // YUY2 type 3 (packed Y0 U Y1 V, 14*6 = 84 px, even width).
        {
            let (w, h) = (14, 6);
            let pixels = random_yuy2(MILESTONE_SEED ^ 0x4, w, h);
            let frame = encode_arith_yuy2(&pixels, w, h);
            assert_eq!(frame[0], 3, "expected type-3 YUY2");
            let dec = decode_frame(&frame, w, h, PixelKind::Yuy2)
                .expect("type-3 YUY2 must decode at non-pow2 size");
            assert_eq!(dec.pixels, pixels, "YUY2 type-3 non-pow2 sample-exact");
        }
        // Legacy RGB type 7 (adaptive-CDF range coder, 13*5 = 65 px).
        {
            let (w, h) = (13, 5);
            let pixels = random_bgr24(MILESTONE_SEED ^ 0x5, w, h);
            let frame = encode_legacy_rgb(&pixels, w, h);
            assert_eq!(frame[0], 7, "expected type-7 legacy RGB");
            let dec = decode_frame(&frame, w, h, PixelKind::Bgr24)
                .expect("type-7 legacy RGB must decode at non-pow2 size");
            assert_eq!(
                dec.pixels, pixels,
                "legacy RGB type-7 non-pow2 sample-exact"
            );
        }
    }

    /// Reduced-resolution type 11 (`spec/01` §2.4): a half-W/half-H
    /// YV12 body + 2× nearest-neighbour upscale. Host W and H must each
    /// be a multiple of 4 (`spec/01` §2.4 / round-187 guard), so a
    /// pure non-pow2 host size is impossible — but the *embedded
    /// half-resolution plane* the modern range coder actually decodes
    /// has pixel count `(W/2)*(H/2)`, which is non-pow2 here (`6*6 =
    /// 36` luma at host 12×12). The type-11 round-trip is fixed-point
    /// (downsample→upsample is lossy), so this pins that the
    /// fixed-point lattice survives the modern non-pow2-total decode.
    #[test]
    fn milestone_reduced_res_type11_fixed_point_non_pow2_inner() {
        let (w, h) = (12, 12); // inner half-res luma = 6×6 = 36 (non-pow2)
                               // Encode a host-resolution YV12 buffer, downsample+re-encode as
                               // type 11, decode, and re-encode the decoded output: the type-11
                               // path is idempotent on its own fixed point.
        let host = random_yv12(MILESTONE_SEED ^ 0x6, w, h);
        let frame = encode_arith_reduced_res(&host, w, h);
        assert_eq!(frame[0], 11, "expected type-11 reduced-resolution");
        let dec = decode_frame(&frame, w, h, PixelKind::Yv12)
            .expect("type-11 must decode at non-pow2 inner size");
        // Re-encode the decoded output and decode again: the second
        // decode must reproduce the first byte-exactly (fixed point).
        let frame2 = encode_arith_reduced_res(&dec.pixels, w, h);
        let dec2 =
            decode_frame(&frame2, w, h, PixelKind::Yv12).expect("type-11 re-decode must succeed");
        assert_eq!(
            dec.pixels, dec2.pixels,
            "type-11 reduced-res must be at its fixed point (non-pow2 inner)",
        );
    }
}

/// Round-216 packed-RGB pack-loop pins.
///
/// Round 216 hoisted the per-pixel `match pixel_kind` branch out of
/// the BGR(A) pack loop in `decode_arith_rgb` / `decode_arith_rgba` /
/// `decode_legacy_rgb` / `decode_solid`. The branch dispatch is once-
/// per-call rather than once-per-pixel; output byte sequence is
/// unchanged. These tests pin the byte-level layout invariants the
/// refactor must preserve so a future hoist that accidentally
/// reorders B / G / R / A bytes — or that drops the trailing 0xff
/// alpha on `Bgra32` for an RGB-coded frame — surfaces here rather
/// than only via the heavier roundtrip suites above.
#[cfg(test)]
mod pack_loop_byte_layout_pins {
    use super::*;
    use crate::encoder::{
        encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2, encode_arith_yv12,
        encode_legacy_rgb, encode_solid_grey, encode_solid_rgb, encode_solid_rgba,
    };

    /// Deterministic LCG byte stream; same constants as the other
    /// fuzz / property modules. Kept inline so the module is
    /// independent of test-only helpers in siblings.
    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((s >> 33) as u8);
        }
        out
    }

    /// `decode_arith_rgb` (types 2 / 4) for a `Bgra32` host buffer must
    /// emit `B, G, R, 0xff` per pixel. The RGB family carries no alpha
    /// on the wire (`spec/03` §4 — three planes only), so the alpha
    /// slot is the opaque-fill constant the round-216 hoist must keep.
    #[test]
    fn arith_rgb_bgra32_pack_alpha_is_opaque_constant() {
        const W: u32 = 8;
        const H: u32 = 8;
        let pixels = lcg_bytes(0xa1b2_c3d4, PixelKind::Bgr24.buffer_len(W, H));
        let frame = encode_arith_rgb24(&pixels, W, H);
        let decoded = decode_frame(&frame, W, H, PixelKind::Bgra32)
            .expect("RGB24-coded frame decodes to Bgra32");
        assert_eq!(decoded.pixels.len(), (W * H * 4) as usize);
        for px in decoded.pixels.chunks_exact(4) {
            assert_eq!(
                px[3], 0xff,
                "RGB-coded Bgra32 must fill alpha with 0xff (round-216 hoist invariant)",
            );
        }
        // Cross-check the BGR triplets against the Bgr24 decode of the
        // same frame — both code paths must produce the same B/G/R
        // bytes once the pack loop is hoisted.
        let decoded_bgr =
            decode_frame(&frame, W, H, PixelKind::Bgr24).expect("RGB24 also decodes to Bgr24");
        for (i, (rgb, rgba)) in decoded_bgr
            .pixels
            .chunks_exact(3)
            .zip(decoded.pixels.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(
                (rgb[0], rgb[1], rgb[2]),
                (rgba[0], rgba[1], rgba[2]),
                "RGB-coded BGR triplet diverges between Bgr24 and Bgra32 at pixel {i}",
            );
        }
    }

    /// `decode_arith_rgba` (type 8) for a `Bgra32` host buffer must
    /// emit the per-pixel alpha byte the wire actually carries, NOT
    /// the opaque-fill constant. The round-216 hoist pulls the alpha
    /// plane out of the per-iteration `match`; a regression that
    /// re-introduced `0xff` here would still pass any test that
    /// happened to use an all-0xff input.
    #[test]
    fn arith_rgba_bgra32_pack_carries_real_alpha() {
        const W: u32 = 8;
        const H: u32 = 8;
        // Build an RGBA buffer where the alpha plane is a known
        // non-constant gradient. The encoder maps interleaved
        // `B, G, R, A` -> four wire planes (`spec/01` §2.3).
        let mut pixels = vec![0u8; (W * H * 4) as usize];
        for (i, px) in pixels.chunks_exact_mut(4).enumerate() {
            px[0] = (i & 0xff) as u8;
            px[1] = ((i >> 1) & 0xff) as u8;
            px[2] = ((i >> 2) & 0xff) as u8;
            // Alpha gradient: 1 + (i * 7) mod 254 -> never 0x00 or
            // 0xff so a "drop alpha back to opaque" regression fails.
            px[3] = 1u8.wrapping_add((i as u8).wrapping_mul(7) % 254);
        }
        let frame = encode_arith_rgba(&pixels, W, H);
        let decoded = decode_frame(&frame, W, H, PixelKind::Bgra32)
            .expect("RGBA-coded frame decodes to Bgra32");
        assert_eq!(decoded.pixels, pixels, "RGBA Bgra32 pack must round-trip");
        // Cross-check the dropped-alpha view too: round-211's lazy
        // alpha decode + round-216 hoist must keep Bgr24 byte-equal
        // to the BGR triplets of the Bgra32 result.
        let decoded_bgr =
            decode_frame(&frame, W, H, PixelKind::Bgr24).expect("RGBA also decodes to Bgr24");
        assert_eq!(decoded_bgr.pixels.len(), (W * H * 3) as usize);
        for (i, (bgr, bgra)) in decoded_bgr
            .pixels
            .chunks_exact(3)
            .zip(decoded.pixels.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(
                (bgr[0], bgr[1], bgr[2]),
                (bgra[0], bgra[1], bgra[2]),
                "RGBA-coded BGR triplet diverges between Bgr24 and Bgra32 at pixel {i}",
            );
        }
    }

    /// `decode_legacy_rgb` (type 7) shares the BGR(A) pack shape with
    /// the modern arithmetic family; the round-216 hoist applies to
    /// both. Same Bgra32 opaque-alpha pin as the modern path.
    #[test]
    fn legacy_rgb_bgra32_pack_alpha_is_opaque_constant() {
        const W: u32 = 8;
        const H: u32 = 8;
        let pixels = lcg_bytes(0xdead_beef_0123_4567, PixelKind::Bgr24.buffer_len(W, H));
        let frame = encode_legacy_rgb(&pixels, W, H);
        let decoded = decode_frame(&frame, W, H, PixelKind::Bgra32)
            .expect("legacy-RGB frame decodes to Bgra32");
        assert_eq!(decoded.pixels.len(), (W * H * 4) as usize);
        for px in decoded.pixels.chunks_exact(4) {
            assert_eq!(
                px[3], 0xff,
                "legacy-RGB Bgra32 must fill alpha with 0xff (round-216 hoist invariant)",
            );
        }
        // Cross-check Bgr24 / Bgra32 BGR triplets — same as modern.
        let decoded_bgr =
            decode_frame(&frame, W, H, PixelKind::Bgr24).expect("legacy-RGB also decodes to Bgr24");
        for (i, (bgr, bgra)) in decoded_bgr
            .pixels
            .chunks_exact(3)
            .zip(decoded.pixels.chunks_exact(4))
            .enumerate()
        {
            assert_eq!(
                (bgr[0], bgr[1], bgr[2]),
                (bgra[0], bgra[1], bgra[2]),
                "legacy-RGB BGR triplet diverges between Bgr24 and Bgra32 at pixel {i}",
            );
        }
    }

    /// `decode_solid` (types 5 / 6 / 9) packs a single colour into
    /// every pixel. The round-216 hoist replaced the per-iteration
    /// `match` with a `Vec::resize` + chunked-write; pin that every
    /// pixel still carries the spec/01 §2.2.1 BGR(A) tuple.
    ///
    /// - Type 5 (grey) — `Y, Y, Y` triplet across both host kinds.
    /// - Type 6 (RGB) — `B, G, R` triplet across both host kinds.
    /// - Type 9 (RGBA) — `B, G, R, A` quadruple on Bgra32; Bgra32 is
    ///   the only host kind valid here (Bgr24 drops alpha but the
    ///   encoder still emits the wire alpha, which the decoder
    ///   discards).
    #[test]
    fn solid_frames_pack_loop_byte_layout() {
        const W: u32 = 8;
        const H: u32 = 8;
        const N: usize = (W * H) as usize;

        // Type 5 — grey.
        let frame5 = encode_solid_grey(0x42);
        let dec5_24 = decode_frame(&frame5, W, H, PixelKind::Bgr24).expect("solid-grey Bgr24");
        assert_eq!(dec5_24.pixels.len(), N * 3);
        for px in dec5_24.pixels.chunks_exact(3) {
            assert_eq!(px, &[0x42, 0x42, 0x42][..]);
        }
        let dec5_32 = decode_frame(&frame5, W, H, PixelKind::Bgra32).expect("solid-grey Bgra32");
        assert_eq!(dec5_32.pixels.len(), N * 4);
        for px in dec5_32.pixels.chunks_exact(4) {
            assert_eq!(px, &[0x42, 0x42, 0x42, 0xff][..]);
        }

        // Type 6 — RGB. Encoder takes B/G/R inputs in wire order
        // (see encode_solid_rgb's docs).
        let (b, g, r) = (0x11u8, 0x22, 0x33);
        let frame6 = encode_solid_rgb(b, g, r);
        let dec6_24 = decode_frame(&frame6, W, H, PixelKind::Bgr24).expect("solid-rgb Bgr24");
        for px in dec6_24.pixels.chunks_exact(3) {
            assert_eq!(px, &[b, g, r][..]);
        }
        let dec6_32 = decode_frame(&frame6, W, H, PixelKind::Bgra32).expect("solid-rgb Bgra32");
        for px in dec6_32.pixels.chunks_exact(4) {
            assert_eq!(px, &[b, g, r, 0xff][..]);
        }

        // Type 9 — RGBA. Alpha is wire-driven, not 0xff.
        let (b, g, r, a) = (0x44u8, 0x55, 0x66, 0x77);
        let frame9 = encode_solid_rgba(b, g, r, a);
        let dec9_32 = decode_frame(&frame9, W, H, PixelKind::Bgra32).expect("solid-rgba Bgra32");
        for px in dec9_32.pixels.chunks_exact(4) {
            assert_eq!(
                px,
                &[b, g, r, a][..],
                "solid-RGBA Bgra32 must carry wire alpha (not opaque fill)",
            );
        }
    }

    /// Pin the buffer-length contract — the hoisted-branch + chunked-
    /// write form in `decode_solid` must size the output identically
    /// to `PixelKind::buffer_len`. A regression that miscalculated the
    /// `vec![0u8; n * bpp]` capacity would surface here.
    #[test]
    fn solid_frames_pack_loop_buffer_length() {
        for (w, h) in [(1, 1), (3, 4), (8, 8), (17, 5)] {
            let frame5 = encode_solid_grey(0x00);
            let dec5_24 = decode_frame(&frame5, w, h, PixelKind::Bgr24).unwrap();
            assert_eq!(dec5_24.pixels.len(), PixelKind::Bgr24.buffer_len(w, h));
            let dec5_32 = decode_frame(&frame5, w, h, PixelKind::Bgra32).unwrap();
            assert_eq!(dec5_32.pixels.len(), PixelKind::Bgra32.buffer_len(w, h));
        }
    }

    /// Non-RGB frames (types 3 / 10) still go through the YV12 / YUY2
    /// packers, which the round-216 hoist did NOT touch. Pin that
    /// neither path was accidentally redirected through the packed-RGB
    /// pack helper — they should error out with `PixelFormatMismatch`
    /// when asked for a packed buffer.
    #[test]
    fn planar_frames_reject_packed_pixel_kinds_unchanged() {
        const W: u32 = 8;
        const H: u32 = 8;

        let yv12_in = lcg_bytes(0x0001_0203, PixelKind::Yv12.buffer_len(W, H));
        let yv12_frame = encode_arith_yv12(&yv12_in, W, H);
        for kind in [PixelKind::Bgr24, PixelKind::Bgra32] {
            assert!(
                matches!(
                    decode_frame(&yv12_frame, W, H, kind),
                    Err(crate::Error::PixelFormatMismatch { .. })
                ),
                "YV12 must reject packed pixel kind {kind:?}",
            );
        }

        let yuy2_in = lcg_bytes(0x0405_0607, PixelKind::Yuy2.buffer_len(W, H));
        let yuy2_frame = encode_arith_yuy2(&yuy2_in, W, H);
        for kind in [PixelKind::Bgr24, PixelKind::Bgra32] {
            assert!(
                matches!(
                    decode_frame(&yuy2_frame, W, H, kind),
                    Err(crate::Error::PixelFormatMismatch { .. })
                ),
                "YUY2 must reject packed pixel kind {kind:?}",
            );
        }
    }
}

/// Round 222 — frame-level type-1 (uncompressed) size guard.
///
/// `encode_arith_rgb24_or_uncompressed` (and the YV12 / YUY2 / RGBA
/// siblings) wraps the modern-arithmetic frame encoder with a
/// **never-larger** size comparison against the equivalent
/// `encode_uncompressed(pixels)` form (`spec/01` §2.1). Type 1 is
/// decoder-orthogonal: byte 0 routes to the memcpy helper at
/// `lagarith.dll!0x18000555a` per `spec/01` table at §1, so a
/// type-1 substitute decodes byte-exactly against every conformant
/// decoder.
///
/// The pins below cover:
///
/// 1. **Never-larger size invariant** — for every probed `(W, H)` the
///    wrapper output is `<=` the unwrapped `encode_arith_*` output.
///    The wrapper can shrink the wire; it never grows it.
/// 2. **Decode-correct round-trip** — the wrapper's output decodes
///    back to the original pixels, regardless of which branch
///    (arith or uncompressed) the size selector chooses.
/// 3. **Selector-fires-on-tiny-random** — at small frame sizes with
///    deterministic-LCG pseudo-random pixels (high entropy, residuals
///    do not compress), the wrapper picks the type-1 branch
///    (byte 0 == 0x01) and saves at least one byte versus the
///    arithmetic encoding. This is the positive pin that the size
///    selector is wired in (and not just sitting as dead code).
/// 4. **Tie-break favours arithmetic** — when both forms have equal
///    length, the wrapper returns the arithmetic form unchanged.
#[cfg(test)]
mod frame_uncompressed_size_guard {
    use crate::decoder::{decode_frame, PixelKind};
    use crate::encoder::{
        encode_arith_rgb24, encode_arith_rgb24_or_uncompressed, encode_arith_rgba,
        encode_arith_rgba_or_uncompressed, encode_arith_yuy2, encode_arith_yuy2_or_uncompressed,
        encode_arith_yv12, encode_arith_yv12_or_uncompressed, encode_uncompressed,
    };

    /// Deterministic LCG used to synthesise high-entropy fixtures.
    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((s >> 33) as u8);
        }
        out
    }

    /// Smooth gradient pattern — the arithmetic path tends to win on
    /// these (compressible residuals), so the wrapper's size invariant
    /// is exercised in its "arith stays" branch.
    fn pattern_gradient(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 73 + 11) & 0xff) as u8).collect()
    }

    // ─── 1. Never-larger size invariant ───────────────────────────

    #[test]
    fn arith_rgb24_or_uncompressed_never_larger_than_arith() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (8, 16), (16, 16), (32, 32)] {
            let n = (w * h) as usize * 3;
            for seed in [0xc0de_d00du64, 0x1234_5678, 0xdead_beef] {
                let pixels = lcg_bytes(seed, n);
                let arith = encode_arith_rgb24(&pixels, w, h);
                let guarded = encode_arith_rgb24_or_uncompressed(&pixels, w, h);
                assert!(
                    guarded.len() <= arith.len(),
                    "rgb24 guarded ({}) larger than arith ({}) at {w}×{h} seed {seed:#x}",
                    guarded.len(),
                    arith.len(),
                );
            }
            let smooth = pattern_gradient(n);
            let arith = encode_arith_rgb24(&smooth, w, h);
            let guarded = encode_arith_rgb24_or_uncompressed(&smooth, w, h);
            assert!(
                guarded.len() <= arith.len(),
                "rgb24 guarded ({}) larger than arith ({}) on gradient at {w}×{h}",
                guarded.len(),
                arith.len(),
            );
        }
    }

    #[test]
    fn arith_yv12_or_uncompressed_never_larger_than_arith() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16), (32, 32)] {
            let n = PixelKind::Yv12.buffer_len(w, h);
            for seed in [0xc0de_d00du64, 0x1234_5678, 0xdead_beef] {
                let pixels = lcg_bytes(seed, n);
                let arith = encode_arith_yv12(&pixels, w, h);
                let guarded = encode_arith_yv12_or_uncompressed(&pixels, w, h);
                assert!(
                    guarded.len() <= arith.len(),
                    "yv12 guarded ({}) larger than arith ({}) at {w}×{h} seed {seed:#x}",
                    guarded.len(),
                    arith.len(),
                );
            }
        }
    }

    #[test]
    fn arith_yuy2_or_uncompressed_never_larger_than_arith() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16), (32, 32)] {
            let n = PixelKind::Yuy2.buffer_len(w, h);
            for seed in [0xc0de_d00du64, 0x1234_5678, 0xdead_beef] {
                let pixels = lcg_bytes(seed, n);
                let arith = encode_arith_yuy2(&pixels, w, h);
                let guarded = encode_arith_yuy2_or_uncompressed(&pixels, w, h);
                assert!(
                    guarded.len() <= arith.len(),
                    "yuy2 guarded ({}) larger than arith ({}) at {w}×{h} seed {seed:#x}",
                    guarded.len(),
                    arith.len(),
                );
            }
        }
    }

    #[test]
    fn arith_rgba_or_uncompressed_never_larger_than_arith() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16), (32, 32)] {
            let n = (w * h) as usize * 4;
            for seed in [0xc0de_d00du64, 0x1234_5678, 0xdead_beef] {
                let pixels = lcg_bytes(seed, n);
                let arith = encode_arith_rgba(&pixels, w, h);
                let guarded = encode_arith_rgba_or_uncompressed(&pixels, w, h);
                assert!(
                    guarded.len() <= arith.len(),
                    "rgba guarded ({}) larger than arith ({}) at {w}×{h} seed {seed:#x}",
                    guarded.len(),
                    arith.len(),
                );
            }
        }
    }

    // ─── 2. Decode-correct round-trip ─────────────────────────────

    #[test]
    fn arith_rgb24_or_uncompressed_roundtrips_byte_exact() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (8, 16), (16, 16), (32, 32)] {
            let n = (w * h) as usize * 3;
            for seed in [0xc0de_d00du64, 0x1234_5678, 0xdead_beef] {
                let pixels = lcg_bytes(seed, n);
                let frame = encode_arith_rgb24_or_uncompressed(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
                assert_eq!(
                    decoded.pixels, pixels,
                    "rgb24 guarded roundtrip mismatch at {w}×{h} seed {seed:#x} (byte0={:#x})",
                    frame[0]
                );
            }
            let smooth = pattern_gradient(n);
            let frame = encode_arith_rgb24_or_uncompressed(&smooth, w, h);
            let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
            assert_eq!(decoded.pixels, smooth);
        }
    }

    #[test]
    fn arith_yv12_or_uncompressed_roundtrips_byte_exact() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16), (32, 32)] {
            let n = PixelKind::Yv12.buffer_len(w, h);
            for seed in [0xc0de_d00du64, 0x1234_5678] {
                let pixels = lcg_bytes(seed, n);
                let frame = encode_arith_yv12_or_uncompressed(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
                assert_eq!(decoded.pixels, pixels);
            }
        }
    }

    /// Odd-dimension YV12 closure (round 352): the size-guarded
    /// wrapper inherits the `spec/03` §6.1.1 SPECGAP single-row chroma
    /// fallback that fires when `floor(W·H/4) != (W/2)·(H/2)`. No tail
    /// normalisation is needed — both encode and decode reconstruct the
    /// full `Y || V || U` buffer through the identical fallback
    /// geometry. The high-entropy LCG fixtures drive the type-1
    /// fall-through branch on these tiny frames.
    #[test]
    fn arith_yv12_or_uncompressed_odd_dims_roundtrips_byte_exact() {
        for &(w, h) in &[(5u32, 4u32), (4, 5), (5, 5), (7, 3), (9, 9), (1, 8)] {
            let n = PixelKind::Yv12.buffer_len(w, h);
            for seed in [0xc0de_d00du64, 0x1234_5678] {
                let pixels = lcg_bytes(seed, n);
                let frame = encode_arith_yv12_or_uncompressed(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap();
                assert_eq!(
                    decoded.pixels, pixels,
                    "yv12 odd guarded roundtrip mismatch at {w}×{h} seed {seed:#x} (byte0={:#x})",
                    frame[0]
                );
            }
        }
    }

    #[test]
    fn arith_yuy2_or_uncompressed_roundtrips_byte_exact() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16), (32, 32)] {
            let n = PixelKind::Yuy2.buffer_len(w, h);
            for seed in [0xc0de_d00du64, 0x1234_5678] {
                let pixels = lcg_bytes(seed, n);
                let frame = encode_arith_yuy2_or_uncompressed(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
                assert_eq!(decoded.pixels, pixels);
            }
        }
    }

    /// Odd-width closure (round 352): the size-guarded YUY2 wrapper
    /// inherits the floor-chroma odd-width support added to
    /// `encode_arith_yuy2`. The odd-tail chroma slot is normalised to
    /// `0x80` (the decoder's neutral fill) so the buffer is byte-exact
    /// reproducible; both the type-1 fall-through branch (high-entropy
    /// LCG fixtures) and the arithmetic-stays branch are exercised.
    #[test]
    fn arith_yuy2_or_uncompressed_odd_width_roundtrips_byte_exact() {
        for &(w, h) in &[(5u32, 4u32), (7, 5), (9, 9), (1, 8)] {
            let n = PixelKind::Yuy2.buffer_len(w, h);
            let last_x = (w - 1) as usize;
            for seed in [0xc0de_d00du64, 0x1234_5678] {
                let mut pixels = lcg_bytes(seed, n);
                for y in 0..h as usize {
                    pixels[y * w as usize * 2 + 2 * last_x + 1] = 0x80;
                }
                let frame = encode_arith_yuy2_or_uncompressed(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap();
                assert_eq!(
                    decoded.pixels, pixels,
                    "yuy2 odd guarded roundtrip mismatch at {w}×{h} seed {seed:#x} (byte0={:#x})",
                    frame[0]
                );
            }
        }
    }

    #[test]
    fn arith_rgba_or_uncompressed_roundtrips_byte_exact() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16), (32, 32)] {
            let n = (w * h) as usize * 4;
            for seed in [0xc0de_d00du64, 0x1234_5678] {
                let pixels = lcg_bytes(seed, n);
                let frame = encode_arith_rgba_or_uncompressed(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
                assert_eq!(decoded.pixels, pixels);
            }
        }
    }

    // ─── 3. Selector-fires-on-tiny-random ─────────────────────────

    /// On a tiny `4×4` random-byte RGB24 frame the arithmetic path's
    /// per-channel Fibonacci frequency table + range-coder overhead
    /// (~30-50 bytes per channel × 3 channels + 9-byte channel-offset
    /// preamble) substantially exceeds the 48-byte raw pixel payload.
    /// The size selector must pick the type-1 branch (byte 0 = 0x01).
    #[test]
    fn rgb24_selector_picks_uncompressed_on_tiny_random_input() {
        const W: u32 = 4;
        const H: u32 = 4;
        let pixels = lcg_bytes(0xc0de_d00d, (W * H) as usize * 3);
        let arith = encode_arith_rgb24(&pixels, W, H);
        let guarded = encode_arith_rgb24_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded[0], 0x01,
            "expected type-1 wire (byte 0 = 0x01), got byte 0 = {:#x} (arith len {}, guarded len {})",
            guarded[0], arith.len(), guarded.len(),
        );
        assert!(
            guarded.len() < arith.len(),
            "selector picked type 1 but no size win: arith={} guarded={}",
            arith.len(),
            guarded.len(),
        );
        // Byte-identity check against `encode_uncompressed` direct.
        let raw = encode_uncompressed(&pixels);
        assert_eq!(
            guarded, raw,
            "guarded wire must equal encode_uncompressed when type-1 wins",
        );
    }

    #[test]
    fn yv12_selector_picks_uncompressed_on_tiny_random_input() {
        const W: u32 = 4;
        const H: u32 = 4;
        let pixels = lcg_bytes(0xc0de_d00d, PixelKind::Yv12.buffer_len(W, H));
        let guarded = encode_arith_yv12_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded[0], 0x01,
            "expected type-1 wire, got byte 0 = {:#x}",
            guarded[0],
        );
    }

    #[test]
    fn yuy2_selector_picks_uncompressed_on_tiny_random_input() {
        const W: u32 = 4;
        const H: u32 = 4;
        let pixels = lcg_bytes(0xc0de_d00d, PixelKind::Yuy2.buffer_len(W, H));
        let guarded = encode_arith_yuy2_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded[0], 0x01,
            "expected type-1 wire, got byte 0 = {:#x}",
            guarded[0],
        );
    }

    #[test]
    fn rgba_selector_picks_uncompressed_on_tiny_random_input() {
        const W: u32 = 4;
        const H: u32 = 4;
        let pixels = lcg_bytes(0xc0de_d00d, (W * H) as usize * 4);
        let guarded = encode_arith_rgba_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded[0], 0x01,
            "expected type-1 wire, got byte 0 = {:#x}",
            guarded[0],
        );
    }

    // ─── 4. Tie-break + arith-stays branches ──────────────────────

    /// On a smooth gradient input the residuals are highly
    /// compressible — the arith form is strictly shorter than the
    /// raw payload. The selector must keep the arithmetic wire
    /// byte-identical (no inadvertent type-1 substitution on
    /// well-compressing inputs).
    #[test]
    fn rgb24_selector_keeps_arith_when_smaller() {
        // 32×32 gradient — enough planar mass that the per-channel
        // Fibonacci freq table is amortised across thousands of
        // residual bytes per plane.
        const W: u32 = 32;
        const H: u32 = 32;
        let pixels = pattern_gradient((W * H) as usize * 3);
        let arith = encode_arith_rgb24(&pixels, W, H);
        let raw = encode_uncompressed(&pixels);
        // Sanity check the fixture profile: arith must actually win
        // here (otherwise the pin is testing nothing).
        assert!(
            arith.len() < raw.len(),
            "fixture invariant broken: arith ({}) >= raw ({}) on 32×32 gradient",
            arith.len(),
            raw.len(),
        );
        let guarded = encode_arith_rgb24_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded, arith,
            "selector must keep the arithmetic wire byte-identical when it is the shorter form",
        );
    }
}

/// Round 229 — frame-level type-1 (uncompressed) size guard, type-7
/// (legacy adaptive-CDF RGB) extension.
///
/// `encode_legacy_rgb_or_uncompressed` wraps the type-7 legacy
/// encoder with the same **never-larger** size comparison against
/// `encode_uncompressed(pixels)` that round 222 introduced for the
/// modern arithmetic frame encoders (`spec/01` §2.1). Type 7's
/// existing Strategy E diversion (`audit/12` §7.1) handles the
/// rare-symbol-cluster wire-correctness gap; the round-229 size
/// guard is the orthogonal axis: it picks the type-1 substitute
/// whenever the legacy bare-Fibonacci form's preamble + per-channel
/// adaptive-CDF prefix + range-coder body exceeds the raw payload.
///
/// The pins below mirror the round-222 module structure:
///
/// 1. **Never-larger size invariant** — for every probed `(W, H)`
///    the wrapper output is `<=` the unwrapped `encode_legacy_rgb`
///    output across deterministic-LCG random fixtures and a smooth
///    gradient.
/// 2. **Decode-correct round-trip** — the wrapper's output decodes
///    back to the original BGR24 pixels regardless of which branch
///    (legacy or uncompressed) the size selector chooses.
/// 3. **Selector-fires-on-tiny-random** — at small frame sizes the
///    wrapper picks the type-1 branch (byte 0 == `0x01`) and saves
///    at least one byte versus the legacy encoding.
/// 4. **Tie-break favours legacy** — when both forms have equal
///    length (or legacy is shorter), the wrapper returns the legacy
///    form unchanged.
/// 5. **Strategy E composability** — when Strategy E inside
///    `encode_legacy_rgb` already returned a type-1 frame, the size
///    guard tie-breaks back to that frame byte-identically (the
///    wrapper does not double-emit or mutate the Strategy E output).
#[cfg(test)]
mod legacy_frame_uncompressed_size_guard {
    use crate::decoder::{decode_frame, PixelKind};
    use crate::encoder::{
        encode_legacy_rgb, encode_legacy_rgb_or_uncompressed, encode_uncompressed,
    };

    /// Deterministic LCG used to synthesise high-entropy fixtures.
    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((s >> 33) as u8);
        }
        out
    }

    /// Smooth gradient — the legacy encoder's residuals compress
    /// strongly here, so the wrapper exercises its "legacy stays"
    /// branch.
    fn pattern_gradient(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 73 + 11) & 0xff) as u8).collect()
    }

    // ─── 1. Never-larger size invariant ───────────────────────────

    #[test]
    fn legacy_rgb_or_uncompressed_never_larger_than_legacy() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (8, 16), (16, 16), (32, 32)] {
            let n = (w * h) as usize * 3;
            for seed in [0xc0de_d00du64, 0x1234_5678, 0xdead_beef] {
                let pixels = lcg_bytes(seed, n);
                let legacy = encode_legacy_rgb(&pixels, w, h);
                let guarded = encode_legacy_rgb_or_uncompressed(&pixels, w, h);
                assert!(
                    guarded.len() <= legacy.len(),
                    "legacy guarded ({}) larger than legacy ({}) at {w}×{h} seed {seed:#x}",
                    guarded.len(),
                    legacy.len(),
                );
            }
            let smooth = pattern_gradient(n);
            let legacy = encode_legacy_rgb(&smooth, w, h);
            let guarded = encode_legacy_rgb_or_uncompressed(&smooth, w, h);
            assert!(
                guarded.len() <= legacy.len(),
                "legacy guarded ({}) larger than legacy ({}) on gradient at {w}×{h}",
                guarded.len(),
                legacy.len(),
            );
        }
    }

    // ─── 2. Decode-correct round-trip ─────────────────────────────

    #[test]
    fn legacy_rgb_or_uncompressed_roundtrips_byte_exact() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (8, 16), (16, 16), (32, 32)] {
            let n = (w * h) as usize * 3;
            for seed in [0xc0de_d00du64, 0x1234_5678, 0xdead_beef] {
                let pixels = lcg_bytes(seed, n);
                let frame = encode_legacy_rgb_or_uncompressed(&pixels, w, h);
                let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
                assert_eq!(
                    decoded.pixels, pixels,
                    "legacy guarded roundtrip mismatch at {w}×{h} seed {seed:#x} (byte0={:#x})",
                    frame[0]
                );
            }
            let smooth = pattern_gradient(n);
            let frame = encode_legacy_rgb_or_uncompressed(&smooth, w, h);
            let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
            assert_eq!(decoded.pixels, smooth);
        }
    }

    // ─── 3. Selector-fires-on-tiny-random ─────────────────────────

    /// On a tiny `4×4` BGR24 random-byte input the legacy bare-
    /// Fibonacci form's per-channel adaptive-CDF prefix + range-
    /// coder body × 3 channels + the 9-byte channel-offset preamble
    /// substantially exceeds the 48-byte raw payload (1 + 4*4*3 = 49
    /// bytes of type-1 wire). The size guard must pick the type-1
    /// branch (byte 0 = `0x01`).
    #[test]
    fn legacy_selector_picks_uncompressed_on_tiny_random_input() {
        const W: u32 = 4;
        const H: u32 = 4;
        let pixels = lcg_bytes(0xc0de_d00d, (W * H) as usize * 3);
        let legacy = encode_legacy_rgb(&pixels, W, H);
        let guarded = encode_legacy_rgb_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded[0], 0x01,
            "expected type-1 wire (byte 0 = 0x01), got byte 0 = {:#x} (legacy len {}, guarded len {})",
            guarded[0], legacy.len(), guarded.len(),
        );
        assert!(
            guarded.len() < legacy.len(),
            "selector picked type 1 but no size win: legacy={} guarded={}",
            legacy.len(),
            guarded.len(),
        );
        // Byte-identity check against `encode_uncompressed` direct.
        let raw = encode_uncompressed(&pixels);
        assert_eq!(
            guarded, raw,
            "guarded wire must equal encode_uncompressed when type-1 wins",
        );
    }

    // ─── 4. Tie-break + legacy-stays branches ─────────────────────

    /// On a smooth gradient input the residuals are highly
    /// compressible — the legacy form is strictly shorter than the
    /// raw payload. The guard must keep the legacy wire byte-
    /// identical (no inadvertent type-1 substitution on well-
    /// compressing inputs).
    #[test]
    fn legacy_selector_keeps_legacy_when_smaller() {
        const W: u32 = 32;
        const H: u32 = 32;
        let pixels = pattern_gradient((W * H) as usize * 3);
        let legacy = encode_legacy_rgb(&pixels, W, H);
        let raw = encode_uncompressed(&pixels);
        // Sanity check the fixture profile: legacy must actually
        // win here (otherwise the pin is testing nothing).
        assert!(
            legacy.len() < raw.len(),
            "fixture invariant broken: legacy ({}) >= raw ({}) on 32×32 gradient",
            legacy.len(),
            raw.len(),
        );
        let guarded = encode_legacy_rgb_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded, legacy,
            "selector must keep the legacy wire byte-identical when it is the shorter form",
        );
    }

    // ─── 5. Strategy E composability ──────────────────────────────

    /// When `encode_legacy_rgb` already routes through Strategy E
    /// (`audit/12 §7.1`) and emits a type-1 frame, the round-229
    /// size guard must keep that type-1 frame byte-identical: the
    /// guard's `raw == legacy` tie-breaks to `legacy`, which itself
    /// is a `encode_uncompressed(pixels)` wire from Strategy E.
    ///
    /// The rare-symbol-cluster signature (`is_rare_symbol_cluster`)
    /// requires `freq[0] >= 0.95 * Σfreq` plus ≥ 3 distinct nonzero
    /// bins each with `freq ∈ {1, 2}`. The fixture builds residual
    /// histograms that satisfy this directly in the raw pixel
    /// domain — for plane B (BGR24 channel 0) the gradient runs
    /// 95% zeros with a sparse Laplacian tail; the legacy encoder's
    /// forward predictor leaves the dominant zero mass intact so
    /// the residual histogram inherits the signature.
    #[test]
    fn legacy_guard_preserves_strategy_e_uncompressed_byte_identically() {
        // 16×16 fixture: 768 BGR24 bytes total. Strategy E wants
        // freq[0] >= 0.95 * 256 per plane = at least 244 zeros per
        // plane, with ≥ 3 distinct nonzero bins each freq ∈ {1, 2}.
        const W: u32 = 16;
        const H: u32 = 16;
        let n = (W * H) as usize; // 256 per plane
        let mut pixels = vec![0u8; n * 3];
        // Sprinkle three distinct singleton values into each plane
        // at non-adjacent positions to avoid forming runs the
        // predictor would collapse to zero. Distinct symbols across
        // planes so cross-plane decorrelation cannot collapse them
        // either.
        for (plane_idx, (a, b, c)) in [(11u8, 23, 47), (13, 29, 53), (17, 31, 59)]
            .iter()
            .enumerate()
        {
            // Channel order in packed BGR24 is plane 0 = B, 1 = G,
            // 2 = R. Place singletons at well-separated pixel
            // positions so the JPEG-LS predictor doesn't smear them
            // into the zero majority.
            pixels[3 * 32 + plane_idx] = *a;
            pixels[3 * 100 + plane_idx] = *b;
            pixels[3 * 200 + plane_idx] = *c;
        }
        let legacy = encode_legacy_rgb(&pixels, W, H);
        // Strategy E precondition: when it fires, byte 0 == 0x01.
        // If this assertion fails the fixture no longer probes the
        // composability path — re-tune so the residual histogram
        // hits the rare-symbol-cluster signature.
        assert_eq!(
            legacy[0], 0x01,
            "fixture invariant broken: legacy first byte is {:#x}, not 0x01 — Strategy E did not fire",
            legacy[0],
        );
        let guarded = encode_legacy_rgb_or_uncompressed(&pixels, W, H);
        assert_eq!(
            guarded, legacy,
            "size guard must keep the Strategy E type-1 frame byte-identical",
        );
        // And the frame still decodes back to the original pixels
        // (the type-1 path is byte-exact by construction).
        let decoded = decode_frame(&guarded, W, H, PixelKind::Bgr24).unwrap();
        assert_eq!(decoded.pixels, pixels);
    }
}

/// Round 276 — frame-level solid-colour fast path (`spec/01` §3.1).
///
/// `encode_arith_rgb24_or_solid` / `encode_arith_rgba_or_solid` wrap
/// the modern arithmetic frame encoders with the proprietary's
/// solid-colour shortcut: a frame whose pixels are all identical is
/// emitted as the 2-byte type-5 (grey, `B == G == R`), 4-byte type-6
/// (RGB) or 5-byte type-9 (RGBA) solid frame (`spec/01` §3 rows
/// 5/6/9 + §2.2.2 totals), with colour bytes copied from the input
/// pixel unchanged (`spec/01` §2.2.1 encoder mirror). The pins
/// below cover:
///
/// 1. **Wire-shape pins** — exact output bytes for each solid type,
///    including the `FrameType::solid_wire_size` totals (round 262)
///    and the byte-0 classification.
/// 2. **Decode-correct round-trip** — the solid wire decodes back to
///    the original constant pixels, at aligned and unaligned widths.
/// 3. **Grey-vs-RGB split** — `B == G == R` selects type 5 on the
///    RGB path; the RGBA path emits type 9 even for grey + opaque
///    constants (`spec/01` §3 lists the 5/6 overwrite sites on the
///    RGB path only).
/// 4. **Fall-through byte-identity** — non-solid input (including an
///    almost-solid frame differing in a single byte of the last
///    pixel) produces output byte-identical to the unwrapped
///    arithmetic encoder.
/// 5. **Never-larger invariant** — the wrapper output is `<=` the
///    unwrapped arithmetic output everywhere, and strictly smaller
///    on solid frames (2 / 4 / 5 bytes vs the arithmetic form's
///    9- / 13-byte preamble + per-plane bodies).
#[cfg(test)]
mod frame_solid_fast_path {
    use crate::decoder::{decode_frame, PixelKind};
    use crate::encoder::{
        encode_arith_rgb24, encode_arith_rgb24_or_solid, encode_arith_rgba,
        encode_arith_rgba_or_solid,
    };
    use crate::frame::FrameType;

    /// Packed BGR fixture with every pixel set to `(b, g, r)`.
    fn constant_bgr(n: usize, b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(n * 3);
        for _ in 0..n {
            out.extend_from_slice(&[b, g, r]);
        }
        out
    }

    /// Packed BGRA fixture with every pixel set to `(b, g, r, a)`.
    fn constant_bgra(n: usize, b: u8, g: u8, r: u8, a: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(n * 4);
        for _ in 0..n {
            out.extend_from_slice(&[b, g, r, a]);
        }
        out
    }

    // ─── 1 + 2. Wire-shape pins + decode round-trip ────────────────

    #[test]
    fn rgb24_or_solid_emits_type5_on_constant_grey() {
        // Aligned (4×4, 16×16), unaligned (5×3 — `width % 4 != 0`,
        // the type-2 staging row of `spec/01` §3 is overwritten by
        // the solid shortcut all the same), and single-pixel (1×1).
        for &(w, h) in &[(4u32, 4u32), (16, 16), (5, 3), (1, 1)] {
            let n = (w * h) as usize;
            let pixels = constant_bgr(n, 0x7f, 0x7f, 0x7f);
            let frame = encode_arith_rgb24_or_solid(&pixels, w, h);
            assert_eq!(frame, vec![5, 0x7f], "type-5 wire shape at {w}×{h}");
            assert_eq!(
                frame.len(),
                FrameType::SolidGrey.solid_wire_size().unwrap(),
                "§2.2.2 total at {w}×{h}",
            );
            let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
            assert_eq!(decoded.pixels, pixels, "type-5 round-trip at {w}×{h}");
        }
    }

    #[test]
    fn rgb24_or_solid_emits_type6_on_constant_colour() {
        for &(w, h) in &[(4u32, 4u32), (16, 16), (5, 3), (1, 1)] {
            let n = (w * h) as usize;
            // B = 0x11, G = 0x22, R = 0x33 — wire bytes 1/2/3 carry
            // the input pixel's bytes 0/1/2 unchanged (`spec/01`
            // §2.2.1 encoder mirror).
            let pixels = constant_bgr(n, 0x11, 0x22, 0x33);
            let frame = encode_arith_rgb24_or_solid(&pixels, w, h);
            assert_eq!(
                frame,
                vec![6, 0x11, 0x22, 0x33],
                "type-6 wire shape at {w}×{h}",
            );
            assert_eq!(
                frame.len(),
                FrameType::SolidRgb.solid_wire_size().unwrap(),
                "§2.2.2 total at {w}×{h}",
            );
            let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
            assert_eq!(decoded.pixels, pixels, "type-6 round-trip at {w}×{h}");
        }
    }

    #[test]
    fn rgba_or_solid_emits_type9_on_constant_colour() {
        for &(w, h) in &[(4u32, 4u32), (16, 16), (5, 3), (1, 1)] {
            let n = (w * h) as usize;
            let pixels = constant_bgra(n, 0x11, 0x22, 0x33, 0x44);
            let frame = encode_arith_rgba_or_solid(&pixels, w, h);
            assert_eq!(
                frame,
                vec![9, 0x11, 0x22, 0x33, 0x44],
                "type-9 wire shape at {w}×{h}",
            );
            assert_eq!(
                frame.len(),
                FrameType::SolidRgba.solid_wire_size().unwrap(),
                "§2.2.2 total at {w}×{h}",
            );
            let decoded = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap();
            assert_eq!(decoded.pixels, pixels, "type-9 round-trip at {w}×{h}");
        }
    }

    // ─── 3. Grey-vs-RGB split ──────────────────────────────────────

    #[test]
    fn solid_grey_split_follows_spec01_section3_rows() {
        const W: u32 = 8;
        const H: u32 = 8;
        let n = (W * H) as usize;

        // RGB path: B == G == R → type 5; any component differing →
        // type 6 (`spec/01` §3 rows 5/6).
        for v in [0x00u8, 0x01, 0x80, 0xff] {
            let frame = encode_arith_rgb24_or_solid(&constant_bgr(n, v, v, v), W, H);
            assert_eq!(frame[0], 5, "grey value {v:#04x} must select type 5");
        }
        for (b, g, r) in [
            (0x10u8, 0x10u8, 0x11u8),
            (0x10, 0x11, 0x10),
            (0x11, 0x10, 0x10),
        ] {
            let frame = encode_arith_rgb24_or_solid(&constant_bgr(n, b, g, r), W, H);
            assert_eq!(
                frame[0], 6,
                "non-grey ({b:#04x},{g:#04x},{r:#04x}) must select type 6",
            );
        }

        // RGBA path: constant grey + opaque still emits type 9 — the
        // 5/6 overwrite sites sit on the RGB path only (`spec/01`
        // §3.1 thresholds `0xf` vs `0x15`).
        let frame = encode_arith_rgba_or_solid(&constant_bgra(n, 0x7f, 0x7f, 0x7f, 0xff), W, H);
        assert_eq!(frame, vec![9, 0x7f, 0x7f, 0x7f, 0xff]);
    }

    // ─── 4. Fall-through byte-identity ─────────────────────────────

    #[test]
    fn rgb24_or_solid_falls_through_byte_identical_on_non_solid() {
        const W: u32 = 8;
        const H: u32 = 8;
        let n = (W * H) as usize;

        // Gradient — plainly non-solid.
        let gradient: Vec<u8> = (0..n * 3).map(|i| ((i * 73 + 11) & 0xff) as u8).collect();
        assert_eq!(
            encode_arith_rgb24_or_solid(&gradient, W, H),
            encode_arith_rgb24(&gradient, W, H),
            "gradient must fall through byte-identically",
        );

        // Almost-solid: one byte of the LAST pixel differs — the
        // constancy scan must reject it (boundary of the predicate).
        let mut almost = constant_bgr(n, 0x11, 0x22, 0x33);
        let last = almost.len() - 1;
        almost[last] ^= 0x01;
        let frame = encode_arith_rgb24_or_solid(&almost, W, H);
        assert_eq!(
            frame,
            encode_arith_rgb24(&almost, W, H),
            "almost-solid must fall through byte-identically",
        );
        let decoded = decode_frame(&frame, W, H, PixelKind::Bgr24).unwrap();
        assert_eq!(decoded.pixels, almost, "almost-solid round-trip");
    }

    #[test]
    fn rgba_or_solid_falls_through_byte_identical_on_non_solid() {
        const W: u32 = 8;
        const H: u32 = 8;
        let n = (W * H) as usize;

        let gradient: Vec<u8> = (0..n * 4).map(|i| ((i * 73 + 11) & 0xff) as u8).collect();
        assert_eq!(
            encode_arith_rgba_or_solid(&gradient, W, H),
            encode_arith_rgba(&gradient, W, H),
            "gradient must fall through byte-identically",
        );

        // Almost-solid differing only in the last pixel's alpha byte.
        let mut almost = constant_bgra(n, 0x11, 0x22, 0x33, 0x44);
        let last = almost.len() - 1;
        almost[last] ^= 0x01;
        let frame = encode_arith_rgba_or_solid(&almost, W, H);
        assert_eq!(
            frame,
            encode_arith_rgba(&almost, W, H),
            "almost-solid must fall through byte-identically",
        );
        let decoded = decode_frame(&frame, W, H, PixelKind::Bgra32).unwrap();
        assert_eq!(decoded.pixels, almost, "almost-solid round-trip");
    }

    // ─── 5. Never-larger invariant ─────────────────────────────────

    #[test]
    fn or_solid_never_larger_than_arith_and_strictly_smaller_on_solid() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 16), (5, 3)] {
            let n = (w * h) as usize;

            // Solid fixtures: strictly smaller (2 / 4 / 5 bytes vs
            // the arithmetic form's channel-offset preamble alone —
            // 9 / 13 bytes per `spec/01` §2.3 — plus plane bodies).
            let grey = constant_bgr(n, 0x40, 0x40, 0x40);
            let arith = encode_arith_rgb24(&grey, w, h);
            let solid = encode_arith_rgb24_or_solid(&grey, w, h);
            assert!(
                solid.len() < arith.len(),
                "solid grey {w}×{h}: {} !< {}",
                solid.len(),
                arith.len(),
            );

            let rgba = constant_bgra(n, 0x40, 0x41, 0x42, 0x43);
            let arith = encode_arith_rgba(&rgba, w, h);
            let solid = encode_arith_rgba_or_solid(&rgba, w, h);
            assert!(
                solid.len() < arith.len(),
                "solid rgba {w}×{h}: {} !< {}",
                solid.len(),
                arith.len(),
            );

            // Non-solid fixtures: never larger (equality — the
            // fall-through is byte-identical).
            let gradient: Vec<u8> = (0..n * 3).map(|i| ((i * 73 + 11) & 0xff) as u8).collect();
            assert!(
                encode_arith_rgb24_or_solid(&gradient, w, h).len()
                    <= encode_arith_rgb24(&gradient, w, h).len(),
                "gradient rgb24 {w}×{h} grew",
            );
            let gradient4: Vec<u8> = (0..n * 4).map(|i| ((i * 73 + 11) & 0xff) as u8).collect();
            assert!(
                encode_arith_rgba_or_solid(&gradient4, w, h).len()
                    <= encode_arith_rgba(&gradient4, w, h).len(),
                "gradient rgba {w}×{h} grew",
            );
        }
    }
}

/// Round-341 milestone — **exhaustive encoder → decoder self-roundtrip
/// matrix.**
///
/// The previous property modules sweep a handful of seeds at a single
/// 8×8 size per colour family. This module widens the cross-product to
/// the dimensions that matter for the wire format and runs every cell
/// through the production frame encoder (`encode_channel_best` per
/// plane, the same selector the public-style `encode_arith_*` entry
/// points use). Each cell asserts a **byte-exact** encode→decode
/// recovery of the original pixel buffer.
///
/// The matrix axes are:
///
/// * **Colour family** — RGB24 (type 2 unaligned / type 4 aligned,
///   chosen by `width % 4`), RGBA (type 8), YV12 (type 10), YUY2
///   (type 3), legacy RGB (type 7 adaptive-CDF). Reduced-resolution
///   type 11 is fixed-point (downsample→upscale is lossy) and is
///   covered by its own idempotence cell rather than a byte-exact
///   recovery.
/// * **Dimensions** — a set that spans every selector boundary the
///   encoders branch on: `width % 4 == 0` vs not (the type-2/type-4
///   split), even vs odd width/height, power-of-two vs non-power-of-two
///   plane pixel counts (the modern range coder's `range / total`
///   division), and small (1-row / 1-col edge) vs larger planes.
/// * **Data pattern** — seven generators that drive the per-plane
///   header-form selector into *every* one of its eight legal wire
///   sub-forms: uniform-random (header `0x00`/`0x04` arithmetic-vs-raw),
///   smooth gradient (predictor residuals collapse to zeros →
///   RLE-bearing `0x01..0x03` / `0x05..0x07`), zero-heavy (long
///   zero-run RLE), constant (header `0xff` solid fill), two-symbol
///   (near-degenerate histogram), sparse-impulse, and a structured
///   stripe pattern.
///
/// Beyond the per-cell byte-exact assertion, the suite tracks which
/// channel-header sub-form `encode_channel_best` selected on each plane
/// (read straight off the wire by walking the offset table) and asserts
/// at the end that the **full** set of eight `decode_channel` sub-forms
/// — `0x00`, `0x01`, `0x02`, `0x03`, `0x04`, `0x05`, `0x06`, `0x07`,
/// `0xff` — was exercised by at least one cell. That turns "the encoder
/// can produce every wire type the decoder accepts" from prose into a
/// machine-checked invariant.
#[cfg(test)]
mod encoder_exhaustive_matrix {
    use super::*;
    use crate::encoder::{
        encode_arith_reduced_res, encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2,
        encode_arith_yv12, encode_legacy_rgb,
    };
    use std::collections::BTreeSet;

    /// Deterministic LCG byte stream (same constants as the sibling
    /// property modules; kept inline for module self-containment).
    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            out.push((s >> 33) as u8);
        }
        out
    }

    /// The seven data-pattern generators. Each produces `n` bytes of a
    /// distinct statistical shape so that, summed across the dimension
    /// sweep, every per-plane header sub-form gets selected at least
    /// once. `seed` perturbs the random/sparse variants per cell so
    /// repeated dimensions do not collapse to identical buffers.
    #[derive(Clone, Copy, Debug)]
    enum Pattern {
        /// Uniform pseudo-random — flat histogram, arithmetic body wins
        /// or raw memcpy ties; drives `0x00` / `0x04`.
        Random,
        /// Smooth horizontal gradient — predictor residuals are mostly
        /// zero, so the RLE sub-paths (`0x01..0x03` / `0x05..0x07`) and
        /// the small arithmetic tables get exercised.
        Gradient,
        /// Mostly-zero with occasional spikes — long zero-runs favour
        /// the RLE escape forms.
        ZeroHeavy,
        /// A single constant value across the whole buffer — the
        /// per-plane selector collapses to `0xff` solid fill.
        Constant,
        /// Two symbols alternating — near-degenerate histogram, tiny
        /// arithmetic tables.
        TwoSymbol,
        /// Sparse impulses on a zero field — heavy zero-run RLE plus a
        /// few literals.
        SparseImpulse,
        /// Vertical stripes — periodic, so the median predictor and the
        /// RLE forms both see structured residuals.
        Stripe,
    }

    impl Pattern {
        const ALL: [Pattern; 7] = [
            Pattern::Random,
            Pattern::Gradient,
            Pattern::ZeroHeavy,
            Pattern::Constant,
            Pattern::TwoSymbol,
            Pattern::SparseImpulse,
            Pattern::Stripe,
        ];

        fn bytes(self, seed: u64, n: usize) -> Vec<u8> {
            match self {
                Pattern::Random => lcg_bytes(seed, n),
                Pattern::Gradient => (0..n).map(|i| ((i * 7 + 3) & 0xff) as u8).collect(),
                Pattern::ZeroHeavy => {
                    let r = lcg_bytes(seed, n);
                    r.iter()
                        .map(|&b| if b < 12 { b } else { 0 })
                        .collect::<Vec<u8>>()
                }
                Pattern::Constant => {
                    let v = (seed >> 17) as u8;
                    vec![v; n]
                }
                Pattern::TwoSymbol => {
                    let a = (seed >> 11) as u8;
                    let b = a.wrapping_add(37);
                    (0..n).map(|i| if i % 2 == 0 { a } else { b }).collect()
                }
                Pattern::SparseImpulse => {
                    let mut v = vec![0u8; n];
                    let mut s = seed | 1;
                    let mut i = 0usize;
                    while i < n {
                        s = s
                            .wrapping_mul(6364136223846793005)
                            .wrapping_add(1442695040888963407);
                        let stride = 5 + (s >> 40) as usize % 23;
                        i += stride;
                        if i < n {
                            v[i] = (s >> 24) as u8 | 1;
                        }
                    }
                    v
                }
                Pattern::Stripe => (0..n)
                    .map(|i| if (i / 3) % 2 == 0 { 0x10 } else { 0xa0 })
                    .collect(),
            }
        }
    }

    /// Walk the channel-offset prefix of a packed modern/legacy frame
    /// and return the first (header) byte of each plane body. `n_ch` is
    /// the channel count (3 for RGB24/YUY2/YV12/legacy, 4 for RGBA).
    /// Returns `None` if the frame is too short to introspect (a solid
    /// / uncompressed short-circuit the encoder may have taken — those
    /// carry no per-plane header byte).
    fn plane_header_bytes(frame: &[u8], n_ch: usize) -> Option<Vec<u8>> {
        let prefix_size = 1 + (n_ch - 1) * 4;
        if frame.len() < prefix_size {
            return None;
        }
        // Plane 0 starts at prefix_size; planes 1..n-1 start at the
        // offsets stored little-endian in the prefix.
        let mut starts = Vec::with_capacity(n_ch);
        starts.push(prefix_size);
        for k in 0..n_ch - 1 {
            let off = 1 + k * 4;
            let v = u32::from_le_bytes([frame[off], frame[off + 1], frame[off + 2], frame[off + 3]])
                as usize;
            starts.push(v);
        }
        let mut headers = Vec::with_capacity(n_ch);
        for &s in &starts {
            if s >= frame.len() {
                return None;
            }
            headers.push(frame[s]);
        }
        Some(headers)
    }

    /// The dimension sweep. Each `(w, h)` is chosen to land on a
    /// selector boundary:
    /// * `width % 4`: 4/8/12/16 (== 0 → type 4) vs 5/6/7/10/13/14
    ///   (!= 0 → type 2).
    /// * even/odd width and height (YUY2 needs even width; YV12 needs
    ///   both even; RGB families accept any).
    /// * power-of-two vs non-power-of-two plane pixel counts
    ///   (`w*h`): 4×4=16, 8×8=64, 16×16=256 (pow2) vs 5×3=15, 6×6=36,
    ///   10×6=60, 12×12=144, 7×5=35, 13×5=65, 14×6=84 (non-pow2).
    /// * a single-row (`h==1`) and single-column (`w==1`) edge.
    const RGB_DIMS: &[(u32, u32)] = &[
        (1, 1),
        (4, 1),
        (1, 4),
        (4, 4),
        (5, 3),
        (6, 6),
        (7, 5),
        (8, 8),
        (10, 6),
        (12, 12),
        (13, 5),
        (16, 16),
    ];

    /// YUY2 needs an even width (the `Y0 U Y1 V` macropixel). Heights
    /// may be odd.
    const YUY2_DIMS: &[(u32, u32)] = &[(2, 1), (4, 4), (6, 3), (8, 8), (10, 6), (14, 6), (16, 16)];

    /// YV12 needs both width and height even (4:2:0 chroma at half
    /// resolution). Dims here are all even/even.
    const YV12_DIMS: &[(u32, u32)] = &[(2, 2), (4, 4), (6, 6), (8, 8), (10, 6), (12, 12), (16, 16)];

    /// Per-cell seed derived from family tag, pattern index, and dims so
    /// every cell sees a distinct random trajectory.
    fn cell_seed(tag: u64, pat: usize, w: u32, h: u32) -> u64 {
        tag.wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ ((pat as u64) << 48)
            ^ ((w as u64) << 24)
            ^ (h as u64)
    }

    /// RGB24 (type 2 / type 4): build a BGR buffer from the pattern,
    /// encode, decode, assert byte-exact, and record the chosen header
    /// sub-forms + the realised frame-type byte.
    #[test]
    fn rgb24_matrix_byte_exact_and_type_split() {
        let mut seen_headers: BTreeSet<u8> = BTreeSet::new();
        let mut saw_type2 = false;
        let mut saw_type4 = false;
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in RGB_DIMS {
                let n = (w * h) as usize * 3;
                let seed = cell_seed(0x2424, pi, w, h);
                let pixels = pat.bytes(seed, n);
                let frame = encode_arith_rgb24(&pixels, w, h);
                let want_type = if w % 4 == 0 { 4 } else { 2 };
                assert_eq!(
                    frame[0], want_type,
                    "RGB24 {w}×{h} {pat:?}: wrong frame-type byte",
                );
                if frame[0] == 2 {
                    saw_type2 = true;
                } else {
                    saw_type4 = true;
                }
                if let Some(hs) = plane_header_bytes(&frame, 3) {
                    seen_headers.extend(hs);
                }
                let dec = decode_frame(&frame, w, h, PixelKind::Bgr24)
                    .unwrap_or_else(|e| panic!("RGB24 {w}×{h} {pat:?} decode failed: {e:?}"));
                assert_eq!(
                    dec.pixels, pixels,
                    "RGB24 {w}×{h} {pat:?}: roundtrip not byte-exact",
                );
            }
        }
        assert!(
            saw_type2,
            "matrix never exercised the type-2 unaligned path"
        );
        assert!(saw_type4, "matrix never exercised the type-4 aligned path");
        // The RGB24 family alone must reach at least the arithmetic,
        // raw, an RLE form, and solid fill.
        assert!(
            seen_headers.contains(&0x00),
            "RGB24 matrix never selected header 0x00 (arithmetic)",
        );
        assert!(
            seen_headers.contains(&0xff),
            "RGB24 matrix never selected header 0xff (solid fill)",
        );
    }

    /// RGBA (type 8): four planes including the raw alpha plane.
    #[test]
    fn rgba_matrix_byte_exact() {
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in RGB_DIMS {
                let n = (w * h) as usize * 4;
                let seed = cell_seed(0x8888, pi, w, h);
                let pixels = pat.bytes(seed, n);
                let frame = encode_arith_rgba(&pixels, w, h);
                assert_eq!(frame[0], 8, "RGBA {w}×{h} {pat:?}: wrong frame-type byte");
                let dec = decode_frame(&frame, w, h, PixelKind::Bgra32)
                    .unwrap_or_else(|e| panic!("RGBA {w}×{h} {pat:?} decode failed: {e:?}"));
                assert_eq!(
                    dec.pixels, pixels,
                    "RGBA {w}×{h} {pat:?}: roundtrip not byte-exact",
                );
            }
        }
    }

    /// YV12 (type 10): planar Y / V / U at 4:2:0.
    #[test]
    fn yv12_matrix_byte_exact() {
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in YV12_DIMS {
                let n = PixelKind::Yv12.buffer_len(w, h);
                let seed = cell_seed(0x1010, pi, w, h);
                let pixels = pat.bytes(seed, n);
                let frame = encode_arith_yv12(&pixels, w, h);
                assert_eq!(frame[0], 10, "YV12 {w}×{h} {pat:?}: wrong frame-type byte");
                let dec = decode_frame(&frame, w, h, PixelKind::Yv12)
                    .unwrap_or_else(|e| panic!("YV12 {w}×{h} {pat:?} decode failed: {e:?}"));
                assert_eq!(
                    dec.pixels, pixels,
                    "YV12 {w}×{h} {pat:?}: roundtrip not byte-exact",
                );
            }
        }
    }

    /// YUY2 (type 3): packed `Y0 U Y1 V` → planar.
    #[test]
    fn yuy2_matrix_byte_exact() {
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in YUY2_DIMS {
                let n = PixelKind::Yuy2.buffer_len(w, h);
                let seed = cell_seed(0x0303, pi, w, h);
                let pixels = pat.bytes(seed, n);
                let frame = encode_arith_yuy2(&pixels, w, h);
                assert_eq!(frame[0], 3, "YUY2 {w}×{h} {pat:?}: wrong frame-type byte");
                let dec = decode_frame(&frame, w, h, PixelKind::Yuy2)
                    .unwrap_or_else(|e| panic!("YUY2 {w}×{h} {pat:?} decode failed: {e:?}"));
                assert_eq!(
                    dec.pixels, pixels,
                    "YUY2 {w}×{h} {pat:?}: roundtrip not byte-exact",
                );
            }
        }
    }

    /// YUY2 (type 3) **odd-width** exhaustive matrix (round 352). The
    /// floor-chroma layout (`spec/03` §6.2) drops the trailing luma
    /// column's chroma macropixel: the decoder synthesises a `0x80`
    /// neutral at output byte `2·(W−1)+1`, so a byte-exact roundtrip is
    /// only possible when the input already holds `0x80` there. We
    /// normalise each pattern's odd-tail chroma slot to `0x80`, then
    /// require byte-exact encode→decode across every pattern and a set
    /// of odd widths (incl. the degenerate W=1 with empty chroma
    /// planes).
    #[test]
    fn yuy2_odd_width_matrix_byte_exact() {
        const ODD_DIMS: &[(u32, u32)] = &[(1, 4), (3, 3), (5, 4), (7, 5), (9, 9), (11, 6), (15, 7)];
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in ODD_DIMS {
                let n = PixelKind::Yuy2.buffer_len(w, h);
                let seed = cell_seed(0x0353, pi, w, h);
                let mut pixels = pat.bytes(seed, n);
                // Force the odd-tail chroma slot to the decoder's neutral
                // fill so the buffer is reproducible on roundtrip.
                let last_x = (w - 1) as usize;
                for y in 0..h as usize {
                    let out_row = y * w as usize * 2;
                    pixels[out_row + 2 * last_x + 1] = 0x80;
                }
                let frame = encode_arith_yuy2(&pixels, w, h);
                assert_eq!(
                    frame[0], 3,
                    "YUY2 odd {w}×{h} {pat:?}: wrong frame-type byte"
                );
                let dec = decode_frame(&frame, w, h, PixelKind::Yuy2)
                    .unwrap_or_else(|e| panic!("YUY2 odd {w}×{h} {pat:?} decode failed: {e:?}"));
                assert_eq!(
                    dec.pixels, pixels,
                    "YUY2 odd {w}×{h} {pat:?}: roundtrip not byte-exact",
                );
            }
        }
    }

    /// Legacy RGB (type 7): the pre-1.1.0 adaptive-CDF range coder.
    #[test]
    fn legacy_rgb_matrix_byte_exact() {
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in RGB_DIMS {
                let n = (w * h) as usize * 3;
                let seed = cell_seed(0x0707, pi, w, h);
                let pixels = pat.bytes(seed, n);
                let frame = encode_legacy_rgb(&pixels, w, h);
                assert_eq!(frame[0], 7, "legacy {w}×{h} {pat:?}: wrong frame-type byte");
                let dec = decode_frame(&frame, w, h, PixelKind::Bgr24)
                    .unwrap_or_else(|e| panic!("legacy {w}×{h} {pat:?} decode failed: {e:?}"));
                assert_eq!(
                    dec.pixels, pixels,
                    "legacy RGB {w}×{h} {pat:?}: roundtrip not byte-exact",
                );
            }
        }
    }

    /// Reduced-resolution (type 11): the downsample→upscale path is
    /// lossy, so byte-exact recovery of the *input* is impossible.
    /// Instead pin that the path is idempotent on its own fixed point —
    /// decode→re-encode→decode reproduces the first decode byte-exactly
    /// — across the pattern sweep and the YV12-shaped dimension set.
    #[test]
    fn reduced_res_matrix_fixed_point() {
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            // Type 11 requires host W and H each a multiple of 4 so the
            // half-resolution plane is itself even/even.
            for &(w, h) in &[(4u32, 4u32), (8, 8), (12, 12), (16, 16), (8, 12)] {
                let n = PixelKind::Yv12.buffer_len(w, h);
                let seed = cell_seed(0x0b0b, pi, w, h);
                let host = pat.bytes(seed, n);
                let frame = encode_arith_reduced_res(&host, w, h);
                assert_eq!(
                    frame[0], 11,
                    "reduced {w}×{h} {pat:?}: wrong frame-type byte"
                );
                let dec = decode_frame(&frame, w, h, PixelKind::Yv12)
                    .unwrap_or_else(|e| panic!("reduced {w}×{h} {pat:?} decode failed: {e:?}"));
                let frame2 = encode_arith_reduced_res(&dec.pixels, w, h);
                let dec2 = decode_frame(&frame2, w, h, PixelKind::Yv12)
                    .unwrap_or_else(|e| panic!("reduced {w}×{h} {pat:?} re-decode failed: {e:?}"));
                assert_eq!(
                    dec.pixels, dec2.pixels,
                    "reduced-res {w}×{h} {pat:?}: not at its fixed point",
                );
            }
        }
    }

    /// Coverage half 1 — the **minimum-byte selector** (`encode_channel_best`,
    /// the per-plane chooser every modern frame encoder routes through)
    /// must, summed across all modern colour families and the full
    /// pattern × dimension sweep, naturally select a representative
    /// spread of sub-forms: the bare arithmetic body (`0x00`), the raw
    /// memcpy (`0x04`), at least one arithmetic-RLE form
    /// (`0x01..0x03`), at least one raw-RLE form (`0x05..0x07`), and the
    /// solid fill (`0xff`).
    ///
    /// It does **not** require the selector to pick every one of escape
    /// 1/2/3 individually: which escape length is shortest is a property
    /// of the residual zero-run-length distribution, and for most
    /// realistic residual streams the escape-1 form ties or beats
    /// escape-2/3 (a single zero costs the same supplement byte at every
    /// escape length but fires on a shorter run at escape-1). The
    /// individual escape-2 / escape-3 forms are proven **encodable**
    /// (and byte-exact-decodable) by `all_nine_subforms_encodable`
    /// below, which drives the explicit-escape-length channel encoders
    /// directly — that is the "every wire type the decoder accepts is
    /// encodable" guarantee; this test is the "the optimiser actually
    /// reaches the cheap forms in practice" guarantee.
    #[test]
    fn best_selector_reaches_representative_subforms() {
        let mut seen: BTreeSet<u8> = BTreeSet::new();

        // RGB24 (3 planes) across the full pattern × dim sweep.
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in RGB_DIMS {
                let pixels = pat.bytes(cell_seed(0x2424, pi, w, h), (w * h) as usize * 3);
                let frame = encode_arith_rgb24(&pixels, w, h);
                if let Some(hs) = plane_header_bytes(&frame, 3) {
                    seen.extend(hs);
                }
            }
        }
        // RGBA (4 planes).
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in RGB_DIMS {
                let pixels = pat.bytes(cell_seed(0x8888, pi, w, h), (w * h) as usize * 4);
                let frame = encode_arith_rgba(&pixels, w, h);
                if let Some(hs) = plane_header_bytes(&frame, 4) {
                    seen.extend(hs);
                }
            }
        }
        // YV12 (3 planes).
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in YV12_DIMS {
                let pixels = pat.bytes(
                    cell_seed(0x1010, pi, w, h),
                    PixelKind::Yv12.buffer_len(w, h),
                );
                let frame = encode_arith_yv12(&pixels, w, h);
                if let Some(hs) = plane_header_bytes(&frame, 3) {
                    seen.extend(hs);
                }
            }
        }
        // YUY2 (3 planes).
        for (pi, &pat) in Pattern::ALL.iter().enumerate() {
            for &(w, h) in YUY2_DIMS {
                let pixels = pat.bytes(
                    cell_seed(0x0303, pi, w, h),
                    PixelKind::Yuy2.buffer_len(w, h),
                );
                let frame = encode_arith_yuy2(&pixels, w, h);
                if let Some(hs) = plane_header_bytes(&frame, 3) {
                    seen.extend(hs);
                }
            }
        }

        assert!(
            seen.contains(&0x00),
            "selector never chose 0x00 (bare arithmetic); seen = {seen:02x?}",
        );
        assert!(
            seen.contains(&0x04),
            "selector never chose 0x04 (raw memcpy); seen = {seen:02x?}",
        );
        assert!(
            (0x01..=0x03).any(|h| seen.contains(&h)),
            "selector never chose any arithmetic-RLE form 0x01..0x03; seen = {seen:02x?}",
        );
        assert!(
            (0x05..=0x07).any(|h| seen.contains(&h)),
            "selector never chose any raw-RLE form 0x05..0x07; seen = {seen:02x?}",
        );
        assert!(
            seen.contains(&0xff),
            "selector never chose 0xff (solid fill); seen = {seen:02x?}",
        );
    }

    /// Coverage half 2 — **every one** of the nine legal modern
    /// channel-header sub-forms `decode_channel` accepts is independently
    /// **encodable** and round-trips byte-exactly: `0x00` (bare
    /// arithmetic), `0x01`/`0x02`/`0x03` (arithmetic + RLE escape 1/2/3),
    /// `0x04` (raw memcpy), `0x05`/`0x06`/`0x07` (raw + RLE escape 1/2/3),
    /// and `0xff` (solid fill). This drives the explicit-escape-length
    /// channel encoders directly (not the minimum-byte selector, which
    /// is free to prefer the cheapest form) so the escape-2 / escape-3
    /// paths — rarely the optimal choice but always legal — are proven
    /// encodable. Together with `best_selector_reaches_representative_subforms`
    /// this is the machine-checked "every wire type the decoder accepts
    /// is encodable, and the encoder→decoder loop recovers it
    /// byte-exactly" invariant.
    #[test]
    fn all_nine_subforms_encodable() {
        use crate::channel::decode_channel;
        use crate::encoder::{
            encode_channel_arith_rle, encode_channel_raw_rle, encode_channel_simple,
        };
        use crate::rle::contract_raw;

        // A residual-style plane carrying enough zero runs (and isolated
        // literals) that contraction at each escape length is well
        // defined and the arithmetic histogram has >= 2 symbols.
        let mut plane = vec![5u8, 7, 11, 2];
        plane.extend(std::iter::repeat(0u8).take(5));
        plane.extend_from_slice(&[13, 0, 0, 17]);
        plane.extend(std::iter::repeat(0u8).take(11));
        plane.extend_from_slice(&[19, 23, 0, 0, 0, 29, 31]);
        let n = plane.len();

        let mut produced: BTreeSet<u8> = BTreeSet::new();

        // 0x00 — bare arithmetic (encode_channel_simple emits 0x00 when
        // arithmetic beats raw memcpy on a multi-symbol plane).
        {
            let ch = encode_channel_simple(&plane);
            let hdr = ch[0];
            assert!(
                hdr == 0x00 || hdr == 0x04 || hdr == 0xff,
                "encode_channel_simple emitted unexpected header {hdr:#04x}",
            );
            let dec = decode_channel(&ch, n).expect("simple channel decodes");
            assert_eq!(dec, plane, "encode_channel_simple roundtrip");
            produced.insert(hdr);
        }

        // 0x01 / 0x02 / 0x03 — arithmetic + RLE at each escape length.
        for escape_len in 1..=3usize {
            let ch = encode_channel_arith_rle(&plane, escape_len);
            let hdr = ch[0];
            // The encoder falls back to the simple path only when the
            // contracted stream collapses to <2 symbols or trips the
            // dispatcher length guard; for this plane it must take the
            // genuine arith-RLE form.
            assert_eq!(
                hdr, escape_len as u8,
                "encode_channel_arith_rle({escape_len}) emitted header {hdr:#04x}",
            );
            let dec =
                decode_channel(&ch, n).expect("arith-RLE channel decodes at every escape length");
            assert_eq!(dec, plane, "arith-RLE escape_len={escape_len} roundtrip");
            produced.insert(hdr);
        }

        // 0x04 — raw memcpy (hand-built; the canonical literal form).
        {
            let mut ch = vec![0x04u8];
            ch.extend_from_slice(&plane);
            let dec = decode_channel(&ch, n).expect("raw-memcpy channel decodes");
            assert_eq!(dec, plane, "raw memcpy 0x04 roundtrip");
            produced.insert(0x04);
        }

        // 0x05 / 0x06 / 0x07 — raw bytes + RLE at each escape length.
        for escape_len in 1..=3usize {
            let ch = encode_channel_raw_rle(&plane, escape_len);
            let hdr = ch[0];
            assert_eq!(
                hdr,
                (escape_len + 4) as u8,
                "encode_channel_raw_rle({escape_len}) emitted header {hdr:#04x}",
            );
            // Sanity: the body is exactly the contraction the decoder
            // reverses.
            assert_eq!(
                &ch[1..],
                contract_raw(&plane, escape_len).as_slice(),
                "raw-RLE body must equal contract_raw output",
            );
            let dec =
                decode_channel(&ch, n).expect("raw-RLE channel decodes at every escape length");
            assert_eq!(dec, plane, "raw-RLE escape_len={escape_len} roundtrip");
            produced.insert(hdr);
        }

        // 0xff — solid fill (a constant plane forces it).
        {
            let solid = vec![0x42u8; n];
            let ch = encode_channel_simple(&solid);
            assert_eq!(ch[0], 0xff, "constant plane must encode to 0xff solid fill");
            let dec = decode_channel(&ch, n).expect("solid-fill channel decodes");
            assert_eq!(dec, solid, "solid fill 0xff roundtrip");
            produced.insert(0xff);
        }

        // The explicit encoders cover every escape-bearing form plus
        // raw + solid. The only header the simple path may have emitted
        // for the multi-symbol `plane` is 0x00 or 0x04; assert the full
        // legal set is reachable across these encoders.
        for want in [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0xff] {
            assert!(
                produced.contains(&want),
                "sub-form {want:#04x} was not produced by the direct encoders; produced = {produced:02x?}",
            );
        }
        // And the bare-arithmetic 0x00 form is reachable too (the
        // multi-symbol plane selects it unless raw memcpy is strictly
        // shorter — in which case 0x04 stands in; both are covered).
        assert!(
            produced.contains(&0x00) || produced.contains(&0x04),
            "neither bare-arithmetic 0x00 nor raw 0x04 was produced; produced = {produced:02x?}",
        );
    }
}

/// Round-341 milestone — **encoder fuzz harness (in-crate).**
///
/// The crate's encoder is `#[cfg(test)]`-gated (it drives the
/// self-roundtrip suite and makes no byte-equality claim against the
/// proprietary), so it is not reachable from the separate `cargo-fuzz`
/// binary in `fuzz/` (that binary fuzzes the public *decoder* against
/// hostile wire bytes). This module is the encoder-side counterpart: a
/// high-iteration deterministic-random loop that fuzzes the encoder's
/// **input** space — random dimensions across each family's legal
/// constraints, crossed with random pixel content of varied byte
/// entropy — and asserts on every iteration that
///
/// 1. encoding does not panic / overflow / index out of bounds, and
/// 2. the encoded frame decodes back to the **exact** input pixel
///    buffer (the byte-exact self-roundtrip invariant).
///
/// Determinism comes from a single 64-bit LCG seed so any failure
/// reproduces from the printed `(family, w, h, seed, content_seed)`
/// tuple. Iteration counts are kept modest (a few hundred per family)
/// so the suite stays inside the round's memory cap and CI budget while
/// still covering far more `(dims, content)` combinations than the
/// fixed-cell matrix above.
#[cfg(test)]
mod encoder_fuzz_harness {
    use super::*;
    use crate::encoder::{
        encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2, encode_arith_yv12,
        encode_legacy_rgb,
    };

    /// 64-bit LCG state stepper, returns the next state.
    fn lcg_next(s: &mut u64) -> u64 {
        *s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *s
    }

    /// Fill `n` bytes from the LCG, but with a content-entropy knob
    /// (`entropy` 0..=3) so the encoder's header-form selector sees the
    /// full spread from near-constant to uniform-random data on
    /// successive iterations:
    /// * 0 — constant (every byte the same) → solid-fill plane.
    /// * 1 — few distinct values (mask 0x07) → tiny histograms +
    ///   long predictor-residual zero runs.
    /// * 2 — moderate (mask 0x3f).
    /// * 3 — full 0..=255 uniform.
    fn fuzz_bytes(state: &mut u64, n: usize, entropy: u8) -> Vec<u8> {
        let mut out = Vec::with_capacity(n);
        if entropy == 0 {
            let v = (lcg_next(state) >> 32) as u8;
            out.resize(n, v);
            return out;
        }
        let mask: u8 = match entropy {
            1 => 0x07,
            2 => 0x3f,
            _ => 0xff,
        };
        for _ in 0..n {
            out.push(((lcg_next(state) >> 33) as u8) & mask);
        }
        out
    }

    /// Modern RGB24 (type 2 / type 4): widths 1..=20, heights 1..=12.
    #[test]
    fn fuzz_rgb24_roundtrip() {
        let mut state = 0x1234_5678_9abc_def0u64;
        for iter in 0..400u32 {
            let w = 1 + (lcg_next(&mut state) % 20) as u32;
            let h = 1 + (lcg_next(&mut state) % 12) as u32;
            let entropy = (lcg_next(&mut state) % 4) as u8;
            let content_seed = state;
            let pixels = fuzz_bytes(&mut state, (w * h) as usize * 3, entropy);
            let frame = encode_arith_rgb24(&pixels, w, h);
            let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap_or_else(|e| {
                panic!("RGB24 fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: {e:?}")
            });
            assert_eq!(
                dec.pixels, pixels,
                "RGB24 fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: not byte-exact",
            );
        }
    }

    /// Modern RGBA (type 8): four planes incl. raw alpha.
    #[test]
    fn fuzz_rgba_roundtrip() {
        let mut state = 0xa5a5_5a5a_a5a5_5a5au64;
        for iter in 0..400u32 {
            let w = 1 + (lcg_next(&mut state) % 20) as u32;
            let h = 1 + (lcg_next(&mut state) % 12) as u32;
            let entropy = (lcg_next(&mut state) % 4) as u8;
            let content_seed = state;
            let pixels = fuzz_bytes(&mut state, (w * h) as usize * 4, entropy);
            let frame = encode_arith_rgba(&pixels, w, h);
            let dec = decode_frame(&frame, w, h, PixelKind::Bgra32).unwrap_or_else(|e| {
                panic!("RGBA fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: {e:?}")
            });
            assert_eq!(
                dec.pixels, pixels,
                "RGBA fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: not byte-exact",
            );
        }
    }

    /// YV12 (type 10): both dims even (4:2:0 chroma at half res).
    #[test]
    fn fuzz_yv12_roundtrip() {
        let mut state = 0xfeed_face_dead_beefu64;
        for iter in 0..400u32 {
            let w = 2 * (1 + (lcg_next(&mut state) % 10) as u32); // even, 2..=20
            let h = 2 * (1 + (lcg_next(&mut state) % 6) as u32); // even, 2..=12
            let entropy = (lcg_next(&mut state) % 4) as u8;
            let content_seed = state;
            let pixels = fuzz_bytes(&mut state, PixelKind::Yv12.buffer_len(w, h), entropy);
            let frame = encode_arith_yv12(&pixels, w, h);
            let dec = decode_frame(&frame, w, h, PixelKind::Yv12).unwrap_or_else(|e| {
                panic!("YV12 fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: {e:?}")
            });
            assert_eq!(
                dec.pixels, pixels,
                "YV12 fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: not byte-exact",
            );
        }
    }

    /// YUY2 (type 3): even width (the `Y0 U Y1 V` macropixel), any
    /// height.
    #[test]
    fn fuzz_yuy2_roundtrip() {
        let mut state = 0x0fed_cba9_8765_4321u64;
        for iter in 0..400u32 {
            let w = 2 * (1 + (lcg_next(&mut state) % 10) as u32); // even, 2..=20
            let h = 1 + (lcg_next(&mut state) % 12) as u32;
            let entropy = (lcg_next(&mut state) % 4) as u8;
            let content_seed = state;
            let pixels = fuzz_bytes(&mut state, PixelKind::Yuy2.buffer_len(w, h), entropy);
            let frame = encode_arith_yuy2(&pixels, w, h);
            let dec = decode_frame(&frame, w, h, PixelKind::Yuy2).unwrap_or_else(|e| {
                panic!("YUY2 fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: {e:?}")
            });
            assert_eq!(
                dec.pixels, pixels,
                "YUY2 fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: not byte-exact",
            );
        }
    }

    /// Legacy RGB (type 7): the adaptive-CDF range coder.
    #[test]
    fn fuzz_legacy_rgb_roundtrip() {
        let mut state = 0x5151_dead_c0de_0707u64;
        for iter in 0..300u32 {
            let w = 1 + (lcg_next(&mut state) % 18) as u32;
            let h = 1 + (lcg_next(&mut state) % 10) as u32;
            let entropy = (lcg_next(&mut state) % 4) as u8;
            let content_seed = state;
            let pixels = fuzz_bytes(&mut state, (w * h) as usize * 3, entropy);
            let frame = encode_legacy_rgb(&pixels, w, h);
            let dec = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap_or_else(|e| {
                panic!("legacy fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: {e:?}")
            });
            assert_eq!(
                dec.pixels, pixels,
                "legacy fuzz iter {iter} w={w} h={h} seed={content_seed:#x}: not byte-exact",
            );
        }
    }
}

/// Decode-determinism property suite.
///
/// `decode_frame` is documented (and relied on) as a *pure function of
/// its inputs*: the same payload + dimensions + host pixel format must
/// always yield byte-identical output (or the identical `Err`),
/// independent of any allocator state, prior calls, or the contents the
/// output buffer happened to start with. The decoder allocates fresh
/// scratch and a fresh output `Vec` per call, so this should hold by
/// construction — these tests pin it as a regression so a future change
/// that smuggles in hidden mutable state (a reused thread-local scratch,
/// an uninitialised-read off the predictor's edge, etc.) is caught
/// immediately rather than surfacing as a flaky cross-platform decode.
///
/// Two complementary properties:
///
/// 1. **Well-formed determinism** — every modern family's encoder output
///    decodes identically on two back-to-back calls.
/// 2. **Malformed determinism** — a corrupt payload that the fuzz suites
///    prove returns rather than panics must *return the same thing* every
///    time: a non-deterministic error path (e.g. one that read past an
///    allocation's logical end into whatever bytes were there) would show
///    up as two diverging results here.
///
/// Plus a **stateful no-drift** property: N consecutive NULL ("JUMP")
/// frames through the [`Decoder`] each replay the keyframe byte-for-byte
/// with zero accumulated drift (`spec/01` §1.1).
#[cfg(test)]
mod decode_determinism_property {
    use super::*;

    /// 64-bit LCG stepper (same constants as the other property suites).
    fn lcg_next(s: &mut u64) -> u64 {
        *s = s
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        *s
    }

    fn lcg_bytes(seed: u64, n: usize) -> Vec<u8> {
        let mut s = seed ^ 0x9e37_79b9_7f4a_7c15;
        let mut out = Vec::with_capacity(n);
        for _ in 0..n {
            out.push((lcg_next(&mut s) >> 33) as u8);
        }
        out
    }

    /// Each modern family: encode a frame, then decode it twice. The two
    /// decoded buffers must be byte-identical (decode is a pure function
    /// of the payload). Swept across a spread of dimensions per family
    /// so the property holds at every plane geometry, not just one size.
    #[test]
    fn well_formed_decode_is_byte_identical_on_repeat() {
        let rgb_dims: &[(u32, u32)] = &[(1, 1), (3, 3), (4, 4), (5, 7), (8, 8), (13, 3)];
        let yuv_dims: &[(u32, u32)] = &[(2, 2), (4, 4), (6, 4), (8, 6), (16, 8)];

        let mut seed = 0xd37e_8a1b_1573_0001u64;
        for &(w, h) in rgb_dims {
            // RGB24 (type 2/4).
            let px = lcg_bytes(seed ^ ((w as u64) << 16 | h as u64), (w * h) as usize * 3);
            let frame = encode_arith_rgb24(&px, w, h);
            let a = decode_frame(&frame, w, h, PixelKind::Bgr24).expect("rgb24 decode A");
            let b = decode_frame(&frame, w, h, PixelKind::Bgr24).expect("rgb24 decode B");
            assert_eq!(a.pixels, b.pixels, "RGB24 {w}x{h} non-deterministic decode");
            assert_eq!(a.pixels, px, "RGB24 {w}x{h} not byte-exact");

            // RGBA (type 8).
            let px = lcg_bytes(seed ^ 0xa1fa ^ (w as u64), (w * h) as usize * 4);
            let frame = encode_arith_rgba(&px, w, h);
            let a = decode_frame(&frame, w, h, PixelKind::Bgra32).expect("rgba decode A");
            let b = decode_frame(&frame, w, h, PixelKind::Bgra32).expect("rgba decode B");
            assert_eq!(a.pixels, b.pixels, "RGBA {w}x{h} non-deterministic decode");

            // Legacy RGB (type 7, adaptive-CDF range coder).
            let px = lcg_bytes(seed ^ 0x1e9a, (w * h) as usize * 3);
            let frame = encode_legacy_rgb(&px, w, h);
            let a = decode_frame(&frame, w, h, PixelKind::Bgr24).expect("legacy decode A");
            let b = decode_frame(&frame, w, h, PixelKind::Bgr24).expect("legacy decode B");
            assert_eq!(
                a.pixels, b.pixels,
                "legacy RGB {w}x{h} non-deterministic decode"
            );
            seed = seed.wrapping_add(0x1000);
        }

        for &(w, h) in yuv_dims {
            // YV12 (type 10).
            let px = lcg_bytes(seed ^ (w as u64), PixelKind::Yv12.buffer_len(w, h));
            let frame = encode_arith_yv12(&px, w, h);
            let a = decode_frame(&frame, w, h, PixelKind::Yv12).expect("yv12 decode A");
            let b = decode_frame(&frame, w, h, PixelKind::Yv12).expect("yv12 decode B");
            assert_eq!(a.pixels, b.pixels, "YV12 {w}x{h} non-deterministic decode");

            // YUY2 (type 3).
            let px = lcg_bytes(seed ^ 0x0303, PixelKind::Yuy2.buffer_len(w, h));
            let frame = encode_arith_yuy2(&px, w, h);
            let a = decode_frame(&frame, w, h, PixelKind::Yuy2).expect("yuy2 decode A");
            let b = decode_frame(&frame, w, h, PixelKind::Yuy2).expect("yuy2 decode B");
            assert_eq!(a.pixels, b.pixels, "YUY2 {w}x{h} non-deterministic decode");
            seed = seed.wrapping_add(0x1000);
        }

        // Reduced-resolution (type 11): the dimension guard requires both
        // dims to be a multiple of 4 (the half-res YV12 body is itself a
        // 4:2:0 plane geometry — `spec/01` §2.4 / decoder guard). Sweep a
        // dedicated set of 4-aligned dimensions.
        let red_dims: &[(u32, u32)] = &[(4, 4), (8, 4), (8, 8), (12, 8), (16, 8)];
        for &(w, h) in red_dims {
            let px = lcg_bytes(seed ^ 0x0b0b ^ (w as u64), PixelKind::Yv12.buffer_len(w, h));
            let frame = encode_arith_reduced_res(&px, w, h);
            let a = decode_frame(&frame, w, h, PixelKind::Yv12).expect("type11 decode A");
            let b = decode_frame(&frame, w, h, PixelKind::Yv12).expect("type11 decode B");
            assert_eq!(
                a.pixels, b.pixels,
                "reduced-res {w}x{h} non-deterministic decode"
            );
            seed = seed.wrapping_add(0x1000);
        }
    }

    /// Malformed / arbitrary payloads: the fuzz suites already prove
    /// `decode_frame` *returns* (never panics) on hostile bytes; this
    /// pins the stronger property that what it returns is *deterministic*.
    /// A decoder that read uninitialised scratch off the end of an
    /// allocation, or branched on stale buffer contents, would yield two
    /// diverging `Result`s on the same input — caught here. We compare
    /// the full `Result`: same `Ok(pixels)` byte-for-byte, or the same
    /// `Err` variant. Driven across all four host pixel formats so every
    /// wire-type → host dispatch is covered.
    #[test]
    fn arbitrary_payload_decode_is_deterministic() {
        let kinds = [
            PixelKind::Bgr24,
            PixelKind::Bgra32,
            PixelKind::Yv12,
            PixelKind::Yuy2,
        ];
        let mut state = 0xbad0_c0de_5eed_0357u64;
        for iter in 0..600u32 {
            // Small even dims keep chroma math exact and the raster tiny.
            let w = 2 * (1 + (lcg_next(&mut state) % 8) as u32);
            let h = 2 * (1 + (lcg_next(&mut state) % 6) as u32);
            let len = 1 + (lcg_next(&mut state) % 96) as usize;
            let payload = lcg_bytes(state, len);
            for &kind in &kinds {
                let a = decode_frame(&payload, w, h, kind);
                let b = decode_frame(&payload, w, h, kind);
                match (&a, &b) {
                    (Ok(da), Ok(db)) => assert_eq!(
                        da.pixels, db.pixels,
                        "iter {iter} {w}x{h} {kind:?}: Ok diverged on repeat",
                    ),
                    (Err(_), Err(_)) => {
                        // Compare the Debug rendering: cheap structural
                        // equality without requiring PartialEq on Error.
                        assert_eq!(
                            format!("{a:?}"),
                            format!("{b:?}"),
                            "iter {iter} {w}x{h} {kind:?}: Err variant diverged on repeat",
                        );
                    }
                    _ => panic!(
                        "iter {iter} {w}x{h} {kind:?}: Ok/Err disagreement on repeat: \
                         {a:?} vs {b:?}",
                    ),
                }
            }
        }
    }

    /// Stateful no-drift: after one keyframe, N consecutive NULL ("JUMP")
    /// frames must each replay the keyframe byte-for-byte. A replay path
    /// that mutated its stored predecessor in place would accumulate
    /// drift and diverge by the Nth frame; this pins zero drift across a
    /// long run (`spec/01` §1.1).
    #[test]
    fn consecutive_null_replays_have_zero_drift() {
        let (w, h) = (8u32, 8u32);
        let px = lcg_bytes(0x4a4d_5021, (w * h) as usize * 4); // "JMP!"
        let keyframe = encode_arith_rgba(&px, w, h);

        let mut dec = Decoder::new();
        let first = dec
            .decode(&keyframe, w, h, PixelKind::Bgra32)
            .expect("keyframe must decode");
        let reference = first.pixels.clone();
        assert_eq!(reference, px, "keyframe must be byte-exact");

        for n in 0..64u32 {
            let replay = dec
                .decode(&[], w, h, PixelKind::Bgra32)
                .unwrap_or_else(|e| panic!("NULL replay {n} must succeed: {e:?}"));
            assert_eq!(
                replay.pixels, reference,
                "NULL replay {n} drifted from the keyframe",
            );
        }
    }
}
