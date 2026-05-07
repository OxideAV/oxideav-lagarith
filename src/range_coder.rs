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
#[derive(Debug, Clone)]
pub(crate) struct Cdf {
    cum: [u32; 257],
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
        Ok(Self { cum })
    }

    /// Total mass `cum[256]`.
    #[inline]
    pub fn total(&self) -> u32 {
        self.cum[256]
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
    fn renormalise(&mut self) -> Result<()> {
        while self.range <= TOP {
            // Saturating advance — if we're about to walk past the
            // end of the channel body, treat the missing bytes as
            // zero. This matches the proprietary's behaviour when
            // the channel body's range-coder tail flush is short
            // enough that the decoder has already produced its full
            // pixel count and is just shifting state out (the
            // caller's symbol-count guard exits before any garbage
            // is observed).
            let prev = self.src.get(self.cursor).copied().unwrap_or(0);
            let next = self.src.get(self.cursor + 1).copied().unwrap_or(0);
            self.low =
                (self.low << 8).wrapping_add((((prev & 1) as u32) << 7) | ((next as u32) >> 1));
            self.range <<= 8;
            self.cursor += 1;
        }
        Ok(())
    }

    /// Decode a single symbol against the supplied CDF.
    pub fn decode_symbol(&mut self, cdf: &Cdf) -> Result<u8> {
        // Per `spec/02` §5 (clean-room form): q = range / total; the
        // symbol is the s for which `cum[s]*q <= low < cum[s+1]*q`.
        let total = cdf.total();
        debug_assert!(total > 0, "Cdf::total must be non-zero");
        let q = self.range / total;
        let target = (self.low / q).min(total - 1);
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
    /// arrives. `None` only at the very start (no symbol committed
    /// yet); after the first emission this always holds the
    /// most-recently-stored output byte.
    cache: Option<u8>,
    low: u32,
    range: u32,
}

#[cfg(test)]
impl RangeEncoder {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            pending_ffs: 0,
            cache: None,
            low: 0,
            range: INIT_RANGE,
        }
    }

    /// Commit one final output byte: flush the cache + the FF chain
    /// (kept as-is because we know the carry-out has not happened
    /// for them), then store `byte` as the new cache (it may still
    /// be incremented by a future carry).
    fn shift_low(&mut self) {
        // Carry-out detection: bit 31 of low + the implicit carry
        // from low + range computations. `spec/02` §6.2: when bit
        // 31 of `low` is set we have a carry to back-propagate.
        let carry = (self.low >> 31) & 1 == 1;
        let byte = ((self.low >> 23) & 0xff) as u8;
        if carry {
            // Flush cache+1, then `pending_ffs` zeros (the old FFs
            // rolled over by the back-walk per `spec/02` §6.2).
            if let Some(c) = self.cache.take() {
                self.buf.push(c.wrapping_add(1));
            }
            for _ in 0..self.pending_ffs {
                self.buf.push(0x00);
            }
            self.pending_ffs = 0;
            self.cache = Some(byte);
        } else if byte == 0xff {
            // Defer: the next non-0xff byte that comes through here
            // (or the final flush) will commit this 0xff in its
            // un-carry'd form. Increment the pending count.
            self.pending_ffs += 1;
        } else {
            // Steady-state: flush cache + the FF chain (un-carry'd)
            // and cache the new byte.
            if let Some(c) = self.cache.take() {
                self.buf.push(c);
            }
            for _ in 0..self.pending_ffs {
                self.buf.push(0xff);
            }
            self.pending_ffs = 0;
            self.cache = Some(byte);
        }
        // Mask to bits 0..22 then shift up by 8 — clears the
        // already-emitted byte (bits 23..30) and the carry bit (31).
        self.low = (self.low & 0x007f_ffff) << 8;
    }

    fn renormalise(&mut self) {
        while self.range <= TOP {
            self.shift_low();
            self.range <<= 8;
        }
    }

    /// Encode symbol `s` against the supplied CDF.
    pub fn encode_symbol(&mut self, cdf: &Cdf, s: usize) {
        let total = cdf.total();
        let q = self.range / total;
        let lo = cdf.lo(s);
        let hi = cdf.lo(s + 1);
        let added = lo * q;
        self.low = self.low.wrapping_add(added);
        self.range = (hi - lo) * q;
        self.renormalise();
    }

    /// Flush the four-byte tail per `spec/02` §6.3.
    pub fn finish(mut self) -> Vec<u8> {
        // The proprietary's encoder writes the four-byte tail
        // directly from the running `low`. We must absorb any
        // outstanding carry-out into `cache` first so the tail bytes
        // are aligned to the same `low` the decoder will see at
        // init.
        let carry = (self.low >> 31) & 1 == 1;
        if carry {
            if let Some(c) = self.cache.take() {
                self.buf.push(c.wrapping_add(1));
            }
            for _ in 0..self.pending_ffs {
                self.buf.push(0x00);
            }
        } else {
            if let Some(c) = self.cache.take() {
                self.buf.push(c);
            }
            for _ in 0..self.pending_ffs {
                self.buf.push(0xff);
            }
        }
        self.pending_ffs = 0;
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
}
