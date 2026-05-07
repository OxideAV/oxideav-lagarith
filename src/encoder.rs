//! Test-only encoder used to drive the self-roundtrip suite.
//!
//! Not exposed in the public API (gated `#[cfg(test)]` from
//! `lib.rs`). The encoder produces frames the decoder accepts but
//! makes no claim to byte-equality with the proprietary's encoder
//! output — that's an Auditor concern for later rounds.

#![cfg(test)]

use crate::fibonacci::encode_freq_table;
use crate::frame::pack_channels;
use crate::predict::{apply_plane_forward, cross_plane_decorrelate_rgb_forward};
use crate::range_coder::{Cdf, RangeEncoder};
use crate::rle::contract_raw;

/// Encode a uncompressed (frame type 1) frame.
pub fn encode_uncompressed(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + payload.len());
    out.push(1);
    out.extend_from_slice(payload);
    out
}

/// Encode a NULL ("JUMP") frame: an empty payload that signals to a
/// stateful decoder to replay the previous frame (`spec/01` §1.1).
pub fn encode_null() -> Vec<u8> {
    Vec::new()
}

/// Encode a Solid-Grey (type 5) frame.
pub fn encode_solid_grey(value: u8) -> Vec<u8> {
    vec![5, value]
}

/// Encode a Solid-RGB (type 6) frame. Bytes are written to wire
/// positions 1, 2, 3 -> output +0, +1, +2 (BGR memory order).
pub fn encode_solid_rgb(b: u8, g: u8, r: u8) -> Vec<u8> {
    vec![6, b, g, r]
}

/// Encode a Solid-RGBA (type 9) frame.
pub fn encode_solid_rgba(b: u8, g: u8, r: u8, a: u8) -> Vec<u8> {
    vec![9, b, g, r, a]
}

/// Build a per-channel byte sequence using the channel-header sub-
/// path that produces the smallest bytes for the given plane.
/// Round-1 strategy: try header-`0x00` (Fibonacci + range-coded) and
/// header-`0x04` (raw memcpy), pick whichever is smaller.
pub fn encode_channel_simple(plane: &[u8]) -> Vec<u8> {
    // Build the arithmetic-coded variant first (header 0x00).
    let mut freq = [0u32; 256];
    for &b in plane {
        freq[b as usize] = freq[b as usize].saturating_add(1);
    }
    // If the plane is empty, emit a header-`0xff` solid fill of
    // zeros (the range coder needs at least one non-zero freq).
    if plane.is_empty() {
        return vec![0xff, 0];
    }
    // If all symbols collapsed to one value, emit a solid fill —
    // the range coder still works but takes more bytes.
    let nonzero = freq.iter().filter(|&&f| f > 0).count();
    if nonzero == 1 {
        // header 0xff + the byte
        return vec![0xff, plane[0]];
    }

    // Encode arithmetic body.
    let cdf = Cdf::from_frequencies(&freq).expect("CDF builds for non-empty plane");
    let mut enc = RangeEncoder::new();
    for &b in plane {
        enc.encode_symbol(&cdf, b as usize);
    }
    let body = enc.finish();
    let prefix = encode_freq_table(&freq);

    let mut out = Vec::with_capacity(1 + prefix.len() + body.len());
    out.push(0x00);
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&body);

    // Compare against raw-memcpy alternative (header 0x04).
    if 1 + plane.len() < out.len() {
        let mut raw = Vec::with_capacity(1 + plane.len());
        raw.push(0x04);
        raw.extend_from_slice(plane);
        raw
    } else {
        out
    }
}

/// Encode a single channel using header 0x01..0x03 (arithmetic +
/// RLE). `escape_len` must be 1, 2, or 3.
pub fn encode_channel_arith_rle(plane: &[u8], escape_len: usize) -> Vec<u8> {
    debug_assert!((1..=3).contains(&escape_len));
    // Build pre-RLE symbol stream.
    let symbols = contract_raw(plane, escape_len);
    let pre_rle_count = symbols.len();

    // Build frequency table over the symbol stream.
    let mut freq = [0u32; 256];
    for &b in &symbols {
        freq[b as usize] = freq[b as usize].saturating_add(1);
    }
    // If everything mapped to one symbol, we can't use the arithmetic
    // path. (Should be rare.) Fall back to the simple path which
    // will pick header 0xff or raw.
    let nonzero = freq.iter().filter(|&&f| f > 0).count();
    if nonzero <= 1 || symbols.is_empty() {
        return encode_channel_simple(plane);
    }
    // The dispatcher fall-back rule: if u32 length >= n_pixels the
    // dispatcher reads the bytes 1..4 as the start of the Fibonacci
    // prefix instead. Make sure pre_rle_count < n_pixels, otherwise
    // pick the header-0 path.
    if pre_rle_count >= plane.len() {
        return encode_channel_simple(plane);
    }

    let cdf = Cdf::from_frequencies(&freq).unwrap();
    let mut enc = RangeEncoder::new();
    for &b in &symbols {
        enc.encode_symbol(&cdf, b as usize);
    }
    let body = enc.finish();
    let prefix = encode_freq_table(&freq);

    let mut out = Vec::with_capacity(5 + prefix.len() + body.len());
    out.push(escape_len as u8);
    out.extend_from_slice(&(pre_rle_count as u32).to_le_bytes());
    out.extend_from_slice(&prefix);
    out.extend_from_slice(&body);
    out
}

/// Encode an arithmetic RGB24 frame (type 4). Input is packed BGR
/// row-major, `width * height * 3` bytes long.
pub fn encode_arith_rgb24(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    debug_assert_eq!(pixels.len(), n * 3);

    // Split BGR (output memory order) into the per-plane buffers.
    // Wire plane order: R(=output+0=B), G(=output+1), B(=output+2=R).
    let mut plane_b = Vec::with_capacity(n); // wire R
    let mut plane_g = Vec::with_capacity(n);
    let mut plane_r = Vec::with_capacity(n); // wire B
    for px in pixels.chunks_exact(3) {
        plane_b.push(px[0]);
        plane_g.push(px[1]);
        plane_r.push(px[2]);
    }

    // Cross-plane decorrelation (forward).
    cross_plane_decorrelate_rgb_forward(&mut plane_b, &plane_g, &mut plane_r);

    // Spatial predictor (forward).
    let res_b = apply_plane_forward(&plane_b, width as usize, height as usize);
    let res_g = apply_plane_forward(&plane_g, width as usize, height as usize);
    let res_r = apply_plane_forward(&plane_r, width as usize, height as usize);

    // Per-channel encode.
    let ch_b = encode_channel_simple(&res_b);
    let ch_g = encode_channel_simple(&res_g);
    let ch_r = encode_channel_simple(&res_r);

    // Choose type 4 (width % 4 == 0) or type 2 (otherwise).
    let type_byte = if width % 4 == 0 { 4 } else { 2 };
    pack_channels(type_byte, &[&ch_b, &ch_g, &ch_r])
}

/// Encode an arithmetic YV12 frame (type 10). Input is concatenated
/// `Y || V || U` planes (the same layout the YV12 decoder produces
/// per `spec/03` §6.1). Each plane goes through the per-plane
/// forward predictor independently — no cross-plane decorrelation.
pub fn encode_arith_yv12(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let y_pixels = w * h;
    let c_pixels = y_pixels / 4;
    debug_assert_eq!(pixels.len(), y_pixels + 2 * c_pixels);

    let plane_y = &pixels[..y_pixels];
    let plane_v = &pixels[y_pixels..y_pixels + c_pixels];
    let plane_u = &pixels[y_pixels + c_pixels..];

    let res_y = apply_plane_forward(plane_y, w, h);
    let cw = w / 2;
    let ch = h / 2;
    let (res_v, res_u) = if cw * ch == c_pixels {
        (
            apply_plane_forward(plane_v, cw, ch),
            apply_plane_forward(plane_u, cw, ch),
        )
    } else {
        (
            apply_plane_forward(plane_v, c_pixels, 1),
            apply_plane_forward(plane_u, c_pixels, 1),
        )
    };

    let ch_y = encode_channel_simple(&res_y);
    let ch_v = encode_channel_simple(&res_v);
    let ch_u = encode_channel_simple(&res_u);

    pack_channels(10, &[&ch_y, &ch_v, &ch_u])
}

/// Encode an arithmetic RGBA frame (type 8). Input is packed BGRA.
pub fn encode_arith_rgba(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    debug_assert_eq!(pixels.len(), n * 4);

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

    let res_b = apply_plane_forward(&plane_b, width as usize, height as usize);
    let res_g = apply_plane_forward(&plane_g, width as usize, height as usize);
    let res_r = apply_plane_forward(&plane_r, width as usize, height as usize);
    let res_a = apply_plane_forward(&plane_a, width as usize, height as usize);

    let ch_b = encode_channel_simple(&res_b);
    let ch_g = encode_channel_simple(&res_g);
    let ch_r = encode_channel_simple(&res_r);
    let ch_a = encode_channel_simple(&res_a);

    pack_channels(8, &[&ch_b, &ch_g, &ch_r, &ch_a])
}
