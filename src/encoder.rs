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
use crate::range_coder::{Cdf, RangeEncoder, TOP};
use crate::rle::contract_raw;

/// Largest transmitted-frequency total the modern range coder can
/// decode without the `q = range / total` quotient (`spec/02` §5)
/// collapsing to zero.
///
/// `spec/02` §2 puts the post-renormalisation `range` in
/// `[TOP + 1, 2^31]` (TOP = `0x800000`). Step A/B/C all start from
/// `q = range / total`; for the arithmetic to stay valid the
/// quotient must be `>= 1` at the worst case `range = TOP + 1`,
/// which requires `total <= TOP + 1`. We cap at `TOP` so the
/// worst-case quotient is `(TOP + 1) / TOP = 1` with one bit of
/// headroom.
///
/// `spec/04` §5 documents that the proprietary loader normalises the
/// per-symbol histogram (its `0x180001530..0x18000158a` block) before
/// building the reciprocal-multiply LUT — the same `q >= 1` guarantee,
/// reached the same way. The validation correction in `spec/04` §5
/// then establishes that the *wire* still carries a raw byte-histogram
/// whose total equals the symbol count for the fixtures probed (16 for
/// 4x4, 256 for 16x16, …) — all far below `TOP`. So this cap is a
/// no-op for every plane the proprietary fixtures exercise; it only
/// engages for planes whose symbol count exceeds `TOP` (> ~8.39M
/// residuals, e.g. a single 4K+ plane), where a raw total would push
/// `q` to zero and break the coder.
const MAX_MODERN_TOTAL: u32 = TOP;

/// Rescale a 256-entry raw histogram so its total stays within
/// [`MAX_MODERN_TOTAL`], preserving the nonzero set (every
/// `freq[s] > 0` maps to a transmitted frequency `>= 1`, so no symbol
/// the plane actually uses becomes undecodable).
///
/// When the raw total already fits, the histogram is returned
/// **unchanged** — this keeps the wire byte-identical to the raw-
/// histogram form for the common small/medium plane (`spec/04` §5
/// validation correction), so the existing self-roundtrip fixtures
/// are unaffected.
///
/// When the raw total exceeds the cap, frequencies are scaled by
/// `floor(freq[s] * cap / sum)`, clamped up to `1` for every nonzero
/// slot, then any residual overshoot is trimmed from the largest
/// slots (never below `1`). The result satisfies
/// `sum(out) <= MAX_MODERN_TOTAL` and `out[s] > 0 <=> freq[s] > 0`.
/// Because the encoder transmits this exact rescaled table and the
/// decoder rebuilds the identical CDF from it (`spec/04` §6), the
/// arithmetic coder remains exact: only the probability model
/// changes, never the symbol→byte mapping, so decoded bytes match
/// the input byte-for-byte.
fn rescale_to_max_total(freq: &[u32; 256], cap: u32) -> [u32; 256] {
    let sum: u64 = freq.iter().map(|&f| f as u64).sum();
    if sum <= cap as u64 {
        return *freq;
    }
    let cap64 = cap as u64;
    let mut out = [0u32; 256];
    for s in 0..256 {
        if freq[s] == 0 {
            continue;
        }
        // floor(freq * cap / sum), clamped to >= 1 so a used symbol
        // never drops out of the transmitted table.
        let scaled = (freq[s] as u64 * cap64 / sum).max(1);
        out[s] = scaled as u32;
    }
    // The `max(1, …)` clamps can lift the running sum above `cap`
    // (each of the up-to-256 nonzero slots can add at most 1 over its
    // floored share). Trim the overshoot from the largest slots,
    // never reducing a nonzero slot below 1.
    let mut total: u64 = out.iter().map(|&f| f as u64).sum();
    while total > cap64 {
        // Find the largest slot with headroom above 1.
        let mut victim = usize::MAX;
        let mut best = 1u32;
        for (s, &f) in out.iter().enumerate() {
            if f > best {
                best = f;
                victim = s;
            }
        }
        // `total > cap` with every nonzero slot already at 1 is
        // impossible: at most 256 slots × 1 = 256 <= cap. So a victim
        // with `f > 1` always exists here.
        debug_assert_ne!(victim, usize::MAX, "overshoot trim found no victim");
        out[victim] -= 1;
        total -= 1;
    }
    out
}

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

    // Rescale the histogram so the transmitted total stays inside the
    // modern coder's `q = range / total >= 1` operating range
    // (`spec/02` §2 / §5; see `MAX_MODERN_TOTAL`). For planes whose
    // symbol count is <= TOP this is the raw histogram unchanged.
    let freq = rescale_to_max_total(&freq, MAX_MODERN_TOTAL);

    // Encode arithmetic body. The CDF and the transmitted prefix come
    // from the *same* rescaled table, so the decoder rebuilds the
    // identical CDF and recovers the original bytes (`spec/04` §6).
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

    // Same modern-coder `q >= 1` cap as the header-0x00 path: the
    // pre-RLE symbol stream's histogram must not push the transmitted
    // total past TOP (`spec/02` §5). No-op for the small streams the
    // RLE fixtures exercise.
    let freq = rescale_to_max_total(&freq, MAX_MODERN_TOTAL);

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

/// Encode a single channel using the **raw-RLE** wire form
/// (channel headers `0x05..0x07`, `escape_len = header - 4`).
///
/// Layout: `[header_byte, rle_compressed_plane...]` — no Fibonacci
/// prefix, no arithmetic body. The decoder's `expand_raw` reverses
/// the contraction directly. Per `spec/03` §2.1 row "header
/// 0x05..0x07": "Raw bytes at offset 1, post-processed with RLE
/// escape `escape_len = header - 4`."
///
/// This form is attractive on planes whose residual histogram is
/// flat enough that arithmetic coding gains little (so the
/// Fibonacci-prefix overhead dominates), yet still carry enough
/// zero runs for the `spec/05` escape to shrink the byte count
/// below the raw `0x04` baseline.
pub fn encode_channel_raw_rle(plane: &[u8], escape_len: usize) -> Vec<u8> {
    debug_assert!((1..=3).contains(&escape_len));
    let compressed = contract_raw(plane, escape_len);
    let mut out = Vec::with_capacity(1 + compressed.len());
    out.push((escape_len as u8) + 4); // 0x05, 0x06, or 0x07
    out.extend_from_slice(&compressed);
    out
}

/// Encode a single channel by **trying every supported header form
/// and returning the smallest valid wire body**.
///
/// `spec/06` §1.7 + §2.7 specify how the proprietary encoder at
/// `lagarith.dll!0x18000c500` selects between the `0x00`, `0x01..0x03`,
/// `0x04`, and `0x05..0x07` sub-paths — the encoder's optimisation
/// role is the per-plane choice (the wire form is decoder-blind to
/// which sub-path the encoder picked, only the resulting bytes
/// matter). The cleanroom encoder cannot reproduce the proprietary
/// heuristic byte-exactly without the disassembled selector, but it
/// can produce a *legal* + *minimum-byte* choice by encoding all
/// candidates and selecting whichever yields the shortest output.
///
/// Candidate set:
/// * `0xff` — solid fill (2 bytes; valid only when the plane is
///   constant or empty).
/// * `0x00` — Fibonacci-prefixed arithmetic, no RLE.
/// * `0x01`, `0x02`, `0x03` — Fibonacci-prefixed arithmetic with
///   pre-RLE contraction at `escape_len = header`. The `spec/06`
///   §1.5 fall-back rule (pre-RLE symbol count `>= n_pixels`
///   diverts to header-`0x00` semantics) is enforced by skipping
///   the candidate whose pre-RLE count would trip it.
/// * `0x04` — raw memcpy (no entropy, no RLE).
/// * `0x05`, `0x06`, `0x07` — raw bytes with RLE post-processing
///   at `escape_len = header - 4`. No Fibonacci prefix.
///
/// Tie-breaker: lower header byte wins (so a tie between `0x00`
/// and `0x04` keeps the historical `encode_channel_simple`
/// preference). Because every candidate's wire body is what the
/// existing `decode_channel` dispatcher already accepts, the
/// returned bytes always self-roundtrip; the new
/// `encode_channel_best_*` roundtrip tests below verify each
/// header form decodes back to the input plane.
///
/// **Wire compatibility.** The choice between sub-paths is
/// per-channel and externally invisible — a decoder reads byte 0,
/// routes to the matching sub-path, and recovers the same plane
/// regardless of which form the encoder picked. So replacing
/// `encode_channel_simple` with `encode_channel_best` in a frame
/// encoder cannot regress self-roundtrip correctness; it can only
/// shorten the output. As of round 174 every modern frame encoder
/// (`encode_arith_rgb24` / `encode_arith_yv12` / `encode_arith_yuy2`
/// / `encode_arith_rgba` and their `encode_arith_reduced_res`
/// dispatcher) routes per-channel through this selector. The
/// `encode_channel_simple` entry point is retained for direct use
/// (header `0x00` / `0x04` two-candidate path) by the rescale-cap
/// test scaffold (`encode_channel_simple_capped`) and by callers
/// that want the historical wire byte-identically.
pub fn encode_channel_best(plane: &[u8]) -> Vec<u8> {
    // Solid-fill / empty short-circuit — preserves the
    // `encode_channel_simple` early-return semantics (2 bytes is
    // unbeatable).
    if plane.is_empty() {
        return vec![0xff, 0];
    }
    let mut freq = [0u32; 256];
    for &b in plane {
        freq[b as usize] = freq[b as usize].saturating_add(1);
    }
    let nonzero = freq.iter().filter(|&&f| f > 0).count();
    if nonzero == 1 {
        return vec![0xff, plane[0]];
    }

    // Candidate 0x00 — Fibonacci-prefix + arithmetic, no RLE.
    let mut best = build_header_zero(plane, &freq);

    // Candidates 0x01..=0x03 — Fibonacci-prefix + arithmetic with
    // pre-RLE contraction. Skipped when the dispatcher fall-back
    // rule (`spec/06` §1.5) would trip.
    for escape_len in 1..=3usize {
        if let Some(candidate) = build_header_arith_rle(plane, escape_len) {
            if candidate.len() < best.len() {
                best = candidate;
            }
        }
    }

    // Candidate 0x04 — raw memcpy.
    let raw = {
        let mut v = Vec::with_capacity(1 + plane.len());
        v.push(0x04);
        v.extend_from_slice(plane);
        v
    };
    if raw.len() < best.len() {
        best = raw;
    }

    // Candidates 0x05..=0x07 — raw bytes with RLE post-processing.
    for escape_len in 1..=3usize {
        let candidate = encode_channel_raw_rle(plane, escape_len);
        if candidate.len() < best.len() {
            best = candidate;
        }
    }

    best
}

/// Build the header-`0x00` (Fibonacci + arithmetic, no RLE) wire
/// body for `plane`, given its pre-computed histogram. Used by
/// `encode_channel_best` to keep the candidate construction in one
/// place. Caller guarantees `plane` is non-empty and has at least
/// two distinct symbols.
fn build_header_zero(plane: &[u8], freq: &[u32; 256]) -> Vec<u8> {
    let freq = rescale_to_max_total(freq, MAX_MODERN_TOTAL);
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
    out
}

/// Build a header-`0x01..0x03` (Fibonacci + arithmetic with pre-RLE
/// contraction) wire body. Returns `None` when the candidate is
/// illegal under `spec/06` §1.5 (pre-RLE symbol count >= n_pixels
/// would trip the dispatcher fall-back to header-0 semantics) or
/// when the contracted symbol stream collapsed to a single symbol
/// (the arithmetic coder requires `nonzero >= 2`).
fn build_header_arith_rle(plane: &[u8], escape_len: usize) -> Option<Vec<u8>> {
    debug_assert!((1..=3).contains(&escape_len));
    let symbols = contract_raw(plane, escape_len);
    let pre_rle_count = symbols.len();
    if symbols.is_empty() || pre_rle_count >= plane.len() {
        return None;
    }
    let mut freq = [0u32; 256];
    for &b in &symbols {
        freq[b as usize] = freq[b as usize].saturating_add(1);
    }
    let nonzero = freq.iter().filter(|&&f| f > 0).count();
    if nonzero < 2 {
        return None;
    }
    let freq = rescale_to_max_total(&freq, MAX_MODERN_TOTAL);
    let cdf = Cdf::from_frequencies(&freq).ok()?;
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
    Some(out)
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

    // Spatial predictor (forward) — **Rule B** first-column-of-row,
    // matching the decoder (ffmpeg-confirmed; see `decode_arith_rgb`).
    let res_b =
        apply_plane_forward_with_rule(&plane_b, width as usize, height as usize, FirstColRule::B);
    let res_g =
        apply_plane_forward_with_rule(&plane_g, width as usize, height as usize, FirstColRule::B);
    let res_r =
        apply_plane_forward_with_rule(&plane_r, width as usize, height as usize, FirstColRule::B);

    // Per-channel encode — header-form selector picks the shortest
    // wire body from the eight legal forms `decode_channel` accepts
    // (`spec/03` §2.1 + `spec/06` §1.7). Per-channel and per-plane
    // choices are externally invisible (the decoder dispatches on
    // byte 0) so the wire stays decode-compatible with every
    // conformant decoder; only the size can change.
    let ch_b = encode_channel_best(&res_b);
    let ch_g = encode_channel_best(&res_g);
    let ch_r = encode_channel_best(&res_r);

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

    // Per-channel header-form selector — see `encode_channel_best`
    // for the candidate set and `spec/06` §1.5 fall-back guard.
    let ch_y = encode_channel_best(&res_y);
    let ch_v = encode_channel_best(&res_v);
    let ch_u = encode_channel_best(&res_u);

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

    // Per-channel header-form selector — see `encode_channel_best`.
    let ch_y = encode_channel_best(&res_y);
    let ch_u = encode_channel_best(&res_u);
    let ch_v = encode_channel_best(&res_v);

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

/// Encode one **type-7 (legacy RGB)** channel by trying every
/// supported wire form and returning the smallest valid body —
/// the legacy-fork analogue of [`encode_channel_best`] for the
/// modern fork.
///
/// `spec/07` §6.3 explicitly frames the choice between
/// **bare-Fibonacci** (header `0x00`, the canonical cleanroom
/// path) and the three **RLE-then-Fibonacci** sub-paths
/// (header `0x01..=0x03`, one per `escape_len ∈ {1, 2, 3}`)
/// as an *encoder-side compression trade-off* — bare-Fibonacci
/// works for any frequency distribution, while
/// RLE-then-Fibonacci compresses the freq-table channel prefix
/// better when the freq table is sparse (long runs of zero
/// frequencies for unused symbols). A canonical encoder
/// implementing all paths and picking the shortest is the
/// most flexible legal form.
///
/// The legacy fork's channel-header dispatcher
/// ([`crate::channel::decode_legacy_channel`]) accepts headers
/// `0x00..=0x03` and rejects anything outside that range, so the
/// candidate set is exhaustively four: one bare-Fibonacci form
/// plus three RLE-then-Fibonacci variants. Tie-breaker: the
/// bare-Fibonacci form wins on a tie (preserves the historical
/// `encode_legacy_channel` preference and keeps the wire byte-
/// identical on inputs where the RLE prefix is no smaller).
///
/// **Wire compatibility.** As with the modern fork's
/// `encode_channel_best`, the choice between legacy sub-paths is
/// per-channel and externally invisible — the decoder reads byte 0,
/// routes to the matching sub-path, and recovers the same plane
/// regardless of which form the encoder picked
/// (`spec/07` §1.3 / §6.3). Replacing `encode_legacy_channel` with
/// `encode_legacy_channel_best` in [`encode_legacy_rgb`] cannot
/// regress self-roundtrip correctness; it can only shrink output.
pub fn encode_legacy_channel_best(plane: &[u8]) -> Vec<u8> {
    debug_assert!(!plane.is_empty(), "encode_legacy_channel_best: empty plane");

    // Candidate `0x00` — bare-Fibonacci (always legal for any
    // non-empty plane).
    let mut best = encode_legacy_channel(plane);

    // Candidates `0x01..=0x03` — RLE-then-Fibonacci. The RLE
    // contraction targets the *Fibonacci-coded freq table* (256-byte
    // proprietary canonical buffer), not the residuals; whether it
    // shrinks the channel depends on the freq-table byte stream
    // having zero-byte runs the escape can swallow. Always legal —
    // there is no fall-back rule on the legacy path equivalent to
    // `spec/06` §1.5; the trampoline accepts any escape_len in
    // {1, 2, 3} per `spec/07` §2.3 / §2.4.
    for escape_len in 1..=3usize {
        let candidate = encode_legacy_channel_rle(plane, escape_len);
        if candidate.len() < best.len() {
            best = candidate;
        }
    }

    best
}

/// Encode a **type-7 (legacy RGB)** frame using
/// [`encode_legacy_channel_best`] per-channel — same pipeline as
/// [`encode_legacy_rgb`] except the per-channel entropy stage is the
/// header-form selector. Strategy E (`audit/12 §7.1`) still
/// propagates: rare-symbol-cluster residual histograms divert to
/// type 1 (the divergence is in the range-coder body, not the
/// channel-prefix form, so it triggers identically here).
///
/// Originally introduced round 141 to drive the legacy header-form
/// selector's roundtrip suite while the frame-level `encode_legacy_rgb`
/// kept its bare-Fibonacci call site. Round 174 flipped
/// `encode_legacy_rgb` to call `encode_legacy_channel_best` per-
/// channel; on every realistic histogram the cleanroom encoder
/// produces the selector picks bare-Fib (header `0x00`) byte-
/// identically to `encode_legacy_channel` (per the
/// `legacy_best_always_picks_bare_on_realistic_inputs` pin), so
/// `encode_legacy_rgb_best` and `encode_legacy_rgb` produce the
/// same wire bytes today. This helper is retained as an explicit
/// "always pick the best legacy sub-path" entry point.
pub fn encode_legacy_rgb_best(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
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

    if type7_residuals_need_strategy_e(&res_b, &res_g, &res_r) {
        return encode_uncompressed(pixels);
    }

    let ch_b = encode_legacy_channel_best(&res_b);
    let ch_g = encode_legacy_channel_best(&res_g);
    let ch_r = encode_legacy_channel_best(&res_r);

    pack_channels(7, &[&ch_b, &ch_g, &ch_r])
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

    // Per-channel selector — header-form picker across the four
    // legal legacy sub-paths (`0x00..=0x03`). On every realistic
    // residual histogram the cleanroom encoder can produce the
    // selector picks the bare-Fibonacci form (header `0x00`)
    // byte-identically to `encode_legacy_channel`, per the
    // `legacy_best_always_picks_bare_on_realistic_inputs` pin —
    // so flipping the production call site here is a structural
    // never-larger guarantee + a forward hook for any future
    // Fibonacci variant the spec adds that does emit zero bytes.
    let ch_b = encode_legacy_channel_best(&res_b);
    let ch_g = encode_legacy_channel_best(&res_g);
    let ch_r = encode_legacy_channel_best(&res_r);

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

    // **Rule B** first-column-of-row, matching the decoder.
    let res_b =
        apply_plane_forward_with_rule(&plane_b, width as usize, height as usize, FirstColRule::B);
    let res_g =
        apply_plane_forward_with_rule(&plane_g, width as usize, height as usize, FirstColRule::B);
    let res_r =
        apply_plane_forward_with_rule(&plane_r, width as usize, height as usize, FirstColRule::B);
    let res_a =
        apply_plane_forward_with_rule(&plane_a, width as usize, height as usize, FirstColRule::B);

    // Per-channel header-form selector — see `encode_channel_best`.
    let ch_b = encode_channel_best(&res_b);
    let ch_g = encode_channel_best(&res_g);
    let ch_r = encode_channel_best(&res_r);
    let ch_a = encode_channel_best(&res_a);

    pack_channels(8, &[&ch_b, &ch_g, &ch_r, &ch_a])
}

// ─── Round 222 — frame-level type-1 (uncompressed) size guard ──────
//
// The modern arithmetic frame encoders (`encode_arith_rgb24` /
// `encode_arith_yv12` / `encode_arith_yuy2` / `encode_arith_rgba`)
// always emit a header byte + channel-offset table (9 or 13 bytes for
// the 3- and 4-channel families respectively per `spec/01` §2.3) plus
// at least the Fibonacci-prefix-plus-arithmetic body of every plane.
// On inputs whose residuals do not compress — high-entropy / random
// patterns at small frame sizes — the per-channel `encode_channel_best`
// selector still routes to a positive-overhead form (the 257-entry
// Fibonacci freq table alone occupies several dozen bytes), so the
// emitted wire can exceed the raw pixel buffer.
//
// `spec/01` §2.1 defines type 1 ("uncompressed") as
// `{ byte 0 = 0x01, bytes 1..N = uncompressed pixel data }` — the
// host pixel buffer with a 1-byte prefix, in the source layout
// (RGB24 / RGB32 / RGBA / YUY2 / YV12). The decoder dispatches on
// byte 0, so a type-1 frame is a structurally legal substitute for
// any type-2/3/4/8/10 frame carrying the same pixels.
//
// The wrappers below add a never-larger guarantee at the frame level:
// each computes both forms and returns the smaller. This mirrors the
// existing per-channel header-form selector (`encode_channel_best`,
// `spec/03` §2.1 + `spec/06` §1.7) and the type-7 `encode_legacy_rgb_
// _best`'s Strategy E fallback (`audit/12` §7.1) one level up. Type 1
// is decoder-orthogonal — every conformant decoder routes byte 0 = 1
// to the memcpy helper at `lagarith.dll!0x18000555a` per `spec/01`
// §2.1 / table at §1 — so the size-based switch cannot regress
// wire compatibility. The 64-bit proprietary encoder does not produce
// type 1 in the wild (`spec/01` §3 row 1: "the encoder does **not**
// produce uncompressed type-1 output in this build"), making this a
// strict structural improvement over the proprietary's own emission
// path on inputs where arith overhead exceeds the raw payload.

/// Frame-level **size-based type-1 fallback** wrapping
/// [`encode_arith_rgb24`]. Returns the shorter of the arithmetic-coded
/// frame and the equivalent type-1 (uncompressed) frame
/// (`spec/01` §2.1). Tie-breaks in favour of the arithmetic form (keeps
/// the existing wire bytes byte-identical when both are equal).
pub fn encode_arith_rgb24_or_uncompressed(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let arith = encode_arith_rgb24(pixels, width, height);
    let raw = encode_uncompressed(pixels);
    if raw.len() < arith.len() {
        raw
    } else {
        arith
    }
}

/// Frame-level **size-based type-1 fallback** wrapping
/// [`encode_arith_yv12`]. See [`encode_arith_rgb24_or_uncompressed`].
pub fn encode_arith_yv12_or_uncompressed(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let arith = encode_arith_yv12(pixels, width, height);
    let raw = encode_uncompressed(pixels);
    if raw.len() < arith.len() {
        raw
    } else {
        arith
    }
}

/// Frame-level **size-based type-1 fallback** wrapping
/// [`encode_arith_yuy2`]. See [`encode_arith_rgb24_or_uncompressed`].
pub fn encode_arith_yuy2_or_uncompressed(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let arith = encode_arith_yuy2(pixels, width, height);
    let raw = encode_uncompressed(pixels);
    if raw.len() < arith.len() {
        raw
    } else {
        arith
    }
}

/// Frame-level **size-based type-1 fallback** wrapping
/// [`encode_arith_rgba`]. See [`encode_arith_rgb24_or_uncompressed`].
pub fn encode_arith_rgba_or_uncompressed(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let arith = encode_arith_rgba(pixels, width, height);
    let raw = encode_uncompressed(pixels);
    if raw.len() < arith.len() {
        raw
    } else {
        arith
    }
}

/// Frame-level **size-based type-1 fallback** wrapping
/// [`encode_legacy_rgb`]. Round 229 extension of the round-222
/// modern-arithmetic guard to the type-7 (legacy adaptive-CDF RGB)
/// path.
///
/// `encode_legacy_rgb` already routes rare-symbol-cluster residuals
/// to type 1 via Strategy E (`audit/12` §7.1) — that diversion is a
/// **wire-correctness** decision (the rare-symbol-cluster fixture
/// class is the one the flat-257-entry CDF and the pair-packed
/// 513-entry CDF disagree on, per `audit/12` §5..§6). The round-229
/// guard is the orthogonal **size-based** axis: even when the
/// residual histograms clear the rare-symbol-cluster signature, the
/// legacy bare-Fibonacci form still carries a 9-byte channel-offset
/// preamble plus the per-channel adaptive-CDF prefix and range-coder
/// body, which on tiny / high-entropy inputs exceeds the
/// `1 + W*H*3`-byte raw payload.
///
/// `spec/01` §2.1 defines type 1 as `{ byte 0 = 0x01, bytes 1..N =
/// uncompressed pixel data }`. Every conformant decoder dispatches
/// byte 0 → the memcpy helper (`spec/01` table at §1), so a type-1
/// substitute decodes byte-exactly against any decoder that accepts
/// type 7. Returns the shorter of the two forms, tie-breaking in
/// favour of the legacy form so already-compressing inputs stay
/// byte-identical to the existing `encode_legacy_rgb` output. The
/// Strategy E diversion already inside `encode_legacy_rgb` is
/// preserved by construction: when it fires `encode_legacy_rgb`
/// returns a type-1 frame (byte 0 = `0x01`), and the size guard
/// (`raw.len() == legacy.len()`) tie-breaks back to that already-
/// type-1 wire, byte-identical.
pub fn encode_legacy_rgb_or_uncompressed(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    let legacy = encode_legacy_rgb(pixels, width, height);
    let raw = encode_uncompressed(pixels);
    if raw.len() < legacy.len() {
        raw
    } else {
        legacy
    }
}

// ─── Round 276 — frame-level solid-colour fast path (`spec/01` §3.1) ─
//
// The proprietary encoder's RGB and RGBA paths carry a solid-colour
// shortcut: after the per-channel arithmetic encode, the compressed
// size is checked against a threshold (`0xf` at
// `lagarith.dll!0x180002c65` on the RGB path, `0x15` at
// `lagarith.dll!0x180002f7f` on the RGBA path); on a match the
// encoder discards the arithmetic output, overwrites the type byte
// with 5 / 6 (RGB — 5 when the input pixel's R == G == B, else 6,
// `spec/01` §3 rows 5/6) or 9 (RGBA, `spec/01` §3 row 9), and writes
// 1 / 3 / 4 colour bytes copied from the input pixel unchanged
// (encoder mirror at `lagarith.dll!0x180002c8c..0x180002cda` /
// `0x180002f8c..0x180002fc8`, `spec/01` §2.2.1). The committed
// frame sizes are the §2.2.2 totals: 2 / 4 / 5 bytes.
//
// A solid-frame substitution is lossless **iff every pixel of the
// frame is identical** — `decode_solid` replicates the wire colour
// bytes into every output pixel (`spec/01` §2.2). The proprietary
// gates on its post-encode size threshold as a proxy for that
// condition; the wrappers below gate on the exact-constancy
// predicate itself, which is the necessary-and-sufficient lossless
// condition the threshold stands in for. On every genuinely solid
// frame both gates agree and the emitted wire bytes are identical
// (type byte + input-pixel colour bytes); on non-solid frames the
// wrappers fall through to the arithmetic frame encoder
// byte-identically.

/// Scan a packed 3-byte-per-pixel buffer for frame-wide constancy.
/// Returns the common `(B, G, R)` tuple when every pixel equals
/// pixel 0, `None` otherwise (including on an empty buffer — there
/// is no input pixel to copy colour bytes from per `spec/01`
/// §2.2.1's encoder mirror).
fn solid_colour_bgr(pixels: &[u8]) -> Option<(u8, u8, u8)> {
    let mut it = pixels.chunks_exact(3);
    let first = it.next()?;
    let (b, g, r) = (first[0], first[1], first[2]);
    it.all(|px| px[0] == b && px[1] == g && px[2] == r)
        .then_some((b, g, r))
}

/// Scan a packed 4-byte-per-pixel buffer for frame-wide constancy.
/// Returns the common `(B, G, R, A)` tuple when every pixel equals
/// pixel 0, `None` otherwise (including on an empty buffer).
fn solid_colour_bgra(pixels: &[u8]) -> Option<(u8, u8, u8, u8)> {
    let mut it = pixels.chunks_exact(4);
    let first = it.next()?;
    let (b, g, r, a) = (first[0], first[1], first[2], first[3]);
    it.all(|px| px[0] == b && px[1] == g && px[2] == r && px[3] == a)
        .then_some((b, g, r, a))
}

/// Frame-level **solid-colour fast path** wrapping
/// [`encode_arith_rgb24`] — the `spec/01` §3.1 RGB-path shortcut.
///
/// When every input pixel is identical, emits the 2-byte type-5
/// (Solid-Grey) frame if the pixel's `B == G == R` (`spec/01` §3
/// row 5 "the input pixel's R == G == B") or the 4-byte type-6
/// (Solid-RGB) frame otherwise (`spec/01` §3 row 6), with the
/// colour bytes copied from the input pixel unchanged (`spec/01`
/// §2.2.1 encoder mirror; §2.2.2 total sizes). Non-solid input
/// falls through to [`encode_arith_rgb24`] byte-identically. The
/// fast path sits on the shared RGB path **before** the type-2/4
/// width split commits, so it applies to both `width % 4 == 0` and
/// unaligned widths (`spec/01` §3 rows 2/4 vs rows 5/6 — the solid
/// overwrite replaces whichever type byte the width split staged).
pub fn encode_arith_rgb24_or_solid(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    if let Some((b, g, r)) = solid_colour_bgr(pixels) {
        return if b == g && g == r {
            encode_solid_grey(b)
        } else {
            encode_solid_rgb(b, g, r)
        };
    }
    encode_arith_rgb24(pixels, width, height)
}

/// Frame-level **solid-colour fast path** wrapping
/// [`encode_arith_rgba`] — the `spec/01` §3.1 RGBA-path shortcut.
///
/// When every input pixel is identical, emits the 5-byte type-9
/// (Solid-RGBA) frame (`spec/01` §3 row 9; threshold `0x15` at
/// `lagarith.dll!0x180002f7f`), with the four colour bytes copied
/// from the input pixel unchanged (`spec/01` §2.2.1 — wire byte 4
/// → output `+3` = A). The RGBA path has **no** greyscale
/// sub-shortcut: a constant grey + opaque BGRA frame still emits
/// type 9, never type 5/6 (`spec/01` §3 lists the 5/6 overwrite
/// sites on the RGB path only). Non-solid input falls through to
/// [`encode_arith_rgba`] byte-identically.
pub fn encode_arith_rgba_or_solid(pixels: &[u8], width: u32, height: u32) -> Vec<u8> {
    if let Some((b, g, r, a)) = solid_colour_bgra(pixels) {
        return encode_solid_rgba(b, g, r, a);
    }
    encode_arith_rgba(pixels, width, height)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::decode_channel;

    /// Header-0x00 encode path with an explicit total cap, used by the
    /// rescale tests to drive [`rescale_to_max_total`] through the full
    /// modern-coder wire at small scale (a production-cap test would
    /// need a > TOP-pixel plane = tens of MB). Always emits the
    /// header-0x00 form (no raw-memcpy fallback) so the arithmetic +
    /// rescaled-prefix path is exercised unconditionally.
    fn encode_channel_simple_capped(plane: &[u8], cap: u32) -> Vec<u8> {
        let mut freq = [0u32; 256];
        for &b in plane {
            freq[b as usize] = freq[b as usize].saturating_add(1);
        }
        let freq = rescale_to_max_total(&freq, cap);
        let cdf = Cdf::from_frequencies(&freq).unwrap();
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
        out
    }

    /// `rescale_to_max_total` leaves a histogram whose total already
    /// fits under the cap **byte-identical** — the common small/medium
    /// plane keeps the raw-histogram wire (`spec/04` §5 validation
    /// correction).
    #[test]
    fn rescale_noop_when_total_fits() {
        let mut freq = [0u32; 256];
        freq[0] = 4000;
        freq[7] = 50;
        freq[255] = 46; // total 4096 << TOP
        let out = rescale_to_max_total(&freq, MAX_MODERN_TOTAL);
        assert_eq!(out, freq, "fits-under-cap must be a verbatim passthrough");
    }

    /// Above the cap, the rescaled total never exceeds the cap, and
    /// every symbol the plane actually used keeps a transmitted
    /// frequency `>= 1` (no used symbol drops out of the table).
    #[test]
    fn rescale_caps_total_and_preserves_nonzero() {
        let mut freq = [0u32; 256];
        // Dominant symbol plus a long tail of rare-but-present symbols.
        freq[0] = 10_000_000; // > TOP on its own
        for s in 1..=200u32 {
            freq[s as usize] = 1; // rarest possible — must survive
        }
        let cap = MAX_MODERN_TOTAL;
        let out = rescale_to_max_total(&freq, cap);
        let total: u64 = out.iter().map(|&f| f as u64).sum();
        assert!(total <= cap as u64, "total {total} exceeds cap {cap}");
        for s in 0..256 {
            assert_eq!(
                out[s] > 0,
                freq[s] > 0,
                "nonzero-preservation broken at symbol {s}",
            );
        }
    }

    /// A small cap that forces many `max(1, …)` clamps still yields a
    /// total within the cap (overshoot-trim path).
    #[test]
    fn rescale_small_cap_overshoot_trim() {
        let mut freq = [0u32; 256];
        for s in 0..256u32 {
            freq[s as usize] = 1000; // 256 equal slots, total 256_000
        }
        let cap = 300u32; // forces clamps + trim
        let out = rescale_to_max_total(&freq, cap);
        let total: u64 = out.iter().map(|&f| f as u64).sum();
        assert!(total <= cap as u64, "total {total} exceeds cap {cap}");
        assert!(out.iter().all(|&f| f >= 1), "every used slot stays >= 1");
    }

    /// End-to-end self-roundtrip through the modern wire with a tiny
    /// cap: the rescaled histogram both drives the CDF and is the
    /// transmitted prefix, so `decode_channel` rebuilds the identical
    /// CDF and recovers the original residual bytes byte-for-byte
    /// (`spec/04` §6). This is the small-scale stand-in for a
    /// > TOP-pixel plane — the same code path, the same invariant.
    #[test]
    fn rescale_capped_channel_roundtrip() {
        // A plane whose raw total (700) far exceeds the tiny cap (64),
        // with a dominant symbol and several rare ones.
        let mut plane = vec![0u8; 600];
        plane.extend(std::iter::repeat_n(17u8, 60));
        plane.extend(std::iter::repeat_n(200u8, 30));
        plane.extend([3u8, 9, 9, 250, 1, 1, 1, 1, 128, 128]);
        let n = plane.len();
        for cap in [16u32, 32, 64, 128, 300] {
            let channel = encode_channel_simple_capped(&plane, cap);
            let decoded = decode_channel(&channel, n).unwrap();
            assert_eq!(decoded, plane, "roundtrip mismatch at cap {cap}");
        }
    }

    /// A genuine `total > TOP` plane round-trips at the production cap.
    /// Sized just over TOP so the raw histogram would drive
    /// `q = range / total` to zero without the rescale; with the
    /// rescale it decodes byte-exactly. (~8.4 MB plane — the one heavy
    /// test that proves the production-cap path.)
    #[test]
    fn rescale_production_cap_large_plane_roundtrip() {
        let n = TOP as usize + 1024; // just past the q>=1 cliff
        let mut plane = vec![0u8; n];
        // Sprinkle a handful of non-zero symbols so the histogram has
        // a real tail (and the encode isn't a degenerate solid fill).
        for (i, b) in plane.iter_mut().enumerate() {
            if i % 4096 == 0 {
                *b = ((i / 4096) % 200 + 1) as u8;
            }
        }
        let channel = encode_channel_simple(&plane);
        // Must be the arithmetic header-0x00 form (rescale engaged),
        // not a raw-memcpy fallback.
        assert_eq!(channel[0], 0x00, "expected header-0x00 arith channel");
        let decoded = decode_channel(&channel, n).unwrap();
        assert_eq!(decoded, plane, "large-plane roundtrip mismatch");
    }

    // ─────────── encode_channel_best — header-form selection ───────────

    /// `encode_channel_best` never produces a larger wire body than
    /// `encode_channel_simple` (which only ever picks between 0x00
    /// and 0x04). The new selector strictly extends the candidate
    /// set, so the worst case is a tie — never a regression.
    #[test]
    fn best_never_larger_than_simple() {
        for plane in best_test_planes() {
            let simple = encode_channel_simple(&plane);
            let best = encode_channel_best(&plane);
            assert!(
                best.len() <= simple.len(),
                "best ({}) larger than simple ({}) on plane len {} (best header {:#x}, simple header {:#x})",
                best.len(),
                simple.len(),
                plane.len(),
                best[0],
                simple[0],
            );
        }
    }

    /// Every candidate header form `encode_channel_best` can pick
    /// round-trips through `decode_channel` back to the input plane,
    /// regardless of which form the selector ended up choosing.
    #[test]
    fn best_roundtrips_through_decoder() {
        for plane in best_test_planes() {
            let channel = encode_channel_best(&plane);
            let decoded = decode_channel(&channel, plane.len()).unwrap();
            assert_eq!(
                decoded,
                plane,
                "roundtrip mismatch (best header {:#x}, plane len {})",
                channel[0],
                plane.len(),
            );
        }
    }

    /// On a zero-run-heavy residual (post-gradient Lagarith
    /// residuals are dominated by zeros per `spec/06` §6.4), the
    /// selector picks one of the RLE-bearing sub-paths (`0x01..=0x03`
    /// for arith+RLE or `0x05..=0x07` for raw+RLE) — never raw 0x04,
    /// and ideally not header 0x00 (whose lack of RLE is exactly what
    /// the encoder heuristic at `lagarith.dll!0x18000c500` switches
    /// away from on these inputs per `spec/06` §2.8).
    #[test]
    fn best_picks_rle_form_on_zero_heavy_plane() {
        // 95% zeros, sparse non-zero symbols — the canonical post-
        // gradient residual profile.
        let mut plane = vec![0u8; 950];
        for i in 0..50 {
            plane.push(((i * 37 + 11) & 0xff) as u8);
        }
        // Interleave so the zeros form runs of varying lengths
        // (the RLE escape needs runs >= escape_len to fire).
        plane.rotate_left(7);
        let best = encode_channel_best(&plane);
        let header = best[0];
        assert!(
            (0x01..=0x03).contains(&header) || (0x05..=0x07).contains(&header),
            "expected RLE-bearing header, got {header:#x} (len {})",
            best.len(),
        );
        // And it must still decode back to the input.
        let decoded = decode_channel(&best, plane.len()).unwrap();
        assert_eq!(decoded, plane);
    }

    /// `encode_channel_raw_rle` (the new `0x05..=0x07` primitive)
    /// roundtrips through `decode_channel` at every escape length.
    #[test]
    fn raw_rle_channel_roundtrips() {
        // A plane with a few long zero runs interleaved with
        // non-zero stretches — exercises both the RLE escape and
        // the literal byte path inside the contractor.
        let mut plane = vec![1u8, 2, 3];
        plane.extend(std::iter::repeat_n(0u8, 50));
        plane.extend([7, 8, 9]);
        plane.extend(std::iter::repeat_n(0u8, 10));
        plane.extend([11, 0, 0, 0, 12, 13]);
        for escape_len in 1..=3usize {
            let channel = encode_channel_raw_rle(&plane, escape_len);
            assert_eq!(channel[0], escape_len as u8 + 4);
            let decoded = decode_channel(&channel, plane.len()).unwrap();
            assert_eq!(
                decoded, plane,
                "raw-rle roundtrip mismatch at e={escape_len}"
            );
        }
    }

    /// On a plane whose histogram is *flat* (every byte equally
    /// likely → the arithmetic coder cannot compress) but still
    /// has occasional zero runs, the raw-RLE form (`0x05..=0x07`)
    /// is the right choice — header `0x00` would pay the
    /// Fibonacci-prefix overhead for an arithmetic body that
    /// matches the raw byte count. This test pins that intuition
    /// by constructing a flat-256 plane with a single moderate zero
    /// run and asserting the selector beats raw `0x04`.
    #[test]
    fn best_beats_raw_on_flat_with_zero_runs() {
        // 256 distinct symbols once + a 20-byte zero run = a flat
        // histogram with one RLE-able stretch.
        let mut plane: Vec<u8> = (0u8..=255).collect();
        plane.extend(std::iter::repeat_n(0u8, 20));
        let raw_len = 1 + plane.len(); // header 0x04 + payload
        let best = encode_channel_best(&plane);
        assert!(
            best.len() <= raw_len,
            "best ({}) didn't improve on raw ({}) — header was {:#x}",
            best.len(),
            raw_len,
            best[0],
        );
        let decoded = decode_channel(&best, plane.len()).unwrap();
        assert_eq!(decoded, plane);
    }

    /// On a representative post-gradient Lagarith residual profile
    /// (95% zeros + sparse Laplacian tail), the selector must shrink
    /// the channel below header-`0x00`'s output by a non-trivial
    /// margin — the entire point of the new selector is to claim the
    /// `spec/06` §6.4 byte-budget gap that header-`0x00`'s
    /// no-RLE form cedes on this input class. The pin is loose
    /// (`>= 10%`) so noise in the Fibonacci-prefix encoding can't
    /// flap it, but tight enough that a regression to the old
    /// `encode_channel_simple`-equivalent path would fail it.
    #[test]
    fn best_size_delta_on_residual_profile() {
        // 95% zeros + 5% Laplacian-distributed non-zero bytes — the
        // canonical post-gradient Lagarith residual histogram per
        // `spec/06` §6.4.
        let mut plane = vec![0u8; 1900];
        for i in 0..100u32 {
            // A geometric-ish tail: more small values, fewer large.
            let v = ((i * 7) % 16 + 1) as u8;
            plane.push(v);
        }
        plane.rotate_left(13); // spread the non-zeros through the run
        let simple = encode_channel_simple(&plane);
        let best = encode_channel_best(&plane);
        let delta = simple.len() as i64 - best.len() as i64;
        let pct = 100.0 * delta as f64 / simple.len() as f64;
        // On the 1900-zero / 100-non-zero fixture above, the
        // measured saving is 53 bytes (143 → 90, **37% smaller**),
        // with header 0x01 (arith + RLE at escape_len=1) winning.
        // The `>= 10%` pin is loose so a future Fibonacci-prefix
        // rework can't false-trip this — the headline number lives
        // in the README, the test just guarantees the selector
        // beats the no-RLE form by a meaningful margin on the
        // residual profile the codec is designed for.
        assert!(
            pct >= 10.0,
            "best={} simple={} delta={} ({pct:.1}% gain) — selector failed to shrink residual",
            best.len(),
            simple.len(),
            delta,
        );
    }

    // ─────────── encode_legacy_channel_best — type-7 sub-path selection ───────────

    /// `encode_legacy_channel_best` never produces a larger wire body
    /// than `encode_legacy_channel` (the bare-Fibonacci baseline).
    /// The selector strictly extends the candidate set with three
    /// RLE-then-Fibonacci alternatives; the worst case is a tie —
    /// never a regression.
    #[test]
    fn legacy_best_never_larger_than_bare_fib() {
        for plane in legacy_best_test_planes() {
            let bare = encode_legacy_channel(&plane);
            let best = encode_legacy_channel_best(&plane);
            assert!(
                best.len() <= bare.len(),
                "legacy best ({}) larger than bare ({}) on plane len {} \
                 (best header {:#x}, bare header {:#x})",
                best.len(),
                bare.len(),
                plane.len(),
                best[0],
                bare[0],
            );
        }
    }

    /// Every candidate header form `encode_legacy_channel_best` can
    /// pick round-trips through `decode_legacy_channel` back to the
    /// input plane, regardless of which sub-path the selector chose.
    #[test]
    fn legacy_best_roundtrips_through_decoder() {
        use crate::channel::decode_legacy_channel;
        for plane in legacy_best_test_planes() {
            let channel = encode_legacy_channel_best(&plane);
            let decoded = decode_legacy_channel(&channel, plane.len()).unwrap();
            assert_eq!(
                decoded,
                plane,
                "legacy roundtrip mismatch (best header {:#x}, plane len {})",
                channel[0],
                plane.len(),
            );
        }
    }

    /// The selector accepts only the four legal legacy headers
    /// (`0x00..=0x03`); anything else would be rejected by
    /// `decode_legacy_channel` as `Error::BadChannelHeader`.
    #[test]
    fn legacy_best_only_emits_legal_headers() {
        for plane in legacy_best_test_planes() {
            let channel = encode_legacy_channel_best(&plane);
            let header = channel[0];
            assert!(
                (0x00..=0x03).contains(&header),
                "legacy selector emitted illegal header {header:#x}",
            );
        }
    }

    /// Tie-breaker invariant: when the bare-Fibonacci form and the
    /// shortest RLE-then-Fibonacci candidate produce equal-length
    /// wire bodies (the realistic case — see
    /// `legacy_best_always_picks_bare_on_realistic_inputs`), the
    /// selector must return the bare-Fibonacci form unchanged. This
    /// preserves the historical `encode_legacy_channel` byte stream
    /// on every fixture and keeps the existing legacy roundtrip
    /// tests in `roundtrip_tests.rs` byte-identical against the
    /// new selector.
    #[test]
    fn legacy_best_tie_breaker_prefers_bare() {
        // Use the same sparse fixture as
        // `legacy_best_always_picks_bare_on_realistic_inputs`'s
        // `sparse_7` case — empirically bare ties with all three
        // RLE candidates (each +4 bytes from the u32 length field).
        let mut plane = Vec::with_capacity(800);
        for i in 0..800 {
            plane.push(if i % 7 == 0 {
                ((i / 7) % 5 + 1) as u8
            } else {
                0
            });
        }
        let bare = encode_legacy_channel(&plane);
        let best = encode_legacy_channel_best(&plane);
        // Selector must pick bare on a tie (or strict win).
        assert!(best.len() <= bare.len());
        assert_eq!(best[0], 0x00, "tie-breaker must keep bare-Fib form");
        // And the bytes must be byte-identical to the bare form, so
        // the existing roundtrip suite's byte-by-byte assumptions
        // about `encode_legacy_channel` output (if any) still hold.
        assert_eq!(best, bare, "bare-tie must be byte-identical to bare");
    }

    /// `encode_legacy_rgb_best` (frame-level wrapper) round-trips
    /// through `decode_frame` for the same fixture sizes the
    /// existing `encode_legacy_rgb` roundtrip tests cover. This
    /// confirms the per-channel selector composes correctly with
    /// the type-7 frame-layout dispatcher (`spec/01` + `spec/07`
    /// §1.2 channel-offset header).
    #[test]
    fn legacy_rgb_best_frame_roundtrips() {
        use crate::decoder::{decode_frame, PixelKind};
        // 4×4, 8×8, 16×12 — the existing legacy_rgb_roundtrip_*
        // sizes from `roundtrip_tests.rs`. Stick to RGB-pattern
        // inputs that won't trigger Strategy E.
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 12)] {
            let n = (w * h) as usize;
            // BGR24 pattern: gradient + offset so residuals aren't
            // rare-symbol-cluster.
            let mut pixels = Vec::with_capacity(n * 3);
            for i in 0..n {
                pixels.push(((i * 5 + 11) & 0xff) as u8);
                pixels.push(((i * 7 + 3) & 0xff) as u8);
                pixels.push(((i * 11 + 19) & 0xff) as u8);
            }
            let frame = encode_legacy_rgb_best(&pixels, w, h);
            // Type byte 7 — confirms the frame-layout wrapper wasn't
            // diverted to type 1 (`encode_uncompressed`) by Strategy E.
            assert_eq!(
                frame[0], 7,
                "legacy_rgb_best Strategy E unexpectedly fired at {w}x{h}"
            );
            let decoded = decode_frame(&frame, w, h, PixelKind::Bgr24).unwrap();
            assert_eq!(
                decoded.pixels, pixels,
                "legacy_rgb_best roundtrip mismatch at {w}x{h}",
            );
        }
    }

    /// Empirical finding: on every realistic residual histogram
    /// probed, the Fibonacci-coded freq table produces **zero
    /// `0x00` bytes** (the variable-length bit packing is too dense
    /// to leave 8 zero bits aligned at any byte boundary), so the
    /// RLE-then-Fibonacci sub-paths (`0x01..=0x03`) cannot reduce
    /// the channel-prefix size — they only add the 4-byte u32
    /// length field of `spec/07` §2.3, ending up 4 bytes *larger*
    /// than bare-Fibonacci on every fixture. The selector therefore
    /// always picks header `0x00` on realistic input. This test
    /// pins that invariant so a future Fibonacci-encoding tweak
    /// that does emit zero bytes (e.g. a padding form) would
    /// surface as a deliberate failure here rather than silently
    /// changing wire bytes.
    ///
    /// This is the encoder-direction empirical correction to
    /// `spec/07` §6.3's framing of the two sub-paths as a
    /// "compression trade-off": with the proprietary's bit-packed
    /// Fibonacci layout, the trade-off has only one viable choice
    /// for the encoder; the RLE-then-Fibonacci sub-path exists in
    /// the decoder dispatcher for completeness (and to support
    /// any future Fibonacci variant the spec adds) but is dead
    /// weight on every histogram the cleanroom encoder can
    /// reasonably produce.
    #[test]
    fn legacy_best_always_picks_bare_on_realistic_inputs() {
        // Spread of realistic profiles: dense, sparse, two-symbol,
        // biased-mid-band. None should leave the bare-Fibonacci form
        // by more than zero bytes (i.e. selector's choice == bare).
        let cases: &[(&str, Vec<u8>)] = &[
            ("dense_256", (0u8..=255).collect()),
            ("sparse_7", {
                let mut p = Vec::new();
                for i in 0..800 {
                    p.push(if i % 7 == 0 {
                        ((i / 7) % 5 + 1) as u8
                    } else {
                        0
                    });
                }
                p
            }),
            ("two_symbol", {
                let mut p = vec![0u8; 1000];
                for i in (0..1000).step_by(13) {
                    p[i] = 1;
                }
                p
            }),
            ("biased_50", {
                let mut p = Vec::new();
                for i in 0..2000 {
                    p.push(if i % 3 == 0 { ((i % 50) + 5) as u8 } else { 0 });
                }
                p
            }),
        ];
        for (label, plane) in cases {
            let bare = encode_legacy_channel(plane);
            let best = encode_legacy_channel_best(plane);
            assert_eq!(
                best.len(),
                bare.len(),
                "{label}: RLE-then-Fib unexpectedly beat bare-Fib by {} bytes \
                 (bare={}, best={}, header={:#x}) — likely the Fibonacci \
                 encoder began emitting 0x00 bytes the RLE escape can swallow; \
                 if intentional, update this pin and the README empirical note.",
                bare.len() as i64 - best.len() as i64,
                bare.len(),
                best.len(),
                best[0],
            );
            assert_eq!(best[0], 0x00, "{label}: selector picked non-bare form");
        }
    }

    /// `encode_legacy_rgb_best` is never larger than `encode_legacy_rgb`
    /// on the fixtures both can handle (Strategy E branches identically
    /// on the same input — both fall back to type 1 on rare-cluster
    /// residuals, both run the entropy stage otherwise). The shorter
    /// frame on either input proves the per-channel selector
    /// propagates through the frame-layout wrapper.
    #[test]
    fn legacy_rgb_best_frame_never_larger() {
        for &(w, h) in &[(4u32, 4u32), (8, 8), (16, 12)] {
            let n = (w * h) as usize;
            let mut pixels = Vec::with_capacity(n * 3);
            for i in 0..n {
                pixels.push(((i * 5 + 11) & 0xff) as u8);
                pixels.push(((i * 7 + 3) & 0xff) as u8);
                pixels.push(((i * 11 + 19) & 0xff) as u8);
            }
            let bare = encode_legacy_rgb(&pixels, w, h);
            let best = encode_legacy_rgb_best(&pixels, w, h);
            assert!(
                best.len() <= bare.len(),
                "legacy_rgb_best frame ({}) larger than legacy_rgb ({}) at {w}x{h}",
                best.len(),
                bare.len(),
            );
        }
    }

    /// Strategy E (audit/12 §7.1) propagates through the new
    /// `encode_legacy_rgb_best` exactly as it does through the
    /// existing `encode_legacy_rgb` / `encode_legacy_rgb_rle` paths
    /// — the rare-symbol-cluster signature lives in the *residual
    /// histogram* fed to the entropy stage, which is invariant
    /// across the channel-prefix form choices the selector picks
    /// between. So a near-flat input that diverts to type 1 in
    /// `encode_legacy_rgb` must also divert to type 1 here.
    #[test]
    fn legacy_rgb_best_strategy_e_propagates() {
        // The 33×27 near-flat fixture from
        // `legacy_rgb_strategy_e_routes_near_flat_to_type_1` in
        // `roundtrip_tests.rs` (audit/12 §3 canonical size). A solid
        // colour plane with a single centre-pixel perturbation
        // produces `freq[0] >= 0.95 * pixel_count` after predict +
        // decorrelate plus a small rare-symbol tail, tripping the
        // signature on at least one plane.
        let (w, h) = (33u32, 27);
        let n = (w * h) as usize;
        let (bg_b, bg_g, bg_r) = (0xa0u8, 0xd7u8, 0x40u8);
        let mut pixels = Vec::with_capacity(n * 3);
        for _ in 0..n {
            pixels.push(bg_b);
            pixels.push(bg_g);
            pixels.push(bg_r);
        }
        // Flip the green byte of the centre pixel by +0x40 — same
        // bit-twist `near_flat_bgr24` applies in the existing tests.
        let centre = (n / 2) * 3 + 1;
        pixels[centre] = pixels[centre].wrapping_add(0x40);
        let frame = encode_legacy_rgb_best(&pixels, w, h);
        // Type 1 = uncompressed; Strategy E successfully diverted.
        assert_eq!(
            frame[0], 1,
            "Strategy E must propagate through encode_legacy_rgb_best (got type {})",
            frame[0],
        );
    }

    /// Fixture set for the legacy selector tests — a mix of profiles
    /// that exercise both the bare-Fibonacci and the
    /// RLE-then-Fibonacci sub-paths. None of these residual
    /// histograms trip the rare-symbol-cluster signature (so they
    /// stay on the legacy entropy path rather than diverting to
    /// type 1 in the frame-level wrapper).
    fn legacy_best_test_planes() -> Vec<Vec<u8>> {
        let mut planes = Vec::new();

        // (a) Small deterministic plane — drives the dispatcher's
        // 2-byte channel prefix at minimum size.
        planes.push(vec![10u8, 20, 30, 40, 50, 60, 70, 80]);

        // (b) Medium plane with a flat-ish histogram — bare
        // Fibonacci's freq table is *dense* (every entry nonzero),
        // so RLE on the freq-table buffer has nothing to compress
        // and bare-Fib should win.
        let dense: Vec<u8> = (0u8..=255).collect();
        planes.push(dense);

        // (c) Sparse-histogram plane — only a handful of distinct
        // symbols, so most of the 256-entry transmitted freq table
        // is zero codewords. RLE on the freq-table buffer should
        // dominate.
        let mut sparse = Vec::with_capacity(400);
        for i in 0..400 {
            sparse.push(((i % 11) * 17) as u8);
        }
        planes.push(sparse);

        // (d) Plane that produces a moderately concentrated
        // histogram — somewhere between (b) and (c). Tests the
        // "neither obviously wins" regime.
        let mut mid = Vec::with_capacity(300);
        for i in 0..300u32 {
            mid.push((i.wrapping_mul(37).wrapping_add(11) & 0x3f) as u8);
        }
        planes.push(mid);

        // (e) Small ramp — minimum non-degenerate size.
        planes.push((0u8..16).collect());

        planes
    }

    /// Shared fixture set for the selector tests — a mix of profiles
    /// that exercise each candidate header form.
    fn best_test_planes() -> Vec<Vec<u8>> {
        let mut planes = Vec::new();

        // (a) Empty + solid — short-circuit cases (header 0xff).
        planes.push(Vec::new());
        planes.push(vec![42u8; 64]);

        // (b) Zero-run-heavy (RLE-friendly).
        let mut zero_heavy = vec![0u8; 200];
        for (i, b) in zero_heavy.iter_mut().enumerate() {
            if i % 17 == 0 {
                *b = ((i * 13 + 3) & 0xff) as u8;
            }
        }
        planes.push(zero_heavy);

        // (c) Highly skewed histogram — arithmetic-friendly.
        let mut skewed = vec![0u8; 256];
        skewed[10] = 200;
        skewed[200] = 17;
        planes.push(skewed);

        // (d) Near-uniform histogram — raw-friendly.
        planes.push((0u8..=255).collect());

        // (e) Long zeros mixed with a Laplacian-like tail.
        let mut tail = vec![0u8; 600];
        for i in 0..100 {
            tail.push(((i as u32 * 7 % 11) + 1) as u8);
        }
        for i in 0..40 {
            tail.push(((i as u32 * 211) ^ 0x55) as u8);
        }
        planes.push(tail);

        // (f) Small plane (degenerate sizes around the dispatcher's
        // 5-byte header overhead).
        planes.push(vec![1u8, 0, 0, 2, 3]);
        planes.push(vec![5u8; 4]);
        planes.push(vec![1u8, 2, 3, 4, 5, 6, 7, 8]);

        planes
    }
}
