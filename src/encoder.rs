//! Test-only encoder used to drive the self-roundtrip suite.
//!
//! Not exposed in the public API (gated `#[cfg(test)]` from
//! `lib.rs`). The encoder produces frames the decoder accepts but
//! makes no claim to byte-equality with the proprietary's encoder
//! output — that's an Auditor concern for later rounds.

#![cfg(test)]

use crate::fibonacci::encode_freq_table;
use crate::frame::pack_channels;
use crate::legacy_range_coder::{
    build_legacy_cdf, encode_legacy_freq_table, is_rare_symbol_cluster, LegacyRangeEncoder,
};
use crate::predict::{
    apply_plane_forward, apply_plane_forward_with_rule, cross_plane_decorrelate_rgb_forward,
    FirstColRule,
};
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

/// Encode an arithmetic YUY2 frame (type 3, round 3). Input is
/// packed YUY2 (`Y0 U Y1 V` per pair of pixels at columns `2k,
/// 2k+1`). Width must be even (the encoder mirrors the proprietary's
/// macropixel constraint).
pub fn encode_arith_yuy2(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    debug_assert_eq!(pixels.len(), w * h * 2);
    debug_assert_eq!(w % 2, 0, "encoder requires even width for YUY2");
    let cw = w / 2;
    let y_pixels = w * h;
    let c_pixels = cw * h;

    // Unpack the packed YUY2 buffer into three planes (Y, U, V).
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

/// Encode a reduced-resolution YV12 frame (type 11, round 3).
///
/// Per `spec/01` §2.4 the wire body is the same as a type-10 frame
/// at half-W/half-H. The 64-bit Lagarith encoder does **not**
/// produce type 11 in the wild — this helper exists only to drive
/// the round-3 self-roundtrip suite. Width and height must both be
/// at least 2.
///
/// Input layout: full-resolution `Y || V || U` planes (the same
/// layout the YV12 decoder produces). The encoder downsamples each
/// plane by 2× via skip-by-2 (every other row, every other column —
/// the inverse of the decoder's nearest-neighbour upscaler) and
/// re-routes through [`encode_arith_yv12`], rewriting byte 0 to
/// `0x0b`.
pub fn encode_arith_reduced_res(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let half_w = w / 2;
    let half_h = h / 2;
    debug_assert!(half_w >= 1 && half_h >= 1);
    debug_assert_eq!(w % 2, 0);
    debug_assert_eq!(h % 2, 0);
    let big_y = w * h;
    let big_cw = w / 2;
    let big_ch = h / 2;
    let big_c = big_cw * big_ch;
    debug_assert_eq!(pixels.len(), big_y + 2 * big_c);

    // Skip-by-2 downsample of each plane.
    let small_cw = half_w / 2;
    let small_ch = half_h / 2;
    let small_y = half_w * half_h;
    let small_c = small_cw * small_ch;
    let mut buf = Vec::with_capacity(small_y + 2 * small_c);
    // Y
    for y in 0..half_h {
        let row = (2 * y) * w;
        for x in 0..half_w {
            buf.push(pixels[row + 2 * x]);
        }
    }
    // V
    for y in 0..small_ch {
        let row = big_y + (2 * y) * big_cw;
        for x in 0..small_cw {
            buf.push(pixels[row + 2 * x]);
        }
    }
    // U
    for y in 0..small_ch {
        let row = big_y + big_c + (2 * y) * big_cw;
        for x in 0..small_cw {
            buf.push(pixels[row + 2 * x]);
        }
    }

    let mut frame = encode_arith_yv12(&buf, half_w as u32, half_h as u32);
    // Rewrite byte 0 from 0x0a (type 10) to 0x0b (type 11).
    frame[0] = 11;
    frame
}

/// Encode one **type-7 (legacy RGB)** channel using the
/// **RLE-then-Fibonacci** wire path of `spec/07` §2.3 / §6.3
/// (outer header ∈ {0x01, 0x02, 0x03}).
///
/// Wire layout:
/// ```text
///  byte 0:        outer channel-header byte = escape_len ∈ {1, 2, 3}
///  bytes 1..5:    u32 LE post-RLE byte count L (≤ 256)
///  bytes 5..5+M:  RLE-compressed input expanding to the L-byte
///                 Fibonacci-coded freq-table buffer
///  byte 5+M:      post-Fibonacci 1-byte reservation (only when the
///                 Fibonacci stream length is a multiple of 8 bits)
///  remainder:     legacy range-coder body
/// ```
///
/// The encoder runs `encode_legacy_channel` to compute the bare-
/// Fibonacci freq-table bit stream + reservation byte, then RLE-
/// contracts that byte stream into a smaller wire body using the
/// `spec/05` zero-run-escape encoding with the supplied `escape_len`.
///
/// Test-only helper for round-5 self-roundtrip coverage of the
/// header `0x01..=0x03` decode path. The cleanroom encoder ships
/// header-0 only at the frame level (`encode_legacy_rgb` always
/// uses `encode_legacy_channel`); this helper exists so the round-5
/// roundtrip suite can exercise the RLE-then-Fibonacci sub-path
/// end-to-end.
pub fn encode_legacy_channel_rle(plane: &[u8], escape_len: usize) -> Vec<u8> {
    debug_assert!((1..=3).contains(&escape_len));
    debug_assert!(!plane.is_empty(), "encode_legacy_channel_rle: empty plane");

    // 1. Compute the bare-Fibonacci freq + transmit_freq + CDF the
    // same way `encode_legacy_channel` does, but emit just the
    // Fibonacci buffer (no outer header / inner flag).
    let mut freq = [0u32; 256];
    for &b in plane {
        freq[b as usize] = freq[b as usize].saturating_add(1);
    }
    let (cdf_initial, total) = build_legacy_cdf(&freq).unwrap();
    let mut transmit_freq = [0u32; 256];
    for c in 0..256 {
        transmit_freq[c] = cdf_initial[c + 1] - cdf_initial[c];
    }
    for c in 0..256 {
        if freq[c] > 0 && transmit_freq[c] == 0 {
            let donor = (0..256)
                .max_by_key(|&i| {
                    if transmit_freq[i] > 1 {
                        transmit_freq[i]
                    } else {
                        0
                    }
                })
                .filter(|&i| transmit_freq[i] > 1);
            if let Some(d) = donor {
                transmit_freq[c] += 1;
                transmit_freq[d] -= 1;
            }
        }
    }
    debug_assert_eq!(
        transmit_freq.iter().map(|&f| f as u64).sum::<u64>(),
        total as u64
    );
    let mut cdf = vec![0u32; 257];
    let mut acc: u32 = 0;
    for c in 0..256 {
        cdf[c] = acc;
        acc = acc.wrapping_add(transmit_freq[c]);
    }
    cdf[256] = acc;

    // 2. Fibonacci-encode the freq table.
    let (fib_bytes, fib_aligned) = encode_legacy_freq_table(&transmit_freq);

    // 3. The post-RLE buffer is `fib_bytes` (the Fibonacci freq table)
    // — the decoder of `decode_legacy_rle_then_fib` expects the
    // post-RLE expansion output to BE the Fibonacci-coded byte stream
    // the freq-table decoder walks. Pad to the proprietary's 256-byte
    // canonical buffer size (the Fib decoder ignores trailing bytes
    // beyond `fib_bytes` because it stops after 256 frequencies have
    // been decoded). Padding is `0x00` per cleanroom convention.
    let post_rle_len = fib_bytes.len();
    debug_assert!(
        post_rle_len <= 256,
        "Fibonacci freq table must fit in 256-byte buffer"
    );

    // 4. RLE-contract the Fibonacci byte stream.
    let rle_compressed = crate::rle::contract_raw(&fib_bytes, escape_len);

    // 5. Range-encode the plane against the CDF.
    let mut enc = LegacyRangeEncoder::new(cdf, total);
    for &b in plane {
        enc.encode_byte(b);
    }
    let body = enc.finish();

    // 6. Stitch the RLE-then-Fibonacci channel layout.
    let mut out = Vec::with_capacity(5 + rle_compressed.len() + 1 + body.len());
    out.push(escape_len as u8); // outer channel header
    out.extend_from_slice(&(post_rle_len as u32).to_le_bytes());
    out.extend_from_slice(&rle_compressed);
    if fib_aligned {
        // Post-Fibonacci 1-byte reservation per audit/08 §3.2 — the
        // legacy range decoder's priming-byte read at offset +1 of
        // its first argument skips this byte.
        out.push(0x00);
    }
    out.extend_from_slice(&body);
    out
}

/// Encode one **type-7 (legacy RGB)** channel into a byte sequence
/// using the bare-Fibonacci 2-byte channel prefix path of
/// `spec/07` §6.3. The output layout:
///
/// ```text
///  byte 0:        outer channel-header byte = 0x00
///  byte 1:        inner codec-mode flag     = 0x00 (bare Fib)
///  bytes 2..2+N:  Fibonacci-coded 256-entry freq table
///  byte 2+N:      post-Fibonacci 1-byte reservation (only when
///                 the encoded bit stream length is a multiple of 8)
///  remainder:     legacy range-coder body
/// ```
///
/// Per `spec/07` §6.2, the cleanroom encoder may transmit any
/// frequency table whose post-§3 rescale produces the CDF the
/// encoder uses for arithmetic coding. The simplest legal choice —
/// what we do here — is to transmit the histogram counts directly;
/// the decoder's `build_legacy_cdf` re-runs the same algorithm and
/// produces the same CDF.
pub fn encode_legacy_channel(plane: &[u8]) -> Vec<u8> {
    debug_assert!(!plane.is_empty(), "encode_legacy_channel: empty plane");

    // 1. Histogram.
    let mut freq = [0u32; 256];
    for &b in plane {
        freq[b as usize] = freq[b as usize].saturating_add(1);
    }

    // 2. Run build_legacy_cdf on the histogram so encoder + decoder
    // land on the same CDF. The transmitted frequency table is the
    // post-rescale per-symbol delta of that CDF (`cdf[c+1] - cdf[c]`).
    let (cdf_initial, total) = build_legacy_cdf(&freq).unwrap();
    let mut transmit_freq = [0u32; 256];
    for c in 0..256 {
        transmit_freq[c] = cdf_initial[c + 1] - cdf_initial[c];
    }

    // 3. Patch any symbol with hist[c] > 0 whose rescale collapsed
    // it to 0 (would prevent the range encoder from selecting that
    // symbol). Steal one count from the largest non-zero bin.
    for c in 0..256 {
        if freq[c] > 0 && transmit_freq[c] == 0 {
            // Find the largest donor with > 1 count to give up.
            let donor = (0..256)
                .max_by_key(|&i| {
                    if transmit_freq[i] > 1 {
                        transmit_freq[i]
                    } else {
                        0
                    }
                })
                .filter(|&i| transmit_freq[i] > 1);
            if let Some(d) = donor {
                transmit_freq[c] += 1;
                transmit_freq[d] -= 1;
            }
        }
    }

    // 4. Build the final CDF from the patched frequencies (no
    // rescale: sum is already `total`).
    debug_assert_eq!(
        transmit_freq.iter().map(|&f| f as u64).sum::<u64>(),
        total as u64
    );
    let mut cdf = vec![0u32; 257];
    let mut acc: u32 = 0;
    for c in 0..256 {
        cdf[c] = acc;
        acc = acc.wrapping_add(transmit_freq[c]);
    }
    cdf[256] = acc;

    // 5. Fibonacci-encode the patched frequencies.
    let (fib_bytes, fib_aligned) = encode_legacy_freq_table(&transmit_freq);

    // 6. Range-encode the plane against the CDF.
    let mut enc = LegacyRangeEncoder::new(cdf, total);
    for &b in plane {
        enc.encode_byte(b);
    }
    let body = enc.finish();

    // 7. Stitch the channel layout together.
    let mut out = Vec::with_capacity(2 + fib_bytes.len() + 1 + body.len());
    out.push(0x00); // outer channel header (header == 0 path)
    out.push(0x00); // inner codec-mode flag (bare Fibonacci)
    out.extend_from_slice(&fib_bytes);
    if fib_aligned {
        // Post-Fibonacci 1-byte reservation per audit/08 §3.2 / spec/07
        // §9.1 item 7c. Only emitted when the encoded bit stream ends
        // on a byte boundary; the decoder's matching skip lives in
        // `channel::decode_legacy_channel`.
        out.push(0x00);
    }
    out.extend_from_slice(&body);
    out
}

/// Compute a 256-entry histogram from a residual plane.
fn histogram_from_plane(plane: &[u8]) -> [u32; 256] {
    let mut freq = [0u32; 256];
    for &b in plane {
        freq[b as usize] = freq[b as usize].saturating_add(1);
    }
    freq
}

/// Strategy E predicate (`audit/12 §7.1`): given the three residual
/// planes that would feed the type-7 entropy stage, return `true`
/// iff any plane's histogram matches the rare-symbol-cluster
/// signature. When `true`, the encoder must route around type 7 and
/// emit a type-1 (uncompressed) frame instead — type 1's roundtrip
/// is byte-exact on every fixture, sidestepping the
/// flat-CDF / pair-packed-CDF wire-format divergence that
/// `audit/12 §5..§6` localised to this fixture class.
fn type7_residuals_need_strategy_e(res_b: &[u8], res_g: &[u8], res_r: &[u8]) -> bool {
    let fb = histogram_from_plane(res_b);
    let fg = histogram_from_plane(res_g);
    let fr = histogram_from_plane(res_r);
    is_rare_symbol_cluster(&fb) || is_rare_symbol_cluster(&fg) || is_rare_symbol_cluster(&fr)
}

/// Encode a **type-7 (legacy RGB)** frame from a packed BGR24 input.
/// Same plane-decorrelation pipeline as `encode_arith_rgb24` — only
/// the per-channel entropy stage differs (legacy adaptive-CDF range
/// coder per `spec/07`, not the modern range coder + RLE dispatcher
/// of `spec/02` + `spec/06`).
///
/// Strategy E (`audit/12 §7.1`, wired round 6): when any of the
/// three residual planes (B', G, R') matches the rare-symbol-cluster
/// signature (`is_rare_symbol_cluster`), the encoder routes around
/// the type-7 emission and emits a type-1 (uncompressed) frame
/// instead. Type-1's roundtrip is byte-exact on every fixture
/// (`spec/01 §2.1`, `audit/11 §4.5`), so the resulting wire bytes
/// are always decode-correct against any conformant decoder. The
/// reason for this fallback is that on rare-symbol-cluster
/// histograms the cleanroom's flat-257-entry CDF and the
/// proprietary's pair-packed-513-entry CDF diverge at coarse
/// granularity (`audit/12 §5..§6`); the divergence is invisible
/// against our self-roundtrip suite (encoder + decoder share the
/// same CDF) but would surface against a hypothetical proprietary
/// decoder.
///
/// Test-only — the proprietary build does not produce type 7
/// (`spec/07` §6 + §9.2 item 8); cleanroom encoder ships only the
/// bare-Fibonacci `header == 0` path.
pub fn encode_legacy_rgb(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let n = width as usize * height as usize;
    debug_assert_eq!(pixels.len(), n * 3);

    let mut plane_b = Vec::with_capacity(n);
    let mut plane_g = Vec::with_capacity(n);
    let mut plane_r = Vec::with_capacity(n);
    for px in pixels.chunks_exact(3) {
        plane_b.push(px[0]);
        plane_g.push(px[1]);
        plane_r.push(px[2]);
    }

    // Cross-plane decorrelation (forward) — same as types 2 / 4.
    cross_plane_decorrelate_rgb_forward(&mut plane_b, &plane_g, &mut plane_r);

    // Spatial predictor (forward) — type 7 uses **Rule B** for the
    // first-column-of-row predictor (`spec/07` §9.1 item 7b).
    let res_b =
        apply_plane_forward_with_rule(&plane_b, width as usize, height as usize, FirstColRule::B);
    let res_g =
        apply_plane_forward_with_rule(&plane_g, width as usize, height as usize, FirstColRule::B);
    let res_r =
        apply_plane_forward_with_rule(&plane_r, width as usize, height as usize, FirstColRule::B);

    // Strategy E (`audit/12 §7.1`): route rare-symbol-cluster
    // residual histograms to type 1 to sidestep the flat / pair-
    // packed CDF divergence on the fixture class identified in
    // `audit/12 §3.6 / §5`.
    if type7_residuals_need_strategy_e(&res_b, &res_g, &res_r) {
        return encode_uncompressed(pixels);
    }

    let ch_b = encode_legacy_channel(&res_b);
    let ch_g = encode_legacy_channel(&res_g);
    let ch_r = encode_legacy_channel(&res_r);

    pack_channels(7, &[&ch_b, &ch_g, &ch_r])
}

/// Encode a **type-7 (legacy RGB)** frame using the **RLE-then-
/// Fibonacci** channel sub-path (`spec/07` §2.3 / §2.4). Same
/// pipeline as [`encode_legacy_rgb`] except the per-channel encoder
/// is [`encode_legacy_channel_rle`] with the supplied `escape_len`
/// (1, 2, or 3). Strategy E (`audit/12 §7.1`) propagates: a rare-
/// symbol-cluster residual histogram on any plane causes the
/// encoder to emit a type-1 (uncompressed) frame instead.
/// Test-only — drives the round-5 roundtrip suite for the
/// channel-header `0x01..=0x03` decode path.
pub fn encode_legacy_rgb_rle(pixels: &[u8], width: u32, height: u32, escape_len: usize) -> Vec<u8> {
    debug_assert!((1..=3).contains(&escape_len));
    let n = width as usize * height as usize;
    debug_assert_eq!(pixels.len(), n * 3);

    let mut plane_b = Vec::with_capacity(n);
    let mut plane_g = Vec::with_capacity(n);
    let mut plane_r = Vec::with_capacity(n);
    for px in pixels.chunks_exact(3) {
        plane_b.push(px[0]);
        plane_g.push(px[1]);
        plane_r.push(px[2]);
    }

    cross_plane_decorrelate_rgb_forward(&mut plane_b, &plane_g, &mut plane_r);

    let res_b =
        apply_plane_forward_with_rule(&plane_b, width as usize, height as usize, FirstColRule::B);
    let res_g =
        apply_plane_forward_with_rule(&plane_g, width as usize, height as usize, FirstColRule::B);
    let res_r =
        apply_plane_forward_with_rule(&plane_r, width as usize, height as usize, FirstColRule::B);

    // Strategy E propagates through the RLE-then-Fibonacci sub-path
    // — both sub-paths share the same flat-CDF range coder, so the
    // same fixture class triggers the same divergence.
    if type7_residuals_need_strategy_e(&res_b, &res_g, &res_r) {
        return encode_uncompressed(pixels);
    }

    let ch_b = encode_legacy_channel_rle(&res_b, escape_len);
    let ch_g = encode_legacy_channel_rle(&res_g, escape_len);
    let ch_r = encode_legacy_channel_rle(&res_r, escape_len);

    pack_channels(7, &[&ch_b, &ch_g, &ch_r])
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
