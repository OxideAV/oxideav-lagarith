//! Probability-model normalizer for the modern range coder, per the
//! recovered helper at `lagarith.dll!0x180001050`
//! (`provenance/52-extractor-round3-quotient-derivation.md`).
//!
//! The wire format carries a **raw byte-histogram** frequency table
//! whose total equals the per-channel symbol count (`spec/04` §5,
//! audit/01 §3.2 validation correction). The reference codec never
//! codes against that raw table directly: after the Fibonacci
//! probability prefix is parsed, the model-init helper sums the raw
//! `freq[]` and invokes the normalizer at `0x180001050`, which
//! rewrites the model in place so that its **total is an exact power
//! of two** (`provenance/52` §1). Every later "divide by total" in
//! the coder then degenerates to `q = range >> shift` with
//! `shift = log2(total_normalized)` (`spec/02` §5 step 1) — exact for
//! *all* input histograms, power-of-two or not. This is what resolves
//! the r398 floor-recip / ceil-recip ±1 divergence at non-power-of-two
//! totals: the reference designs the non-pow2 division out of the
//! coder entirely (`provenance/52` §3).
//!
//! The five recovered steps (`provenance/52` §2):
//!
//! 1. `pow2` = smallest power of two `>= total`; if `pow2 == total`
//!    the rescale + correction are skipped (fast path).
//! 2. `freq[i] = trunc((double)freq[i] * ((double)pow2 /
//!    (double)total))` — IEEE-754 double rescale, truncate toward
//!    zero (`cvttsd2si`).
//! 3. Cumulative-sum correction: `deficit = pow2 - sum(freq)`. An
//!    overshoot (`deficit < 0`, rare double-rounding artefact) is
//!    absorbed into symbol 0's slot; a deficit hands out `+1`
//!    round-robin over the **nonzero** slots among the low 128
//!    symbols (cursor masked `& 0x7f`), never resurrecting a
//!    zero-probability symbol, until `sum == pow2` exactly.
//! 4. `shift = log2(pow2)` is published for the coder's
//!    `q = range >> shift`.
//! 5. The in-place prefix sum turns `freq[]` into `cum[]`.
//!
//! This module implements steps 1–3 and returns the normalized
//! frequency table; steps 4–5 are realised by
//! [`crate::range_coder::Cdf::from_wire_frequencies`], which builds
//! the cumulative table and derives the shift from the (now
//! power-of-two) total.
//!
//! ## Applicability
//!
//! The normalizer belongs to the **modern** coder's model path only
//! (frame types 2 / 3 / 4 / 8 / 10 / 11; `provenance/52` §4). The
//! legacy type-7 coder builds its CDF differently (per-frame
//! histogram prefix-sum; `spec/07`) and is untouched.
//!
//! ## Rounding-mode caveat
//!
//! `provenance/52` §5 notes the i386 build converts with `fistp`
//! (rounding per the x87 control word) where the x86-64 build
//! truncates with `cvttsd2si`. Per the trace's recommendation this
//! implementation targets the **x86-64 truncation semantics** (Rust
//! `as u32` on a non-negative in-range `f64` truncates toward zero,
//! exactly `cvttsd2si`).

use crate::error::{Error, Result};
use crate::range_coder::INIT_RANGE;

/// Normalize a raw wire frequency table into the range-coder-ready
/// model of `lagarith.dll!0x180001050`: the returned table sums to
/// the smallest power of two `>= sum(freq)` exactly.
///
/// Errors:
///
/// * [`Error::ProbabilityTableOverflow`] — the raw total does not fit
///   in `u32` (the reference's 32-bit total accumulation would wrap).
/// * [`Error::EmptyProbabilityTable`] — the raw total is zero.
/// * [`Error::ProbabilityTotalExceedsRange`] — the raw total exceeds
///   `2^31` (= the coder's initial `range`, `spec/02` §2). No power
///   of two `>= total` fits in the reference's 32-bit `pow2`
///   register — its doubling loop (`add r8d,r8d`) would wrap to zero
///   and never terminate — and a total above `range` can never
///   satisfy the `q >= 1` decode invariant anyway.
/// * [`Error::ProbabilityTableUnnormalizable`] — the correction step
///   has a deficit to distribute but every low-128 symbol slot is
///   zero. The reference's distribution loop never terminates on
///   this shape (`provenance/52` §2 step 3 walks slots `1..=128`
///   wrapping, skipping zeros), so no conformant stream can carry it;
///   it is rejected as a wire error.
pub(crate) fn normalize_wire_freq_table(freq: &[u32; 256]) -> Result<[u32; 256]> {
    // Raw total, 32-bit checked (the caller at `0x180001330` sums the
    // freq array in 32-bit registers; a wrap would silently corrupt
    // the model, so surface it as a wire error instead).
    let mut total: u32 = 0;
    for &f in freq {
        total = total
            .checked_add(f)
            .ok_or(Error::ProbabilityTableOverflow)?;
    }
    if total == 0 {
        return Err(Error::EmptyProbabilityTable);
    }
    if total > INIT_RANGE {
        return Err(Error::ProbabilityTotalExceedsRange);
    }

    // Step 1 — smallest power of two >= total (the reference's
    // `pow2 += pow2` doubling loop; `total <= 2^31` guarantees the
    // result fits in u32). Power-of-two fast path: the model is
    // already normalized — skip the rescale + correction entirely
    // (`cmp r8d,eax ; je` at `0x18000107d`).
    let pow2 = total.next_power_of_two();
    if pow2 == total {
        return Ok(*freq);
    }

    // Step 2 — rescale every frequency by `pow2/total` in IEEE-754
    // double precision, truncating toward zero. `scale` is computed
    // once (`divsd`); each entry is widened, multiplied (`mulsd`) and
    // converted back with `cvttsd2si` semantics. All operands are
    // exactly representable (u32 -> f64 is exact) and every product
    // is < 2^31 + 1, so `as u32` neither saturates nor loses the
    // truncation behaviour.
    let scale = f64::from(pow2) / f64::from(total);
    let mut out = [0u32; 256];
    let mut actual: u32 = 0;
    for (slot, &f) in out.iter_mut().zip(freq.iter()) {
        let v = (f64::from(f) * scale) as u32;
        *slot = v;
        // The 4-way unrolled reference accumulation is plain 32-bit
        // adds; `sum(trunc(f * scale)) <= pow2 + 256 < 2^32`, so this
        // cannot wrap on any input that passed the guards above.
        actual += v;
    }

    // Step 3 — cumulative-sum correction: force `sum == pow2`.
    let mut deficit = i64::from(pow2) - i64::from(actual);
    if deficit < 0 {
        // Overshoot (rare double-rounding artefact): the reference
        // adds the negative deficit straight into symbol 0's slot
        // (`add [r10+0x4],r9d` — a plain 32-bit two's-complement
        // add). Mirror it with a wrapping add; if the slot
        // underflows, the resulting table fails the downstream
        // cumulative-sum overflow guard exactly as the reference's
        // garbage model would fail to decode.
        out[0] = out[0].wrapping_add(deficit as u32);
        return Ok(out);
    }
    if deficit > 0 {
        // Deficit distribution walks model slots 1..=128 — symbols
        // 0..=127 — wrapping (`and ecx,0x7f`), incrementing only
        // nonzero slots. Terminates because `deficit <= 256` (each
        // truncation loses < 1, plus double-rounding slack) and each
        // 128-slot lap decrements at least once when a nonzero low
        // slot exists.
        if out[..128].iter().all(|&f| f == 0) {
            return Err(Error::ProbabilityTableUnnormalizable);
        }
        let mut s = 0usize;
        while deficit > 0 {
            if out[s] != 0 {
                out[s] += 1;
                deficit -= 1;
            }
            s = (s + 1) & 0x7f;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Step-1 fast path: a power-of-two total returns the input
    /// unchanged — no rescale, no correction (`provenance/52` §2
    /// step 1, `je 0x18000125b`).
    #[test]
    fn pow2_total_is_identity() {
        let mut freq = [0u32; 256];
        freq[0] = 13;
        freq[7] = 2;
        freq[255] = 1; // total = 16 = 2^4
        let out = normalize_wire_freq_table(&freq).unwrap();
        assert_eq!(out, freq);
    }

    /// total = 1 (single symbol, single count) is the smallest
    /// power-of-two total (2^0) and takes the identity fast path.
    #[test]
    fn total_one_is_identity() {
        let mut freq = [0u32; 256];
        freq[42] = 1;
        let out = normalize_wire_freq_table(&freq).unwrap();
        assert_eq!(out, freq);
    }

    /// Unit frequencies rescale deterministically: every `1 * scale`
    /// lands in `(1, 2)` (since `total < pow2 < 2 * total`), so each
    /// nonzero slot floors back to 1 and the deficit is exactly
    /// `pow2 - nonzero_count`, handed out round-robin from symbol 0.
    #[test]
    fn unit_frequencies_deficit_distribution() {
        let mut freq = [0u32; 256];
        for slot in freq.iter_mut().take(5) {
            *slot = 1; // total = 5, pow2 = 8
        }
        let out = normalize_wire_freq_table(&freq).unwrap();
        // deficit = 8 - 5 = 3 -> symbols 0, 1, 2 get +1 each.
        assert_eq!(&out[..5], &[2, 2, 2, 1, 1]);
        assert_eq!(out[5..].iter().sum::<u32>(), 0);
        assert_eq!(out.iter().sum::<u32>(), 8);
    }

    /// The distribution cursor wraps modulo 128: with only two
    /// nonzero low-half slots and a deficit of 3, symbol 0 receives
    /// a second increment on the wrap lap (`and ecx,0x7f`).
    #[test]
    fn deficit_distribution_wraps_low_128() {
        let mut freq = [0u32; 256];
        freq[0] = 1;
        freq[1] = 1;
        freq[130] = 1;
        freq[131] = 1;
        freq[132] = 1; // total = 5, pow2 = 8, deficit = 3
        let out = normalize_wire_freq_table(&freq).unwrap();
        // Only symbols 0 and 1 are in the distribution window
        // (slots 1..=128 of the model = symbols 0..=127); the
        // upper-half symbols keep their floor-rescaled value.
        assert_eq!(out[0], 3); // +1 first lap, +1 wrap lap
        assert_eq!(out[1], 2); // +1 first lap
        assert_eq!(out[130], 1);
        assert_eq!(out[131], 1);
        assert_eq!(out[132], 1);
        assert_eq!(out.iter().sum::<u32>(), 8);
    }

    /// Symbol 127 (model slot 128) is INSIDE the distribution window
    /// and symbol 128 (slot 129) is outside it — the `& 0x7f` mask
    /// wraps the slot cursor 1 -> 128 -> 1 (`provenance/52` §2
    /// step 3: "the low 128 symbol slots (freq[1..128], wrapping)").
    #[test]
    fn distribution_window_boundary_symbol_127_in_128_out() {
        // Force a deficit large enough to lap: 127 zeros then two
        // nonzero slots at the window edge.
        let mut freq = [0u32; 256];
        freq[127] = 1;
        freq[128] = 1;
        freq[129] = 1; // total = 3, pow2 = 4, deficit = 1
        let out = normalize_wire_freq_table(&freq).unwrap();
        // Only symbol 127 is eligible; it takes the whole deficit.
        assert_eq!(out[127], 2);
        assert_eq!(out[128], 1);
        assert_eq!(out[129], 1);
        assert_eq!(out.iter().sum::<u32>(), 4);
    }

    /// Zero-probability slots are never resurrected: the correction
    /// skips them even when the cursor passes over them many times.
    #[test]
    fn deficit_never_resurrects_zero_slots() {
        let mut freq = [0u32; 256];
        freq[3] = 1;
        freq[9] = 1;
        freq[100] = 1; // total = 3, pow2 = 4, deficit = 1
        let out = normalize_wire_freq_table(&freq).unwrap();
        assert_eq!(out[3], 2); // first nonzero slot in cursor order
        assert_eq!(out[9], 1);
        assert_eq!(out[100], 1);
        for (s, &f) in out.iter().enumerate() {
            if ![3, 9, 100].contains(&s) {
                assert_eq!(f, 0, "slot {s} resurrected");
            }
        }
    }

    /// The normalized total is exactly `pow2` for a spread of
    /// non-power-of-two totals (the load-bearing invariant of
    /// `provenance/52` §3: sum(out) == 2^shift identically).
    #[test]
    fn normalized_total_is_exact_pow2_sweep() {
        // Deterministic LCG over histogram shapes.
        let mut seed = 0x1234_5678u32;
        let mut rng = move || {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            seed
        };
        for case in 0..500 {
            let mut freq = [0u32; 256];
            let nonzero = 2 + (rng() % 254) as usize;
            for _ in 0..nonzero {
                let s = (rng() % 128) as usize; // keep low-half mass
                freq[s] += 1 + rng() % 997;
            }
            let raw_total: u64 = freq.iter().map(|&f| u64::from(f)).sum();
            let out = normalize_wire_freq_table(&freq).unwrap();
            let total: u64 = out.iter().map(|&f| u64::from(f)).sum();
            assert!(
                total.is_power_of_two(),
                "case {case}: total {total} not a power of two (raw {raw_total})"
            );
            assert!(
                total >= raw_total && total < 2 * raw_total.next_power_of_two(),
                "case {case}: total {total} out of band for raw {raw_total}"
            );
            // Nonzero set preserved exactly.
            for s in 0..256 {
                assert_eq!(
                    out[s] == 0,
                    freq[s] == 0,
                    "case {case}: nonzero set changed at symbol {s}"
                );
            }
        }
    }

    /// Deficit with an all-zero low half cannot be corrected — the
    /// reference's distribution loop would never terminate, so the
    /// table is rejected as a wire error.
    #[test]
    fn upper_half_only_mass_is_unnormalizable() {
        let mut freq = [0u32; 256];
        freq[200] = 1;
        freq[201] = 1;
        freq[202] = 1; // total = 3, pow2 = 4, deficit = 1, low half empty
        assert_eq!(
            normalize_wire_freq_table(&freq),
            Err(Error::ProbabilityTableUnnormalizable)
        );
    }

    /// Upper-half-only mass at a power-of-two total is FINE — the
    /// fast path never reaches the correction step.
    #[test]
    fn upper_half_only_mass_pow2_total_ok() {
        let mut freq = [0u32; 256];
        freq[200] = 3;
        freq[201] = 1; // total = 4 = 2^2
        let out = normalize_wire_freq_table(&freq).unwrap();
        assert_eq!(out, freq);
    }

    /// Raw total above 2^31: no 32-bit power of two >= total exists
    /// (the reference's doubling register wraps), and the total can
    /// never satisfy `q >= 1`. Rejected up front.
    #[test]
    fn total_above_2_pow_31_rejected() {
        let mut freq = [0u32; 256];
        freq[0] = INIT_RANGE; // 2^31
        freq[1] = 1;
        assert_eq!(
            normalize_wire_freq_table(&freq),
            Err(Error::ProbabilityTotalExceedsRange)
        );
    }

    /// Raw total exactly 2^31 is the largest admissible total and is
    /// its own power of two (identity fast path).
    #[test]
    fn total_exactly_2_pow_31_is_identity() {
        let mut freq = [0u32; 256];
        freq[0] = INIT_RANGE;
        let out = normalize_wire_freq_table(&freq).unwrap();
        assert_eq!(out, freq);
    }

    /// 32-bit total overflow is rejected (the reference's 32-bit
    /// accumulation would silently wrap).
    #[test]
    fn total_overflow_rejected() {
        let mut freq = [0u32; 256];
        freq[0] = u32::MAX;
        freq[1] = 1;
        assert_eq!(
            normalize_wire_freq_table(&freq),
            Err(Error::ProbabilityTableOverflow)
        );
    }

    /// All-zero table is rejected.
    #[test]
    fn empty_table_rejected() {
        let freq = [0u32; 256];
        assert_eq!(
            normalize_wire_freq_table(&freq),
            Err(Error::EmptyProbabilityTable)
        );
    }

    /// Large non-pow2 total near the encoder cap: the invariant
    /// `sum == pow2` must hold at scale, and the per-slot values must
    /// match the floor-rescale within the +1 correction band.
    #[test]
    fn large_total_rescale_within_correction_band() {
        let mut freq = [0u32; 256];
        for (s, slot) in freq.iter_mut().enumerate() {
            *slot = 17 + (s as u32 * 131) % 40_000;
        }
        let total: u32 = freq.iter().sum();
        assert!(!total.is_power_of_two());
        let pow2 = total.next_power_of_two();
        let out = normalize_wire_freq_table(&freq).unwrap();
        assert_eq!(
            out.iter().map(|&f| u64::from(f)).sum::<u64>(),
            u64::from(pow2)
        );
        let scale = f64::from(pow2) / f64::from(total);
        for s in 0..256 {
            let floored = (f64::from(freq[s]) * scale) as u32;
            assert!(
                out[s] == floored || out[s] == floored + 1 || out[s] == floored + 2,
                "symbol {s}: {} not within correction band of floor {floored}",
                out[s]
            );
        }
    }
}
