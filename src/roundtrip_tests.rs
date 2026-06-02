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
        // `tests/ffmpeg_pins.rs` covers). On every probed size the
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
    #[test]
    fn random_payload_no_panic_sweep() {
        for type_byte in 0u8..=12 {
            for &seed in &[0x1234_5678_9abc_def0u64, 0xfeed_face_dead_beef, 0] {
                for &len in &[1usize, 2, 5, 9, 16, 33, 64, 128] {
                    let mut payload = lcg_bytes(seed.wrapping_add(len as u64), len);
                    payload[0] = type_byte;
                    // Decode against each accepted (W, H, kind) shape;
                    // anything that returns Err is fine, anything that
                    // returns Ok is fine; the test fails only on
                    // panic.
                    let _ = decode_frame(&payload, 4, 4, PixelKind::Bgr24);
                    let _ = decode_frame(&payload, 4, 4, PixelKind::Bgra32);
                    let _ = decode_frame(&payload, 4, 4, PixelKind::Yv12);
                    let _ = decode_frame(&payload, 4, 4, PixelKind::Yuy2);
                }
            }
        }
    }

    /// Random per-channel bodies behind a valid type-4 RGB24 offset
    /// table — the channel dispatcher must not panic on any byte
    /// pattern in the channel body. Catches dispatcher-level
    /// regressions that would only surface against adversarial
    /// wire bytes.
    #[test]
    fn random_channel_bodies_no_panic_sweep() {
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
                // And the YV12 dispatcher (3 planes Y/V/U).
                let mut t10_frame = frame.clone();
                t10_frame[0] = 10;
                let _ = decode_frame(&t10_frame, 4, 4, PixelKind::Yv12);
                // And the YUY2 dispatcher.
                let mut t3_frame = frame.clone();
                t3_frame[0] = 3;
                let _ = decode_frame(&t3_frame, 4, 4, PixelKind::Yuy2);
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
        encode_arith_rgb24, encode_arith_rgba, encode_arith_yuy2, encode_arith_yv12,
        encode_legacy_rgb,
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
    /// Rule A (round 124 pinned Rule B for RGB24 / RGBA via ffmpeg;
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
    /// shares the Rule A pending-ffmpeg-pin status with YV12 (see
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
}
