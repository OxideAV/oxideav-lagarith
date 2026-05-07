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
//! without depending on the LUT. The decoder uses the search loop;
//! the LUT is therefore not shipped here.

use std::sync::OnceLock;

/// CSV source for the residual-RLE forward LUT (256 × u32, low byte
/// is the value; upper bytes always zero — see CSV header block).
const RLE_FWD_LUT_CSV: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tables/01-residual-rle-decoder-lut.csv",
));

/// CSV source for the residual-RLE inverse LUT (256 × u8). Used by
/// the test-only encoder to pick the supplement byte that decodes
/// back to a desired run length.
#[cfg(test)]
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
/// Test-only because the decoder side uses [`rle_fwd_lut`]; the
/// encoder mirror is currently `#[cfg(test)]` (round 1 ships
/// decoder + roundtrip tests only).
#[cfg(test)]
pub fn rle_inv_lut() -> &'static [u8; 256] {
    static CACHE: OnceLock<[u8; 256]> = OnceLock::new();
    CACHE.get_or_init(|| parse_u8_csv::<256>(RLE_INV_LUT_CSV))
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

#[cfg(test)]
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
}
