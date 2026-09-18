//! Spatial predictor + cross-plane decorrelation per `spec/03`.
//!
//! - **Row 0** of every plane uses the **left** predictor (cumulative
//!   8-bit sum of the row's residuals).
//! - **Rows ≥ 1** use the **JPEG-LS clamped median** predictor.
//! - **RGB-family frames** apply `R += G; B += G` on the final
//!   pixel buffer; the alpha plane (RGBA) is unchanged.
//!
//! ## Per-family predictor selection (round-451 state)
//!
//! Three first-column rules plus a dedicated YUY2 predictor, all
//! discriminated by black-box cross-validation against the
//! independent third-party decoder (`tests/blackbox_encode_pins.rs`):
//!
//! - **Rule B** (`TL = plane[y-2][W-1]` for `y >= 2`, Rule A at
//!   `y == 1`) — the modern arithmetic RGB(A) types (2 / 4 / 8) and
//!   the legacy type-7 path, signed-gradient median
//!   ([`clamped_med`]). Oracle-confirmed for the modern path (round
//!   124, re-confirmed round 451 across every content class).
//! - **[`FirstColRule::Yuv`]** (row 1 predicts `L`; Rule-B median
//!   for rows ≥ 2, signed gradient) — the **YV12 /
//!   reduced-resolution** families (types 10 / 11). Round-451
//!   recovery; closes `spec/06` §6.4 for YV12.
//! - **[`apply_plane_inverse_yuy2`]** — the **YUY2** family
//!   (type 3): raw second row-0 luma sample, plain-`L` first chunk
//!   of row 1 (4 luma / 2 chroma lanes), and the 8-bit-**wrapping**
//!   median ([`clamped_med_wrap`]) elsewhere, Rule-B first column
//!   for rows ≥ 2. Round-451 second-pass recovery; closes `spec/06`
//!   §6.4 for YUY2.
//! - **Rule A** (`TL = L` ⇒ predictor `T`) — the original
//!   `spec/03` §3.3.3 / `spec/06` §3.3–§3.6 "Strategy A" reading. The
//!   docs' round-8 validation against the vendor-encoded corpus
//!   (`spec/03` §3.3.3 / `spec/06` §3.6 validation-corrected
//!   blockquotes, 2026-09-12) recorded it as an **erratum**: Rule A
//!   never matches a vendor RGB24 / RGB32 / RGBA arithmetic stream
//!   with `H >= 3`, on the SIMD path (type 4) and the type-2 / type-8
//!   paths alike — Rule B (row 1 `TL = L`, rows `>= 2`
//!   `TL = plane[y-2][W-1]`) is the single rule for every RGB-family
//!   plane, which is what this crate has shipped since round 124.
//!   `roundtrip_tests::vendor_rgb_family_streams_decode_only_under_rule_b`
//!   pins it per vendor stream. No shipping path selects Rule A; the
//!   variant stays for the tests that discriminate it.

/// Selects the first-column-of-row rule for the inverse predictor.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub(crate) enum FirstColRule {
    /// `TL = L = plane[y-1][W-1]`. The "wrap-around" rule used
    /// unconditionally by the YV12 / YUY2 / reduced-resolution
    /// decode paths (types 3 / 10 / 11) per `spec/06` §3.8 — their
    /// chroma-subsampled plane widths are always 4-byte-aligned, so
    /// the `0x180009f30` predictor never takes a `width % 4` Rule-B
    /// branch. Also the `y == 1` fallback of Rule B (no `y - 2` row).
    ///
    /// Round 451: no shipping decode/encode path selects Rule A any
    /// more — the YV12 / YUY2 / reduced-res families moved to the
    /// oracle-recovered [`FirstColRule::Yuv`] (spec/06 §3.8 flagged
    /// as an erratum candidate; §6.4 closed by black-box
    /// cross-testing). The variant stays for the predictor unit
    /// tests that pin the historical rule's algebra.
    #[cfg_attr(not(test), allow(dead_code))]
    A,
    /// `TL = plane[y-2][W-1]` for `y >= 2` (Rule A for `y == 1`).
    /// The modern arithmetic RGB(A) path (types 2/4/8) and the
    /// legacy type-7 path both use this (`spec/06` §3.2 /
    /// `spec/07` §9.1 item 7b; oracle-confirmed for the modern path
    /// in round 124 and vendor-corpus-confirmed on every RGB24 /
    /// RGB32 / RGBA arithmetic stream with `H >= 3` — `spec/03`
    /// §3.3.3 / `spec/06` §3.6 validation-corrected blockquotes,
    /// 2026-09-12).
    B,
    /// Round-451 oracle-recovered first-column rule for the
    /// **YV12 / YUY2 / reduced-resolution** families (types 3 / 10 /
    /// 11): `TL = plane[y-2][W-1]` for `y >= 2` exactly as Rule B,
    /// but at `y == 1` the `0x180009f30` predictor's carry enters
    /// the row holding `T` (`plane[0][0]`), not `L` — so the median
    /// `MED(L, T, T)` collapses to `L = plane[0][W-1]` (Rule A / B
    /// collapse to `T` there instead). This closes `spec/06` §6.4
    /// (the non-RGB24 row-0 → row-1 carry initialisation, expressly
    /// left open pending cross-testing): black-box cross-validation
    /// discriminates the three candidates — under this rule the
    /// independent third-party decoder reconstructs YV12 / YUY2
    /// frames byte-exactly at every probed geometry and content
    /// class, while both the §3.8 "Strategy A everywhere" reading
    /// and a `TL = 0` carry mis-decode gradient content from
    /// `(0, 1)` / `(0, 2)` onward (`tests/blackbox_encode_pins.rs`).
    /// `spec/06` §3.8's "same `TL = L` (Strategy A)" sentence is
    /// flagged as an erratum candidate.
    Yuv,
}

/// Apply the in-place left-then-clamped-MED reconstruction to a
/// single plane using **Rule A** first-column. `plane` is
/// `width * height` bytes laid out row-major. Test-only convenience
/// wrapper: the shipping decode paths call
/// [`apply_plane_inverse_with_rule`] with an explicit rule
/// ([`FirstColRule::A`] for the YV12 / YUY2 / reduced-res families
/// per `spec/06` §3.8, [`FirstColRule::B`] for the modern / legacy
/// RGB(A) paths).
#[cfg(test)]
pub fn apply_plane_inverse(plane: &mut [u8], width: usize, height: usize) {
    apply_plane_inverse_with_rule(plane, width, height, FirstColRule::A);
}

/// Apply the in-place inverse predictor with a configurable
/// first-column-of-row rule.
pub(crate) fn apply_plane_inverse_with_rule(
    plane: &mut [u8],
    width: usize,
    height: usize,
    rule: FirstColRule,
) {
    debug_assert_eq!(plane.len(), width * height);
    if width == 0 || height == 0 {
        return;
    }
    // Row 0: cumulative sum.
    for x in 1..width {
        plane[x] = plane[x].wrapping_add(plane[x - 1]);
    }
    // Rows 1..H-1.
    for y in 1..height {
        let row_off = y * width;
        let prev_off = (y - 1) * width;
        // First column. Rule A: TL = L = plane[y-1][W-1] -> gradient
        // collapses to T. Rule B (y >= 2): TL = plane[y-2][W-1];
        // gradient = L_wrap + T - TL where L_wrap = plane[y-1][W-1].
        let pred_first = match rule {
            FirstColRule::A => plane[prev_off],
            FirstColRule::B | FirstColRule::Yuv if y >= 2 => {
                let l = plane[prev_off + width - 1]; // plane[y-1][W-1]
                let t = plane[prev_off];
                let tl = plane[(y - 2) * width + width - 1]; // plane[y-2][W-1]
                clamped_med(l, t, tl)
            }
            FirstColRule::B => plane[prev_off],
            // y == 1: the YUV predictor's carry holds T, so
            // MED(L, T, T) = L (round-451 oracle recovery; the
            // clamp is the identity on L).
            FirstColRule::Yuv => plane[prev_off + width - 1],
        };
        plane[row_off] = plane[row_off].wrapping_add(pred_first);
        // Columns 1..W-1.
        for x in 1..width {
            let l = plane[row_off + x - 1];
            let t = plane[prev_off + x];
            let tl = plane[prev_off + x - 1];
            let pred = clamped_med(l, t, tl);
            plane[row_off + x] = plane[row_off + x].wrapping_add(pred);
        }
    }
}

/// YV12-family inverse predictor with an explicit `TL` seed for the
/// row-0 → row-1 transition (`spec/06` §3.8, validated 2026-09-12).
/// The `0x180009f30` predictor's three-plane vector loop seeds `TL`
/// at (row 1, col 0) with `plane[0]` — which collapses the median to
/// `L` and is what [`FirstColRule::Yuv`] hard-codes — but for frames
/// below that loop's size (`W + 4 > ((W*H/4 + W/2) & !3)`, e.g. 4x2)
/// the seed is never written and `TL` is whatever byte precedes the
/// plane in the output buffer. This variant takes that byte as
/// `tl_seed`; with `tl_seed == plane[0]` it is bit-identical to the
/// `Yuv` rule. Vendor-quirk path only (`decode_frame_vendor_layout`).
pub(crate) fn apply_plane_inverse_yuv_seeded(
    plane: &mut [u8],
    width: usize,
    height: usize,
    tl_seed: u8,
) {
    debug_assert_eq!(plane.len(), width * height);
    if width == 0 || height == 0 {
        return;
    }
    for x in 1..width {
        plane[x] = plane[x].wrapping_add(plane[x - 1]);
    }
    for y in 1..height {
        let row_off = y * width;
        let prev_off = (y - 1) * width;
        let l = plane[prev_off + width - 1];
        let t = plane[prev_off];
        let tl = if y == 1 {
            tl_seed
        } else {
            plane[(y - 2) * width + width - 1]
        };
        plane[row_off] = plane[row_off].wrapping_add(clamped_med(l, t, tl));
        for x in 1..width {
            let l = plane[row_off + x - 1];
            let t = plane[prev_off + x];
            let tl = plane[prev_off + x - 1];
            plane[row_off + x] = plane[row_off + x].wrapping_add(clamped_med(l, t, tl));
        }
    }
}

/// Forward (encoder-side) form: produce residuals from a fully-
/// reconstructed plane using **Rule A**. Test-only.
#[cfg(test)]
pub fn apply_plane_forward(plane: &[u8], width: usize, height: usize) -> Vec<u8> {
    apply_plane_forward_with_rule(plane, width, height, FirstColRule::A)
}

/// Forward predictor with configurable first-column-of-row rule.
/// Encode-direction primitive (consumed by [`crate::encoder`]).
pub(crate) fn apply_plane_forward_with_rule(
    plane: &[u8],
    width: usize,
    height: usize,
    rule: FirstColRule,
) -> Vec<u8> {
    debug_assert_eq!(plane.len(), width * height);
    let mut out = vec![0u8; plane.len()];
    if width == 0 || height == 0 {
        return out;
    }
    // Row 0.
    out[0] = plane[0];
    for x in 1..width {
        out[x] = plane[x].wrapping_sub(plane[x - 1]);
    }
    // Rows 1..H-1.
    for y in 1..height {
        let row_off = y * width;
        let prev_off = (y - 1) * width;
        // First column.
        let pred_first = match rule {
            FirstColRule::A => plane[prev_off],
            FirstColRule::B | FirstColRule::Yuv if y >= 2 => {
                let l = plane[prev_off + width - 1];
                let t = plane[prev_off];
                let tl = plane[(y - 2) * width + width - 1];
                clamped_med(l, t, tl)
            }
            FirstColRule::B => plane[prev_off],
            // y == 1: MED(L, T, T) = L — see the inverse form.
            FirstColRule::Yuv => plane[prev_off + width - 1],
        };
        out[row_off] = plane[row_off].wrapping_sub(pred_first);
        for x in 1..width {
            let l = plane[row_off + x - 1];
            let t = plane[prev_off + x];
            let tl = plane[prev_off + x - 1];
            let pred = clamped_med(l, t, tl);
            out[row_off + x] = plane[row_off + x].wrapping_sub(pred);
        }
    }
    out
}

/// JPEG-LS clamped median predictor (`spec/03` §3.3): the median of
/// `{L, T, L+T-TL}`.
#[inline]
fn clamped_med(l: u8, t: u8, tl: u8) -> u8 {
    let l = l as i32;
    let t = t as i32;
    let tl = tl as i32;
    let min_lt = l.min(t);
    let max_lt = l.max(t);
    let gradient = l + t - tl;
    let pred = gradient.clamp(min_lt, max_lt);
    pred as u8
}

/// The YUY2 predictor's clamped median: identical to [`clamped_med`]
/// except the gradient `L + T - TL` is reduced **mod 256 before**
/// clamping (round-451 black-box recovery — the `0x180009f30` YUY2
/// path computes the gradient in a byte-sized lane, so an over- or
/// underflowing gradient wraps and then clamps, where the RGB / YV12
/// paths clamp the signed-widened gradient). Observable on any
/// neighbourhood with `L + T - TL ∉ [0, 255]`.
#[inline]
fn clamped_med_wrap(l: u8, t: u8, tl: u8) -> u8 {
    let g = l.wrapping_add(t).wrapping_sub(tl);
    let min_lt = l.min(t);
    let max_lt = l.max(t);
    g.clamp(min_lt, max_lt)
}

/// Round-451 oracle-recovered **YUY2 plane predictor** (inverse
/// direction), covering the type-3 family's luma and chroma planes.
/// Byte-exactly reproduces the independent third-party decoder's
/// reconstruction on every probed geometry (2×8 .. 128×96, odd
/// chroma widths) and content class (gradient, zero-heavy, two
/// independent full-random streams) — 11/11 EXACT in the round-451
/// capture. The recovered structure:
///
/// * **Row 0** — cumulative left sum; the **luma** plane additionally
///   stores its second sample raw (`plane[0][1] = residual`, the
///   packed first macropixel seeds both `Y0` and `Y1`).
/// * **Row 1, `x < 4` (luma) / `x < 2` (chroma)** — the predictor is
///   plain `L` (the previous sample in linear memory; for `x = 0`
///   that is `plane[0][W-1]`): the first SIMD chunk of the row
///   enters with no usable `T`/`TL` lanes.
/// * **Everywhere else** — the 8-bit-wrapping clamped median
///   ([`clamped_med_wrap`]), with the first column of rows ≥ 2
///   taking the Rule-B `TL = plane[y-2][W-1]`.
///
/// This supersedes `spec/06` §3.8's "same `TL = L` (Strategy A)"
/// description of the `0x180009f30` predictor for the YUY2
/// coordinator and closes the §6.4 open item for YUY2; the YV12
/// coordinator measurably does NOT share it (its planes reconstruct
/// under the signed-gradient median + [`FirstColRule::Yuv`] and
/// diverge under this rule on random content), so the two families
/// keep separate predictor paths.
pub(crate) fn apply_plane_inverse_yuy2(plane: &mut [u8], width: usize, height: usize, luma: bool) {
    debug_assert_eq!(plane.len(), width * height);
    if width == 0 || height == 0 {
        return;
    }
    let special = if luma { 4 } else { 2 };
    // Row 0: cumulative sum; luma keeps plane[1] raw and continues
    // the running sum from it.
    let mut start = 1usize;
    if luma && width >= 2 {
        // plane[1] stays as-is (raw seed).
        start = 2;
    }
    for x in start..width {
        plane[x] = plane[x].wrapping_add(plane[x - 1]);
    }
    for y in 1..height {
        let row_off = y * width;
        let prev_off = row_off - width;
        for x in 0..width {
            let pred = if y == 1 && x < special {
                // First chunk of row 1: plain L (linear memory —
                // x = 0 reads the previous row's last sample).
                plane[row_off + x - 1]
            } else if x == 0 {
                let l = plane[row_off - 1];
                let t = plane[prev_off];
                let tl = plane[(y - 2) * width + width - 1];
                clamped_med_wrap(l, t, tl)
            } else {
                let l = plane[row_off + x - 1];
                let t = plane[prev_off + x];
                let tl = plane[prev_off + x - 1];
                clamped_med_wrap(l, t, tl)
            };
            plane[row_off + x] = plane[row_off + x].wrapping_add(pred);
        }
    }
}

/// Forward (encoder-side) counterpart of
/// [`apply_plane_inverse_yuy2`]; produces the residual stream the
/// inverse integrates back to `plane`.
pub(crate) fn apply_plane_forward_yuy2(
    plane: &[u8],
    width: usize,
    height: usize,
    luma: bool,
) -> Vec<u8> {
    debug_assert_eq!(plane.len(), width * height);
    let mut out = vec![0u8; width * height];
    if width == 0 || height == 0 {
        return out;
    }
    let special = if luma { 4 } else { 2 };
    out[0] = plane[0];
    let mut start = 1usize;
    if luma && width >= 2 {
        out[1] = plane[1]; // raw seed
        start = 2;
    }
    for x in start..width {
        out[x] = plane[x].wrapping_sub(plane[x - 1]);
    }
    for y in 1..height {
        let row_off = y * width;
        let prev_off = row_off - width;
        for x in 0..width {
            let pred = if y == 1 && x < special {
                plane[row_off + x - 1]
            } else if x == 0 {
                let l = plane[row_off - 1];
                let t = plane[prev_off];
                let tl = plane[(y - 2) * width + width - 1];
                clamped_med_wrap(l, t, tl)
            } else {
                let l = plane[row_off + x - 1];
                let t = plane[prev_off + x];
                let tl = plane[prev_off + x - 1];
                clamped_med_wrap(l, t, tl)
            };
            out[row_off + x] = plane[row_off + x].wrapping_sub(pred);
        }
    }
    out
}

/// Reverse the cross-plane G-pivot decorrelation in place: R += G;
/// B += G. Each slice has the same length (`spec/03` §4).
pub fn cross_plane_decorrelate_rgb(r: &mut [u8], g: &[u8], b: &mut [u8]) {
    debug_assert_eq!(r.len(), g.len());
    debug_assert_eq!(b.len(), g.len());
    for i in 0..g.len() {
        r[i] = r[i].wrapping_add(g[i]);
        b[i] = b[i].wrapping_add(g[i]);
    }
}

/// Forward G-pivot decorrelation (encoder-side): R -= G; B -= G.
/// Encode-direction primitive (consumed by [`crate::encoder`]).
pub(crate) fn cross_plane_decorrelate_rgb_forward(r: &mut [u8], g: &[u8], b: &mut [u8]) {
    debug_assert_eq!(r.len(), g.len());
    debug_assert_eq!(b.len(), g.len());
    for i in 0..g.len() {
        r[i] = r[i].wrapping_sub(g[i]);
        b[i] = b[i].wrapping_sub(g[i]);
    }
}

// ─────────────────────── tests ───────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_roundtrip_small() {
        let plane: Vec<u8> = (0..64).map(|i| (i * 17 + 3) as u8).collect();
        let residuals = apply_plane_forward(&plane, 8, 8);
        let mut recon = residuals.clone();
        apply_plane_inverse(&mut recon, 8, 8);
        assert_eq!(recon, plane);
    }

    #[test]
    fn predictor_roundtrip_uneven() {
        let plane: Vec<u8> = (0..(13 * 7)).map(|i| ((i * 23) ^ 0xa5) as u8).collect();
        let residuals = apply_plane_forward(&plane, 13, 7);
        let mut recon = residuals.clone();
        apply_plane_inverse(&mut recon, 13, 7);
        assert_eq!(recon, plane);
    }

    #[test]
    fn predictor_handles_single_row() {
        let plane: Vec<u8> = vec![10, 20, 30, 40, 50];
        let residuals = apply_plane_forward(&plane, 5, 1);
        assert_eq!(residuals[0], 10);
        assert_eq!(residuals[1], 10);
        assert_eq!(residuals[2], 10);
        let mut recon = residuals.clone();
        apply_plane_inverse(&mut recon, 5, 1);
        assert_eq!(recon, plane);
    }

    #[test]
    fn cross_plane_roundtrip() {
        let mut r = vec![1u8, 2, 3, 4];
        let g = vec![10u8, 20, 30, 40];
        let mut b = vec![100u8, 99, 98, 97];
        let r0 = r.clone();
        let b0 = b.clone();
        cross_plane_decorrelate_rgb_forward(&mut r, &g, &mut b);
        cross_plane_decorrelate_rgb(&mut r, &g, &mut b);
        assert_eq!(r, r0);
        assert_eq!(b, b0);
    }

    /// Sanity-check the clamped median itself.
    #[test]
    fn clamped_med_known_values() {
        // L=10, T=20, TL=15 -> gradient=15 -> in [10,20] -> 15.
        assert_eq!(clamped_med(10, 20, 15), 15);
        // L=10, T=20, TL=5 -> gradient=25 -> clamp to max=20.
        assert_eq!(clamped_med(10, 20, 5), 20);
        // L=10, T=20, TL=25 -> gradient=5 -> clamp to min=10.
        assert_eq!(clamped_med(10, 20, 25), 10);
    }

    #[test]
    fn predictor_rule_b_roundtrip_4x4() {
        // 4 rows -> exercises the y >= 2 Rule-B path.
        let plane: Vec<u8> = (0..16).map(|i| (i * 19 + 5) as u8).collect();
        let residuals = apply_plane_forward_with_rule(&plane, 4, 4, FirstColRule::B);
        let mut recon = residuals.clone();
        apply_plane_inverse_with_rule(&mut recon, 4, 4, FirstColRule::B);
        assert_eq!(recon, plane);
    }

    #[test]
    fn predictor_rule_b_roundtrip_uneven() {
        // 11 rows of 7 — multiple rows trigger the y >= 2 path.
        let plane: Vec<u8> = (0..(11 * 7))
            .map(|i| ((i as u32).wrapping_mul(173) ^ 0x37) as u8)
            .collect();
        let residuals = apply_plane_forward_with_rule(&plane, 7, 11, FirstColRule::B);
        let mut recon = residuals.clone();
        apply_plane_inverse_with_rule(&mut recon, 7, 11, FirstColRule::B);
        assert_eq!(recon, plane);
    }

    #[test]
    fn predictor_rule_b_y1_falls_back_to_rule_a() {
        // For 2-row planes, Rule B has no `y-2`; falls back to Rule A.
        // Verify by comparing residual stream byte-for-byte.
        let plane: Vec<u8> = (0..16).map(|i| (i * 23 + 11) as u8).collect();
        let res_a = apply_plane_forward_with_rule(&plane, 8, 2, FirstColRule::A);
        let res_b = apply_plane_forward_with_rule(&plane, 8, 2, FirstColRule::B);
        assert_eq!(res_a, res_b);
    }

    /// Historical-rule algebra pin (Rule A is no longer selected by
    /// any shipping path — see the module header): Rule-A
    /// reconstruction inverts Rule-A residuals exactly, on a multi-row plane
    /// whose first columns straddle so Rule A and Rule B genuinely
    /// diverge — i.e. the choice is observable, not degenerate.
    #[test]
    fn rule_a_yuv_first_column_inverts_itself() {
        // 4×4 plane like a chroma quadrant; first columns vary across
        // rows so the rule choice matters (cf.
        // `predictor_rule_b_diverges_from_rule_a_on_row_2`).
        let plane: Vec<u8> = vec![
            10, 20, 30, 40, // row 0
            50, 60, 70, 80, // row 1
            90, 100, 110, 120, // row 2
            130, 140, 150, 160, // row 3
        ];
        // Encode + decode both under Rule A (the YUV-family selection).
        let res_a = apply_plane_forward_with_rule(&plane, 4, 4, FirstColRule::A);
        let mut recon = res_a.clone();
        apply_plane_inverse_with_rule(&mut recon, 4, 4, FirstColRule::A);
        assert_eq!(recon, plane, "Rule A round-trip must be lossless");
        // And the residual stream genuinely differs from Rule B on this
        // plane, so the YUV path's Rule-A choice is observable: decoding
        // the Rule-A residuals under Rule B would corrupt rows >= 2.
        let res_b = apply_plane_forward_with_rule(&plane, 4, 4, FirstColRule::B);
        assert_ne!(
            res_a, res_b,
            "Rule A and Rule B must diverge so the §3.8 choice is testable"
        );
        let mut wrong = res_a.clone();
        apply_plane_inverse_with_rule(&mut wrong, 4, 4, FirstColRule::B);
        assert_ne!(
            wrong, plane,
            "decoding Rule-A residuals under Rule B must NOT reproduce the plane"
        );
    }

    #[test]
    fn predictor_rule_b_diverges_from_rule_a_on_row_2() {
        // Rule A and Rule B differ at row >= 2 col 0 when L_wrap and T
        // straddle TL_far. Build a plane where the residuals at (2, 0)
        // differ between the two rules.
        let plane: Vec<u8> = vec![
            10, 20, 30, 40, // row 0
            50, 60, 70, 80, // row 1
            90, 100, 110, 120, // row 2
            130, 140, 150, 160, // row 3
        ];
        let res_a = apply_plane_forward_with_rule(&plane, 4, 4, FirstColRule::A);
        let res_b = apply_plane_forward_with_rule(&plane, 4, 4, FirstColRule::B);
        // Row 0 and row 1 first-col differ only on rows >= 2 with B.
        assert_eq!(res_a[..8], res_b[..8]);
        // The (2, 0) and (3, 0) residuals depend on the rule.
        assert!(
            res_a[8] != res_b[8] || res_a[12] != res_b[12],
            "Rule A and Rule B should differ for at least one first-column residual"
        );
    }
}
