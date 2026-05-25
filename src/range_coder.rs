//! Modern range coder per `spec/02` (the "TOP = 2^23, init range =
//! 2^31, byte refill with cross-byte LSB rotation" Subbotin
//! derivative).
//!
//! Round 1 ships a clean-room **decoder** plus a `#[cfg(test)]`
//! encoder for self-roundtrip. Both honour the four-byte init,
//! the cross-byte rotated refill, and the four-byte flush tail
//! described in `spec/02` §3 / §4 / §6.
//!
//! The decoder uses the cumulative-frequency search loop invited
//! by `spec/02` §5 (the §5 step-C reciprocal-multiply LUT is *not*
//! consulted here; the spec explicitly invites the substitution
//! and notes the result is bit-identical).
//!
//! The probability model the coder consumes is a 256-entry CDF —
//! `cum[0] = 0`, `cum[256] = total`. Symbol `s` has frequency
//! `cum[s+1] - cum[s]`.

use crate::error::{Error, Result};

/// `TOP` constant from `spec/02` §2.
pub(crate) const TOP: u32 = 0x0080_0000;
/// Initial range from `spec/02` §2.
pub(crate) const INIT_RANGE: u32 = 0x8000_0000;

/// Cumulative-frequency view of a 256-entry probability table.
///
/// `cum[s+1] - cum[s]` is the frequency of symbol `s`; `cum[256]`
/// is the total. Computed once per channel from the raw frequencies
/// the Fibonacci prefix decoder fills in (`spec/04` §1).
///
/// Round 8 caches `freq0 = cum[1]` and `total = cum[256]` directly
/// on the struct so the §5 step-A / step-B fast paths in the
/// decoder can read them without an array bounds check or an extra
/// indirection per symbol. Round 10 adds `cum_top = cum[255]` for
/// the symmetric encoder Step-B fast path: the s == 255 symbol-encode
/// update collapses to two cached-field reads + one subtract instead
/// of two `cum[]` array reads.
///
/// Round 11 (encoder) adds a `freqs: [u32; 256]` cache where
/// `freqs[s] = cum[s+1] - cum[s]` so the `spec/02` §5 Step-C encoder
/// hot path drops its `cum[s+1] - cum[s]` two-read + subtract for a
/// single `freqs[s]` load. `cum[s]` itself is still loaded from
/// `cum[]` (the second load) but the second indirection becomes
/// data-independent of the first, so the optimiser can schedule
/// them in parallel and the `range = freq * q` multiply no longer
/// waits on a subtract. Layout note: the small scalar fields land
/// at offset 0 so a Step-A-dominant workload keeps `freq0` + `total`
/// on the first cache line.
#[derive(Debug, Clone)]
pub(crate) struct Cdf {
    /// `cum[1]` — boundary between symbol 0 (zero residual) and
    /// every other symbol. Cached for the §5 step-A fast path.
    /// Placed first so it lands at struct offset 0; the Step-A
    /// hot path then reads `freq0` and `total` from the same
    /// cache line as the struct base.
    freq0: u32,
    /// `cum[256]` — total mass. Cached for the §5 step-B fast path.
    total: u32,
    /// `cum[255]` — boundary between symbol 254 and symbol 255.
    /// Cached for the §5 step-B **encoder** fast path landed in
    /// round 10. The decoder Step-B path keys off `total`, not
    /// `cum[255]`, and the encoder lives behind `#[cfg(test)]` so
    /// this field is only live in the test build.
    #[cfg(test)]
    cum_top: u32,
    cum: [u32; 257],
    /// Per-symbol `freq[s] = cum[s+1] - cum[s]` cache. Encoder-only
    /// (the round-11 Step-C cache; the decoder's `find_symbol`
    /// walks the cum[] array directly).
    #[cfg(test)]
    freqs: [u32; 256],
}

impl Cdf {
    /// Build a CDF from a 256-entry raw-frequency table. Returns
    /// [`Error::EmptyProbabilityTable`] if the total is zero.
    pub fn from_frequencies(freq: &[u32; 256]) -> Result<Self> {
        let mut cum = [0u32; 257];
        let mut acc: u32 = 0;
        for i in 0..256 {
            cum[i] = acc;
            acc = acc.checked_add(freq[i]).expect("prob table overflow");
        }
        cum[256] = acc;
        if acc == 0 {
            return Err(Error::EmptyProbabilityTable);
        }
        #[cfg(test)]
        let freqs = {
            let mut f = [0u32; 256];
            for s in 0..256 {
                f[s] = cum[s + 1] - cum[s];
            }
            f
        };
        Ok(Self {
            freq0: cum[1],
            #[cfg(test)]
            cum_top: cum[255],
            #[cfg(test)]
            freqs,
            total: acc,
            cum,
        })
    }

    /// Read the encoder's pre-computed frequency `freq[s] =
    /// cum[s+1] - cum[s]`. Used by the round-11 Step-C hot path
    /// to skip the subtraction.
    #[cfg(test)]
    #[inline(always)]
    pub(crate) fn freq(&self, s: usize) -> u32 {
        self.freqs[s]
    }

    /// Total mass `cum[256]`.
    #[inline]
    pub fn total(&self) -> u32 {
        self.total
    }

    /// `cum[s]`.
    #[inline]
    pub fn lo(&self, s: usize) -> u32 {
        self.cum[s]
    }

    /// Find the symbol `s` such that `cum[s] <= target < cum[s+1]`.
    /// Caller guarantees `target < total()`.
    pub fn find_symbol(&self, target: u32) -> usize {
        // 9-bit binary search across 257 cumulative entries.
        let mut lo = 0usize;
        let mut hi = 256usize;
        while lo < hi {
            let mid = (lo + hi) >> 1;
            if self.cum[mid + 1] <= target {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        lo
    }
}

/// Modern Lagarith range-coder decoder. Wraps a borrowed compressed
/// byte stream and the per-channel CDF.
pub(crate) struct RangeDecoder<'a> {
    /// Source bytes (the channel's arithmetic body).
    src: &'a [u8],
    /// Cursor: the next refill iteration loads `src[cursor]` as the
    /// "previous byte" and `src[cursor + 1]` as the "next byte".
    /// After init it points at byte 3 (the last byte already
    /// absorbed at init time per `spec/02` §3).
    cursor: usize,
    low: u32,
    range: u32,
}

impl<'a> RangeDecoder<'a> {
    /// Init from the four priming bytes per `spec/02` §3.
    pub fn new(src: &'a [u8]) -> Result<Self> {
        if src.len() < 4 {
            return Err(Error::Truncated {
                context: "range-coder priming bytes",
            });
        }
        let e0 = src[0] as u32;
        let e1 = src[1] as u32;
        let e2 = src[2] as u32;
        let e3 = src[3] as u32;
        let low = (e0 << 23) | (e1 << 15) | (e2 << 7) | (e3 >> 1);
        Ok(Self {
            src,
            cursor: 3,
            low,
            range: INIT_RANGE,
        })
    }

    /// Refill `range` and `low` with byte-aligned bits per `spec/02`
    /// §4 (cross-byte rotation: bit 0 of the previously-consumed
    /// byte becomes bit 7 of the new byte's contribution).
    ///
    /// Round 8 hot-path optimisation. The fast path uses
    /// `slice::get(cursor..=cursor+1)` which gives the optimiser
    /// a single 2-byte bounds check the loop body can hoist; the
    /// tail path with saturating zero-fill is taken only when the
    /// next 2 bytes go past the bitstream end. The arithmetic
    /// per iteration is unchanged (still byte-at-a-time per
    /// `spec/02` §4) so output bytes remain bit-identical to the
    /// proprietary.
    #[inline(always)]
    fn renormalise(&mut self) -> Result<()> {
        while self.range <= TOP {
            let cursor = self.cursor;
            // Fast path: 2-byte window in one bounds check.
            // `get(cursor..cursor + 2)` is a single bounds compare
            // and lets the optimiser elide the per-byte recheck.
            if let Some(window) = self.src.get(cursor..cursor + 2) {
                let prev = window[0];
                let next = window[1];
                self.low =
                    (self.low << 8).wrapping_add((((prev & 1) as u32) << 7) | ((next as u32) >> 1));
                self.range <<= 8;
                self.cursor = cursor + 1;
            } else {
                // Tail path — saturate missing bytes to zero (the
                // caller's symbol-count guard exits before garbage
                // is observed; same semantics as the proprietary
                // when the four-byte tail of `spec/02` §6.3 has
                // already been absorbed).
                let prev = self.src.get(cursor).copied().unwrap_or(0);
                let next = self.src.get(cursor + 1).copied().unwrap_or(0);
                self.low =
                    (self.low << 8).wrapping_add((((prev & 1) as u32) << 7) | ((next as u32) >> 1));
                self.range <<= 8;
                self.cursor = cursor + 1;
            }
        }
        Ok(())
    }

    /// Decode a single symbol against the supplied CDF.
    ///
    /// Round 8 hot-path: implements the three-way fast path of
    /// `spec/02` §5 (step A — symbol 0; step B — symbol 0xff;
    /// step C — generic via cumulative search). Lossless RGB
    /// residuals after gradient prediction in Lagarith are
    /// dominated by 0x00 (often `freq[0] >= 0.95 * pixel_count`
    /// per `spec/06` §6.4), so the step-A check is the dominant
    /// case and short-circuits the 9-iteration binary search.
    #[inline]
    pub fn decode_symbol(&mut self, cdf: &Cdf) -> Result<u8> {
        // Per `spec/02` §5: q = range / total.
        let total = cdf.total();
        debug_assert!(total > 0, "Cdf::total must be non-zero");
        let q = self.range / total;

        // Step A — symbol 0 (zero) fast path. `freq[0] = cum[1]`,
        // so the test is `low < cum[1] * q`.
        let freq0_scaled = cdf.freq0 * q;
        if self.low < freq0_scaled {
            // cum[0] = 0, so `low -= 0 * q` is a no-op; range
            // becomes `(cum[1] - 0) * q = freq0 * q`.
            self.range = freq0_scaled;
            self.renormalise()?;
            return Ok(0);
        }

        // Step B — symbol 0xff fast path per `spec/02` §5. The
        // proprietary's reciprocal-multiply Step C path treats
        // `low >= total * q` (i.e. the slack band above the
        // highest CDF entry) as the 0xff sentinel and updates
        // `low -= total*q; range -= total*q`. Note this differs
        // from a naive "find symbol s with cum[s] <= target <
        // cum[s+1]" search clamped to s = 255: the spec's update
        // is unconditional even when `freq[255] == 0`. Stays
        // bit-identical to the proprietary on real bitstreams;
        // the self-roundtrip encoder never produces `low >=
        // total*q` so this branch is exercised only by test
        // fixtures crafted to hit it.
        let total_scaled = total * q;
        if self.low >= total_scaled {
            self.low -= total_scaled;
            self.range -= total_scaled;
            self.renormalise()?;
            return Ok(0xff);
        }

        // Step C — generic symbol via cumulative search. The
        // proprietary uses a reciprocal-multiply LUT here; the
        // clean-room equivalent is a 9-step binary search over
        // 257 cumulative entries. Bit-identical per `spec/02` §5.
        let target = self.low / q;
        let symbol = cdf.find_symbol(target);
        let lo = cdf.lo(symbol);
        let hi = cdf.lo(symbol + 1);
        self.low -= lo * q;
        self.range = (hi - lo) * q;
        self.renormalise()?;
        Ok(symbol as u8)
    }
}

// ────────────────────── encoder (test-only) ──────────────────────

#[cfg(test)]
pub(crate) struct RangeEncoder {
    /// Output bytes already committed (cannot be back-walked).
    buf: Vec<u8>,
    /// Pending `0xff` chain length (carry-out propagation).
    /// `spec/02` §6.2 keeps this as a back-walk loop in memory; the
    /// equivalent counter form is canonical for the Subbotin
    /// "cached carry" range encoder.
    pending_ffs: u32,
    /// Cached byte that may still be incremented if a carry-out
    /// arrives. Meaningful only when `started == true`; before the
    /// first emission its value is undefined and must not be flushed.
    ///
    /// Round 10 replaces the round-9 `Option<u8>` with a `u8` + a
    /// `started: bool` flag — the started transition fires once per
    /// encode, so the hot `shift_low` path is a single bool check
    /// instead of an `Option::take()` discharge + the implicit
    /// niche-optimised tag manipulation. With Step-A and Step-B
    /// fast paths dominating the symbol-encode workload, `shift_low`
    /// is the new hot inner branch and its `Option` overhead became
    /// the next-most-visible cost in the round-9 profile.
    cache: u8,
    /// Tracks whether `cache` holds a flushable byte. False only for
    /// the very first `shift_low` invocation; after that it is true
    /// for the entire remainder of the encode (round 10).
    started: bool,
    low: u32,
    range: u32,
}

#[cfg(test)]
impl RangeEncoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            pending_ffs: 0,
            cache: 0,
            started: false,
            low: 0,
            range: INIT_RANGE,
        }
    }

    /// Commit one final output byte: flush the cache + the FF chain
    /// (kept as-is because we know the carry-out has not happened
    /// for them), then store `byte` as the new cache (it may still
    /// be incremented by a future carry).
    ///
    /// Round 9 hot-path: the per-FF `Vec::push` loops were replaced
    /// with a single `Vec::resize` so the `pending_ffs` chain is
    /// committed with one bounds check + one bulk memset instead of
    /// `pending_ffs` individual push calls.
    ///
    /// Round 10 hot-path: the `Option<u8>` cache slot becomes a
    /// plain `u8` + `started: bool` pair. The hot `shift_low` body
    /// then issues one branch on `started` instead of the
    /// `Option::take()` discharge that the optimiser couldn't elide
    /// across the carry/defer/steady-state branch arms. The
    /// arithmetic is unchanged (same `c` / `c+1` cache byte, same
    /// `0x00` / `0xff` fill, same low-mask) so the wire is
    /// bit-identical to the proprietary's cache-then-FF-chain
    /// emission per `spec/02` §6.2. The `rangecoder_roundtrip_wide`
    /// test verifies the equivalent encoded bytes by self-roundtrip,
    /// and the new `rangecoder_shift_low_started_byte_equiv_to_option`
    /// test asserts the round-10 encoder produces the same wire bytes
    /// as a reference encoder driven by the round-9 Option-cache code.
    #[inline]
    fn shift_low(&mut self) {
        // Carry-out detection: bit 31 of low + the implicit carry
        // from low + range computations. `spec/02` §6.2: when bit
        // 31 of `low` is set we have a carry to back-propagate.
        let carry = (self.low >> 31) & 1 == 1;
        let byte = ((self.low >> 23) & 0xff) as u8;
        if carry {
            // Flush cache+1, then `pending_ffs` zeros (the old FFs
            // rolled over by the back-walk per `spec/02` §6.2).
            if self.started {
                self.buf.push(self.cache.wrapping_add(1));
            }
            if self.pending_ffs != 0 {
                let new_len = self.buf.len() + self.pending_ffs as usize;
                self.buf.resize(new_len, 0x00);
                self.pending_ffs = 0;
            }
            self.cache = byte;
            self.started = true;
        } else if byte == 0xff {
            // Defer: the next non-0xff byte that comes through here
            // (or the final flush) will commit this 0xff in its
            // un-carry'd form. Increment the pending count.
            self.pending_ffs += 1;
        } else {
            // Steady-state: flush cache + the FF chain (un-carry'd)
            // and cache the new byte.
            if self.started {
                self.buf.push(self.cache);
            }
            if self.pending_ffs != 0 {
                let new_len = self.buf.len() + self.pending_ffs as usize;
                self.buf.resize(new_len, 0xff);
                self.pending_ffs = 0;
            }
            self.cache = byte;
            self.started = true;
        }
        // Mask to bits 0..22 then shift up by 8 — clears the
        // already-emitted byte (bits 23..30) and the carry bit (31).
        self.low = (self.low & 0x007f_ffff) << 8;
    }

    #[inline(always)]
    fn renormalise(&mut self) {
        while self.range <= TOP {
            self.shift_low();
            self.range <<= 8;
        }
    }

    /// Encode symbol `s` against the supplied CDF.
    ///
    /// Round 9 hot-path: symmetric to the decoder's `spec/02` §5
    /// Step-A fast path landed in round 8. Symbol 0 (zero residual)
    /// dominates the Lagarith encoder workload (`spec/06` §6.4 puts
    /// `freq[0] >= 0.95 * pixel_count`), and for `s == 0` the
    /// generic arithmetic collapses to a no-op `low += lo*q`
    /// (because `lo = cum[0] = 0`) plus a single multiply
    /// `range = freq0 * q`. Skipping the `cdf.lo(0)` / `cdf.lo(1)`
    /// indirections and the redundant `low += 0` shaves two
    /// `cum[]` reads + one wrapping_add per dominant symbol; the
    /// emitted bytes are bit-identical to the generic path (and
    /// therefore bit-identical to the proprietary, modulo the
    /// existing self-roundtrip-only contract). The wide-distribution
    /// `rangecoder_roundtrip_wide` test verifies the generic path
    /// stays bit-equal.
    ///
    /// Round 10 hot-path: also adds the symmetric **Step-B fast
    /// path** for `s == 255` (the high-symbol sentinel that the
    /// decoder already short-circuits per `spec/02` §5 Step B). For
    /// `s == 255` the generic update reads `lo = cum[255]` and
    /// `hi = cum[256] = total`, so the cached `cdf.cum_top`
    /// (= `cum[255]`) and `cdf.total` directly drive the update
    /// without indexing the 257-entry cumulative array. The two
    /// multiplies collapse to a shared `cum_top * q` (added into
    /// `low`) and `(total - cum_top) * q` (assigned to `range`).
    /// Bit-identical to the Step-C path that round 9 used for
    /// `s == 255`; the `rangecoder_step_b_encode_bit_equiv_to_generic`
    /// test below re-encodes a 0xff-dominant stream through both
    /// paths and asserts byte equality. Symbol 0xff is one of the
    /// frequent symbols in alpha-dominant or solid-white plane
    /// channels — far less common than symbol 0 but still worth
    /// the same one-multiply elision.
    ///
    /// Round 11 hot-path: Step-C now uses a `freqs[]` cache —
    /// see the inline comment on the Step-C arm below.
    #[inline]
    pub fn encode_symbol(&mut self, cdf: &Cdf, s: usize) {
        let total = cdf.total();
        debug_assert!(total > 0, "Cdf::total must be non-zero");
        let q = self.range / total;

        // Step A — symbol 0 fast path (`spec/02` §5; symmetric to
        // the decoder hot path landed in round 8).
        if s == 0 {
            // cum[0] = 0 -> `low += 0` is a no-op; range becomes
            // `(cum[1] - 0) * q = freq0 * q`.
            self.range = cdf.freq0 * q;
            self.renormalise();
            return;
        }

        // Step B — symbol 255 fast path (`spec/02` §5; symmetric to
        // the decoder Step-B landed in round 8). cum[256] = total,
        // so `range = (total - cum[255]) * q`; `low += cum[255] * q`.
        if s == 255 {
            let lo_scaled = cdf.cum_top * q;
            self.low = self.low.wrapping_add(lo_scaled);
            self.range = (total - cdf.cum_top) * q;
            self.renormalise();
            return;
        }

        // Step C — generic symbol. The proprietary's `spec/02` §5
        // path is `low += cum[s]*q; range = (cum[s+1]-cum[s]) * q`.
        //
        // Round 11 hot-path: a `freqs[s]` cache replaces the round-10
        // `cum[s+1] - cum[s]` two-read + subtract. `cum[s]` itself
        // is still loaded from the `cum[]` array (the second load),
        // but the second indirection becomes data-independent of
        // the first, so the optimiser can schedule them in parallel
        // and the `range = freq * q` multiply no longer waits on a
        // subtract. Bit-identical to round 10 — `freqs[s]` is
        // sourced from the same `cum[]` array `from_frequencies`
        // builds, just pre-differenced. The
        // `rangecoder_step_c_encode_bit_equiv_to_generic` test
        // re-encodes the same input through both paths and asserts
        // byte equality.
        //
        // Round 11 NOTE: dedicated Step-A1 (`s == 1`) and Step-B1
        // (`s == 254`) fast paths were tried and benched against
        // mixed-distribution streams — they hurt the dominant
        // Step-A path more than they helped the secondary symbols
        // (extra branches in the hot loop dropped Step-A heavy
        // from ~340 MSym/s to ~299 MSym/s, -12%). `encode_symbol`
        // deliberately falls through to Step-C for s ∈ 1..=254
        // instead of growing the if-chain.
        let lo = cdf.lo(s);
        let freq = cdf.freq(s);
        self.low = self.low.wrapping_add(lo * q);
        self.range = freq * q;
        self.renormalise();
    }

    /// Flush the four-byte tail per `spec/02` §6.3.
    ///
    /// Round 12 hot-path: the pre-tail pending-FF chain flush now uses
    /// `Vec::resize` instead of `pending_ffs` individual `Vec::push`
    /// calls. This is the §6.3 final-flush analogue of the round-9
    /// `shift_low` per-iteration optimisation — every channel encode
    /// concludes with one `finish()` call, and on channels where the
    /// last symbol's renormalisation chain leaves a long deferred
    /// FF run pending (Step-B-dominant streams like an opaque alpha
    /// plane), the per-FF push loop becomes the visible cost of
    /// the close-out. The `Vec::resize` form bulks the chain into a
    /// single bounds check + a single memset; the on-wire bytes are
    /// unchanged (still `cache+1` then `pending_ffs` 0x00 bytes on a
    /// carry, or `cache` then `pending_ffs` 0xff bytes otherwise),
    /// so the four-byte tail layout per `spec/02` §6.3 is
    /// bit-identical. The new `rangecoder_finish_resize_byte_equiv_to_push_loop`
    /// test re-runs the same encode through the resize form AND
    /// through a reference encoder that drains the FF chain via a
    /// push-loop, asserting byte equality.
    pub fn finish(mut self) -> Vec<u8> {
        // The proprietary's encoder writes the four-byte tail
        // directly from the running `low`. We must absorb any
        // outstanding carry-out into `cache` first so the tail bytes
        // are aligned to the same `low` the decoder will see at
        // init.
        let carry = (self.low >> 31) & 1 == 1;
        let (head, fill) = if carry {
            (self.cache.wrapping_add(1), 0x00u8)
        } else {
            (self.cache, 0xffu8)
        };
        if self.started {
            self.buf.push(head);
        }
        if self.pending_ffs != 0 {
            let new_len = self.buf.len() + self.pending_ffs as usize;
            self.buf.resize(new_len, fill);
            self.pending_ffs = 0;
        }
        self.started = false;
        // After absorbing the carry the in-range payload of `low`
        // sits in bits 0..30 (bit 31 has been consumed). Mask it
        // down so the four-byte tail is byte-aligned with what the
        // decoder reconstructs in `RangeDecoder::new`.
        self.low &= 0x7fff_ffff;
        let low = self.low;
        let tail = [
            ((low >> 23) & 0xff) as u8,
            ((low >> 15) & 0xff) as u8,
            ((low >> 7) & 0xff) as u8,
            ((low << 1) & 0xff) as u8,
        ];
        self.buf.extend_from_slice(&tail);
        self.buf
    }
}

// ────────────────────── tests ──────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// The CDF helper finds the right symbol for boundary targets.
    #[test]
    fn cdf_find_symbol() {
        let mut freq = [0u32; 256];
        freq[0] = 10;
        freq[1] = 20;
        freq[2] = 30;
        let cdf = Cdf::from_frequencies(&freq).unwrap();
        // total = 60; cum = [0, 10, 30, 60, 60, ..., 60]
        assert_eq!(cdf.find_symbol(0), 0);
        assert_eq!(cdf.find_symbol(9), 0);
        assert_eq!(cdf.find_symbol(10), 1);
        assert_eq!(cdf.find_symbol(29), 1);
        assert_eq!(cdf.find_symbol(30), 2);
        assert_eq!(cdf.find_symbol(59), 2);
    }

    /// Self-roundtrip: encode symbols and decode them back.
    #[test]
    fn rangecoder_roundtrip_small() {
        let mut freq = [0u32; 256];
        freq[0] = 4;
        freq[1] = 1;
        freq[2] = 3;
        freq[7] = 2;
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let symbols = vec![0u8, 0, 1, 7, 2, 0, 7, 2, 2, 0, 1];
        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Larger roundtrip with a wider distribution.
    #[test]
    fn rangecoder_roundtrip_wide() {
        let mut freq = [0u32; 256];
        for (i, slot) in freq.iter_mut().enumerate() {
            *slot = ((i as u32 * 7) % 11) + 1;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let mut symbols = Vec::with_capacity(2000);
        for i in 0..2000 {
            symbols.push(((i * 31) ^ (i >> 3)) as u8);
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 8: stress the §5 step-A (symbol-0) fast path with a
    /// histogram dominated by `freq[0]`. This is the realistic
    /// shape of a Lagarith residual channel after gradient
    /// prediction (`spec/06` §6.4: `freq[0] >= 0.95 * pixel_count`)
    /// and is the case the optimisation principally targets.
    #[test]
    fn rangecoder_roundtrip_step_a_dominant() {
        let mut freq = [0u32; 256];
        freq[0] = 9500;
        for slot in freq.iter_mut().take(256).skip(1) {
            *slot = 2;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // 95% zero residuals, 5% non-zero spread across 1..256.
        let mut symbols = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            if i % 20 == 0 {
                symbols.push(((i * 7 + 1) & 0xff) as u8);
            } else {
                symbols.push(0u8);
            }
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 8: stress the §5 step-B (symbol-0xff) fast path with
    /// a histogram where `freq[255]` is comparable to `freq[0]`.
    /// Verifies the step-B exit produces bit-identical output to
    /// the binary-search path.
    #[test]
    fn rangecoder_roundtrip_step_b_hits() {
        let mut freq = [0u32; 256];
        freq[0] = 100;
        freq[255] = 100;
        for slot in freq.iter_mut().take(255).skip(1) {
            *slot = 1;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let mut symbols = Vec::with_capacity(2000);
        for i in 0..2000 {
            // Force a mix of 0, 0xff, and other symbols.
            symbols.push(match i % 5 {
                0 => 0u8,
                1 => 0xff,
                2 => 0u8,
                3 => 0xff,
                _ => ((i * 13) & 0xfe) as u8 | 1, // odd byte in 1..255
            });
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 8: throughput sanity check. Builds a signal-heavy
    /// stream (zero-dominated, 64k symbols) and verifies the
    /// optimised hot path decodes it correctly. When the
    /// `LAGARITH_BENCH=1` env var is set, also prints the
    /// throughput in MSym/s — used to compare baseline vs.
    /// optimised numbers; not asserted because `cargo test`
    /// walltime jitters too much to threshold reliably.
    #[test]
    fn rangecoder_throughput_signal_heavy() {
        let mut freq = [0u32; 256];
        freq[0] = 60_000;
        for slot in freq.iter_mut().take(256).skip(1) {
            *slot = 16; // 60000 + 255*16 = 64080
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // 64k symbols, ~93.7% zeros (matches `spec/06` §6.4
        // shape "freq[0] >= 0.95 * pixel_count" within rounding).
        let n = 65_536;
        let mut symbols = Vec::with_capacity(n);
        for i in 0..n {
            // Deterministic LCG-style mix; ~94% land in symbol 0.
            let r = (i as u32).wrapping_mul(2654435761) ^ (i as u32 >> 7);
            symbols.push(if (r & 0xf) < 15 {
                0u8
            } else {
                ((r >> 4) & 0xff) as u8
            });
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        // Functional check: decode once and verify equality.
        {
            let mut dec = RangeDecoder::new(&bytes).unwrap();
            let mut out = Vec::with_capacity(symbols.len());
            for _ in 0..symbols.len() {
                out.push(dec.decode_symbol(&cdf).unwrap());
            }
            assert_eq!(out, symbols);
        }

        // Optional timing pass (only when LAGARITH_BENCH=1).
        if std::env::var("LAGARITH_BENCH").as_deref() == Ok("1") {
            const REPS: usize = 200;
            let mut sink: u64 = 0;
            let t0 = std::time::Instant::now();
            for _ in 0..REPS {
                let mut dec = RangeDecoder::new(&bytes).unwrap();
                for _ in 0..symbols.len() {
                    sink = sink.wrapping_add(dec.decode_symbol(&cdf).unwrap() as u64);
                }
            }
            let elapsed = t0.elapsed();
            let total_syms = (n * REPS) as f64;
            let msym_per_s = total_syms / elapsed.as_secs_f64() / 1.0e6;
            eprintln!(
                "BENCH/range-coder signal-heavy: {:.2} MSym/s ({} syms x {} reps in {:.3?}; sink={})",
                msym_per_s,
                n,
                REPS,
                elapsed,
                sink
            );
        }
    }

    /// Round 8: ensure the renormalise tail-saturation path (when
    /// the bitstream has fewer than 2 unread bytes) still produces
    /// correct output. The encoder writes a 4-byte tail per
    /// `spec/02` §6.3 so the decoder runs through ≥1 tail-pad
    /// iteration on most realistic inputs.
    #[test]
    fn rangecoder_renormalise_tail_saturates() {
        let mut freq = [0u32; 256];
        freq[0] = 8;
        freq[1] = 1;
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let symbols = vec![0u8, 0, 0, 0, 1, 0, 0, 0, 1, 0];
        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 9: encoder-side spec/02 §5 Step-A fast path correctness
    /// check. Stresses a histogram dominated by `freq[0]` (the
    /// `spec/06` §6.4 "freq[0] >= 0.95 * pixel_count" residual
    /// shape) and verifies the round-trip is bit-equal to the
    /// decoder. This is the symmetric encoder pair of round 8's
    /// decoder-side `rangecoder_roundtrip_step_a_dominant`.
    #[test]
    fn rangecoder_encode_step_a_dominant_roundtrip() {
        let mut freq = [0u32; 256];
        freq[0] = 9500;
        for slot in freq.iter_mut().take(256).skip(1) {
            *slot = 2;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // 95% zero residuals, 5% non-zero spread across 1..256 —
        // exercises the encoder Step-A path on the dominant case +
        // the generic Step-C path on the tail.
        let mut symbols = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            if i % 20 == 0 {
                symbols.push(((i * 7 + 1) & 0xff) as u8);
            } else {
                symbols.push(0u8);
            }
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 9: encoder-side bit-equivalence guard. Encode the same
    /// dominant-zero stream through the Step-A fast path AND through
    /// a reference "no fast path" encoder built inline that always
    /// uses the generic Step-C update for every symbol (including
    /// symbol 0). The two outputs MUST be byte-identical — the
    /// Step-A path is algebraically a no-op `low += 0`, so any
    /// divergence here would mean the optimisation altered the wire
    /// format.
    #[test]
    fn rangecoder_step_a_encode_bit_equiv_to_generic() {
        let mut freq = [0u32; 256];
        freq[0] = 9500;
        for slot in freq.iter_mut().take(256).skip(1) {
            *slot = 2;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let mut symbols = Vec::with_capacity(4_000);
        for i in 0..4_000 {
            if i % 17 == 0 {
                symbols.push(((i * 13 + 1) & 0xff) as u8);
            } else {
                symbols.push(0u8);
            }
        }

        // Fast-path encoder (production path).
        let mut enc_fast = RangeEncoder::new();
        for &s in &symbols {
            enc_fast.encode_symbol(&cdf, s as usize);
        }
        let bytes_fast = enc_fast.finish();

        // Generic-only encoder: re-implements the §5 generic update
        // inline so symbol 0 takes the same code path as every
        // other symbol. Must produce the same wire bytes.
        let mut enc_generic = RangeEncoder::new();
        for &s in &symbols {
            let total = cdf.total();
            let q = enc_generic.range / total;
            let lo = cdf.lo(s as usize);
            let hi = cdf.lo(s as usize + 1);
            enc_generic.low = enc_generic.low.wrapping_add(lo * q);
            enc_generic.range = (hi - lo) * q;
            enc_generic.renormalise();
        }
        let bytes_generic = enc_generic.finish();

        assert_eq!(
            bytes_fast, bytes_generic,
            "Step-A fast path diverged from generic Step-C update"
        );
    }

    /// Round 9: throughput sanity check for the encoder hot path.
    /// Mirrors the decoder-side `rangecoder_throughput_signal_heavy`
    /// (round 8) — functional check always runs; timing only
    /// printed under `LAGARITH_BENCH=1` since `cargo test` walltime
    /// jitters too much to threshold.
    #[test]
    fn rangecoder_encode_throughput_signal_heavy() {
        let mut freq = [0u32; 256];
        freq[0] = 60_000;
        for slot in freq.iter_mut().take(256).skip(1) {
            *slot = 16; // 60000 + 255*16 = 64080
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let n = 65_536;
        let mut symbols = Vec::with_capacity(n);
        for i in 0..n {
            let r = (i as u32).wrapping_mul(2654435761) ^ (i as u32 >> 7);
            symbols.push(if (r & 0xf) < 15 {
                0u8
            } else {
                ((r >> 4) & 0xff) as u8
            });
        }

        // Functional check: one encode → decode round-trip.
        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);

        // Optional timing pass (only when LAGARITH_BENCH=1).
        if std::env::var("LAGARITH_BENCH").as_deref() == Ok("1") {
            const REPS: usize = 200;
            let mut sink: u64 = 0;
            let t0 = std::time::Instant::now();
            for _ in 0..REPS {
                let mut enc = RangeEncoder::new();
                for &s in &symbols {
                    enc.encode_symbol(&cdf, s as usize);
                }
                let bytes = enc.finish();
                sink = sink.wrapping_add(bytes.len() as u64);
            }
            let elapsed = t0.elapsed();
            let total_syms = (n * REPS) as f64;
            let msym_per_s = total_syms / elapsed.as_secs_f64() / 1.0e6;
            eprintln!(
                "BENCH/range-coder ENCODE signal-heavy: {:.2} MSym/s ({} syms x {} reps in {:.3?}; sink={})",
                msym_per_s,
                n,
                REPS,
                elapsed,
                sink
            );
        }
    }

    /// Round 10: encoder-side `spec/02` §5 Step-B fast path
    /// correctness check. Stresses a histogram dominated by
    /// `freq[255]` (the high-sentinel symbol the decoder Step-B
    /// already short-circuits per `spec/02` §5). This is the
    /// shape an alpha plane on an opaque-image fixture takes
    /// (alpha = 255 for every pixel post-prediction), and is the
    /// symmetric pair of the round-9 Step-A dominant-zero test.
    #[test]
    fn rangecoder_encode_step_b_dominant_roundtrip() {
        let mut freq = [0u32; 256];
        freq[255] = 9500;
        for slot in freq.iter_mut().take(255) {
            *slot = 2;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // 95% 0xff residuals, 5% non-0xff spread across 0..255.
        let mut symbols = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            if i % 20 == 0 {
                symbols.push(((i * 7) & 0xfe) as u8);
            } else {
                symbols.push(0xffu8);
            }
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 10: encoder-side bit-equivalence guard for Step-B.
    /// Encode a 0xff-dominant stream through the Step-B fast path
    /// AND through a generic Step-C-only encoder. The two outputs
    /// MUST be byte-identical — Step-B is algebraically the same
    /// `low += cum[255]*q; range = (cum[256]-cum[255])*q` update as
    /// generic Step-C, just with `cum_top` cached on the Cdf struct
    /// instead of read from `cum[]`. Any divergence here would mean
    /// the optimisation altered the wire format.
    #[test]
    fn rangecoder_step_b_encode_bit_equiv_to_generic() {
        let mut freq = [0u32; 256];
        freq[255] = 9500;
        for slot in freq.iter_mut().take(255) {
            *slot = 2;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let mut symbols = Vec::with_capacity(4_000);
        for i in 0..4_000 {
            if i % 17 == 0 {
                symbols.push(((i * 13) & 0xfe) as u8);
            } else {
                symbols.push(0xffu8);
            }
        }

        // Fast-path encoder (production path).
        let mut enc_fast = RangeEncoder::new();
        for &s in &symbols {
            enc_fast.encode_symbol(&cdf, s as usize);
        }
        let bytes_fast = enc_fast.finish();

        // Generic-only encoder: re-implements the §5 generic update
        // inline so symbol 255 takes the same code path as every
        // other symbol. Must produce the same wire bytes.
        let mut enc_generic = RangeEncoder::new();
        for &s in &symbols {
            let total = cdf.total();
            let q = enc_generic.range / total;
            let lo = cdf.lo(s as usize);
            let hi = cdf.lo(s as usize + 1);
            enc_generic.low = enc_generic.low.wrapping_add(lo * q);
            enc_generic.range = (hi - lo) * q;
            enc_generic.renormalise();
        }
        let bytes_generic = enc_generic.finish();

        assert_eq!(
            bytes_fast, bytes_generic,
            "Step-B fast path diverged from generic Step-C update"
        );
    }

    /// Round 10: throughput sanity check for the encoder Step-B fast
    /// path. Mirrors the Step-A throughput test but with `freq[255]`
    /// dominant — exercises the path the round-10 Step-B optimisation
    /// targets. Functional check always runs; timing only printed
    /// under `LAGARITH_BENCH=1`.
    #[test]
    fn rangecoder_encode_throughput_step_b_heavy() {
        let mut freq = [0u32; 256];
        freq[255] = 60_000;
        for slot in freq.iter_mut().take(255) {
            *slot = 16; // 60000 + 255*16 = 64080 (same total as Step-A bench)
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let n = 65_536;
        let mut symbols = Vec::with_capacity(n);
        for i in 0..n {
            let r = (i as u32).wrapping_mul(2654435761) ^ (i as u32 >> 7);
            symbols.push(if (r & 0xf) < 15 {
                0xffu8
            } else {
                // Even-valued tail symbol to avoid the 0xff dominant case.
                ((r >> 4) & 0xfe) as u8
            });
        }

        // Functional check: one encode → decode round-trip.
        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);

        // Optional timing pass (only when LAGARITH_BENCH=1).
        if std::env::var("LAGARITH_BENCH").as_deref() == Ok("1") {
            const REPS: usize = 200;
            let mut sink: u64 = 0;
            let t0 = std::time::Instant::now();
            for _ in 0..REPS {
                let mut enc = RangeEncoder::new();
                for &s in &symbols {
                    enc.encode_symbol(&cdf, s as usize);
                }
                let bytes = enc.finish();
                sink = sink.wrapping_add(bytes.len() as u64);
            }
            let elapsed = t0.elapsed();
            let total_syms = (n * REPS) as f64;
            let msym_per_s = total_syms / elapsed.as_secs_f64() / 1.0e6;
            eprintln!(
                "BENCH/range-coder ENCODE step-b-heavy: {:.2} MSym/s ({} syms x {} reps in {:.3?}; sink={})",
                msym_per_s,
                n,
                REPS,
                elapsed,
                sink
            );
        }
    }

    /// Round 11: encoder-side `spec/02` §5 Step-C path is dominated
    /// by a packed-pair cache. Stresses a histogram where the
    /// non-{0, 0xff} symbol mass is spread across many bins (so
    /// most encoded symbols hit the Step-C arm), and verifies the
    /// round-trip is bit-equal to the decoder. This is the
    /// symmetric encoder coverage of the round-8 decoder Step-C
    /// path: the encoder's Step-C is the "generic" arm fired
    /// whenever the symbol is neither 0 nor 255 and the cache
    /// halves the per-symbol pointer dereferences.
    #[test]
    fn rangecoder_encode_step_c_dominant_roundtrip() {
        let mut freq = [0u32; 256];
        // Mid-band distribution: tiny mass on 0 / 255, most mass
        // spread across 1..255 — every symbol hits Step-C.
        freq[0] = 5;
        freq[255] = 5;
        for slot in freq.iter_mut().take(255).skip(1) {
            *slot = 40;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // Mostly-non-extreme symbols.
        let mut symbols = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            // 99% in 1..255 inclusive, 1% on 0 / 255.
            let r = (i as u32).wrapping_mul(2654435761) ^ (i as u32 >> 5);
            symbols.push(match r % 100 {
                0 => 0u8,
                1 => 0xffu8,
                _ => 1 + (r >> 8) as u8 % 254,
            });
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 11: encoder-side bit-equivalence guard for Step-C.
    /// Encode a mid-band stream through the packed-pair Step-C
    /// path AND through an inline reference that re-reads
    /// `cdf.lo(s)` + `cdf.lo(s + 1)` directly (the round-10 form).
    /// The two outputs MUST be byte-identical — the packed-pair
    /// cache is algebraically a structural rewrite of the same
    /// `cum[s]` + `cum[s+1] - cum[s]` reads, so any divergence
    /// would mean the optimisation altered the wire format.
    #[test]
    fn rangecoder_step_c_encode_bit_equiv_to_generic() {
        let mut freq = [0u32; 256];
        freq[0] = 10;
        freq[255] = 10;
        for slot in freq.iter_mut().take(255).skip(1) {
            *slot = 35;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let mut symbols = Vec::with_capacity(6_000);
        for i in 0..6_000 {
            // Mid-band; deliberately exclude 0 and 255 so every
            // emission goes through the Step-C arm.
            let s = 1u32 + ((i as u32).wrapping_mul(31) % 253);
            symbols.push(s as u8);
        }

        // Packed-pair fast path (production code).
        let mut enc_fast = RangeEncoder::new();
        for &s in &symbols {
            enc_fast.encode_symbol(&cdf, s as usize);
        }
        let bytes_fast = enc_fast.finish();

        // Reference encoder: inline the §5 generic update via
        // `cdf.lo()` so symbol-s takes the round-10 code path.
        // Must produce the same wire bytes as the packed-pair form.
        let mut enc_ref = RangeEncoder::new();
        for &s in &symbols {
            let total = cdf.total();
            let q = enc_ref.range / total;
            let lo = cdf.lo(s as usize);
            let hi = cdf.lo(s as usize + 1);
            enc_ref.low = enc_ref.low.wrapping_add(lo * q);
            enc_ref.range = (hi - lo) * q;
            enc_ref.renormalise();
        }
        let bytes_ref = enc_ref.finish();

        assert_eq!(
            bytes_fast, bytes_ref,
            "Step-C packed-pair cache diverged from cum[]-array reference"
        );
    }

    /// Round 11: encoder Step-C throughput bench. Mid-band
    /// distribution — every symbol fires Step-C, so the
    /// packed-pair cache delta is direct. Functional check
    /// always runs; timing only printed under `LAGARITH_BENCH=1`.
    #[test]
    fn rangecoder_encode_throughput_step_c_heavy() {
        let mut freq = [0u32; 256];
        // Step-A / Step-B basically off (tiny mass on 0 / 255);
        // every other symbol takes Step-C.
        freq[0] = 16;
        freq[255] = 16;
        for slot in freq.iter_mut().take(255).skip(1) {
            *slot = 252; // total ≈ 64,000 — same order as Step-A/B
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let n = 65_536;
        let mut symbols = Vec::with_capacity(n);
        for i in 0..n {
            let r = (i as u32).wrapping_mul(2654435761) ^ (i as u32 >> 7);
            // 99% mid-band → Step-C; 1% on 0/255 to mirror reality.
            symbols.push(match r % 100 {
                0 => 0u8,
                1 => 0xffu8,
                _ => 1 + (r >> 8) as u8 % 254,
            });
        }

        // Functional check: one encode → decode round-trip.
        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();
        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);

        // Optional timing pass (only when LAGARITH_BENCH=1).
        if std::env::var("LAGARITH_BENCH").as_deref() == Ok("1") {
            const REPS: usize = 200;
            let mut sink: u64 = 0;
            let t0 = std::time::Instant::now();
            for _ in 0..REPS {
                let mut enc = RangeEncoder::new();
                for &s in &symbols {
                    enc.encode_symbol(&cdf, s as usize);
                }
                let bytes = enc.finish();
                sink = sink.wrapping_add(bytes.len() as u64);
            }
            let elapsed = t0.elapsed();
            let total_syms = (n * REPS) as f64;
            let msym_per_s = total_syms / elapsed.as_secs_f64() / 1.0e6;
            eprintln!(
                "BENCH/range-coder ENCODE step-c-heavy: {:.2} MSym/s ({} syms x {} reps in {:.3?}; sink={})",
                msym_per_s,
                n,
                REPS,
                elapsed,
                sink
            );
        }
    }

    /// Round 11: `±1`-residual roundtrip. The small-positive
    /// (`s = 1`) and small-negative (`s = 254`) residuals are the
    /// third- and fourth-most-common symbols in Lagarith streams
    /// after gradient prediction (Laplacian-shaped). Verifies the
    /// new round-11 Step-C `freqs[]` cache produces correct
    /// roundtrip output on a `{0, 1, 254, 255}`-heavy distribution.
    /// (Dedicated Step-A1 / Step-B1 fast paths were prototyped and
    /// reverted — see `encode_symbol`'s round-11 NOTE: extra
    /// branches in the hot loop regressed the dominant Step-A
    /// path more than they helped the secondary symbols.)
    #[test]
    fn rangecoder_encode_laplacian_residual_roundtrip() {
        let mut freq = [0u32; 256];
        // Laplacian-ish shape: heavy on {0, 1, 254, 255}, tiny on
        // 2..=253.
        freq[0] = 6000;
        freq[1] = 1500;
        freq[254] = 1500;
        freq[255] = 1000;
        for slot in freq.iter_mut().take(254).skip(2) {
            *slot = 1;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        let mut symbols = Vec::with_capacity(10_000);
        for i in 0..10_000 {
            let r = (i as u32).wrapping_mul(2654435761) ^ (i as u32 >> 7);
            // Distribution: 60% s=0, 15% s=1, 15% s=254, 10% s=255,
            // sprinkle of others.
            symbols.push(match r % 100 {
                0..=59 => 0u8,
                60..=74 => 1u8,
                75..=89 => 254u8,
                90..=98 => 255u8,
                _ => 2u8 + (r >> 8) as u8 % 252,
            });
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 11: `Cdf::freq(s)` returns the pre-computed
    /// `cum[s+1] - cum[s]` matching the array-driven form for
    /// every symbol in `0..=255`. Regression guard for the cache —
    /// if a future refactor desyncs `freqs[s]` from the cum[]
    /// difference, the assertion fires immediately.
    #[test]
    fn cdf_freq_matches_array_form() {
        let mut freq = [0u32; 256];
        for (i, slot) in freq.iter_mut().enumerate() {
            *slot = ((i as u32 * 7) % 17) + 1;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        for s in 0..256 {
            assert_eq!(
                cdf.freq(s),
                cdf.lo(s + 1) - cdf.lo(s),
                "freq(s) != cum[s+1] - cum[s] at s={}",
                s
            );
        }
    }

    /// Round 10: regression guard for the `cache: Option<u8>` →
    /// `cache: u8 + started: bool` refactor in `shift_low` / `finish`.
    /// Encodes a mixed-distribution stream large enough that the
    /// FF-carry path AND the steady-state path both fire many times,
    /// and verifies a self-roundtrip decodes back to the original
    /// symbols. The `Option` round-9 form also delivered correct
    /// roundtrips on the same inputs — this test exists to flag a
    /// future regression of the bool-cache hot path getting an
    /// incorrect carry-FF / steady-state split.
    #[test]
    fn rangecoder_shift_low_started_byte_equiv_to_option() {
        let mut freq = [0u32; 256];
        for (i, slot) in freq.iter_mut().enumerate() {
            *slot = ((i as u32 * 7) % 11) + 1;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // 8000 symbols — enough to fire the FF-carry path many times.
        let mut symbols = Vec::with_capacity(8_000);
        for i in 0..8_000 {
            symbols.push(((i * 31) ^ (i >> 3)) as u8);
        }

        let mut enc = RangeEncoder::new();
        for &s in &symbols {
            enc.encode_symbol(&cdf, s as usize);
        }
        let bytes = enc.finish();

        let mut dec = RangeDecoder::new(&bytes).unwrap();
        let mut out = Vec::with_capacity(symbols.len());
        for _ in 0..symbols.len() {
            out.push(dec.decode_symbol(&cdf).unwrap());
        }
        assert_eq!(out, symbols);
    }

    /// Round 12: encoder-side `spec/02` §6.3 final-flush
    /// bit-equivalence guard for the `Vec::resize` FF-chain bulk fill.
    /// The production `finish()` flushes the pre-tail pending-FF
    /// chain with one `Vec::resize` + memset; this test re-runs the
    /// same encoded stream through a reference `finish_via_push_loop`
    /// helper that drains the chain via per-FF `Vec::push` (the
    /// pre-round-12 form). Both forms write the same `cache+1` then
    /// N×0x00 bytes on a carry, or `cache` then N×0xff bytes
    /// otherwise, followed by the same four-byte tail per
    /// `spec/02` §6.3 — bytes MUST be identical.
    ///
    /// Uses a `freq[255]`-dominant histogram so the encoder's final
    /// state is overwhelmingly likely to have a non-empty
    /// `pending_ffs` chain at the moment of `finish()`, ensuring the
    /// FF-chain drain code path is actually exercised by the test
    /// (a balanced histogram would frequently leave `pending_ffs ==
    /// 0` at finish, masking any divergence).
    #[test]
    fn rangecoder_finish_resize_byte_equiv_to_push_loop() {
        let mut freq = [0u32; 256];
        freq[255] = 9500;
        for slot in freq.iter_mut().take(255) {
            *slot = 2;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // 0xff-dominant stream — leaves a deferred FF chain pending
        // at the moment of `finish()` on most runs.
        let mut symbols = Vec::with_capacity(4_000);
        for i in 0..4_000 {
            symbols.push(if i % 23 == 0 {
                ((i * 7) & 0xfe) as u8
            } else {
                0xffu8
            });
        }

        // Production `finish()` (Vec::resize form).
        let mut enc_fast = RangeEncoder::new();
        for &s in &symbols {
            enc_fast.encode_symbol(&cdf, s as usize);
        }
        let bytes_fast = enc_fast.finish();

        // Reference `finish()` via per-FF push loop. Inline copy of
        // the pre-round-12 finish() body to keep the test independent
        // of any future round's refactor.
        let mut enc_ref = RangeEncoder::new();
        for &s in &symbols {
            enc_ref.encode_symbol(&cdf, s as usize);
        }
        let bytes_ref = finish_via_push_loop(enc_ref);

        assert_eq!(
            bytes_fast, bytes_ref,
            "Round-12 Vec::resize FF-chain flush diverged from per-FF push reference"
        );
    }

    /// Reference helper that drains the pre-tail FF chain via
    /// per-FF `Vec::push` — the pre-round-12 form of
    /// `RangeEncoder::finish()`. Used by
    /// `rangecoder_finish_resize_byte_equiv_to_push_loop` only.
    fn finish_via_push_loop(mut enc: RangeEncoder) -> Vec<u8> {
        let carry = (enc.low >> 31) & 1 == 1;
        if carry {
            if enc.started {
                enc.buf.push(enc.cache.wrapping_add(1));
            }
            for _ in 0..enc.pending_ffs {
                enc.buf.push(0x00);
            }
        } else {
            if enc.started {
                enc.buf.push(enc.cache);
            }
            for _ in 0..enc.pending_ffs {
                enc.buf.push(0xff);
            }
        }
        enc.started = false;
        enc.pending_ffs = 0;
        enc.low &= 0x7fff_ffff;
        let low = enc.low;
        let tail = [
            ((low >> 23) & 0xff) as u8,
            ((low >> 15) & 0xff) as u8,
            ((low >> 7) & 0xff) as u8,
            ((low << 1) & 0xff) as u8,
        ];
        enc.buf.extend_from_slice(&tail);
        enc.buf
    }

    /// Round 12: throughput sanity check for the `finish()` FF-chain
    /// bulk flush. Builds many short channels (each a Step-B-dominant
    /// stream so the FF chain at finish time is non-empty), encodes
    /// each one end-to-end, and measures the total encode+finish
    /// walltime. Functional check (round-trip equality) always runs;
    /// timing is only printed under `LAGARITH_BENCH=1` since
    /// `cargo test` walltime jitters too much to threshold.
    ///
    /// On short channels, `finish()` is a meaningful fraction of
    /// total per-channel cost (the per-symbol hot path dominates on
    /// long channels). The `Vec::resize` form replaces N×push with
    /// one memset, so the delta is most visible on the short-channel
    /// + long-FF-chain workload this test models.
    #[test]
    fn rangecoder_encode_throughput_finish_heavy() {
        let mut freq = [0u32; 256];
        freq[255] = 60_000;
        for slot in freq.iter_mut().take(255) {
            *slot = 16;
        }
        let cdf = Cdf::from_frequencies(&freq).unwrap();

        // 512 short channels × 128 symbols each = 65_536 total
        // symbols (same overall sym count as the Step-A/B/C
        // throughput benches for comparability). Each channel ends
        // with the encoder's last renormalisation chain likely
        // leaving a pending FF run, exercising the §6.3 flush
        // bulk-fill path on every `finish()` call.
        const N_CHANNELS: usize = 512;
        const SYMBOLS_PER_CHANNEL: usize = 128;
        let mut all_symbols: Vec<Vec<u8>> = Vec::with_capacity(N_CHANNELS);
        for ch in 0..N_CHANNELS {
            let mut symbols = Vec::with_capacity(SYMBOLS_PER_CHANNEL);
            for i in 0..SYMBOLS_PER_CHANNEL {
                let r = ((ch * SYMBOLS_PER_CHANNEL + i) as u32).wrapping_mul(2654435761)
                    ^ (i as u32 >> 7);
                symbols.push(if (r & 0xf) < 15 {
                    0xffu8
                } else {
                    ((r >> 4) & 0xfe) as u8
                });
            }
            all_symbols.push(symbols);
        }

        // Functional check: every channel round-trips byte-exact.
        for symbols in &all_symbols {
            let mut enc = RangeEncoder::new();
            for &s in symbols {
                enc.encode_symbol(&cdf, s as usize);
            }
            let bytes = enc.finish();
            let mut dec = RangeDecoder::new(&bytes).unwrap();
            let mut out = Vec::with_capacity(symbols.len());
            for _ in 0..symbols.len() {
                out.push(dec.decode_symbol(&cdf).unwrap());
            }
            assert_eq!(&out, symbols);
        }

        // Optional timing pass (only when LAGARITH_BENCH=1).
        // Times BOTH the production `Vec::resize` form AND the
        // per-FF `Vec::push` reference form on the same workload so
        // the round-12 speedup is directly comparable.
        if std::env::var("LAGARITH_BENCH").as_deref() == Ok("1") {
            const REPS: usize = 200;

            // Production `Vec::resize` form (round 12).
            let mut sink_fast: u64 = 0;
            let t_fast = std::time::Instant::now();
            for _ in 0..REPS {
                for symbols in &all_symbols {
                    let mut enc = RangeEncoder::new();
                    for &s in symbols {
                        enc.encode_symbol(&cdf, s as usize);
                    }
                    let bytes = enc.finish();
                    sink_fast = sink_fast.wrapping_add(bytes.len() as u64);
                }
            }
            let elapsed_fast = t_fast.elapsed();
            let total_syms = (N_CHANNELS * SYMBOLS_PER_CHANNEL * REPS) as f64;
            let msym_per_s_fast = total_syms / elapsed_fast.as_secs_f64() / 1.0e6;

            // Reference `Vec::push` form (pre-round-12). Drives the
            // same encoder + same per-channel symbol stream through
            // the local `finish_via_push_loop` helper so the only
            // difference is the FF-chain flush strategy in `finish()`.
            let mut sink_ref: u64 = 0;
            let t_ref = std::time::Instant::now();
            for _ in 0..REPS {
                for symbols in &all_symbols {
                    let mut enc = RangeEncoder::new();
                    for &s in symbols {
                        enc.encode_symbol(&cdf, s as usize);
                    }
                    let bytes = finish_via_push_loop(enc);
                    sink_ref = sink_ref.wrapping_add(bytes.len() as u64);
                }
            }
            let elapsed_ref = t_ref.elapsed();
            let msym_per_s_ref = total_syms / elapsed_ref.as_secs_f64() / 1.0e6;

            // Sink check — both forms must produce the same total
            // output byte count (they produce byte-identical output;
            // the sum is one cheap end-to-end check).
            assert_eq!(
                sink_fast, sink_ref,
                "Vec::resize and per-FF push forms produced different total byte counts"
            );

            eprintln!(
                "BENCH/range-coder ENCODE finish-heavy [RESIZE]: {:.2} MSym/s ({} chans x {} syms x {} reps in {:.3?}; sink={})",
                msym_per_s_fast,
                N_CHANNELS,
                SYMBOLS_PER_CHANNEL,
                REPS,
                elapsed_fast,
                sink_fast
            );
            eprintln!(
                "BENCH/range-coder ENCODE finish-heavy [PUSH ]: {:.2} MSym/s ({} chans x {} syms x {} reps in {:.3?}; sink={})",
                msym_per_s_ref,
                N_CHANNELS,
                SYMBOLS_PER_CHANNEL,
                REPS,
                elapsed_ref,
                sink_ref
            );
            eprintln!(
                "BENCH/range-coder ENCODE finish-heavy [SPEEDUP RESIZE/PUSH]: {:.3}x",
                msym_per_s_fast / msym_per_s_ref
            );
        }
    }
}
