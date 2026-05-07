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
    encode_arith_rgb24, encode_arith_rgba, encode_arith_yv12, encode_channel_arith_rle,
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
fn unsupported_frame_types_surface_distinct_error() {
    // YUY2 (3), legacy RGB (7), and reduced-resolution (11) remain
    // unsupported; YV12 (10) is round-2 supported and exits the
    // unsupported set.
    for b in &[3u8, 7, 11] {
        let r = decode_frame(&[*b], 4, 4, PixelKind::Bgr24);
        assert!(matches!(r, Err(crate::Error::UnsupportedFrameType(_))));
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
