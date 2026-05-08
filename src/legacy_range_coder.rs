//! Legacy adaptive-CDF range coder for **frame type 7** (pre-1.1.0
//! "Obsolete arithmetic coded RGB keyframe") per `spec/07`.
//!
//! Differs from the modern coder of [`super::range_coder`] in two
//! load-bearing ways:
//!
//! 1. The probability distribution is transmitted **per-channel** as a
//!    256-entry frequency table encoded with the 8-value Fibonacci
//!    series `{1, 2, 3, 5, 8, 13, 21, 34}` and a length-class +
//!    suffix-bits encoding (`spec/07` §2.2). No zero-run sub-prefix
//!    (the modern coder's `v == 1` zero-run optimisation is **absent**
//!    from the legacy decoder body, audit/03 §3.1).
//! 2. The decoder builds a 257-entry CDF on the fly from the
//!    transmitted frequencies via the §3 "pair-pack + rescale-to-
//!    pow2 + zigzag-residue + prefix-sum" pipeline.
//!
//! The byte refill model and renormalise threshold (`TOP = 0x800000`)
//! are the same as the modern coder; the per-symbol lookup is a
//! binary search of the 257-entry CDF (`spec/07` §5.2 final paragraph
//! invites any algebraically-equivalent search). The init seed is
//! algebraically equivalent to the modern coder's 4-byte priming
//! (`spec/07` §9.1 item 3).
//!
//! The proprietary build is decode-only for type 7 — but
//! `CLEANROOM-MANUAL §8` ("Both directions, always") requires the
//! cleanroom to ship encoder + decoder for every wire variant.

use crate::error::{Error, Result};
use crate::fibonacci::BitReader;

/// 8-value Fibonacci series used by the legacy frequency-table
/// prefix (`spec/07` §2.2). The seven literal series values plus the
/// implicit base-1 enter the proprietary x86-64 binary at
/// `lagarith.dll!0x180001c94..0x180001cca`; the eighth value `34`
/// is the implicit terminator.
const LEGACY_FIB: [u32; 8] = [1, 2, 3, 5, 8, 13, 21, 34];

/// Renormalisation threshold (`spec/07` §4.3). Identical to the
/// modern coder's `TOP`.
const LEGACY_TOP: u32 = 0x0080_0000;

// ──────────────────── Fibonacci freq-table codec ────────────────────

/// Decode one Fibonacci length-class `v` (>= 1) from a [`BitReader`]
/// per `spec/07` §2.2 stage 1. Walks bits MSB-first; for each
/// "first-of-run-of-ones" bit position adds the corresponding
/// `LEGACY_FIB[k]` into `v`. Termination is on two consecutive `1`
/// bits.
fn decode_legacy_length_class(br: &mut BitReader<'_>) -> Result<u32> {
    let mut v: u32 = 0;
    let mut prev: u8 = 0;
    let mut pos: usize = 0;
    loop {
        let cur = br.read_bit()?;
        if prev == 1 && cur == 1 {
            return Ok(v);
        }
        if cur == 1 {
            if pos >= LEGACY_FIB.len() {
                return Err(Error::FibonacciOverflow);
            }
            v += LEGACY_FIB[pos];
        }
        prev = cur;
        pos += 1;
        // Hard cap: the `0x80` mask consumes 8 bit positions before
        // resetting; the prefix may be at most LEGACY_FIB.len() + 1
        // bits long before terminating.
        if pos > LEGACY_FIB.len() + 1 {
            return Err(Error::FibonacciOverflow);
        }
    }
}

/// Read `m` suffix bits MSB-first into an accumulator pre-initialised
/// to `1`, returning `(1 << m) | s` where `s` is the natural-binary
/// value of the bits. `m == 0` returns 1.
fn read_legacy_suffix(br: &mut BitReader<'_>, m: u32) -> Result<u32> {
    let mut acc: u32 = 1;
    for _ in 0..m {
        let bit = br.read_bit()?;
        acc = (acc << 1) | (bit as u32);
    }
    Ok(acc)
}

/// Decode the 256-entry frequency table from a Fibonacci-coded byte
/// stream per `spec/07` §2.2 (no zero-run sub-prefix — that's the
/// modern coder's path).
///
/// Returns `(freq, bytes_consumed, byte_aligned)`:
/// - `freq` is the 256 raw frequencies.
/// - `bytes_consumed` is the byte count rounded up if the bit stream
///   ends mid-byte (matching the proprietary helper at
///   `lagarith.dll!0x180001c80`'s `byte_advance_count`, *before* the
///   `+1` reservation adjustment).
/// - `byte_aligned` is true iff the bit stream ended on a byte
///   boundary — used by the channel-level decoder to decide whether
///   the post-Fibonacci 1-byte reservation byte is present (audit/08
///   §3.2).
pub(crate) fn decode_legacy_freq_table(src: &[u8]) -> Result<([u32; 256], usize, bool)> {
    let mut br = BitReader::new(src);
    let mut freq = [0u32; 256];
    for slot in freq.iter_mut() {
        let v = decode_legacy_length_class(&mut br)?;
        // `m = v - 1`; m == 0 => freq = 0 (single-bit zero), no
        // suffix bits read.
        if v == 0 {
            // length-class zero is a malformed wire (decoder always
            // sees v >= 1 from a well-formed stream).
            return Err(Error::Truncated {
                context: "legacy Fibonacci length-class (v=0 invalid)",
            });
        }
        let m = v - 1;
        if m == 0 {
            *slot = 0;
        } else if m > 31 {
            return Err(Error::FibonacciOverflow);
        } else {
            let acc = read_legacy_suffix(&mut br, m)?;
            *slot = acc.wrapping_sub(1);
        }
    }
    let byte_aligned = br.is_byte_aligned();
    Ok((freq, br.bytes_consumed(), byte_aligned))
}

// ─────────────────────── CDF construction ───────────────────────

/// Smallest power of two `>= n` (for `n >= 1`).
fn next_pow2(n: u64) -> u64 {
    if n <= 1 {
        return 1;
    }
    let lz = (n - 1).leading_zeros();
    1u64 << (64 - lz)
}

/// Build the 257-entry CDF from a 256-entry raw-frequency table per
/// `spec/07` §3. Returns `(cdf, total)` where `cdf[256] == total`
/// and `total` is a power of two.
///
/// The clean-room implementation here uses the **flat 257-entry CDF**
/// form (`spec/07` §3.4 retracted allowance). Audit/12 §5 retracts
/// the "may equivalently use a flat 257-entry CDF" allowance for a
/// rare-symbol-cluster fixture class (≥3 distinct nonzero bins each
/// with `freq ∈ {1, 2}` within a histogram dominated by
/// `freq[0] >= 0.95 * pixel_count`).
///
/// For our **self-roundtrip discipline** (encoder + decoder share
/// the same `build_cdf` algorithm) the flat form is bit-perfect:
/// both sides run identical CDF construction, so any input the
/// encoder produces, the decoder accepts byte-exactly. The
/// audit/12 retraction concerns *cross-implementation* compatibility
/// (our encoder vs. proprietary decoder, or vice-versa), which would
/// require a proprietary-encoded type-7 fixture in tree (audit/04
/// §5: not yet acquired). See [`is_rare_symbol_cluster`] for the
/// detection signature that a future round may use to route around
/// the affected fixture class via Strategy E (audit/12 §7.1).
pub(crate) fn build_legacy_cdf(freq: &[u32; 256]) -> Result<(Vec<u32>, u32)> {
    let sum_freq: u64 = freq.iter().map(|&f| f as u64).sum();
    if sum_freq == 0 {
        return Err(Error::EmptyProbabilityTable);
    }
    // Step 2: target total = next_pow2(sum_freq).
    let total = next_pow2(sum_freq);
    if total > u32::MAX as u64 {
        return Err(Error::EmptyProbabilityTable);
    }

    // Step 3: rescale via floor(freq[c] * total / sum_freq).
    let mut new_freq = [0u32; 256];
    if sum_freq == total {
        new_freq.copy_from_slice(freq);
    } else {
        let ratio = total as f64 / sum_freq as f64;
        for c in 0..256 {
            new_freq[c] = (freq[c] as f64 * ratio) as u32;
        }
    }

    // Step 4: residue distribution via zigzag walk over slots
    // 0, -1, 1, -2, 2, ... (negative indices interpreted mod 256).
    let new_sum: u64 = new_freq.iter().map(|&f| f as u64).sum();
    if (new_sum as i64) > total as i64 {
        // Over-shoot: subtract from the largest non-zero slots until
        // the difference is absorbed (the proprietary's "subtract
        // from slot-1 of pair-pack" simplification under the flat-
        // CDF model `spec/07` §3.3 final paragraph; held-forward item
        // 11 covers the edge case where slot 1 alone can't absorb).
        let mut deficit = (new_sum - total) as u32;
        for slot in new_freq.iter_mut() {
            if deficit == 0 {
                break;
            }
            let take = (*slot).min(deficit);
            *slot -= take;
            deficit -= take;
        }
    } else if new_sum < total {
        // Distribute residue via zigzag walk per `spec/07` §3.3.
        let mut residue = (total - new_sum) as u32;
        let mut c: u32 = 0;
        let mut idx: i32 = 0;
        while residue > 0 {
            let slot = idx.rem_euclid(256) as usize;
            new_freq[slot] = new_freq[slot].saturating_add(1);
            residue -= 1;
            c += 1;
            if c == 256 {
                c = 0;
                idx = 0;
            } else if c % 2 == 0 {
                idx = (c >> 1) as i32;
            } else {
                idx = -(((c + 1) >> 1) as i32);
            }
        }
    }

    // Step 5: prefix-sum into a flat 257-entry CDF.
    let mut cdf = vec![0u32; 257];
    let mut acc: u32 = 0;
    for c in 0..256 {
        cdf[c] = acc;
        acc = acc.wrapping_add(new_freq[c]);
    }
    cdf[256] = acc;

    // Defensive: residue handling should land cdf[256] on total.
    if cdf[256] as u64 != total {
        // Final patch: bump the largest non-zero slot up/down to land
        // on total. This only fires for over-shoot edge cases that
        // the per-slot zero-clamp leaves unresolved.
        let diff: i64 = total as i64 - cdf[256] as i64;
        if diff > 0 {
            // Bump slot 0 (or any non-zero slot we can find).
            let donor = new_freq.iter().position(|&f| f > 0).unwrap_or(0);
            new_freq[donor] = new_freq[donor].saturating_add(diff as u32);
        } else if diff < 0 {
            let take = (-diff) as u32;
            let donor = new_freq.iter().position(|&f| f >= take).unwrap_or(0);
            new_freq[donor] -= take;
        }
        // Re-prefix-sum.
        acc = 0;
        for c in 0..256 {
            cdf[c] = acc;
            acc = acc.wrapping_add(new_freq[c]);
        }
        cdf[256] = acc;
    }

    Ok((cdf, total as u32))
}

/// Detect the **rare-symbol-cluster signature** of audit/12 §7.1 in
/// a 256-entry histogram: ≥ 3 distinct nonzero bins each with
/// `freq ∈ {1, 2}` within a histogram dominated by
/// `freq[0] >= 0.95 * pixel_count`.
///
/// This is the signature audit/12 §3.6 / §5 identified as the
/// trigger for the flat-CDF / pair-packed-CDF wire-format
/// divergence. A future round implementing Strategy E (audit/12
/// §7.1) — encoder-side route-around for rare-symbol-cluster
/// fixtures — would invoke this on each plane's residual histogram
/// and skip the type-7 emission when it returns `true`, falling
/// back to type 1 (uncompressed) which is byte-exact on every
/// fixture.
///
/// The full Strategy F refactor (pair-packed 513-entry CDF) is
/// audit/12 §7.1's alternative; the round-7 auditor recommends
/// Strategy E because Strategy F's regression risk on the 95/96
/// currently-passing type-7 cells outweighs its benefit (type 7 is
/// decode-only in the proprietary build per `spec/07` §6 / §9.2
/// item 8 — no archival type-7 fixture exists per audit/04 §5).
///
/// Wired into two consumers:
///
/// * [`crate::encoder::encode_legacy_rgb`] /
///   [`crate::encoder::encode_legacy_rgb_rle`] (round 6, encode side):
///   when any of the three residual planes (B', G, R') matches this
///   signature, the encoder skips the type-7 emission and falls
///   through to a type-1 (uncompressed) frame, which is byte-exact
///   on every fixture per `audit/12 §7.1` Strategy E + `audit/13
///   §3` cross-validation.
///
/// * [`crate::channel::decode_legacy_channel`] (round 7, decode side
///   defensive harness): when the transmitted freq table matches
///   this signature, the decoder returns
///   [`Error::LegacyRareSymbolClusterUnsupported`] rather than
///   silently mis-decoding the body. Audit/12 §5..§6 confirms the
///   proprietary's pair-packed 513-entry CDF and the cleanroom's
///   flat 257-entry CDF are *not* bit-equivalent for this signature,
///   so a foreign encoder's stream with this freq table would feed
///   a different residual sequence to our predictor than to the
///   proprietary's. Surfacing the mismatch explicitly is preferable
///   to silent miscoding for any downstream caller that ingests
///   wild type-7 streams.
pub(crate) fn is_rare_symbol_cluster(freq: &[u32; 256]) -> bool {
    let total: u64 = freq.iter().map(|&f| f as u64).sum();
    if total == 0 {
        return false;
    }
    // Dominance check: freq[0] >= 0.95 * total. Use integer math.
    if (freq[0] as u64) * 100 < total * 95 {
        return false;
    }
    // Rare-bin count: distinct nonzero bins with freq ∈ {1, 2}.
    let rare_count = freq[1..].iter().filter(|&&f| f == 1 || f == 2).count();
    rare_count >= 3
}

// ──────────────────── range-coder decoder ────────────────────

/// Find the symbol `s` such that `cdf[s] <= target < cdf[s+1]` via
/// binary search of the 257-entry CDF (algebraically equivalent to
/// the eight-level binary-tree descent of `spec/07` §5.2).
fn cdf_find_symbol(cdf: &[u32], target: u32) -> usize {
    debug_assert_eq!(cdf.len(), 257);
    let mut lo = 0usize;
    let mut hi = 256usize;
    while lo + 1 < hi {
        let mid = (lo + hi) >> 1;
        if cdf[mid] <= target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// Legacy range-coder decoder (`spec/07` §4..§5).
pub(crate) struct LegacyRangeDecoder<'a> {
    src: &'a [u8],
    cursor: usize,
    low: u32,
    range: u32,
    cdf: Vec<u32>,
    total: u32,
    shift: u32,
}

impl<'a> LegacyRangeDecoder<'a> {
    /// Init from the body's 4 priming bytes per `spec/07` §4.2 +
    /// audit-corrected §9.1 item 3 (the "init + 3 refills" sequence
    /// is equivalent to a 4-byte priming
    /// `low = (b1<<23) | (b2<<15) | (b3<<7) | (b4>>1)` followed by
    /// `range = 0x80000000`).
    pub fn new(body: &'a [u8], cdf: Vec<u32>, total: u32) -> Result<Self> {
        if body.len() < 4 {
            return Err(Error::Truncated {
                context: "legacy range-coder priming bytes",
            });
        }
        if total == 0 || (total & (total - 1)) != 0 {
            return Err(Error::EmptyProbabilityTable);
        }
        let b1 = body[0] as u32;
        let b2 = body[1] as u32;
        let b3 = body[2] as u32;
        let b4 = body[3] as u32;
        let low = (b1 << 23) | (b2 << 15) | (b3 << 7) | (b4 >> 1);
        let shift = total.trailing_zeros();
        Ok(Self {
            src: body,
            cursor: 3,
            low,
            range: 0x8000_0000,
            cdf,
            total,
            shift,
        })
    }

    fn renormalise(&mut self) {
        while self.range <= LEGACY_TOP {
            let prev = self.src.get(self.cursor).copied().unwrap_or(0);
            let next = self.src.get(self.cursor + 1).copied().unwrap_or(0);
            self.low =
                (self.low << 8).wrapping_add((((prev & 1) as u32) << 7) | ((next as u32) >> 1));
            self.range <<= 8;
            self.cursor += 1;
        }
    }

    /// Decode one byte symbol per `spec/07` §5.
    pub fn decode_byte(&mut self) -> Result<u8> {
        let q = self.range >> self.shift;
        if q == 0 {
            return Err(Error::EmptyProbabilityTable);
        }
        let mut symbol_index = self.low / q;
        if symbol_index >= self.total {
            symbol_index = self.total - 1;
        }
        let c = cdf_find_symbol(&self.cdf, symbol_index);
        let cdf_low = self.cdf[c];
        let cdf_high = self.cdf[c + 1];
        self.low = self.low.wrapping_sub(cdf_low.wrapping_mul(q));
        self.range = if cdf_high == self.total {
            // Last-symbol fast path per `spec/07` §5.3.
            self.range.wrapping_sub(cdf_low.wrapping_mul(q))
        } else {
            (cdf_high - cdf_low).wrapping_mul(q)
        };
        self.renormalise();
        Ok(c as u8)
    }
}

// ──────────────────── range-coder encoder ────────────────────

/// Legacy range-coder encoder (`spec/07` §6.4). Structurally
/// identical to the modern Subbotin encoder; only the wrapper
/// (channel prefix layout) differs.
#[cfg(test)]
pub(crate) struct LegacyRangeEncoder {
    out: Vec<u8>,
    pending_ffs: u32,
    cache: Option<u8>,
    low: u32,
    range: u32,
    cdf: Vec<u32>,
    total: u32,
    shift: u32,
}

#[cfg(test)]
impl LegacyRangeEncoder {
    pub fn new(cdf: Vec<u32>, total: u32) -> Self {
        let shift = total.trailing_zeros();
        Self {
            out: Vec::new(),
            pending_ffs: 0,
            cache: None,
            low: 0,
            range: 0x8000_0000,
            cdf,
            total,
            shift,
        }
    }

    fn shift_low(&mut self) {
        let carry = (self.low >> 31) & 1 == 1;
        let byte = ((self.low >> 23) & 0xff) as u8;
        if carry {
            if let Some(c) = self.cache.take() {
                self.out.push(c.wrapping_add(1));
            }
            for _ in 0..self.pending_ffs {
                self.out.push(0x00);
            }
            self.pending_ffs = 0;
            self.cache = Some(byte);
        } else if byte == 0xff {
            self.pending_ffs += 1;
        } else {
            if let Some(c) = self.cache.take() {
                self.out.push(c);
            }
            for _ in 0..self.pending_ffs {
                self.out.push(0xff);
            }
            self.pending_ffs = 0;
            self.cache = Some(byte);
        }
        self.low = (self.low & 0x007f_ffff) << 8;
    }

    fn renormalise(&mut self) {
        while self.range <= LEGACY_TOP {
            self.shift_low();
            self.range <<= 8;
        }
    }

    pub fn encode_byte(&mut self, c: u8) {
        let cdf_low = self.cdf[c as usize];
        let cdf_high = self.cdf[c as usize + 1];
        debug_assert_ne!(cdf_low, cdf_high, "encode_byte: zero-prob symbol");
        let q = self.range >> self.shift;
        let added = cdf_low.wrapping_mul(q);
        self.low = self.low.wrapping_add(added);
        self.range = if cdf_high == self.total {
            self.range.wrapping_sub(cdf_low.wrapping_mul(q))
        } else {
            (cdf_high - cdf_low).wrapping_mul(q)
        };
        self.renormalise();
    }

    /// Flush the four-byte tail and commit any remaining cache /
    /// pending-`0xff` chain.
    pub fn finish(mut self) -> Vec<u8> {
        let carry = (self.low >> 31) & 1 == 1;
        if carry {
            if let Some(c) = self.cache.take() {
                self.out.push(c.wrapping_add(1));
            }
            for _ in 0..self.pending_ffs {
                self.out.push(0x00);
            }
        } else {
            if let Some(c) = self.cache.take() {
                self.out.push(c);
            }
            for _ in 0..self.pending_ffs {
                self.out.push(0xff);
            }
        }
        self.pending_ffs = 0;
        self.low &= 0x7fff_ffff;
        let low = self.low;
        let tail = [
            ((low >> 23) & 0xff) as u8,
            ((low >> 15) & 0xff) as u8,
            ((low >> 7) & 0xff) as u8,
            ((low << 1) & 0xff) as u8,
        ];
        self.out.extend_from_slice(&tail);
        self.out
    }
}

// ──────────────────── Fibonacci freq-table encoder ────────────────────

/// Greedy Zeckendorf-style decomposition over the 8-value
/// `LEGACY_FIB` series. Returns the bit-position list (ascending) of
/// non-consecutive summands. `value >= 1`.
#[cfg(test)]
fn legacy_zeckendorf(value: u32) -> Vec<usize> {
    let mut positions: Vec<usize> = Vec::new();
    let mut remaining = value;
    for k in (0..LEGACY_FIB.len()).rev() {
        if LEGACY_FIB[k] <= remaining {
            positions.push(k);
            remaining -= LEGACY_FIB[k];
        }
    }
    debug_assert_eq!(remaining, 0, "value must be representable");
    positions.reverse();
    positions
}

/// Encode a 256-entry frequency table into a Fibonacci-coded byte
/// stream (the inverse of [`decode_legacy_freq_table`]).
///
/// Returns `(bytes, byte_aligned)`. `byte_aligned` is true iff the
/// encoded bit stream length is a multiple of 8 bits — the
/// channel-level encoder uses this to decide whether to emit the
/// post-Fibonacci 1-byte reservation per audit/08 §3.2.
#[cfg(test)]
pub(crate) fn encode_legacy_freq_table(freq: &[u32; 256]) -> (Vec<u8>, bool) {
    use crate::fibonacci::BitWriter;
    let mut bw = BitWriter::new();
    for &f in freq.iter() {
        // length-class on wire = m + 1, where m = bit_length(f+1) - 1.
        let v = f.saturating_add(1);
        let m = v.ilog2();
        let length_class = m + 1; // = v.ilog2() + 1; always >= 1.
                                  // Emit length-class via Zeckendorf decomposition + terminator.
        let positions: std::collections::HashSet<usize> =
            legacy_zeckendorf(length_class).into_iter().collect();
        let highest = *positions.iter().max().expect("length_class >= 1");
        for k in 0..=highest {
            bw.write_bit(if positions.contains(&k) { 1 } else { 0 });
        }
        // Terminator '1'.
        bw.write_bit(1);
        // Suffix bits: low m bits of v, MSB-first.
        if m > 0 {
            for i in (0..m).rev() {
                let bit = ((v >> i) & 1) as u8;
                bw.write_bit(bit);
            }
        }
    }
    let aligned = bw.is_byte_aligned();
    (bw.finish(), aligned)
}

// ──────────────────── tests ────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_fib_freq_roundtrip_simple() {
        let mut freq = [0u32; 256];
        freq[0] = 5;
        freq[10] = 3;
        freq[100] = 1;
        freq[200] = 7;
        freq[255] = 2;
        let (bytes, _aligned) = encode_legacy_freq_table(&freq);
        let (got, _consumed, _aligned2) = decode_legacy_freq_table(&bytes).unwrap();
        assert_eq!(got[..], freq[..]);
    }

    #[test]
    fn legacy_fib_freq_roundtrip_dense() {
        let mut freq = [0u32; 256];
        for (i, slot) in freq.iter_mut().enumerate() {
            *slot = ((i as u32) % 13) + 1;
        }
        let (bytes, _) = encode_legacy_freq_table(&freq);
        let (got, _, _) = decode_legacy_freq_table(&bytes).unwrap();
        for (i, (a, b)) in got.iter().zip(freq.iter()).enumerate() {
            assert_eq!(a, b, "i={i}");
        }
    }

    #[test]
    fn legacy_fib_freq_roundtrip_with_zeros() {
        // Zero entries are encoded as a single-bit zero (length-class
        // 1 = "11" two-bit prefix, no suffix).
        let mut freq = [0u32; 256];
        freq[0] = 1;
        freq[1] = 1;
        // freq[2..255] all zero.
        freq[255] = 5;
        let (bytes, _) = encode_legacy_freq_table(&freq);
        let (got, _, _) = decode_legacy_freq_table(&bytes).unwrap();
        assert_eq!(got, freq);
    }

    #[test]
    fn build_cdf_lands_on_power_of_two_total() {
        let mut freq = [0u32; 256];
        freq[0] = 100;
        freq[1] = 50;
        freq[2] = 25;
        freq[10] = 10;
        let (cdf, total) = build_legacy_cdf(&freq).unwrap();
        assert_eq!(cdf.len(), 257);
        assert_eq!(cdf[256], total);
        // total should be a power of two >= 185.
        assert!(total.is_power_of_two());
        assert!(total >= 185);
        // CDF strictly non-decreasing.
        for w in cdf.windows(2) {
            assert!(w[0] <= w[1], "CDF not monotonic: {} -> {}", w[0], w[1]);
        }
    }

    #[test]
    fn build_cdf_uniform_sum_is_pow2() {
        // A uniform 256-entry table summing to 256 stays at total=256.
        let freq = [1u32; 256];
        let (cdf, total) = build_legacy_cdf(&freq).unwrap();
        assert_eq!(total, 256);
        assert_eq!(cdf[256], 256);
    }

    #[test]
    fn legacy_range_coder_roundtrip_small() {
        let mut freq = [0u32; 256];
        freq[0] = 4;
        freq[1] = 1;
        freq[2] = 3;
        freq[7] = 2;
        let (cdf, total) = build_legacy_cdf(&freq).unwrap();

        let symbols: Vec<u8> = vec![0, 0, 1, 7, 2, 0, 7, 2, 2, 0, 1];
        let mut enc = LegacyRangeEncoder::new(cdf.clone(), total);
        for &s in &symbols {
            enc.encode_byte(s);
        }
        let bytes = enc.finish();

        let mut dec = LegacyRangeDecoder::new(&bytes, cdf, total).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_byte().unwrap());
        }
        assert_eq!(out, symbols);
    }

    #[test]
    fn rare_symbol_cluster_detects_audit_12_canonical_fixture() {
        // audit/12 §5 reproduction: hist[0] = 887 (= 0x3d / total =
        // 887 / 891 ~ 99.5%), hist[0x3d] = 1, hist[0x40] = 2,
        // hist[0xc0] = 1. 3 rare bins + dominant zero -> matches.
        let mut freq = [0u32; 256];
        freq[0] = 887;
        freq[0x3d] = 1;
        freq[0x40] = 2;
        freq[0xc0] = 1;
        assert!(is_rare_symbol_cluster(&freq));
    }

    #[test]
    fn rare_symbol_cluster_rejects_solid() {
        // Solid plane: only one bin nonzero. Not a cluster.
        let mut freq = [0u32; 256];
        freq[0] = 1024;
        assert!(!is_rare_symbol_cluster(&freq));
    }

    #[test]
    fn rare_symbol_cluster_rejects_too_few_rare_bins() {
        // Two rare bins is below the threshold of 3.
        let mut freq = [0u32; 256];
        freq[0] = 1000;
        freq[10] = 1;
        freq[20] = 2;
        assert!(!is_rare_symbol_cluster(&freq));
    }

    #[test]
    fn rare_symbol_cluster_rejects_no_dominant_zero() {
        // Dispersed nonzero distribution (random). No dominant zero.
        let mut freq = [0u32; 256];
        for slot in freq.iter_mut() {
            *slot = 4;
        }
        assert!(!is_rare_symbol_cluster(&freq));
    }

    #[test]
    fn rare_symbol_cluster_rejects_high_freq_neighbours() {
        // Dominant zero but only `freq > 2` neighbours -> no rare
        // cluster. This is the "common-case type-7 fixture" pattern
        // (audit/12 §3.5 PASS rows).
        let mut freq = [0u32; 256];
        freq[0] = 970;
        freq[1] = 10;
        freq[2] = 10;
        freq[3] = 10;
        assert!(!is_rare_symbol_cluster(&freq));
    }

    #[test]
    fn legacy_range_coder_roundtrip_wide() {
        // Build a frequency table over a wider symbol range.
        let mut freq = [0u32; 256];
        for (i, slot) in freq.iter_mut().enumerate() {
            *slot = ((i as u32 * 7) % 11) + 1;
        }
        let (cdf, total) = build_legacy_cdf(&freq).unwrap();

        let mut symbols = Vec::with_capacity(2000);
        for i in 0..2000 {
            symbols.push(((i * 31) ^ (i >> 3)) as u8);
        }
        let mut enc = LegacyRangeEncoder::new(cdf.clone(), total);
        for &s in &symbols {
            enc.encode_byte(s);
        }
        let bytes = enc.finish();

        let mut dec = LegacyRangeDecoder::new(&bytes, cdf, total).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_byte().unwrap());
        }
        assert_eq!(out, symbols);
    }
}
