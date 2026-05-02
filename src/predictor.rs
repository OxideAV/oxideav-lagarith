//! Lagarith spatial predictor.
//!
//! Each plane (after entropy decoding) is a flat `width*height` buffer of
//! 8-bit residuals. The inverse predictor mutates the buffer in place,
//! turning residuals into reconstructed samples.
//!
//! The predictor itself is the same three-corner gradient median used by
//! Huffyuv / LOCO-I / JPEG-LS, **but** with one Lagarith-specific quirk:
//! the gradient `G = L + T - TL` is kept as a **signed 9-bit value** —
//! it is **not** masked to 8 bits before taking the median. This is the
//! single behaviour the trace doc flags as the most common silent-
//! corruption bug for independent decoders.
//!
//! Boundary conditions follow the trace:
//!
//! * Row 0: left-only prediction with `L = 0` for `out[0]`.
//! * Row 1: RGB and YUV bootstrap differently. For YUV the seed `TL` of
//!   row 1 is `out[0]` of row 0; for RGB the seed `TL` is `L` itself,
//!   which collapses the predictor to "top-only" for the first pixel of
//!   the second row.
//! * Row >= 2: full median prediction.

/// Boundary-condition flavour. RGB streams collapse the row-1 seed to a
/// "top-only" predictor; YUV streams seed `TL` from row-0's first pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PredictMode {
    Rgb,
    Yuv,
}

/// Three-sample median (signed inputs, returns one of the three).
#[inline]
fn median3(a: i32, b: i32, c: i32) -> i32 {
    if a > b {
        if b > c {
            b
        } else if a > c {
            c
        } else {
            a
        }
    } else if a > c {
        a
    } else if b > c {
        c
    } else {
        b
    }
}

/// Run the inverse Lagarith predictor over a single plane, in place.
///
/// `data` is the flat `width*height` byte buffer of decoded residuals;
/// after this call it holds reconstructed samples.
pub fn unpredict_plane(data: &mut [u8], width: usize, height: usize, mode: PredictMode) {
    if width == 0 || height == 0 {
        return;
    }
    debug_assert_eq!(data.len(), width.checked_mul(height).unwrap_or(0));

    // Row 0 — left-only with `L = 0` seed.
    let mut left: u8 = 0;
    for x in 0..width {
        left = left.wrapping_add(data[x]);
        data[x] = left;
    }
    if height == 1 {
        return;
    }

    // Row 1 — bootstrap with the first row's data.
    {
        // For RGB the spec collapses to "top-only" for the first pixel of
        // row 1 (TL = L). For YUV the seed TL is row-0's first pixel.
        let row1_offset = width;
        let row0_first = data[0] as i32;
        let l_seed: i32;
        let tl_seed: i32;
        match mode {
            PredictMode::Rgb => {
                // top-only for x=0 of row 1: predicted = T = data[0]
                // (because median(L, T, L+T-L) = median(L, T, T) = T when
                // L is also seeded from itself; we just set L = T = data[0]).
                l_seed = row0_first;
                tl_seed = row0_first;
            }
            PredictMode::Yuv => {
                l_seed = 0;
                tl_seed = row0_first;
            }
        }
        unpredict_row(
            data,
            row1_offset,
            width,
            // top row begins at offset 0
            0,
            l_seed,
            tl_seed,
        );
    }

    // Rows 2..height — full median.
    for y in 2..height {
        let row_off = y * width;
        let top_off = (y - 1) * width;
        // L seed for x = 0 is the previous row's last column (carried over).
        let l_seed = data[top_off - 1] as i32;
        // TL seed is the same column from the row above x = -1 — i.e.
        // two-rows-up's last column.
        let tl_seed = data[top_off.saturating_sub(width + 1)] as i32;
        unpredict_row(data, row_off, width, top_off, l_seed, tl_seed);
    }
}

/// Apply the inverse median predictor to a single row that has a top
/// neighbour available.
///
/// `row_off` is the start of the destination row in `data`; `top_off`
/// is the start of the row immediately above. `l_seed` is the value of
/// the "left" neighbour for column 0 (`out[row, -1]`); `tl_seed` is the
/// "above-left" neighbour for column 0 (`out[row-1, -1]`).
fn unpredict_row(
    data: &mut [u8],
    row_off: usize,
    width: usize,
    top_off: usize,
    l_seed: i32,
    tl_seed: i32,
) {
    let mut left = l_seed;
    let mut top_left = tl_seed;
    for x in 0..width {
        let top = data[top_off + x] as i32;
        // Lagarith quirk: gradient is kept as signed 9-bit, NOT masked to 8.
        let gradient = left + top - top_left;
        let predicted = median3(left, top, gradient);
        let residual = data[row_off + x];
        let out = (predicted + residual as i32) & 0xFF;
        data[row_off + x] = out as u8;
        // Slide window for next column.
        top_left = top;
        left = out;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn median3_is_actual_median() {
        assert_eq!(median3(1, 2, 3), 2);
        assert_eq!(median3(3, 1, 2), 2);
        assert_eq!(median3(2, 3, 1), 2);
        assert_eq!(median3(5, 5, 1), 5);
        assert_eq!(median3(-1, 0, 1), 0);
        // 9-bit gradient: ensure we accept values outside u8 range.
        assert_eq!(median3(255, 200, 300), 255);
        assert_eq!(median3(0, 100, -50), 0);
    }

    #[test]
    fn row0_left_pred_passthrough() {
        // If residuals are exactly the deltas of an arithmetic ramp, row
        // 0 must reconstruct the ramp.
        // residuals = original - left; original = 10,20,30,40,50
        // residuals = 10,10,10,10,10
        let mut residuals = vec![10u8, 10, 10, 10, 10];
        unpredict_plane(&mut residuals, 5, 1, PredictMode::Rgb);
        assert_eq!(residuals, vec![10, 20, 30, 40, 50]);
        // ditto for YUV bootstrap (only row 0; same path).
        let mut residuals = vec![10u8, 10, 10, 10, 10];
        unpredict_plane(&mut residuals, 5, 1, PredictMode::Yuv);
        assert_eq!(residuals, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn solid_zero_residuals_stay_zero() {
        // All-zero residuals must reconstruct an all-zero plane.
        let mut residuals = vec![0u8; 4 * 4];
        unpredict_plane(&mut residuals, 4, 4, PredictMode::Rgb);
        assert_eq!(residuals, vec![0u8; 4 * 4]);
        let mut residuals = vec![0u8; 4 * 4];
        unpredict_plane(&mut residuals, 4, 4, PredictMode::Yuv);
        assert_eq!(residuals, vec![0u8; 4 * 4]);
    }
}
