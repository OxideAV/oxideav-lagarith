//! Per-channel header dispatcher per `spec/03` §2.1 + `spec/06` §1.
//!
//! Decodes a single compressed channel slice into `n_pixels`
//! residuals. The channel-header byte at offset 0 selects the
//! sub-path:
//!
//! | Header | Wire form |
//! | ------ | --------- |
//! | `0x00` | Fibonacci prefix at offset 1 + arithmetic body. No RLE. Range coder produces `n_pixels` symbols. |
//! | `0x01..=0x03` | u32 length at offsets 1..4 (only when `< n_pixels`); Fibonacci prefix at offset 5; arithmetic body produces `u32` pre-RLE symbols. Post-process RLE-expand with `escape_len = header` to fill `n_pixels`. The "u32 ≥ n_pixels" fall-back diverts to header-`0x00` style. |
//! | `0x04` | `n_pixels` raw bytes at offset 1. No entropy. |
//! | `0x05..=0x07` | Raw bytes at offset 1, post-processed with RLE escape `escape_len = header - 4`. |
//! | `0xff` | Constant fill: byte at offset 1 replicated `n_pixels` times. |

use crate::error::{Error, Result};
use crate::fibonacci;
use crate::legacy_range_coder::{build_legacy_cdf, decode_legacy_freq_table, LegacyRangeDecoder};
use crate::range_coder::{Cdf, RangeDecoder};
use crate::rle;

/// Decode one channel into a `Vec<u8>` of `n_pixels` residuals.
/// Returns the residuals; the predictor pipeline runs separately.
pub(crate) fn decode_channel(channel: &[u8], n_pixels: usize) -> Result<Vec<u8>> {
    if channel.is_empty() {
        return Err(Error::Truncated {
            context: "channel header byte",
        });
    }
    let header = channel[0];
    match header {
        0x00 => decode_arith_no_rle(channel, 1, n_pixels),
        0x01..=0x03 => {
            // `spec/06` §1.4: read u32 at bytes 1..4. If >=
            // n_pixels, fall back to header-0 dispatch (Fibonacci
            // prefix begins at byte 1, no RLE).
            if channel.len() < 5 {
                return Err(Error::Truncated {
                    context: "header 0x01..0x03 u32 length field",
                });
            }
            let u32_field =
                u32::from_le_bytes([channel[1], channel[2], channel[3], channel[4]]) as usize;
            if u32_field >= n_pixels {
                decode_arith_no_rle(channel, 1, n_pixels)
            } else {
                let escape_len = header as usize;
                decode_arith_rle(channel, 5, u32_field, n_pixels, escape_len)
            }
        }
        0x04 => {
            if channel.len() < 1 + n_pixels {
                return Err(Error::Truncated {
                    context: "header 0x04 raw plane data",
                });
            }
            Ok(channel[1..1 + n_pixels].to_vec())
        }
        0x05..=0x07 => {
            let escape_len = (header - 4) as usize;
            let body = &channel[1..];
            let (out, _) = rle::expand_raw(body, escape_len, n_pixels)?;
            Ok(out)
        }
        0xff => {
            if channel.len() < 2 {
                return Err(Error::Truncated {
                    context: "header 0xff fill byte",
                });
            }
            Ok(vec![channel[1]; n_pixels])
        }
        other => Err(Error::BadChannelHeader(other)),
    }
}

/// Decode an arithmetic-coded channel (no RLE).
fn decode_arith_no_rle(channel: &[u8], prefix_offset: usize, n_pixels: usize) -> Result<Vec<u8>> {
    if channel.len() < prefix_offset {
        return Err(Error::Truncated {
            context: "channel data shorter than prefix offset",
        });
    }
    // `spec/06` §1.3 empty-channel short-circuit: header=0 + four
    // zero bytes at positions 1..4 means "leave plane at zero".
    if prefix_offset == 1 && channel.len() >= 5 {
        let u32_field = u32::from_le_bytes([channel[1], channel[2], channel[3], channel[4]]);
        if u32_field == 0 {
            return Ok(vec![0u8; n_pixels]);
        }
    }
    let prefix = &channel[prefix_offset..];
    let (freq, prefix_bytes) = fibonacci::decode_freq_table(prefix)?;
    let cdf = Cdf::from_frequencies(&freq)?;

    let body_offset = prefix_offset + prefix_bytes;
    if channel.len() < body_offset {
        return Err(Error::Truncated {
            context: "arithmetic body offset past channel end",
        });
    }
    let body = &channel[body_offset..];
    let mut dec = RangeDecoder::new(body)?;
    let mut symbols = Vec::with_capacity(n_pixels);
    for _ in 0..n_pixels {
        symbols.push(dec.decode_symbol(&cdf)?);
    }
    Ok(symbols)
}

/// Decode one **type-7 (legacy RGB)** channel into a `Vec<u8>` of
/// `n_pixels` residuals per `spec/07` §1.3 / §2.5. Wire layout
/// (header == 0x00 path, the canonical clean-room encoder choice
/// per `spec/07` §6.3):
///
/// | offset | bytes | meaning |
/// | ------ | ----- | ------- |
/// | 0      | 1     | outer channel-header byte (= 0x00) |
/// | 1      | 1     | inner codec-mode flag (= 0x00 for bare Fibonacci) |
/// | 2..    | N     | Fibonacci-coded 256-entry freq table |
/// | 2 + N  | 0..1  | post-Fibonacci 1-byte reservation (audit/08 §3.2) |
/// | 2+N+R  | …     | legacy range-coder body |
///
/// where `N = byte_advance_count` of the Fibonacci helper and
/// `R = 1` iff the bit stream ended on a byte boundary (the
/// reservation byte is *only* present in the byte-aligned case).
pub(crate) fn decode_legacy_channel(channel: &[u8], n_pixels: usize) -> Result<Vec<u8>> {
    if channel.len() < 2 {
        return Err(Error::Truncated {
            context: "legacy type-7 channel prefix",
        });
    }
    let header = channel[0];
    let inner = channel[1];
    if header != 0x00 {
        // Header != 0 selects the RLE-then-Fibonacci sub-path. The
        // round-4 cleanroom encoder ships only header-0 (the Python
        // reference impl notes that header-0 is sufficient for
        // round-trip correctness per `spec/07` §6.3 / §9.2 item 9).
        // We surface the unsupported sub-path as an error rather
        // than silently mis-decoding.
        return Err(Error::BadChannelHeader(header));
    }
    if inner != 0x00 {
        return Err(Error::BadChannelHeader(inner));
    }
    let fib_src = &channel[2..];
    let (freq, fib_bytes, fib_aligned) = decode_legacy_freq_table(fib_src)?;
    let body_offset = 2 + fib_bytes + if fib_aligned { 1 } else { 0 };
    if channel.len() < body_offset {
        return Err(Error::Truncated {
            context: "legacy type-7 channel body offset past end",
        });
    }
    let body = &channel[body_offset..];
    let (cdf, total) = build_legacy_cdf(&freq)?;
    let mut dec = LegacyRangeDecoder::new(body, cdf, total)?;
    let mut out = Vec::with_capacity(n_pixels);
    for _ in 0..n_pixels {
        out.push(dec.decode_byte()?);
    }
    Ok(out)
}

/// Decode an arithmetic-coded channel with inline RLE: produce
/// `pre_rle_symbol_count` symbols, then post-process the symbol
/// sequence with the residual zero-run RLE escape (`spec/05`) to
/// fill an `n_pixels`-sized plane buffer.
fn decode_arith_rle(
    channel: &[u8],
    prefix_offset: usize,
    pre_rle_symbol_count: usize,
    n_pixels: usize,
    escape_len: usize,
) -> Result<Vec<u8>> {
    debug_assert!((1..=3).contains(&escape_len));
    if channel.len() < prefix_offset {
        return Err(Error::Truncated {
            context: "channel data shorter than prefix offset",
        });
    }
    let prefix = &channel[prefix_offset..];
    let (freq, prefix_bytes) = fibonacci::decode_freq_table(prefix)?;
    let cdf = Cdf::from_frequencies(&freq)?;

    let body_offset = prefix_offset + prefix_bytes;
    if channel.len() < body_offset {
        return Err(Error::Truncated {
            context: "arithmetic body offset past channel end",
        });
    }
    let body = &channel[body_offset..];
    let mut dec = RangeDecoder::new(body)?;
    let mut symbols = Vec::with_capacity(pre_rle_symbol_count);
    for _ in 0..pre_rle_symbol_count {
        symbols.push(dec.decode_symbol(&cdf)?);
    }

    // Post-process: the symbol sequence is the same form `expand_raw`
    // consumes (escape_len consecutive zeros + supplement byte, etc.).
    let (plane, _) = rle::expand_raw(&symbols, escape_len, n_pixels)?;
    Ok(plane)
}
