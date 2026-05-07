//! Frame-type byte parsing + per-frame channel-offset table layout
//! per `spec/01` §1 / §2.

use crate::error::{Error, Result};

/// Recognised frame types this build's decoder accepts. Round 1
/// covers every type the spec calls out except YUY2 (3), legacy RGB
/// (7), YV12 (10), and reduced-resolution (11) — which are flagged
/// as future work / out-of-scope.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FrameType {
    Uncompressed,    // 1
    UnalignedRgb24,  // 2
    ArithmeticRgb24, // 4 (RGB24 / RGB32 distinguished by bit-depth)
    SolidGrey,       // 5
    SolidRgb,        // 6
    ArithmeticRgba,  // 8
    SolidRgba,       // 9
}

impl FrameType {
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            1 => Ok(Self::Uncompressed),
            2 => Ok(Self::UnalignedRgb24),
            4 => Ok(Self::ArithmeticRgb24),
            5 => Ok(Self::SolidGrey),
            6 => Ok(Self::SolidRgb),
            8 => Ok(Self::ArithmeticRgba),
            9 => Ok(Self::SolidRgba),
            3 | 7 | 10 | 11 => Err(Error::UnsupportedFrameType(b)),
            0 | 12.. => Err(Error::BadFrameType(b)),
        }
    }

    /// Number of cross-plane channels this frame type carries (the
    /// channel-offset table size is `(channels - 1) * 4` bytes).
    pub fn n_channels(self) -> usize {
        match self {
            Self::Uncompressed | Self::SolidGrey | Self::SolidRgb | Self::SolidRgba => 0,
            Self::UnalignedRgb24 | Self::ArithmeticRgb24 => 3,
            Self::ArithmeticRgba => 4,
        }
    }
}

/// Read the channel-offset table that immediately follows the
/// frame-type byte for arithmetic-coded RGB-family frames per
/// `spec/01` §2.3:
///
/// - 3-channel frames (RGB24/RGB32) carry 2 × u32 = 8 bytes (offsets
///   to G and B; R/plane-0 starts at frame byte 9).
/// - 4-channel frames (RGBA) carry 3 × u32 = 12 bytes (offsets to G,
///   B, A; R/plane-0 starts at frame byte 13).
///
/// Returns the per-channel slices into `frame` in plane order
/// `[R, G, B]` or `[R, G, B, A]`. Each slice spans from its plane's
/// offset to the next plane's offset (or to the end of the frame for
/// the final plane).
pub fn split_channels(frame: &[u8], n_channels: usize) -> Result<Vec<&[u8]>> {
    debug_assert!(n_channels == 3 || n_channels == 4);
    let prefix_size = 1 + (n_channels - 1) * 4;
    if frame.len() < prefix_size {
        return Err(Error::Truncated {
            context: "channel-offset table",
        });
    }
    let mut offsets = Vec::with_capacity(n_channels);
    // Plane 0 starts immediately after the prefix.
    offsets.push(prefix_size);
    for k in 0..(n_channels - 1) {
        let off_bytes = 1 + k * 4;
        let raw = u32::from_le_bytes([
            frame[off_bytes],
            frame[off_bytes + 1],
            frame[off_bytes + 2],
            frame[off_bytes + 3],
        ]) as usize;
        if raw > frame.len() {
            return Err(Error::OffsetOutOfRange);
        }
        offsets.push(raw);
    }
    // Validate ascending and within bounds.
    for w in offsets.windows(2) {
        if w[1] < w[0] {
            return Err(Error::OffsetOutOfRange);
        }
    }
    let mut slices = Vec::with_capacity(n_channels);
    for k in 0..n_channels {
        let start = offsets[k];
        let end = if k + 1 < n_channels {
            offsets[k + 1]
        } else {
            frame.len()
        };
        if end > frame.len() {
            return Err(Error::OffsetOutOfRange);
        }
        slices.push(&frame[start..end]);
    }
    Ok(slices)
}

/// Encoder-side helper (test-only): pack the channel-offset table
/// plus per-channel bodies into one frame buffer with the type byte
/// at position 0.
#[cfg(test)]
pub fn pack_channels(type_byte: u8, channels: &[&[u8]]) -> Vec<u8> {
    let n = channels.len();
    debug_assert!(n == 3 || n == 4);
    let prefix_size = 1 + (n - 1) * 4;
    let total: usize = prefix_size + channels.iter().map(|c| c.len()).sum::<usize>();
    let mut out = Vec::with_capacity(total);
    out.push(type_byte);
    // Compute offsets to plane 1 .. plane n-1 (plane 0 starts at
    // prefix_size).
    let mut acc = prefix_size;
    let mut offs = Vec::with_capacity(n - 1);
    for c in channels.iter().take(n - 1) {
        acc += c.len();
        offs.push(acc as u32);
    }
    for o in offs {
        out.extend_from_slice(&o.to_le_bytes());
    }
    debug_assert_eq!(out.len(), prefix_size);
    for c in channels {
        out.extend_from_slice(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_channels_three() {
        // Build a frame with type byte = 4, 3 channels each "abc",
        // "def", "ghij".
        let ch0 = b"abc".as_slice();
        let ch1 = b"def".as_slice();
        let ch2 = b"ghij".as_slice();
        let frame = pack_channels(4, &[ch0, ch1, ch2]);
        let slices = split_channels(&frame, 3).unwrap();
        assert_eq!(slices.len(), 3);
        assert_eq!(slices[0], ch0);
        assert_eq!(slices[1], ch1);
        assert_eq!(slices[2], ch2);
    }

    #[test]
    fn split_channels_four() {
        let ch0 = b"a".as_slice();
        let ch1 = b"bb".as_slice();
        let ch2 = b"ccc".as_slice();
        let ch3 = b"dddd".as_slice();
        let frame = pack_channels(8, &[ch0, ch1, ch2, ch3]);
        let slices = split_channels(&frame, 4).unwrap();
        assert_eq!(slices.len(), 4);
        assert_eq!(slices[0], ch0);
        assert_eq!(slices[1], ch1);
        assert_eq!(slices[2], ch2);
        assert_eq!(slices[3], ch3);
    }

    #[test]
    fn frame_type_byte_validation() {
        assert!(matches!(
            FrameType::from_byte(0),
            Err(Error::BadFrameType(0))
        ));
        assert!(matches!(
            FrameType::from_byte(12),
            Err(Error::BadFrameType(12))
        ));
        assert!(matches!(
            FrameType::from_byte(3),
            Err(Error::UnsupportedFrameType(3))
        ));
        assert!(matches!(
            FrameType::from_byte(7),
            Err(Error::UnsupportedFrameType(7))
        ));
        assert!(matches!(
            FrameType::from_byte(10),
            Err(Error::UnsupportedFrameType(10))
        ));
        assert!(matches!(
            FrameType::from_byte(11),
            Err(Error::UnsupportedFrameType(11))
        ));
        assert_eq!(FrameType::from_byte(1).unwrap(), FrameType::Uncompressed);
        assert_eq!(FrameType::from_byte(2).unwrap(), FrameType::UnalignedRgb24);
        assert_eq!(FrameType::from_byte(4).unwrap(), FrameType::ArithmeticRgb24);
        assert_eq!(FrameType::from_byte(5).unwrap(), FrameType::SolidGrey);
        assert_eq!(FrameType::from_byte(6).unwrap(), FrameType::SolidRgb);
        assert_eq!(FrameType::from_byte(8).unwrap(), FrameType::ArithmeticRgba);
        assert_eq!(FrameType::from_byte(9).unwrap(), FrameType::SolidRgba);
    }
}
