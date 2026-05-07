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
#[cfg(test)]
use crate::tables::rle_inv_lut;

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

/// Encode a plane of residuals into the escape-byte form for a given
/// escape length (test-only helper).
#[cfg(test)]
pub fn contract_raw(plane: &[u8], escape_len: usize) -> Vec<u8> {
    debug_assert!((1..=3).contains(&escape_len));
    let inv = rle_inv_lut();
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
        // Emit one or more escape sequences. The encoder INV_LUT
        // is 256 entries; only indices 2..=255 yield supplement
        // bytes whose decoded LUT value spans 0..=253, so the max
        // chunk size we can express in one escape is
        // `escape_len + 253`. (`spec/05` §3.2 LUT range is 0..=255
        // but `spec/05` §5.2 padding `INV_LUT[0..=2] = 0` shrinks
        // the encoder side.) Using the algebraic inverse of the
        // forward LUT lets us reach 0..=255 directly, but for
        // round 1 we stay strictly within INV_LUT's encoder
        // contract.
        const MAX_CHUNK_PADDING: usize = 253;
        let mut left = run;
        while left >= escape_len {
            let chunk = left.min(escape_len + MAX_CHUNK_PADDING);
            out.resize(out.len() + escape_len, 0);
            // supplement = INV_LUT[chunk - escape_len + 2].
            let k = chunk - escape_len + 2;
            debug_assert!(k <= 255, "INV_LUT index {k} out of range");
            out.push(inv[k]);
            left -= chunk;
        }
        // The remainder (< escape_len) is emitted literally.
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
}
