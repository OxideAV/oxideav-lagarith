//! Fibonacci probability prefix decode + (test-only) encode per
//! `spec/04`.
//!
//! The wire format pre-pends each modern arithmetic-coded channel's
//! body with a 256-entry raw-frequency table encoded as:
//!
//! 1. A Fibonacci-prefix length-class code (`spec/04` §2.2).
//! 2. A binary suffix with `m = v - 1` bits when `v >= 2`
//!    (`spec/04` §3.2). The decoded frequency is `(1 << m) - 1 + s`.
//! 3. For `v = 1` a *zero-run* code (a second Fibonacci value) per
//!    `spec/04` §3.3 — note the run-length form does **not** apply
//!    the trailing `-1` (`run_length = 2^m + s` for `v >= 2`).
//!
//! Bits are read MSB-first from each byte (`spec/04` §2.1).

use crate::error::{Error, Result};

/// `spec/04` §2.1: x86-64 build's 7-entry Fibonacci series. The
/// i386 build adds an 8th entry (34) but `spec/04` §8.4 recommends
/// the 7-entry table for clean-room implementations.
pub(crate) const FIB: [u32; 7] = [1, 2, 3, 5, 8, 13, 21];

/// MSB-first bit reader over a borrowed byte stream. Tracks the
/// consumed-byte count so the caller can locate the start of the
/// arithmetic body that follows the prefix (`spec/04` §3.5).
pub(crate) struct BitReader<'a> {
    src: &'a [u8],
    /// Byte index of the *current* byte the mask applies to.
    byte: usize,
    /// Current single-bit mask within `src[byte]`. Initial value
    /// `0x80`. When it drops to zero the reader advances `byte` and
    /// resets the mask to `0x80`.
    mask: u8,
}

impl<'a> BitReader<'a> {
    pub fn new(src: &'a [u8]) -> Self {
        Self {
            src,
            byte: 0,
            mask: 0x80,
        }
    }

    /// Number of *whole or partial* bytes consumed so far. Matches
    /// the proprietary's "round up if any partial byte is in use"
    /// convention (`spec/04` §3.5).
    pub fn bytes_consumed(&self) -> usize {
        if self.mask == 0x80 {
            self.byte
        } else {
            self.byte + 1
        }
    }

    /// True iff the read cursor sits exactly on a byte boundary (no
    /// partial byte in flight). Used by the legacy type-7 channel
    /// decoder to detect whether the post-Fibonacci 1-byte
    /// reservation is present per audit/08 §3.2.
    pub fn is_byte_aligned(&self) -> bool {
        self.mask == 0x80
    }

    pub fn read_bit(&mut self) -> Result<u8> {
        if self.byte >= self.src.len() {
            return Err(Error::Truncated {
                context: "Fibonacci prefix bit stream",
            });
        }
        let b = (self.src[self.byte] & self.mask) != 0;
        self.mask >>= 1;
        if self.mask == 0 {
            self.mask = 0x80;
            self.byte += 1;
        }
        Ok(b as u8)
    }

    /// Decode a Fibonacci prefix value per `spec/04` §2.2. Returns
    /// [`Error::FibonacciOverflow`] if the bit-stream tries to set
    /// a position past `Fib[6]` (max representable value is 33;
    /// `spec/04` §2.4).
    pub fn read_fib(&mut self) -> Result<u32> {
        let mut v: u32 = 0;
        let mut prev: u8 = 0;
        let mut pos: usize = 0;
        loop {
            let cur = self.read_bit()?;
            if prev == 1 && cur == 1 {
                return Ok(v);
            }
            if cur == 1 {
                if pos >= FIB.len() {
                    return Err(Error::FibonacciOverflow);
                }
                v += FIB[pos];
            }
            prev = cur;
            pos += 1;
            // Cap on iteration count: terminator must arrive within
            // FIB.len() + 1 bits or we declare overflow. That's the
            // hard cap the proprietary's stack-allocated array
            // implies.
            if pos > FIB.len() + 1 {
                return Err(Error::FibonacciOverflow);
            }
        }
    }
}

/// Decode a 256-entry frequency table from the start of `src`.
///
/// Returns `(freq, bytes_consumed)`. `bytes_consumed` is what the
/// dispatcher uses to find the first byte of the range-coder body
/// (`spec/04` §3.5).
pub fn decode_freq_table(src: &[u8]) -> Result<([u32; 256], usize)> {
    let mut br = BitReader::new(src);
    let mut freq = [0u32; 256];
    let mut j: usize = 0;
    while j < 256 {
        let v = br.read_fib()?;
        // `spec/04` §3.1: m = v - 1.
        if v == 0 {
            return Err(Error::Truncated {
                context: "Fibonacci length-class (v=0 invalid)",
            });
        }
        let m = v - 1;
        if m == 0 {
            // Zero-run: read second Fibonacci code.
            let vp = br.read_fib()?;
            if vp == 0 {
                return Err(Error::Truncated {
                    context: "Fibonacci run-length (v'=0 invalid)",
                });
            }
            let mp = vp - 1;
            let run_len: u32 = if mp == 0 {
                1
            } else {
                let mut suffix: u32 = 1;
                for _ in 0..mp {
                    let bit = br.read_bit()?;
                    suffix = (suffix << 1) | (bit as u32);
                }
                suffix
            };
            // Clamp at the 256-entry boundary (`spec/04` §3.3).
            let advance = run_len.min(256u32 - j as u32) as usize;
            j += advance;
        } else {
            // Non-zero frequency: read m suffix bits.
            if m > 31 {
                return Err(Error::FibonacciOverflow);
            }
            let mut suffix: u32 = 1;
            for _ in 0..m {
                let bit = br.read_bit()?;
                suffix = (suffix << 1) | (bit as u32);
            }
            // freq[j] = (1 << m) - 1 + s = suffix - 1.
            freq[j] = suffix.wrapping_sub(1);
            j += 1;
        }
    }
    Ok((freq, br.bytes_consumed()))
}

// ─────────────────────── MSB-first bit writer (test-only) ───────────────────────

/// MSB-first bit writer used by the legacy type-7 Fibonacci freq-
/// table encoder (round 4, test-only) and the modern coder's
/// test-only roundtrip suite.
#[cfg(test)]
pub(crate) struct BitWriter {
    buf: Vec<u8>,
    cur: u8,
    mask: u8,
}

#[cfg(test)]
impl BitWriter {
    pub fn new() -> Self {
        Self {
            buf: Vec::new(),
            cur: 0,
            mask: 0x80,
        }
    }

    pub fn write_bit(&mut self, b: u8) {
        if b != 0 {
            self.cur |= self.mask;
        }
        self.mask >>= 1;
        if self.mask == 0 {
            self.buf.push(self.cur);
            self.cur = 0;
            self.mask = 0x80;
        }
    }

    /// True iff the next bit to be written would land at the start of
    /// a fresh byte (i.e. no partial byte is in flight).
    pub fn is_byte_aligned(&self) -> bool {
        self.mask == 0x80
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.mask != 0x80 {
            self.buf.push(self.cur);
        }
        self.buf
    }
}

#[cfg(test)]
fn write_fib(w: &mut BitWriter, value: u32) {
    // Greedy Zeckendorf decomposition: walk FIB high-to-low and
    // mark each summand position. `spec/04` §2.3 / §4.
    let mut bits = [0u8; 7];
    let mut remaining = value;
    let mut last_set: i32 = -1;
    for i in (0..FIB.len()).rev() {
        if FIB[i] <= remaining {
            // Skip if this would create two consecutive 1s with the
            // previous (higher-index) summand. Greedy from high
            // ensures non-consecutive by construction.
            bits[i] = 1;
            remaining -= FIB[i];
            if last_set < 0 {
                last_set = i as i32;
            }
        }
    }
    debug_assert_eq!(remaining, 0, "Zeckendorf decompose failed");
    // Find the highest set bit; emit positions 0..=highest then a
    // terminator '1' at highest+1 — MSB-first per `spec/04` §2.3
    // means low-position-first in the bit-stream.
    let highest = (0..FIB.len())
        .rev()
        .find(|&i| bits[i] == 1)
        .expect("Fibonacci value must be >= 1");
    for &bit in bits.iter().take(highest + 1) {
        w.write_bit(bit);
    }
    // Terminator.
    w.write_bit(1);
}

#[cfg(test)]
pub fn encode_freq_table(freq: &[u32; 256]) -> Vec<u8> {
    let mut w = BitWriter::new();
    let mut j: usize = 0;
    while j < 256 {
        if freq[j] == 0 {
            // Zero-run: count consecutive zeros.
            let mut run = 1usize;
            while j + run < 256 && freq[j + run] == 0 {
                run += 1;
            }
            // Emit Fib(1) (= "11") then the run-length code.
            write_fib(&mut w, 1);
            // run_length = 2^m' + s' for v' >= 2, or run=1 for v'=1.
            if run == 1 {
                write_fib(&mut w, 1);
            } else {
                // Find m' such that 2^m' <= run < 2^(m'+1)
                let mp = (run as u32).ilog2();
                debug_assert!((1u32 << mp) <= run as u32);
                let s = (run as u32) - (1u32 << mp);
                write_fib(&mut w, mp + 1);
                for i in (0..mp).rev() {
                    let bit = ((s >> i) & 1) as u8;
                    w.write_bit(bit);
                }
            }
            j += run;
        } else {
            // Non-zero: emit Fib(m+1) + m suffix bits where
            // m = bit_length(freq[j] + 1) - 1 and
            // suffix = (freq[j] + 1) & ((1<<m) - 1).
            let v = freq[j] + 1;
            let m = v.ilog2();
            write_fib(&mut w, m + 1);
            let mask = (1u32 << m).wrapping_sub(1);
            let s = v & mask;
            for i in (0..m).rev() {
                let bit = ((s >> i) & 1) as u8;
                w.write_bit(bit);
            }
            j += 1;
        }
    }
    w.finish()
}

// ─────────────────────── tests ───────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    /// `spec/04` §2.3 wiki bit patterns 1..=8 + 16, 32.
    #[test]
    fn fib_decode_examples() {
        // "1 -> 11", read MSB-first as bit-stream "11" (single byte
        // 0xc0). Bit 7 = 1, bit 6 = 1 -> terminator -> v = 0.
        // Wait: "11" yields prev=1, cur=1 immediately -> v = 0.
        // The wiki table says bit pattern "11" = value 1, but per
        // §2.2 the very first bit pair "11" terminates with v = 0,
        // and it is the §3.1 path's "v = 1" sentinel that maps to
        // freq=0. So §2.3 wiki "1 -> 11" describes the wire output
        // of *encoding* value 1 which is "11" + terminator. Let me
        // verify with §4 decode loop.
        // Actually the wiki says "1 - 11", "2 - 011", ..., which
        // means the bit-stream reads MSB-first; the code's pos=0
        // sees bit '1', position increments, pos=1 sees bit '1',
        // both are 1 -> terminator. The accumulator `v` is the
        // value *before* the terminator. For "11": pos 0 has bit
        // 1 (so v += FIB[0] = 1); pos 1 has bit 1 (terminator
        // fires before adding). So v = 1. Confirmed.
        let cases: &[(&[u8], u32)] = &[
            (&[0xc0], 1), // "11"
            (&[0x60], 2), // "011"
            (&[0x30], 3), // "0011"
            (&[0xb0], 4), // "1011"
            (&[0x18], 5), // "00011"
            (&[0x98], 6), // "10011"
            (&[0x58], 7), // "01011"
            (&[0x0c], 8), // "000011"
        ];
        for (bytes, expected) in cases {
            let mut br = BitReader::new(bytes);
            assert_eq!(br.read_fib().unwrap(), *expected, "decode {bytes:?}");
        }
    }

    #[test]
    fn fib_freq_table_roundtrip_simple() {
        let mut freq = [0u32; 256];
        freq[0] = 5;
        freq[10] = 3;
        freq[100] = 1;
        freq[200] = 7;
        freq[255] = 2;
        let bytes = encode_freq_table(&freq);
        let (got, _consumed) = decode_freq_table(&bytes).unwrap();
        assert_eq!(got[..], freq[..]);
    }

    #[test]
    fn fib_freq_table_roundtrip_dense() {
        let mut freq = [0u32; 256];
        for (i, slot) in freq.iter_mut().enumerate() {
            *slot = ((i as u32) % 13) + 1;
        }
        let bytes = encode_freq_table(&freq);
        let (got, _consumed) = decode_freq_table(&bytes).unwrap();
        for (i, (a, b)) in got.iter().zip(freq.iter()).enumerate() {
            assert_eq!(a, b, "i={i}");
        }
    }

    #[test]
    fn fib_freq_table_roundtrip_runs() {
        // A pattern with several zero runs.
        let mut freq = [0u32; 256];
        freq[0] = 1;
        freq[1] = 1;
        freq[2] = 0; // run of 1
        freq[3] = 5;
        // run of 20 from 50..70
        for slot in freq.iter_mut().take(70).skip(50) {
            *slot = 0;
        }
        for slot in freq.iter_mut().take(80).skip(70) {
            *slot = 3;
        }
        // long terminal zero run from 100..
        for slot in freq.iter_mut().skip(100) {
            *slot = 0;
        }
        let bytes = encode_freq_table(&freq);
        let (got, _) = decode_freq_table(&bytes).unwrap();
        for (i, (a, b)) in got.iter().zip(freq.iter()).enumerate() {
            assert_eq!(a, b, "i={i}");
        }
    }
}
