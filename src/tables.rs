//! Lookup tables used by the residual-RLE escape (`spec/05`).
//!
//! The numeric extracts at `tables/01..02-*.csv` are mirrored from
//! the cleanroom workspace's `docs/video/lagarith/tables/`
//! (re-extracted from the proprietary DLL by
//! `extract-luts.sh`). They are pulled in via [`include_str!`] at
//! compile time and parsed once into static caches; the values are
//! validated against the algebraic forms in `spec/05` §3.2 / §5.2
//! as a unit-test cross-check, so a stale CSV would surface as a
//! test failure.
//!
//! The range-coder reciprocal-multiply LUT (`spec/02` §5 step C) is
//! *not* used by this build's decoder path: `spec/02` §5 explicitly
//! invites the clean-room implementation to substitute a "straight
//! cumulative search" loop, which produces bit-identical output
//! without depending on the LUT. The decoder uses the search loop.
//! The table is nonetheless **bundled** (`tables/00-*.csv`) and
//! exposed via [`recip_lut`] because its numeric form is what pins
//! the power-of-two restriction on the crate's byte-exact
//! cross-decoder pins (`tests/reference_pins.rs`): the characterisation
//! tests below prove that a naive reciprocal-multiply
//! `(range * LUT[total]) >> 32` coincides with the crate's exact
//! `q = range / total` division only when `total` is a power of two,
//! and diverges by ±1 at every non-power-of-two `total` somewhere in
//! the coder's operating range `[2^23 + 1, 2^31]`. This makes
//! machine-checked what `reference_pins.rs` previously only asserted
//! in prose, and records why the *exact* reference quotient at a
//! non-power-of-two total is still an open item (`spec/02` §9 item 1,
//! `spec/04` §9 item 2 — the `0x180001050` cumulative-sum / shift
//! derivation is not covered by the wire-format spec).

use std::sync::OnceLock;

/// CSV source for the range-coder reciprocal-multiply LUT
/// (2048 × u32; `spec/02` §5 step C). See [`recip_lut`].
const RECIP_LUT_CSV: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tables/00-rangecoder-reciprocal-multiply-lut.csv",
));

/// CSV source for the residual-RLE forward LUT (256 × u32, low byte
/// is the value; upper bytes always zero — see CSV header block).
const RLE_FWD_LUT_CSV: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tables/01-residual-rle-decoder-lut.csv",
));

/// CSV source for the residual-RLE inverse LUT (256 × u8). Used by
/// the encoder to pick the supplement byte that decodes back to a
/// desired run length.
const RLE_INV_LUT_CSV: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tables/02-residual-rle-encoder-inv-lut.csv",
));

/// Forward residual-RLE LUT: `LUT[supplement_byte] -> run_length`.
///
/// Algebraically equivalent to:
///
/// ```text
/// LUT[i] = 2 * i        for i in [0, 127]
/// LUT[i] = 511 - 2 * i  for i in [128, 255]
/// ```
///
/// (`spec/05` §3.2). Implementing the algebraic form directly is
/// fine; loading from CSV ensures the binary's literal table is
/// what we agree with.
pub fn rle_fwd_lut() -> &'static [u8; 256] {
    static CACHE: OnceLock<[u8; 256]> = OnceLock::new();
    CACHE.get_or_init(|| {
        let parsed = parse_u32_csv::<256>(RLE_FWD_LUT_CSV);
        let mut out = [0u8; 256];
        for (i, v) in parsed.iter().enumerate() {
            // Spec/05 §3.2: each LUT entry's upper 24 bits are zero
            // and the low byte is the run-length value.
            out[i] = (*v & 0xff) as u8;
            // Algebraic cross-check.
            let expected = if i < 128 { 2 * i } else { 511 - 2 * i };
            debug_assert_eq!(out[i] as usize, expected, "rle_fwd_lut[{i}] mismatch");
        }
        out
    })
}

/// Inverse residual-RLE LUT: `INV_LUT[k] = supplement_byte` such that
/// `LUT[supplement_byte] = k - 2` for `k >= 2`. Indices `0..2` are
/// padding and hold `0` (see `spec/05` §5.2).
///
/// The encoder mirror of [`rle_fwd_lut`]: maps a desired run length
/// back to the supplement byte the decoder expands to that run.
pub fn rle_inv_lut() -> &'static [u8; 256] {
    static CACHE: OnceLock<[u8; 256]> = OnceLock::new();
    CACHE.get_or_init(|| parse_u8_csv::<256>(RLE_INV_LUT_CSV))
}

/// Range-coder reciprocal-multiply LUT (`spec/02` §5 step C):
/// `LUT[i] = floor(2^32 / i)` for `i >= 2`, with `LUT[0] = 0` and
/// `LUT[1] = 0xffffffff` (the exact `2^32 / 1 = 2^32` overflows a
/// `u32`, so the reference stores `2^32 − 1`).
///
/// The crate's decoder does not consult this table — it runs the
/// `spec/02` §5 invariant-box cumulative search with exact
/// `q = range / total`. The table is bundled purely so its numeric
/// form is machine-checkable (see the module-level tests): a naive
/// reciprocal-multiply built from it, `(range * LUT[total]) >> 32`,
/// equals exact division **iff** `total` is a power of two, which is
/// what pins the power-of-two restriction on the byte-exact
/// cross-decoder pins in `tests/reference_pins.rs`.
#[cfg_attr(not(test), allow(dead_code))]
pub fn recip_lut() -> &'static [u32; 2048] {
    static CACHE: OnceLock<[u32; 2048]> = OnceLock::new();
    CACHE.get_or_init(|| parse_u32_csv::<2048>(RECIP_LUT_CSV))
}

/// Parse a Lagarith-tables CSV with header lines starting with `#`
/// (per `tables/README.md`); columns are `index, value_hex,
/// value_dec`. Returns the `value_dec` column as `[u32; N]`.
fn parse_u32_csv<const N: usize>(src: &str) -> [u32; N] {
    let mut out = [0u32; N];
    let mut count = 0usize;
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Skip a leading column-header row "index,value_hex,..." if
        // present. The CSVs in this workspace include such a header.
        if line.starts_with("index") {
            continue;
        }
        let mut parts = line.splitn(3, ',');
        let idx: usize = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("bad index in CSV row: {line}"));
        let _hex = parts.next().expect("missing hex column");
        let dec: u32 = parts
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or_else(|| panic!("bad dec value in CSV row: {line}"));
        assert!(idx < N, "CSV index {idx} out of range for size {N}");
        out[idx] = dec;
        count += 1;
    }
    assert_eq!(count, N, "CSV had {count} rows, expected {N}");
    out
}

fn parse_u8_csv<const N: usize>(src: &str) -> [u8; N] {
    let parsed = parse_u32_csv::<N>(src);
    let mut out = [0u8; N];
    for (i, v) in parsed.iter().enumerate() {
        assert!(*v <= 0xff, "u8 CSV row {i} = {v} out of range");
        out[i] = *v as u8;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `spec/05` §3.2: forward LUT is `2*i` / `511 - 2*i`.
    #[test]
    fn forward_lut_matches_algebraic_form() {
        let lut = rle_fwd_lut();
        for (i, v) in lut.iter().enumerate() {
            let expected = if i < 128 { 2 * i } else { 511 - 2 * i };
            assert_eq!(*v as usize, expected, "LUT[{i}]");
        }
    }

    /// `spec/05` §5.2: `INV_LUT[0..=2] = 0` and otherwise
    /// `LUT[INV_LUT[k]] = k - 2`.
    #[test]
    fn inverse_lut_inverts_forward_lut() {
        let fwd = rle_fwd_lut();
        let inv = rle_inv_lut();
        assert_eq!(inv[0], 0);
        assert_eq!(inv[1], 0);
        assert_eq!(inv[2], 0);
        for (k, &s) in inv.iter().enumerate().skip(2) {
            assert_eq!(fwd[s as usize] as usize, k - 2, "INV_LUT[{k}] = {s}");
        }
    }

    /// `spec/02` §5 step C: the extracted reciprocal-multiply LUT is
    /// exactly `floor(2^32 / i)` for `i >= 2`, with the two boundary
    /// entries `LUT[0] = 0` and `LUT[1] = 0xffffffff` (`2^32` overflows
    /// a `u32`). Pins that a re-extraction that changed any word would
    /// surface here rather than silently.
    #[test]
    fn recip_lut_matches_floor_reciprocal_form() {
        let lut = recip_lut();
        assert_eq!(lut[0], 0, "LUT[0]");
        assert_eq!(lut[1], u32::MAX, "LUT[1] = 2^32 - 1");
        for (i, &v) in lut.iter().enumerate().skip(2) {
            let expected = ((1u64 << 32) / i as u64) as u32;
            assert_eq!(v, expected, "LUT[{i}] = floor(2^32 / {i})");
        }
    }

    /// Steady-state operating band of the modern coder: `range` after
    /// any renormalisation lies in `[2^23 + 1, 2^31]` (`spec/02` §2).
    const RANGE_MIN: u64 = (1 << 23) + 1;
    const RANGE_MAX: u64 = 1 << 31;

    /// Naive reciprocal-multiply built from the LUT, mirroring the
    /// `spec/02` §5 step-C "reciprocal-multiply" acceleration name.
    fn q_recip(range: u64, total: usize) -> u64 {
        (range * recip_lut()[total] as u64) >> 32
    }

    /// `spec/02` §5 invariant box: at a **power-of-two** `total` the
    /// reference's reciprocal-multiply and the crate's exact
    /// `q = range / total` coincide across the whole operating band —
    /// which is *why* the byte-exact cross-decoder pins in
    /// `tests/reference_pins.rs` are held to power-of-two pixel counts.
    /// Checked exhaustively at the band endpoints plus a deterministic
    /// sweep for every `total = 2^k` with `2 <= total < 2048`.
    #[test]
    fn recip_multiply_equals_exact_division_at_power_of_two_totals() {
        for k in 1..11u32 {
            let total = 1usize << k; // 2, 4, ..., 1024
            let mut r = RANGE_MIN;
            let mut lcg: u64 = 0x9E37_79B9_7F4A_7C15 ^ total as u64;
            for _ in 0..4096 {
                let exact = r / total as u64;
                assert_eq!(q_recip(r, total), exact, "total={total} range={r}");
                assert_eq!(r >> k, exact, "shift form total={total} range={r}");
                // Deterministic walk across the band.
                lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
                r = RANGE_MIN + (lcg >> 33) % (RANGE_MAX - RANGE_MIN + 1);
            }
            // Explicit endpoints.
            for &r in &[RANGE_MIN, RANGE_MAX, RANGE_MAX - 1] {
                assert_eq!(
                    q_recip(r, total),
                    r / total as u64,
                    "endpoint total={total}"
                );
            }
        }
    }

    /// The converse that pins the open item: at every **non**-power-of-
    /// two `total` the naive reciprocal-multiply diverges from exact
    /// division by 1 somewhere in the operating band. This is the
    /// machine-checked form of the `reference_pins.rs` prose ("at a
    /// non-power-of-two total the two are not bit-identical") and the
    /// reason the crate cannot claim byte-exact reference parity at
    /// those totals without resolving the open `0x180001050` quotient
    /// derivation (`spec/02` §9 item 1, `spec/04` §9 item 2).
    #[test]
    fn recip_multiply_diverges_from_exact_division_at_non_power_of_two_totals() {
        // The smallest few non-pow2 totals, plus a mid-band spread.
        let sample = [3usize, 5, 6, 7, 9, 10, 11, 100, 255, 1000, 2047];
        for &total in &sample {
            assert!(!total.is_power_of_two(), "sample must be non-pow2: {total}");
            let mut found = false;
            let mut lcg: u64 = 0xD1B5_4A32_D192_ED03 ^ total as u64;
            for _ in 0..200_000 {
                lcg = lcg.wrapping_mul(6364136223846793005).wrapping_add(1);
                let r = RANGE_MIN + (lcg >> 33) % (RANGE_MAX - RANGE_MIN + 1);
                if q_recip(r, total) != r / total as u64 {
                    // Divergence is always an under-estimate by exactly 1
                    // (the reciprocal is a floor, never a ceil).
                    assert_eq!(
                        q_recip(r, total) + 1,
                        r / total as u64,
                        "delta total={total}"
                    );
                    found = true;
                    break;
                }
            }
            assert!(found, "expected a divergence for non-pow2 total={total}");
        }
    }
}
