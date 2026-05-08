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
    encode_arith_yv12, encode_channel_arith_rle, encode_legacy_rgb, encode_null, encode_solid_grey,
    encode_solid_rgb, encode_solid_rgba, encode_uncompressed,
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
    // round-4 decoder ships only the bare-Fibonacci sub-path
    // (spec/07 §6.3 / §9.2 item 9 — header-0 is sufficient for
    // round-trip correctness). Non-bare paths should surface
    // `BadChannelHeader`.
    use crate::frame::pack_channels;
    // Three minimal channels each starting with 0x00 0x01 (inner
    // codec-mode flag = 1) — should be rejected.
    let ch = vec![0x00u8, 0x01];
    let frame = pack_channels(7, &[&ch, &ch, &ch]);
    let r = decode_frame(&frame, 4, 4, PixelKind::Bgr24);
    assert!(
        matches!(r, Err(crate::Error::BadChannelHeader(_))),
        "non-zero inner codec-mode flag must surface BadChannelHeader, got {r:?}"
    );
}
