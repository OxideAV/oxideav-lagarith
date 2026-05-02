//! Per-plane Lagarith decode.
//!
//! Each plane begins with a 1-byte mode selector (`esc_count`) that
//! controls how the rest of the plane is decoded. Only the modes whose
//! bitstream layout is fully described in the trace doc are implemented:
//!
//! * `0xFF` — single-byte constant, fills the whole plane.
//! * `4` — uncompressed: `width * height` raw bytes verbatim.
//!
//! The range-coded modes (`esc_count` ∈ `{1,2,3}`) and the zero-run-only
//! modes (`{5,6,7}`) require the 53-entry sparse VLC for probability
//! magnitudes and the 256-entry probability rescale array. The trace doc
//! explicitly does **not** transcribe those tables; until they land in
//! the docs they return `Error::Unsupported`.
//!
//! After the entropy step, the per-plane median predictor (see
//! [`crate::predictor`]) is applied separately by the caller — these
//! routines deliver raw decoded residuals (or, for the constant /
//! uncompressed paths, raw plane bytes; the predictor is then a no-op).

use oxideav_core::{Error, Result};

/// Outcome of decoding one plane: the reconstructed bytes, plus a flag
/// indicating whether the predictor still needs to run on top.
#[derive(Debug)]
pub struct DecodedPlane {
    pub bytes: Vec<u8>,
    /// `true` for `0xFF` (single constant) — the spec says the predictor
    /// must NOT be applied because the constant is already the final
    /// pixel value. `false` for the entropy-coded paths.
    pub skip_predictor: bool,
}

/// Decode a plane that begins at `plane_start` inside `packet`. The plane
/// extends through `plane_end` (exclusive); the caller derives those
/// bounds from the per-frame offset table.
pub fn decode_plane(
    packet: &[u8],
    plane_start: usize,
    plane_end: usize,
    width: usize,
    height: usize,
) -> Result<DecodedPlane> {
    if plane_end > packet.len() || plane_start >= plane_end {
        return Err(Error::invalid("Lagarith: plane bounds out of range"));
    }
    let plane = &packet[plane_start..plane_end];
    let mode = *plane
        .first()
        .ok_or_else(|| Error::invalid("Lagarith: empty plane payload"))?;

    let pixels = width
        .checked_mul(height)
        .ok_or_else(|| Error::invalid("Lagarith: width*height overflow"))?;

    match mode {
        // Single constant plane.
        0xFF => {
            let value = *plane
                .get(1)
                .ok_or_else(|| Error::invalid("Lagarith: SOLID_PLANE missing constant byte"))?;
            Ok(DecodedPlane {
                bytes: vec![value; pixels],
                skip_predictor: true,
            })
        }
        // Uncompressed: width*height raw bytes after the mode byte.
        4 => {
            if plane.len() < 1 + pixels {
                return Err(Error::invalid(
                    "Lagarith: UNCOMPRESSED plane truncated (need width*height bytes)",
                ));
            }
            Ok(DecodedPlane {
                bytes: plane[1..1 + pixels].to_vec(),
                skip_predictor: false,
            })
        }
        // Range-coded modes — blocked on missing VLC + probability tables.
        1..=3 => Err(Error::unsupported(
            "Lagarith: range-coded planes (esc_count 1..3) require the 53-entry \
             probability VLC and 256-entry probability array, which are not yet \
             in docs/video/lagarith/. See README.md.",
        )),
        // Zero-run-only modes — same blocker.
        5..=7 => Err(Error::unsupported(
            "Lagarith: zero-run-only planes (esc_count 5..7) need the run-length \
             VLC tables, which are not yet in docs/video/lagarith/.",
        )),
        other => Err(Error::invalid(format!(
            "Lagarith: unsupported plane mode byte 0x{other:02x}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_plane_fills_constant() {
        // mode byte 0xFF, value 0x42, then dont-care padding
        let pkt = vec![0xFFu8, 0x42, 0, 0];
        let dp = decode_plane(&pkt, 0, pkt.len(), 4, 2).unwrap();
        assert_eq!(dp.bytes, vec![0x42; 8]);
        assert!(dp.skip_predictor);
    }

    #[test]
    fn uncompressed_plane_round_trips() {
        let mut pkt = vec![4u8];
        pkt.extend_from_slice(&[1, 2, 3, 4, 5, 6]);
        let dp = decode_plane(&pkt, 0, pkt.len(), 3, 2).unwrap();
        assert_eq!(dp.bytes, vec![1, 2, 3, 4, 5, 6]);
        assert!(!dp.skip_predictor);
    }

    #[test]
    fn arith_plane_reports_unsupported() {
        let pkt = vec![1u8, 0, 0, 0, 0];
        let err = decode_plane(&pkt, 0, pkt.len(), 4, 4).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
