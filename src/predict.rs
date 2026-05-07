//! Spatial predictor + cross-plane decorrelation per `spec/03`.
//!
//! - **Row 0** of every plane uses the **left** predictor (cumulative
//!   8-bit sum of the row's residuals).
//! - **Rows ≥ 1** use the **JPEG-LS clamped median** predictor with
//!   the `TL = L = plane[y-1][W-1]` first-column rule (Strategy A
//!   in `spec/06` §3.6).
//! - **RGB-family frames** apply `R += G; B += G` on the final
//!   pixel buffer; the alpha plane (RGBA) is unchanged.

/// Apply the in-place left-then-clamped-MED reconstruction to a
/// single plane. `plane` is `width * height` bytes laid out row-major.
pub fn apply_plane_inverse(plane: &mut [u8], width: usize, height: usize) {
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
        // First column: TL = L = plane[y-1][W-1] -> gradient = T.
        let t = plane[prev_off];
        plane[row_off] = plane[row_off].wrapping_add(t);
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

/// Forward (encoder-side) form: produce residuals from a fully-
/// reconstructed plane. Test-only.
#[cfg(test)]
pub fn apply_plane_forward(plane: &[u8], width: usize, height: usize) -> Vec<u8> {
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
        let t = plane[prev_off];
        out[row_off] = plane[row_off].wrapping_sub(t);
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

/// Forward G-pivot decorrelation (encoder-side, test-only): R -= G;
/// B -= G.
#[cfg(test)]
pub fn cross_plane_decorrelate_rgb_forward(r: &mut [u8], g: &[u8], b: &mut [u8]) {
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
}
