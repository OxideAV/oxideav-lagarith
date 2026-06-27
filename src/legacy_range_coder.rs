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

/// Run the shared `spec/07` §3.2..§3.3 rescale + residue-distribution
/// pipeline, returning `(rescaled_freq, total)` where
/// `Σ rescaled_freq == total` and `total` is the smallest power of
/// two `>= Σ freq`.
///
/// This is the common front-end of both CDF forms — the flat
/// 257-entry CDF of [`build_legacy_cdf`] and the pair-packed
/// 513-entry CDF of [`build_legacy_pair_packed_cdf`] differ only in
/// the §3.4 prefix-sum *layout*, not in the rescale that precedes it.
fn rescale_freq(freq: &[u32; 256]) -> Result<([u32; 256], u32)> {
    let sum_freq: u64 = freq.iter().map(|&f| f as u64).sum();
    if sum_freq == 0 {
        return Err(Error::EmptyProbabilityTable);
    }
    // Step 2 (`spec/07` §3.2): target total = next_pow2(sum_freq).
    let total = next_pow2(sum_freq);
    if total > u32::MAX as u64 {
        return Err(Error::EmptyProbabilityTable);
    }

    // Step 3 (`spec/07` §3.2): rescale via floor(freq[c] * total / sum).
    let mut new_freq = [0u32; 256];
    if sum_freq == total {
        new_freq.copy_from_slice(freq);
    } else {
        let ratio = total as f64 / sum_freq as f64;
        for c in 0..256 {
            new_freq[c] = (freq[c] as f64 * ratio) as u32;
        }
    }

    // Step 4 (`spec/07` §3.3): residue distribution via zigzag walk
    // over slots 0, -1, 1, -2, 2, ... (negative indices mod 256).
    let new_sum: u64 = new_freq.iter().map(|&f| f as u64).sum();
    if (new_sum as i64) > total as i64 {
        // Over-shoot: subtract from the largest non-zero slots until
        // the difference is absorbed (the proprietary's "subtract
        // from slot-1 of pair-pack" simplification; held-forward item
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

    // Defensive: residue handling should land Σ new_freq on total.
    let landed: u64 = new_freq.iter().map(|&f| f as u64).sum();
    if landed != total {
        let diff: i64 = total as i64 - landed as i64;
        if diff > 0 {
            let donor = new_freq.iter().position(|&f| f > 0).unwrap_or(0);
            new_freq[donor] = new_freq[donor].saturating_add(diff as u32);
        } else if diff < 0 {
            let take = (-diff) as u32;
            let donor = new_freq.iter().position(|&f| f >= take).unwrap_or(0);
            new_freq[donor] -= take;
        }
    }

    Ok((new_freq, total as u32))
}

/// Build the 257-entry **flat** CDF from a 256-entry raw-frequency
/// table per `spec/07` §3. Returns `(cdf, total)` where
/// `cdf[256] == total` and `total` is a power of two.
///
/// This is the **self-roundtrip** form (`spec/07` §3.4's flat
/// allowance). Audit/12 §5 retracts the "may equivalently use a flat
/// 257-entry CDF" allowance *for cross-implementation parity* on a
/// rare-symbol-cluster fixture class (≥3 distinct nonzero bins each
/// with `freq ∈ {1, 2}` within a histogram dominated by
/// `freq[0] >= 0.95 * pixel_count`).
///
/// For our self-roundtrip discipline (encoder + decoder share the
/// same `build_cdf` algorithm) the flat form is bit-perfect: both
/// sides run identical CDF construction, so any input the encoder
/// produces, the decoder accepts byte-exactly. To match the
/// *proprietary* decoder on its own (rare-symbol-cluster) streams,
/// use [`build_legacy_pair_packed_cdf`] instead.
pub(crate) fn build_legacy_cdf(freq: &[u32; 256]) -> Result<(Vec<u32>, u32)> {
    let (new_freq, total) = rescale_freq(freq)?;

    // Step 5 (`spec/07` §3.4): prefix-sum into a flat 257-entry CDF.
    let mut cdf = vec![0u32; 257];
    let mut acc: u32 = 0;
    for c in 0..256 {
        cdf[c] = acc;
        acc = acc.wrapping_add(new_freq[c]);
    }
    cdf[256] = acc;

    Ok((cdf, total))
}

/// Build the proprietary's **pair-packed 513-entry** CDF from a
/// 256-entry raw-frequency table per `spec/07` §3.1 + §3.4
/// (audit-corrected) + audit/12 §7.1 "Strategy F". Returns
/// `(pair_cdf, total)` where `total` is the same power-of-two divisor
/// as the flat form and `pair_cdf` has 513 entries.
///
/// ## Layout (`spec/07` §3.1)
///
/// The proprietary builder copies the rescaled frequencies into a
/// 512-cell buffer with a per-symbol `(freq'[c], 1)` interleave — the
/// frequency followed by a sentinel `1` — then prefix-sums it. The
/// 513-entry result has, for symbol `c`:
///
/// * `pair_cdf[2c]`     = lower bound of symbol `c`'s interval
/// * `pair_cdf[2c + 1]` = `pair_cdf[2c] + freq'[c]` (upper bound)
///
/// with the sentinel-`1` between `pair_cdf[2c+1]` and `pair_cdf[2c+2]`
/// being the inter-symbol gap. `pair_cdf[512] == total + 256` (the
/// 256 sentinels contribute 256 units beyond the freq sum).
///
/// ## Why it differs from the flat form (audit/12 §5..§6)
///
/// The range coder of `spec/07` §4..§5 divides `low` by
/// `q = range >> log2(total)`, so the `symbol_index` it computes lies
/// in `[0, total)` — **not** in `[0, total + 256)`. The sentinel-`1`
/// gaps therefore push high-index rare symbols' lower bounds past
/// `total`, making them **unreachable** by the binary descent. This
/// is exactly the proprietary's documented (mis-)decode behaviour:
/// audit/12 §3.6 shows the proprietary decoding the cleanroom-encoded
/// `0xc0` residual as `0xff` because `0xc0`'s pair-packed lower bound
/// (`spec/07` §3.1 worked through audit/12 §5: 1215 for the near_flat
/// fixture) exceeds `total` (1024).
///
/// A self-roundtrip encoder **cannot** use this form (the encoder
/// would place symbols at indices the decoder can never address);
/// the pair-packed CDF exists solely to **decode foreign /
/// proprietary-encoded type-7 streams bit-faithfully**. The
/// cleanroom's own encoder uses the flat form + Strategy E
/// route-around (`encoder.rs`).
pub(crate) fn build_legacy_pair_packed_cdf(freq: &[u32; 256]) -> Result<(Vec<u32>, u32)> {
    let (new_freq, total) = rescale_freq(freq)?;

    // Step 5 (`spec/07` §3.1 + §3.4): pair-pack `(freq'[c], 1)` and
    // prefix-sum into a 513-entry CDF.
    let mut pair = vec![0u32; 513];
    let mut acc: u32 = 0;
    for c in 0..256 {
        pair[2 * c] = acc; // symbol c lower bound
        acc = acc.wrapping_add(new_freq[c]);
        pair[2 * c + 1] = acc; // symbol c upper bound
        acc = acc.wrapping_add(1); // sentinel-1 inter-symbol gap
    }
    pair[512] = acc; // == total + 256

    Ok((pair, total))
}

/// Detect the **rare-symbol-cluster signature** of audit/12 §7.1 in
/// a 256-entry histogram: ≥ 3 distinct nonzero bins each with
/// `freq ∈ {1, 2}` within a histogram dominated by
/// `freq[0] >= 0.95 * pixel_count`.
///
/// This is the signature audit/12 §3.6 / §5 identified as the
/// trigger for the flat-CDF / pair-packed-CDF wire-format
/// divergence: the proprietary's pair-packed 513-entry CDF and the
/// cleanroom's flat 257-entry CDF are *not* bit-equivalent for this
/// fixture class (the sentinel-`1` gaps shift rare symbols' bounds
/// past `total`). Wired into two consumers:
///
/// * [`crate::encoder::encode_legacy_rgb`] /
///   [`crate::encoder::encode_legacy_rgb_rle`] (round 6, encode side
///   Strategy E): when any of the three residual planes (B', G, R')
///   matches this signature, the encoder skips the type-7 emission
///   and falls through to a type-1 (uncompressed) frame, which is
///   byte-exact on every fixture per `audit/12 §7.1` Strategy E +
///   `audit/13 §3` cross-validation. A self-roundtrip pair-packed
///   encoder is impossible (the pair-pack span exceeds the divisor
///   `total`, so symbols would be placed at unaddressable indices),
///   so Strategy E is the only valid encode-side handling.
///
/// * [`crate::channel::decode_legacy_channel`] (round 96, decode side
///   Strategy F): when the transmitted freq table matches this
///   signature the decoder builds the proprietary's pair-packed
///   513-entry CDF ([`build_legacy_pair_packed_cdf`]) and decodes
///   against it via the `spec/07` §5.2 even-index descent, reproducing
///   the proprietary decode bit-faithfully (including its rare-symbol
///   mis-decode, audit/12 §3.6). Such a stream was not produced by
///   our encoder (Strategy E re-routes it), so it is foreign /
///   proprietary-encoded input.
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

/// Find the symbol `c` whose **pair-packed** lower bound
/// `pair_cdf[2c] <= target` is the greatest such, via binary search
/// over the even indices of the 513-entry CDF (`spec/07` §5.2's
/// even-index probes / audit/12 §7.1 Strategy F step 2).
///
/// The descent compares `target` against `pair_cdf[2·mid]` only — the
/// odd entries (`pair_cdf[2c+1]`, the per-symbol upper bounds) are
/// read by the caller for the §5.3 state update, not by the search.
/// Because the pair-packed lower bounds span `[0, total + 256)` while
/// `target` is capped at `total - 1` (`spec/07` §5.1), high-index
/// symbols whose lower bound `>= total` are unreachable — matching
/// the proprietary's documented rare-symbol mis-decode (audit/12 §5).
fn pair_cdf_find_symbol(pair_cdf: &[u32], target: u32) -> usize {
    debug_assert_eq!(pair_cdf.len(), 513);
    let mut lo = 0usize;
    let mut hi = 256usize;
    while lo + 1 < hi {
        let mid = (lo + hi) >> 1;
        if pair_cdf[2 * mid] <= target {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    lo
}

/// CDF layout the [`LegacyRangeDecoder`] addresses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum CdfLayout {
    /// Flat 257-entry CDF: symbol `c`'s bounds are `cdf[c]` /
    /// `cdf[c+1]`. The self-roundtrip form (`spec/07` §3.4 flat
    /// allowance).
    Flat,
    /// Pair-packed 513-entry CDF: symbol `c`'s bounds are
    /// `cdf[2c]` / `cdf[2c+1]`, with sentinel-`1` inter-symbol gaps
    /// (`spec/07` §3.1 + §3.4 audit-corrected). The form that
    /// bit-faithfully reproduces the proprietary decode (audit/12
    /// §7.1 Strategy F).
    PairPacked,
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
    layout: CdfLayout,
}

impl<'a> LegacyRangeDecoder<'a> {
    /// Init from the body's 4 priming bytes per `spec/07` §4.2 +
    /// audit-corrected §9.1 item 3 (the "init + 3 refills" sequence
    /// is equivalent to a 4-byte priming
    /// `low = (b1<<23) | (b2<<15) | (b3<<7) | (b4>>1)` followed by
    /// `range = 0x80000000`).
    ///
    /// Uses the **flat 257-entry CDF** (self-roundtrip form). For the
    /// proprietary's pair-packed 513-entry CDF use
    /// [`LegacyRangeDecoder::new_pair_packed`].
    pub fn new(body: &'a [u8], cdf: Vec<u32>, total: u32) -> Result<Self> {
        Self::new_with_layout(body, cdf, total, CdfLayout::Flat)
    }

    /// Init a decoder that addresses the proprietary's **pair-packed
    /// 513-entry CDF** (`spec/07` §3.1 + §5.2 even-index probes;
    /// audit/12 §7.1 Strategy F). `cdf` must come from
    /// [`build_legacy_pair_packed_cdf`]; `total` is the same
    /// power-of-two divisor.
    ///
    /// This path bit-faithfully reproduces the proprietary decoder's
    /// behaviour on foreign / proprietary-encoded type-7 streams,
    /// **including** its rare-symbol mis-decode (audit/12 §3.6): a
    /// symbol whose pair-packed lower bound exceeds `total` is
    /// unreachable, so the binary descent lands on the nearest
    /// reachable symbol instead.
    pub fn new_pair_packed(body: &'a [u8], pair_cdf: Vec<u32>, total: u32) -> Result<Self> {
        Self::new_with_layout(body, pair_cdf, total, CdfLayout::PairPacked)
    }

    fn new_with_layout(
        body: &'a [u8],
        cdf: Vec<u32>,
        total: u32,
        layout: CdfLayout,
    ) -> Result<Self> {
        if body.len() < 4 {
            return Err(Error::Truncated {
                context: "legacy range-coder priming bytes",
            });
        }
        if total == 0 || (total & (total - 1)) != 0 {
            return Err(Error::EmptyProbabilityTable);
        }
        match layout {
            CdfLayout::Flat => debug_assert_eq!(cdf.len(), 257),
            CdfLayout::PairPacked => debug_assert_eq!(cdf.len(), 513),
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
            layout,
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

    /// Decode one byte symbol per `spec/07` §5. Dispatches on the
    /// CDF layout chosen at construction.
    pub fn decode_byte(&mut self) -> Result<u8> {
        let q = self.range >> self.shift;
        if q == 0 {
            return Err(Error::EmptyProbabilityTable);
        }
        let mut symbol_index = self.low / q;
        if symbol_index >= self.total {
            symbol_index = self.total - 1;
        }
        // §5.2 binary descent + §5.3 bound lookup; the only layout-
        // dependent piece is which entries hold the chosen symbol's
        // lower / upper bounds.
        let (c, cdf_low, cdf_high) = match self.layout {
            CdfLayout::Flat => {
                let c = cdf_find_symbol(&self.cdf, symbol_index);
                (c, self.cdf[c], self.cdf[c + 1])
            }
            CdfLayout::PairPacked => {
                let c = pair_cdf_find_symbol(&self.cdf, symbol_index);
                (c, self.cdf[2 * c], self.cdf[2 * c + 1])
            }
        };
        self.low = self.low.wrapping_sub(cdf_low.wrapping_mul(q));
        self.range = if cdf_high == self.total {
            // Last-symbol fast path per `spec/07` §5.3.
            self.range.wrapping_sub(cdf_low.wrapping_mul(q))
        } else {
            (cdf_high.wrapping_sub(cdf_low)).wrapping_mul(q)
        };
        self.renormalise();
        Ok(c as u8)
    }
}

// ──────────────────── range-coder encoder ────────────────────

/// Legacy range-coder encoder (`spec/07` §6.4). Structurally
/// identical to the modern Subbotin encoder; only the wrapper
/// (channel prefix layout) differs. Encode-direction primitive
/// (consumed by [`crate::encoder`]'s legacy type-7 path, which is
/// reachable from the test suite and `encode_frame` is not routed to
/// it — hence the `not(test)` dead-code allowance).
#[cfg_attr(not(test), allow(dead_code))]
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

#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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
#[cfg_attr(not(test), allow(dead_code))]
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

    // ──────────── pair-packed 513-entry CDF (Strategy F) ────────────

    #[test]
    fn pair_packed_cdf_layout_and_total() {
        // Pair-pack interleaves (freq'[c], 1); the prefix sum produces
        // even-index lower bounds and odd-index upper bounds, with the
        // full span equal to total + 256 sentinels.
        let mut freq = [0u32; 256];
        freq[0] = 100;
        freq[1] = 50;
        freq[2] = 25;
        freq[10] = 10;
        let (flat, total) = build_legacy_cdf(&freq).unwrap();
        let (pair, ptotal) = build_legacy_pair_packed_cdf(&freq).unwrap();
        assert_eq!(pair.len(), 513);
        assert_eq!(ptotal, total, "divisor total identical to flat form");
        // Even-index lower bound of symbol c = flat[c] + c sentinels.
        for c in 0..256 {
            assert_eq!(
                pair[2 * c],
                flat[c] + c as u32,
                "pair lower bound of symbol {c} = flat[{c}] + {c} sentinels",
            );
            // Upper bound = lower + freq'[c].
            let freq_c = flat[c + 1] - flat[c];
            assert_eq!(pair[2 * c + 1], pair[2 * c] + freq_c);
        }
        assert_eq!(pair[512], total + 256);
    }

    #[test]
    fn pair_packed_decode_is_length_correct_and_avoids_unreachable() {
        // Decode the audit/12 §5 near_flat freq table against the
        // pair-packed CDF. The high rare symbols (0x40 at pair-lower
        // 1085, 0xc0 at 1215) exceed total=1024 and so are unreachable
        // by the §5.1 capped symbol_index — the decoder must never
        // emit them, matching the proprietary's mis-decode (audit/12
        // §3.6 — 0xc0 → 0xff).
        let mut freq = [0u32; 256];
        freq[0] = 887;
        freq[0x3d] = 1;
        freq[0x40] = 2;
        freq[0xc0] = 1;
        let (pair, total) = build_legacy_pair_packed_cdf(&freq).unwrap();
        assert_eq!(total, 1024);
        assert!(pair[2 * 0x40] >= total);
        assert!(pair[2 * 0xc0] >= total);

        // A dummy body; the range coder reads bytes via its cursor and
        // pads with zero past the end (no truncation).
        let body: Vec<u8> = (0..64u8).map(|i| i.wrapping_mul(37) ^ 0x5a).collect();
        let mut dec = LegacyRangeDecoder::new_pair_packed(&body, pair, total).unwrap();
        let mut out = Vec::with_capacity(50);
        for _ in 0..50 {
            out.push(dec.decode_byte().unwrap());
        }
        assert_eq!(out.len(), 50);
        assert!(
            !out.contains(&0x40) && !out.contains(&0xc0),
            "unreachable pair-packed symbols decoded: {out:?}",
        );
    }

    #[test]
    fn pair_packed_decode_deterministic() {
        // Two decodes of the same body + CDF must agree (the pair-
        // packed path has no hidden state).
        let mut freq = [0u32; 256];
        freq[0] = 900;
        freq[5] = 2;
        freq[9] = 1;
        freq[200] = 1;
        let (pair, total) = build_legacy_pair_packed_cdf(&freq).unwrap();
        let body: Vec<u8> = (0..80u8).map(|i| i.wrapping_mul(91)).collect();
        let mut a = LegacyRangeDecoder::new_pair_packed(&body, pair.clone(), total).unwrap();
        let mut b = LegacyRangeDecoder::new_pair_packed(&body, pair, total).unwrap();
        for _ in 0..40 {
            assert_eq!(a.decode_byte().unwrap(), b.decode_byte().unwrap());
        }
    }
}
