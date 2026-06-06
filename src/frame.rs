//! Frame-type byte parsing + per-frame channel-offset table layout
//! per `spec/01` §1 / §2.

use crate::error::{Error, Result};

/// Recognised frame types this build's decoder accepts. Round 3 adds
/// YUY2 (3) and reduced-resolution (11); round 4 adds legacy RGB
/// (type 7, `spec/07` adaptive-CDF range coder).
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum FrameType {
    Uncompressed,    // 1
    UnalignedRgb24,  // 2
    ArithmeticYuy2,  // 3
    ArithmeticRgb24, // 4 (RGB24 / RGB32 distinguished by bit-depth)
    SolidGrey,       // 5
    SolidRgb,        // 6
    LegacyRgb,       // 7 (pre-1.1.0 adaptive-CDF range coder per spec/07)
    ArithmeticRgba,  // 8
    SolidRgba,       // 9
    ArithmeticYv12,  // 10
    ReducedResYv12,  // 11 (= type 10 at half-W/half-H + 2× upscale)
}

impl FrameType {
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            1 => Ok(Self::Uncompressed),
            2 => Ok(Self::UnalignedRgb24),
            3 => Ok(Self::ArithmeticYuy2),
            4 => Ok(Self::ArithmeticRgb24),
            5 => Ok(Self::SolidGrey),
            6 => Ok(Self::SolidRgb),
            7 => Ok(Self::LegacyRgb),
            8 => Ok(Self::ArithmeticRgba),
            9 => Ok(Self::SolidRgba),
            10 => Ok(Self::ArithmeticYv12),
            11 => Ok(Self::ReducedResYv12),
            0 | 12.. => Err(Error::BadFrameType(b)),
        }
    }

    /// Wire-level frame-type byte that this variant maps back to per
    /// the [`spec/01` §1.3 dispatch table][spec01-13]. Round-trips
    /// with [`from_byte`](Self::from_byte) on every accepted input
    /// (the legal set is `1..=11`).
    ///
    /// [spec01-13]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/01-frame-data-layout.md
    pub fn to_byte(self) -> u8 {
        match self {
            Self::Uncompressed => 1,
            Self::UnalignedRgb24 => 2,
            Self::ArithmeticYuy2 => 3,
            Self::ArithmeticRgb24 => 4,
            Self::SolidGrey => 5,
            Self::SolidRgb => 6,
            Self::LegacyRgb => 7,
            Self::ArithmeticRgba => 8,
            Self::SolidRgba => 9,
            Self::ArithmeticYv12 => 10,
            Self::ReducedResYv12 => 11,
        }
    }

    /// Number of cross-plane channels this frame type carries (the
    /// channel-offset table size is `(channels - 1) * 4` bytes).
    pub fn n_channels(self) -> usize {
        match self {
            Self::Uncompressed | Self::SolidGrey | Self::SolidRgb | Self::SolidRgba => 0,
            Self::UnalignedRgb24
            | Self::ArithmeticYuy2
            | Self::ArithmeticRgb24
            | Self::LegacyRgb
            | Self::ArithmeticYv12
            | Self::ReducedResYv12 => 3,
            Self::ArithmeticRgba => 4,
        }
    }

    /// `true` for the literal-pixel-data frame type (`spec/01` §2.1):
    /// the wire layout is `{ byte 0 = 0x01, bytes 1..N = raw pixel
    /// data }` with no Lagarith transformation applied.
    pub fn is_uncompressed(self) -> bool {
        matches!(self, Self::Uncompressed)
    }

    /// `true` for the three solid-colour frame types (`spec/01`
    /// §2.2): types 5 (grey), 6 (RGB), and 9 (RGBA). The wire payload
    /// is exactly 1 type byte + 1 / 3 / 4 colour bytes (`spec/01`
    /// §2.2.2 — `2 / 4 / 5` bytes total).
    pub fn is_solid(self) -> bool {
        matches!(self, Self::SolidGrey | Self::SolidRgb | Self::SolidRgba)
    }

    /// `true` for the seven arithmetic-coded frame types (`spec/01`
    /// §2.3): 2 (Unaligned-RGB24), 3 (Arithmetic-YUY2), 4
    /// (Arithmetic-RGB24), 7 (Legacy-RGB, decode-only), 8
    /// (Arithmetic-RGBA), 10 (Arithmetic-YV12), and 11
    /// (Reduced-Res). Each of these carries the channel-offset
    /// prefix table per `spec/01` §2.3.
    pub fn is_arithmetic(self) -> bool {
        matches!(
            self,
            Self::UnalignedRgb24
                | Self::ArithmeticYuy2
                | Self::ArithmeticRgb24
                | Self::LegacyRgb
                | Self::ArithmeticRgba
                | Self::ArithmeticYv12
                | Self::ReducedResYv12
        )
    }

    /// `true` for the legacy (pre-1.1.0) adaptive-CDF RGB frame type
    /// — type 7 — that the proprietary 64-bit encoder **never
    /// produces** per `spec/01` §3 row 7 ("(none) | Confirms wiki:
    /// 'Decoding support only'"). Our clean-room crate implements
    /// both encode and decode for this type for round-trip testing,
    /// but it remains absent from real-world Lagarith 64-bit outputs.
    pub fn is_legacy_decode_only(self) -> bool {
        matches!(self, Self::LegacyRgb)
    }

    /// `true` for the reduced-resolution frame type (`spec/01` §2.4):
    /// type 11. Wire body is a YV12-shaped payload at half-W /
    /// half-H; the decoder upscales each plane by 2× to fill the
    /// host-requested W / H. The 64-bit encoder never produces type
    /// 11 (`spec/01` §3 row 11 + §2.4 last paragraph); only the
    /// i386 build may.
    pub fn is_reduced_resolution(self) -> bool {
        matches!(self, Self::ReducedResYv12)
    }

    /// `true` for the planar YUV frame types — Arithmetic-YV12 (type
    /// 10) and Reduced-Res (type 11 — a YV12 body at half-W / half-H
    /// per `spec/01` §2.4). The planar-YUV types share a `+9`-byte
    /// channel-offset prefix (`spec/01` §2.3 row 10) and the
    /// Y/V/U plane order.
    pub fn is_planar_yv12(self) -> bool {
        matches!(self, Self::ArithmeticYv12 | Self::ReducedResYv12)
    }

    /// `true` for the packed-pixel YUV frame type — Arithmetic-YUY2
    /// (type 3) per `spec/01` §2.3 row 3 — whose wire body is
    /// arithmetic-coded into Y/U/V planes then packed at decode time
    /// back into the YUY2 4:2:2 packed pixel layout.
    pub fn is_packed_yuy2(self) -> bool {
        matches!(self, Self::ArithmeticYuy2)
    }

    /// `true` for the packed-pixel RGB(A) arithmetic frame types
    /// (`spec/01` §2.3 rows 2, 4, 7, 8): Unaligned-RGB24 (2),
    /// Arithmetic-RGB24 (4), Legacy-RGB (7), and Arithmetic-RGBA
    /// (8). Each one decodes into three or four planes and packs
    /// back into a packed BGR / BGRA pixel buffer per the host's
    /// Windows `BI_RGB` / `BI_BITFIELDS` memory order (`spec/01`
    /// §2.2.1 audit-corrected blockquote).
    pub fn is_packed_rgb(self) -> bool {
        matches!(
            self,
            Self::UnalignedRgb24 | Self::ArithmeticRgb24 | Self::LegacyRgb | Self::ArithmeticRgba
        )
    }

    /// `true` for the 64-bit-encoder-produced frame types per
    /// `spec/01` §3 (encoder-side cross-check). The 64-bit encoder
    /// emits types 2, 3, 4, 5, 6, 8, 9, and 10; it **does not**
    /// emit types 1 (uncompressed — wiki §"intended more for
    /// debugging than actual use"), 7 (legacy — wiki "Decoding
    /// support only"), or 11 (reduced-resolution — only the i386
    /// build).
    pub fn is_produced_by_v64_encoder(self) -> bool {
        matches!(
            self,
            Self::UnalignedRgb24
                | Self::ArithmeticYuy2
                | Self::ArithmeticRgb24
                | Self::SolidGrey
                | Self::SolidRgb
                | Self::ArithmeticRgba
                | Self::SolidRgba
                | Self::ArithmeticYv12
        )
    }

    /// Channel-offset prefix size in bytes for arithmetic-coded
    /// frame types per `spec/01` §2.3: `1 + (n_channels − 1) × 4`
    /// = `9` for 3-channel arithmetic types (2 / 3 / 4 / 7 / 10 /
    /// 11) and `13` for the 4-channel RGBA arithmetic type (8). For
    /// the literal types (uncompressed, solid) — which carry no
    /// channel-offset table — the prefix size is `1` (the lone
    /// frame-type byte).
    ///
    /// The channel-offset table itself follows the type byte: it
    /// occupies bytes `1..self.channel_offset_table_size()` of the
    /// frame, with the first arithmetic-coded channel starting at
    /// `self.prefix_size()`.
    pub fn prefix_size(self) -> usize {
        if self.is_arithmetic() {
            1 + (self.n_channels() - 1) * 4
        } else {
            1
        }
    }

    /// Channel-offset table size in bytes for arithmetic-coded
    /// frame types per `spec/01` §2.3: `(n_channels − 1) × 4`
    /// = `8` for 3-channel arithmetic types and `12` for the
    /// 4-channel RGBA arithmetic type. For the literal types
    /// (uncompressed, solid) the channel-offset table is absent
    /// (size `0`).
    pub fn channel_offset_table_size(self) -> usize {
        self.prefix_size() - 1
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
        assert_eq!(FrameType::from_byte(1).unwrap(), FrameType::Uncompressed);
        assert_eq!(FrameType::from_byte(2).unwrap(), FrameType::UnalignedRgb24);
        assert_eq!(FrameType::from_byte(3).unwrap(), FrameType::ArithmeticYuy2);
        assert_eq!(FrameType::from_byte(4).unwrap(), FrameType::ArithmeticRgb24);
        assert_eq!(FrameType::from_byte(5).unwrap(), FrameType::SolidGrey);
        assert_eq!(FrameType::from_byte(6).unwrap(), FrameType::SolidRgb);
        assert_eq!(FrameType::from_byte(7).unwrap(), FrameType::LegacyRgb);
        assert_eq!(FrameType::from_byte(8).unwrap(), FrameType::ArithmeticRgba);
        assert_eq!(FrameType::from_byte(9).unwrap(), FrameType::SolidRgba);
        assert_eq!(FrameType::from_byte(10).unwrap(), FrameType::ArithmeticYv12);
        assert_eq!(FrameType::from_byte(11).unwrap(), FrameType::ReducedResYv12);
    }

    /// Every accepted frame-type byte round-trips losslessly through
    /// [`FrameType::from_byte`] and [`FrameType::to_byte`], and
    /// re-classification of the round-tripped byte returns the same
    /// variant (i.e. `from_byte ∘ to_byte = id` on the accepted set
    /// `1..=11`).
    #[test]
    fn frame_type_roundtrip_to_byte() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert_eq!(ft.to_byte(), b, "to_byte mismatch on {b}");
            assert_eq!(
                FrameType::from_byte(ft.to_byte()).unwrap(),
                ft,
                "from_byte ∘ to_byte not identity on {b}"
            );
        }
    }

    /// Every variant of [`FrameType`] reports exactly one of the
    /// three mutually-exclusive top-level classes per `spec/01` §2:
    /// uncompressed (§2.1, type 1), solid (§2.2, types 5 / 6 / 9),
    /// or arithmetic (§2.3, types 2 / 3 / 4 / 7 / 8 / 10 / 11). The
    /// three classification predicates therefore partition the
    /// 11-element accepted set without overlap or gap.
    #[test]
    fn frame_type_top_level_classes_partition_accepted_set() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            let unc = ft.is_uncompressed() as u8;
            let sol = ft.is_solid() as u8;
            let arith = ft.is_arithmetic() as u8;
            assert_eq!(
                unc + sol + arith,
                1,
                "exactly one top-level class must hold for {ft:?} (got unc={unc} sol={sol} arith={arith})"
            );
        }
    }

    /// `is_uncompressed` matches type 1 only per `spec/01` §2.1.
    #[test]
    fn frame_type_is_uncompressed() {
        assert!(FrameType::Uncompressed.is_uncompressed());
        for b in 2u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert!(
                !ft.is_uncompressed(),
                "{ft:?} should not be is_uncompressed"
            );
        }
    }

    /// `is_solid` matches exactly types 5 / 6 / 9 per `spec/01` §2.2.
    #[test]
    fn frame_type_is_solid() {
        let solids = [
            FrameType::SolidGrey,
            FrameType::SolidRgb,
            FrameType::SolidRgba,
        ];
        for ft in solids {
            assert!(ft.is_solid(), "{ft:?} should be is_solid");
        }
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert_eq!(ft.is_solid(), solids.contains(&ft), "{ft:?}");
        }
    }

    /// `is_arithmetic` matches exactly types 2 / 3 / 4 / 7 / 8 /
    /// 10 / 11 per `spec/01` §2.3.
    #[test]
    fn frame_type_is_arithmetic() {
        let ariths = [
            FrameType::UnalignedRgb24,
            FrameType::ArithmeticYuy2,
            FrameType::ArithmeticRgb24,
            FrameType::LegacyRgb,
            FrameType::ArithmeticRgba,
            FrameType::ArithmeticYv12,
            FrameType::ReducedResYv12,
        ];
        for ft in ariths {
            assert!(ft.is_arithmetic(), "{ft:?} should be is_arithmetic");
        }
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert_eq!(ft.is_arithmetic(), ariths.contains(&ft), "{ft:?}");
        }
    }

    /// `is_legacy_decode_only` matches type 7 only per `spec/01` §3
    /// row 7 ("(none) | Confirms wiki: 'Decoding support only'").
    #[test]
    fn frame_type_is_legacy_decode_only() {
        assert!(FrameType::LegacyRgb.is_legacy_decode_only());
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            if !matches!(ft, FrameType::LegacyRgb) {
                assert!(
                    !ft.is_legacy_decode_only(),
                    "{ft:?} should not be is_legacy_decode_only"
                );
            }
        }
    }

    /// `is_reduced_resolution` matches type 11 only per `spec/01`
    /// §2.4.
    #[test]
    fn frame_type_is_reduced_resolution() {
        assert!(FrameType::ReducedResYv12.is_reduced_resolution());
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            if !matches!(ft, FrameType::ReducedResYv12) {
                assert!(
                    !ft.is_reduced_resolution(),
                    "{ft:?} should not be is_reduced_resolution"
                );
            }
        }
    }

    /// `is_planar_yv12` matches types 10 / 11 (the YV12-family
    /// arithmetic types) per `spec/01` §2.3 row 10 + §2.4. The two
    /// types share the `+9`-byte channel-offset prefix and the
    /// Y/V/U plane order.
    #[test]
    fn frame_type_is_planar_yv12() {
        let planars = [FrameType::ArithmeticYv12, FrameType::ReducedResYv12];
        for ft in planars {
            assert!(ft.is_planar_yv12(), "{ft:?} should be is_planar_yv12");
        }
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert_eq!(ft.is_planar_yv12(), planars.contains(&ft), "{ft:?}");
        }
    }

    /// `is_packed_yuy2` matches type 3 only per `spec/01` §2.3 row 3.
    #[test]
    fn frame_type_is_packed_yuy2() {
        assert!(FrameType::ArithmeticYuy2.is_packed_yuy2());
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            if !matches!(ft, FrameType::ArithmeticYuy2) {
                assert!(!ft.is_packed_yuy2(), "{ft:?} should not be is_packed_yuy2");
            }
        }
    }

    /// `is_packed_rgb` matches exactly the packed-pixel RGB(A)
    /// arithmetic types: 2 / 4 / 7 / 8 per `spec/01` §2.3.
    #[test]
    fn frame_type_is_packed_rgb() {
        let packed_rgb = [
            FrameType::UnalignedRgb24,
            FrameType::ArithmeticRgb24,
            FrameType::LegacyRgb,
            FrameType::ArithmeticRgba,
        ];
        for ft in packed_rgb {
            assert!(ft.is_packed_rgb(), "{ft:?} should be is_packed_rgb");
        }
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert_eq!(ft.is_packed_rgb(), packed_rgb.contains(&ft), "{ft:?}");
        }
    }

    /// The four arithmetic-type sub-classifiers (`is_planar_yv12`,
    /// `is_packed_yuy2`, `is_packed_rgb`) partition the seven
    /// arithmetic types per `spec/01` §2.3 — every arithmetic frame
    /// type belongs to exactly one of {planar-YV12, packed-YUY2,
    /// packed-RGB}, and no non-arithmetic frame type matches any
    /// of them.
    #[test]
    fn frame_type_arithmetic_subclasses_partition_arithmetic_set() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            let p_yv12 = ft.is_planar_yv12() as u8;
            let p_yuy2 = ft.is_packed_yuy2() as u8;
            let p_rgb = ft.is_packed_rgb() as u8;
            let sum = p_yv12 + p_yuy2 + p_rgb;
            if ft.is_arithmetic() {
                assert_eq!(
                    sum, 1,
                    "exactly one arithmetic sub-class must hold for {ft:?}"
                );
            } else {
                assert_eq!(
                    sum, 0,
                    "no arithmetic sub-class should hold for non-arithmetic {ft:?}"
                );
            }
        }
    }

    /// `is_produced_by_v64_encoder` matches exactly the 8 types the
    /// 64-bit DLL encoder emits per `spec/01` §3: the encoder's
    /// immediate-byte writes hit types 2 / 3 / 4 / 5 / 6 / 8 / 9 /
    /// 10. Types 1, 7, and 11 have "(none)" in the §3 table.
    #[test]
    fn frame_type_is_produced_by_v64_encoder() {
        let produced = [
            FrameType::UnalignedRgb24,
            FrameType::ArithmeticYuy2,
            FrameType::ArithmeticRgb24,
            FrameType::SolidGrey,
            FrameType::SolidRgb,
            FrameType::ArithmeticRgba,
            FrameType::SolidRgba,
            FrameType::ArithmeticYv12,
        ];
        for ft in produced {
            assert!(
                ft.is_produced_by_v64_encoder(),
                "{ft:?} should be produced by v64 encoder"
            );
        }
        let not_produced = [
            FrameType::Uncompressed,
            FrameType::LegacyRgb,
            FrameType::ReducedResYv12,
        ];
        for ft in not_produced {
            assert!(
                !ft.is_produced_by_v64_encoder(),
                "{ft:?} should NOT be produced by v64 encoder"
            );
        }
        // And the produced + not_produced sets cover the full
        // accepted byte range (1..=11) exactly once.
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert_eq!(
                ft.is_produced_by_v64_encoder(),
                produced.contains(&ft),
                "{ft:?}"
            );
        }
    }

    /// The channel-offset prefix size matches `spec/01` §2.3:
    /// 9 bytes (= 1 type byte + 2 × u32 offsets) for the 3-channel
    /// arithmetic types (2 / 3 / 4 / 7 / 10 / 11), and 13 bytes (=
    /// 1 + 3 × u32) for the 4-channel RGBA type (8). Solid and
    /// uncompressed types report `1` (the lone frame-type byte —
    /// no channel-offset table).
    #[test]
    fn frame_type_prefix_size_matches_spec() {
        // Three-channel arithmetic types: 9-byte prefix.
        let three_ch = [
            FrameType::UnalignedRgb24,
            FrameType::ArithmeticYuy2,
            FrameType::ArithmeticRgb24,
            FrameType::LegacyRgb,
            FrameType::ArithmeticYv12,
            FrameType::ReducedResYv12,
        ];
        for ft in three_ch {
            assert_eq!(ft.prefix_size(), 9, "{ft:?} prefix should be 9");
            assert_eq!(
                ft.channel_offset_table_size(),
                8,
                "{ft:?} offset-table should be 8"
            );
        }
        // Four-channel RGBA: 13-byte prefix.
        assert_eq!(FrameType::ArithmeticRgba.prefix_size(), 13);
        assert_eq!(FrameType::ArithmeticRgba.channel_offset_table_size(), 12);
        // Solid / uncompressed: 1-byte prefix, no offset table.
        for ft in [
            FrameType::Uncompressed,
            FrameType::SolidGrey,
            FrameType::SolidRgb,
            FrameType::SolidRgba,
        ] {
            assert_eq!(ft.prefix_size(), 1, "{ft:?} prefix should be 1");
            assert_eq!(
                ft.channel_offset_table_size(),
                0,
                "{ft:?} offset-table should be 0"
            );
        }
    }

    /// The `prefix_size()` accessor is consistent with the
    /// [`split_channels`] helper for every 3- and 4-channel
    /// arithmetic frame type: a frame packed via [`pack_channels`]
    /// at the canonical type byte produces a slice whose first
    /// channel begins at offset `ft.prefix_size()`.
    #[test]
    fn frame_type_prefix_size_consistent_with_split_channels() {
        for ft in [
            FrameType::UnalignedRgb24,
            FrameType::ArithmeticYuy2,
            FrameType::ArithmeticRgb24,
            FrameType::LegacyRgb,
            FrameType::ArithmeticYv12,
            FrameType::ReducedResYv12,
        ] {
            let ch0 = b"a".as_slice();
            let ch1 = b"b".as_slice();
            let ch2 = b"c".as_slice();
            let frame = pack_channels(ft.to_byte(), &[ch0, ch1, ch2]);
            let slices = split_channels(&frame, ft.n_channels()).unwrap();
            // First channel starts at offset prefix_size; its body
            // is the first packed channel.
            assert_eq!(
                slices[0], ch0,
                "{ft:?} first channel should equal packed ch0"
            );
            assert_eq!(ft.prefix_size(), 9, "{ft:?} prefix_size should be 9");
        }
        // RGBA family: 4 channels + 13-byte prefix.
        let ch0 = b"a".as_slice();
        let ch1 = b"b".as_slice();
        let ch2 = b"c".as_slice();
        let ch3 = b"d".as_slice();
        let frame = pack_channels(FrameType::ArithmeticRgba.to_byte(), &[ch0, ch1, ch2, ch3]);
        let slices = split_channels(&frame, FrameType::ArithmeticRgba.n_channels()).unwrap();
        assert_eq!(slices[0], ch0);
        assert_eq!(slices[3], ch3);
        assert_eq!(FrameType::ArithmeticRgba.prefix_size(), 13);
    }
}
