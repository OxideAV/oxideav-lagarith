//! Residual-RLE escape expand / contract per `spec/05`.
//!
//! All three escape lengths (1, 2, 3) follow the same wire pattern:
//! `escape_len` consecutive zero bytes followed by **one supplement
//! byte** that drives a 256-entry permutation LUT. The expansion
//! produces `escape_len + LUT[supplement]` zero residuals.
//!
//! Used in two transports:
//!
//! - **Raw bytes** (channel-header `0x05..0x07`, escape_len = h - 4):
//!   `expand_raw` post-processes a borrowed byte slice directly.
//! - **Modern range coder, post-process form** (channel-header
//!   `0x01..0x03`, escape_len = h): the dispatcher decodes the full
//!   range-coder symbol stream first (driven by the channel's u32
//!   pre-RLE symbol-stream length field per `spec/06` §1.4), then
//!   feeds those bytes into `expand_raw`. `spec/05` §1.3 / `spec/06`
//!   §2.2 note that the proprietary fuses RLE expansion into the
//!   range-coder loop as an optimisation; the post-process form is
//!   bit-equivalent to a clean-room implementation.

use crate::error::{Error, Result};
use crate::tables::rle_fwd_lut;

/// Expand an escaped byte sequence into a plane buffer of `n_pixels`
/// residuals. Returns the number of bytes consumed from `src`.
///
/// `escape_len` must be in `1..=3`.
pub fn expand_raw(src: &[u8], escape_len: usize, n_pixels: usize) -> Result<(Vec<u8>, usize)> {
    debug_assert!((1..=3).contains(&escape_len));
    let lut = rle_fwd_lut();
    let mut out = vec![0u8; n_pixels];
    let mut j: usize = 0; // output cursor
    let mut i: usize = 0; // input cursor

    while j < n_pixels {
        // Look at the next up-to-`escape_len` bytes for a leading
        // zero run.
        let remaining_input = src.len().saturating_sub(i);
        // If we don't have enough input bytes for a full escape
        // probe, the channel must have a shorter literal tail. Fall
        // back to literal byte handling below.
        let probe_len = escape_len.min(remaining_input);
        let mut zero_run = 0usize;
        for k in 0..probe_len {
            if src[i + k] == 0 {
                zero_run += 1;
            } else {
                break;
            }
        }
        if zero_run == escape_len {
            // Escape fires: consume escape_len zeros + one
            // supplement byte and emit (escape_len + LUT[s])
            // residuals as zero.
            if remaining_input < escape_len + 1 {
                return Err(Error::Truncated {
                    context: "RLE escape supplement byte",
                });
            }
            let s = src[i + escape_len] as usize;
            i += escape_len + 1;
            let total_zeros = escape_len + lut[s] as usize;
            // Output buffer is pre-zeroed; just advance the cursor
            // (clamp at n_pixels per `spec/05` §4.2).
            let advance = total_zeros.min(n_pixels - j);
            j += advance;
        } else {
            // Either zero_run < escape_len followed by non-zero (or
            // by end-of-input). Emit `zero_run` zeros literally,
            // then if input remains emit the next byte literally.
            for _ in 0..zero_run {
                if j >= n_pixels {
                    break;
                }
                out[j] = 0;
                j += 1;
            }
            i += zero_run;
            if j < n_pixels {
                if i >= src.len() {
                    return Err(Error::Truncated {
                        context: "RLE input ran out before output filled",
                    });
                }
                out[j] = src[i];
                j += 1;
                i += 1;
            }
        }
    }
    Ok((out, i))
}

/// Supplement byte for a desired escape padding `n` (the run-length
/// extension beyond `escape_len`), computed via the algebraic
/// inverse of the forward permutation LUT (`spec/05` §3.2 / §5.3):
///
/// ```text
/// n even -> n / 2                (in [0, 127])
/// n odd  -> 255 - (n - 1) / 2    (in [128, 255])
/// ```
///
/// Domain is the **full** `0..=255` — two values wider than the
/// staged encoder INV_LUT's index form (`spec/05` §5.2 pads
/// `INV_LUT[0..=2] = 0`, so `INV_LUT[chunk - e + 2]` tops out at
/// padding 253). `spec/05` §5.3 ratifies the algebraic inverse as an
/// equivalent encoder-side choice yielding identical results, and
/// §5.4's canonical per-run emit uses the full `min(R - e, 255)`
/// padding. A unit test below pins agreement with the staged
/// `tables/02` INV_LUT on the overlapping domain and full-range
/// forward-LUT inversion.
#[inline]
fn supplement_for_padding(n: usize) -> u8 {
    debug_assert!(n <= 255, "escape padding {n} out of range");
    if n % 2 == 0 {
        (n / 2) as u8
    } else {
        (255 - (n - 1) / 2) as u8
    }
}

/// Encode a plane of residuals into the escape-byte form for a given
/// escape length. Encode-direction primitive (consumed by
/// [`crate::encoder`]); the algebraic inverse of [`expand_raw`].
pub(crate) fn contract_raw(plane: &[u8], escape_len: usize) -> Vec<u8> {
    debug_assert!((1..=3).contains(&escape_len));
    let mut out = Vec::with_capacity(plane.len());
    let mut i = 0usize;
    while i < plane.len() {
        if plane[i] != 0 {
            out.push(plane[i]);
            i += 1;
            continue;
        }
        // Count zeros at the current position.
        let mut run = 0usize;
        while i + run < plane.len() && plane[i + run] == 0 {
            run += 1;
        }
        // If the run is shorter than escape_len, emit literally.
        if run < escape_len {
            out.resize(out.len() + run, 0);
            i += run;
            continue;
        }
        // Emit one or more escape sequences, each covering
        // `escape_len + padding` zeros with `padding <= 255` — the
        // canonical greedy split of `spec/05` §5.4 (per escape:
        // `min(R - e, 255)` supplement, repeating as necessary).
        // The supplement byte comes from the §5.3 algebraic inverse,
        // which reaches the full padding range `0..=255` (the staged
        // INV_LUT's index arithmetic stops at 253; §5.3 pins both
        // forms as equivalent where they overlap).
        const MAX_CHUNK_PADDING: usize = 255;
        let mut left = run;
        while left >= escape_len {
            let chunk = left.min(escape_len + MAX_CHUNK_PADDING);
            out.resize(out.len() + escape_len, 0);
            out.push(supplement_for_padding(chunk - escape_len));
            left -= chunk;
        }
        // The remainder (< escape_len) is emitted literally. Safe:
        // fewer than `escape_len` consecutive zeros can never fire
        // the decoder's escape probe (`spec/05` §2.1), and the runs
        // partition the zero region so no escape overshoots the
        // plane (`spec/05` §7.4).
        out.resize(out.len() + left, 0);
        i += run;
    }
    out
}

// ─────────────────────── tests ───────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip_one(plane: &[u8], escape_len: usize) {
        let encoded = contract_raw(plane, escape_len);
        let (got, _consumed) = expand_raw(&encoded, escape_len, plane.len()).unwrap();
        assert_eq!(got.as_slice(), plane);
    }

    #[test]
    fn rle_roundtrip_trivial() {
        for e in 1..=3 {
            roundtrip_one(&[1, 2, 3, 4, 5], e);
        }
    }

    #[test]
    fn rle_roundtrip_short_zero_run_below_threshold() {
        // For escape_len=3, a run of 2 zeros must NOT trigger the
        // escape; it must emit literally.
        roundtrip_one(&[1, 0, 0, 2, 3], 3);
    }

    #[test]
    fn rle_roundtrip_long_run() {
        let mut plane = vec![1u8, 2];
        plane.extend(std::iter::repeat(0u8).take(500));
        plane.extend_from_slice(&[3, 4]);
        for e in 1..=3 {
            roundtrip_one(&plane, e);
        }
    }

    #[test]
    fn rle_roundtrip_at_threshold() {
        // exactly escape_len zeros adjacent to non-zero.
        for e in 1..=3 {
            let mut plane = vec![5u8];
            plane.extend(std::iter::repeat(0u8).take(e));
            plane.extend_from_slice(&[7, 8]);
            roundtrip_one(&plane, e);
        }
    }

    #[test]
    fn rle_roundtrip_terminal_run() {
        let mut plane = vec![1u8, 2, 3];
        plane.extend(std::iter::repeat(0u8).take(100));
        for e in 1..=3 {
            roundtrip_one(&plane, e);
        }
    }

    /// The `spec/05` §5.3 algebraic inverse covers the **full**
    /// padding domain: `LUT[supplement_for_padding(n)] == n` for
    /// every `n` in `0..=255` (the forward LUT is the staged
    /// `tables/01` extract).
    #[test]
    fn algebraic_supplement_inverts_forward_lut_full_range() {
        let fwd = rle_fwd_lut();
        for n in 0..=255usize {
            let s = supplement_for_padding(n);
            assert_eq!(fwd[s as usize] as usize, n, "padding {n} -> supplement {s}");
        }
    }

    /// On the domain the staged encoder INV_LUT (`tables/02`)
    /// expresses — index `k` in `2..=255`, padding `k - 2` in
    /// `0..=253` — the algebraic inverse agrees byte-for-byte
    /// (`spec/05` §5.3: "Both yield identical results").
    #[test]
    fn algebraic_supplement_agrees_with_staged_inv_lut() {
        let inv = crate::tables::rle_inv_lut();
        for (k, &s) in inv.iter().enumerate().skip(2) {
            assert_eq!(
                supplement_for_padding(k - 2),
                s,
                "padding {} (INV_LUT index {k})",
                k - 2,
            );
        }
    }

    /// Zero runs at and around the single-escape capacity boundary
    /// (`escape_len + 254`, `+ 255`, `+ 256`, and a multi-chunk run)
    /// round-trip at every escape length — the paddings 254 / 255
    /// were unreachable through the INV_LUT index form.
    #[test]
    fn rle_roundtrip_full_padding_boundary_runs() {
        for e in 1..=3usize {
            for extra in [253usize, 254, 255, 256, 511, 512] {
                let mut plane = vec![9u8];
                plane.extend(std::iter::repeat_n(0u8, e + extra));
                plane.push(7);
                roundtrip_one(&plane, e);
            }
        }
    }

    /// `spec/05` §5.4 canonical emit: a run of exactly
    /// `escape_len + 255` zeros is a **single** escape — `escape_len`
    /// zero bytes + supplement `0x80` (`inverse_LUT(255) = 128`).
    #[test]
    fn full_capacity_run_is_single_escape() {
        for e in 1..=3usize {
            let plane = vec![0u8; e + 255];
            let encoded = contract_raw(&plane, e);
            let mut expected = vec![0u8; e];
            expected.push(0x80);
            assert_eq!(encoded, expected, "e={e}");
            let (got, consumed) = expand_raw(&encoded, e, plane.len()).unwrap();
            assert_eq!(got, plane);
            assert_eq!(consumed, encoded.len());
        }
    }

    /// Measured size shrink from the full-range supplement: an
    /// all-zero 4096-byte plane at `escape_len = 1` contracts to
    /// exactly `2 * (4096 / 256) = 32` bytes (16 escapes of 256
    /// zeros each). The 253-capped chunking needed 17 escapes
    /// (34 bytes) — a 5.9% saving on this wire form.
    #[test]
    fn full_padding_range_shrinks_long_runs() {
        let plane = vec![0u8; 4096];
        let encoded = contract_raw(&plane, 1);
        assert_eq!(encoded.len(), 32, "expected 16 x [0x00, 0x80] escapes");
        for pair in encoded.chunks_exact(2) {
            assert_eq!(pair, [0x00, 0x80]);
        }
        let (got, _) = expand_raw(&encoded, 1, plane.len()).unwrap();
        assert_eq!(got, plane);
    }
}
