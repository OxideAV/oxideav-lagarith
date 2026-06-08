//! Frame-type byte parsing + per-frame channel-offset table layout
//! per `spec/01` §1 / §2.

use crate::decoder::PixelKind;
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

    /// `true` for the frame types whose wire form carries an
    /// explicit alpha plane / alpha byte — exactly the two RGBA
    /// frame types: [`ArithmeticRgba`](Self::ArithmeticRgba) (8) and
    /// [`SolidRgba`](Self::SolidRgba) (9).
    ///
    /// Type 8 (`spec/01` §2.3 row 8 + `spec/03` §4.3) splits four
    /// planes (R / G / B / A) on the wire; the alpha plane is
    /// decoded independently (left predictor on row 0, JPEG-LS
    /// median on rows ≥ 1) with **no** cross-plane decorrelation
    /// against G (only R and B receive the post-prediction
    /// `+= G` correction). Type 9 (`spec/01` §2.2 row 9) is the
    /// solid-RGBA literal frame whose wire payload is exactly
    /// four colour bytes (R / G / B / A) replicated to fill the
    /// host BGR(A) buffer.
    ///
    /// Type 1 ([`Uncompressed`](Self::Uncompressed)) is **excluded**
    /// even though a Bgra32 host buffer requested at decode time
    /// will carry an alpha byte per pixel: the type-1 wire body is
    /// the source pixel buffer in its source layout per `spec/01`
    /// §2.1 ("RGB24 / RGB32 / RGBA / YUY2 / YV12 with no Lagarith
    /// transformation applied"), so the presence of an alpha byte
    /// on the wire is a host-format property, not a frame-type
    /// property — there is no Lagarith-coded "alpha plane" on the
    /// wire to speak of. The three solid-RGB types (5 grey, 6 RGB)
    /// are also excluded because their wire payload is 1 / 3 colour
    /// bytes respectively (`spec/01` §2.2.2) and the host BGRA
    /// buffer's alpha byte is filled by an immediate-byte store
    /// at `lagarith.dll!0x180009486` to the constant `0xff`
    /// (`spec/03` §4 third bullet: "since RGB32 has no alpha plane
    /// on the wire"). Every other frame type (2 / 3 / 4 / 7 / 10 /
    /// 11) is similarly RGB / YV12 / YUY2 on the wire — no alpha.
    ///
    /// Returns the same set as `n_channels() == 4` for the two
    /// frame types whose `n_channels` is defined (a structural
    /// invariant pinned by
    /// `frame_type_has_alpha_plane_implies_four_channels` for
    /// arithmetic types and by exhaustive enumeration for the solid
    /// type whose `n_channels` returns 0 by the existing convention).
    /// Mirrors [`PixelKind::has_alpha`](crate::PixelKind::has_alpha)
    /// on the frame-type axis: `PixelKind::has_alpha` reports whether
    /// the host buffer reserves an alpha byte per pixel; this
    /// accessor reports whether the wire form supplies one.
    pub fn has_alpha_plane(self) -> bool {
        matches!(self, Self::ArithmeticRgba | Self::SolidRgba)
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

    /// `true` if this frame type accepts the given host
    /// [`PixelKind`] at decode time. The compatibility relation
    /// is anchored in `spec/01` §2.1 (uncompressed payload is
    /// format-agnostic), `spec/01` §2.2.1 (solid frames target
    /// the Windows `BI_RGB` 24-bpp or 32-bpp DIB buffer), and
    /// `spec/03` §6.1 / §6.2 (YV12 / YUY2 wire forms target their
    /// matching host planar / packed pixel kinds):
    ///
    /// | Frame type | Accepted [`PixelKind`] set |
    /// | ---------- | -------------------------- |
    /// | [`Uncompressed`](Self::Uncompressed) (1) | all four — `Bgr24`, `Bgra32`, `Yv12`, `Yuy2` (the wire body is the host pixel buffer verbatim — `spec/01` §2.1 "RGB24 / RGB32 / RGBA / YUY2 / YV12 with no Lagarith transformation applied"). |
    /// | [`SolidGrey`](Self::SolidGrey) (5) / [`SolidRgb`](Self::SolidRgb) (6) / [`SolidRgba`](Self::SolidRgba) (9) | `Bgr24` / `Bgra32` only (`spec/01` §2.2.1 BGR memory order — the proprietary's solid-fill helper at `lagarith.dll!0x1800049e0` targets the host BGR(A) memory layout). |
    /// | [`UnalignedRgb24`](Self::UnalignedRgb24) (2) / [`ArithmeticRgb24`](Self::ArithmeticRgb24) (4) / [`LegacyRgb`](Self::LegacyRgb) (7) / [`ArithmeticRgba`](Self::ArithmeticRgba) (8) | `Bgr24` / `Bgra32` only — the four packed-RGB arithmetic families pack the per-plane decode output into the host BGR(A) buffer per `spec/01` §2.3. |
    /// | [`ArithmeticYv12`](Self::ArithmeticYv12) (10) / [`ReducedResYv12`](Self::ReducedResYv12) (11) | `Yv12` only — `spec/03` §6.1 + `spec/01` §2.4 (the type-11 wire body is a half-W / half-H YV12 form, upscaled into the host YV12 buffer at decode time). |
    /// | [`ArithmeticYuy2`](Self::ArithmeticYuy2) (3) | `Yuy2` only — `spec/03` §6.2 packed `Y0 U Y1 V` macropixel layout. |
    ///
    /// This is the predicate the per-frame-type decoders enforce
    /// at function entry — the literal pixel-kind matches in
    /// `decoder::decode_arith_yv12` / `decode_arith_yuy2` /
    /// `decode_reduced_res` and the `PixelKind::bytes_per_pixel`
    /// (`packed_bpp`) gate in `decoder::decode_solid` /
    /// `decode_arith_rgb` / `decode_arith_rgba` /
    /// `decode_legacy_rgb` collectively realise this relation.
    /// The new accessor lets downstream callers introspect the
    /// compatibility relation without re-running the dispatcher
    /// or interrogating the dispatcher's
    /// [`Error::PixelFormatMismatch`](crate::error::Error::PixelFormatMismatch)
    /// surface; structurally it mirrors the
    /// [`PixelKind::bytes_per_pixel`] / [`PixelKind::is_planar`] /
    /// [`PixelKind::is_packed`] partition (round 245) on the
    /// frame-type axis.
    pub fn accepts_pixel_kind(self, pixel_kind: PixelKind) -> bool {
        match self {
            // §2.1: uncompressed wire payload is the source pixel
            // buffer in its source layout with no transformation —
            // all four host pixel kinds are valid targets.
            Self::Uncompressed => true,
            // §2.2.1: solid-colour frames target Windows BGR(A)
            // memory order via `lagarith.dll!0x1800049e0`. The
            // packed-bpp gate in `decoder::decode_solid` rejects
            // `Yv12` / `Yuy2` up-front.
            Self::SolidGrey | Self::SolidRgb | Self::SolidRgba => pixel_kind.is_rgb_family(),
            // §2.3: the four packed-RGB arithmetic families pack
            // the per-plane output into BGR(A); the
            // `packed_bpp().ok_or(PixelFormatMismatch)` gate in
            // each decoder mirrors this.
            Self::UnalignedRgb24
            | Self::ArithmeticRgb24
            | Self::LegacyRgb
            | Self::ArithmeticRgba => pixel_kind.is_rgb_family(),
            // §6.1 + §2.4: the YV12 wire families target the YV12
            // planar host buffer only.
            Self::ArithmeticYv12 | Self::ReducedResYv12 => matches!(pixel_kind, PixelKind::Yv12),
            // §6.2: the YUY2 wire family targets the YUY2 packed
            // host buffer only.
            Self::ArithmeticYuy2 => matches!(pixel_kind, PixelKind::Yuy2),
        }
    }

    /// The list of host [`PixelKind`] values this frame type
    /// accepts at decode time — the structural inverse of
    /// [`accepts_pixel_kind`](Self::accepts_pixel_kind).
    ///
    /// Returns one of:
    ///
    /// * All four (`Bgr24` / `Bgra32` / `Yv12` / `Yuy2`) for
    ///   [`Uncompressed`](Self::Uncompressed) (`spec/01` §2.1).
    /// * The two-element RGB-family slice (`Bgr24` / `Bgra32`)
    ///   for the three solid types (5 / 6 / 9, `spec/01` §2.2.1)
    ///   and the four packed-RGB arithmetic types (2 / 4 / 7 / 8,
    ///   `spec/01` §2.3).
    /// * The single-element `Yv12` slice for [`ArithmeticYv12`](Self::ArithmeticYv12)
    ///   (10) and [`ReducedResYv12`](Self::ReducedResYv12) (11)
    ///   (`spec/03` §6.1 + `spec/01` §2.4).
    /// * The single-element `Yuy2` slice for
    ///   [`ArithmeticYuy2`](Self::ArithmeticYuy2) (3)
    ///   (`spec/03` §6.2).
    ///
    /// The returned slice's element-wise membership coincides
    /// exactly with [`accepts_pixel_kind`](Self::accepts_pixel_kind)
    /// returning `true` on every accepted [`PixelKind`] — a
    /// structural invariant pinned by
    /// `frame_type_accepts_pixel_kind_consistent_with_compatible_set`.
    pub fn compatible_pixel_kinds(self) -> &'static [PixelKind] {
        const ALL: &[PixelKind] = &[
            PixelKind::Bgr24,
            PixelKind::Bgra32,
            PixelKind::Yv12,
            PixelKind::Yuy2,
        ];
        const RGB: &[PixelKind] = &[PixelKind::Bgr24, PixelKind::Bgra32];
        const YV12_ONLY: &[PixelKind] = &[PixelKind::Yv12];
        const YUY2_ONLY: &[PixelKind] = &[PixelKind::Yuy2];
        match self {
            Self::Uncompressed => ALL,
            Self::SolidGrey | Self::SolidRgb | Self::SolidRgba => RGB,
            Self::UnalignedRgb24
            | Self::ArithmeticRgb24
            | Self::LegacyRgb
            | Self::ArithmeticRgba => RGB,
            Self::ArithmeticYv12 | Self::ReducedResYv12 => YV12_ONLY,
            Self::ArithmeticYuy2 => YUY2_ONLY,
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

    /// `accepts_pixel_kind` matches the per-frame-type decoder
    /// gates: uncompressed accepts all four host pixel kinds
    /// (`spec/01` §2.1 "RGB24 / RGB32 / RGBA / YUY2 / YV12"),
    /// the three solid types (5 / 6 / 9) and four packed-RGB
    /// arithmetic types (2 / 4 / 7 / 8) accept BGR-family only
    /// (`spec/01` §2.2.1 + §2.3), the two YV12-family types (10 /
    /// 11) accept `Yv12` only (`spec/03` §6.1 + `spec/01` §2.4),
    /// and YUY2 (3) accepts `Yuy2` only (`spec/03` §6.2).
    #[test]
    fn frame_type_accepts_pixel_kind_table() {
        // Uncompressed accepts everything.
        for pk in PixelKind::all() {
            assert!(
                FrameType::Uncompressed.accepts_pixel_kind(pk),
                "Uncompressed should accept {pk:?}",
            );
        }
        // Solid + packed-RGB arithmetic: BGR(A) only.
        let rgb_only = [
            FrameType::SolidGrey,
            FrameType::SolidRgb,
            FrameType::SolidRgba,
            FrameType::UnalignedRgb24,
            FrameType::ArithmeticRgb24,
            FrameType::LegacyRgb,
            FrameType::ArithmeticRgba,
        ];
        for ft in rgb_only {
            assert!(ft.accepts_pixel_kind(PixelKind::Bgr24), "{ft:?} Bgr24");
            assert!(ft.accepts_pixel_kind(PixelKind::Bgra32), "{ft:?} Bgra32");
            assert!(!ft.accepts_pixel_kind(PixelKind::Yv12), "{ft:?} Yv12");
            assert!(!ft.accepts_pixel_kind(PixelKind::Yuy2), "{ft:?} Yuy2");
        }
        // YV12 / Reduced-Res: Yv12 only.
        for ft in [FrameType::ArithmeticYv12, FrameType::ReducedResYv12] {
            assert!(!ft.accepts_pixel_kind(PixelKind::Bgr24), "{ft:?} Bgr24");
            assert!(!ft.accepts_pixel_kind(PixelKind::Bgra32), "{ft:?} Bgra32");
            assert!(ft.accepts_pixel_kind(PixelKind::Yv12), "{ft:?} Yv12");
            assert!(!ft.accepts_pixel_kind(PixelKind::Yuy2), "{ft:?} Yuy2");
        }
        // YUY2: Yuy2 only.
        assert!(!FrameType::ArithmeticYuy2.accepts_pixel_kind(PixelKind::Bgr24));
        assert!(!FrameType::ArithmeticYuy2.accepts_pixel_kind(PixelKind::Bgra32));
        assert!(!FrameType::ArithmeticYuy2.accepts_pixel_kind(PixelKind::Yv12));
        assert!(FrameType::ArithmeticYuy2.accepts_pixel_kind(PixelKind::Yuy2));
    }

    /// Every accepted (frame-type byte, pixel-kind) pair has at
    /// least one acceptance: `accepts_pixel_kind` returns `true`
    /// for **at least one** [`PixelKind`] on every frame-type byte
    /// in the legal `1..=11` set — no frame type is structurally
    /// unreachable from the host-buffer side.
    #[test]
    fn frame_type_accepts_pixel_kind_non_empty() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            let any = PixelKind::all().iter().any(|&pk| ft.accepts_pixel_kind(pk));
            assert!(
                any,
                "{ft:?} must accept at least one PixelKind (no orphan frame type)",
            );
        }
    }

    /// `compatible_pixel_kinds` returns the exact element-wise
    /// set of [`PixelKind`] values for which
    /// [`accepts_pixel_kind`](FrameType::accepts_pixel_kind)
    /// returns `true`. Together the two accessors are
    /// equivalent classifiers — one slice-shaped, the other
    /// predicate-shaped — and disagree on no input.
    #[test]
    fn frame_type_accepts_pixel_kind_consistent_with_compatible_set() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            let compat = ft.compatible_pixel_kinds();
            for pk in PixelKind::all() {
                let in_set = compat.contains(&pk);
                let accepted = ft.accepts_pixel_kind(pk);
                assert_eq!(
                    in_set, accepted,
                    "{ft:?} / {pk:?}: compatible_pixel_kinds.contains == accepts_pixel_kind must hold",
                );
            }
        }
    }

    /// `compatible_pixel_kinds` returns the exact slice expected
    /// by `spec/01` §2.1 / §2.2.1 / §2.3 / §2.4 + `spec/03` §6.1
    /// / §6.2: all four for Uncompressed, BGR(A) for solid /
    /// packed-RGB arithmetic, single-element `Yv12` for the YV12
    /// family, single-element `Yuy2` for YUY2. Pins the exact
    /// element sequence (not just contains) so downstream
    /// iteration order is part of the public contract.
    #[test]
    fn frame_type_compatible_pixel_kinds_exact_sequence() {
        assert_eq!(
            FrameType::Uncompressed.compatible_pixel_kinds(),
            &[
                PixelKind::Bgr24,
                PixelKind::Bgra32,
                PixelKind::Yv12,
                PixelKind::Yuy2,
            ],
        );
        for ft in [
            FrameType::SolidGrey,
            FrameType::SolidRgb,
            FrameType::SolidRgba,
            FrameType::UnalignedRgb24,
            FrameType::ArithmeticRgb24,
            FrameType::LegacyRgb,
            FrameType::ArithmeticRgba,
        ] {
            assert_eq!(
                ft.compatible_pixel_kinds(),
                &[PixelKind::Bgr24, PixelKind::Bgra32],
                "{ft:?} compatible_pixel_kinds must be [Bgr24, Bgra32]",
            );
        }
        for ft in [FrameType::ArithmeticYv12, FrameType::ReducedResYv12] {
            assert_eq!(
                ft.compatible_pixel_kinds(),
                &[PixelKind::Yv12],
                "{ft:?} compatible_pixel_kinds must be [Yv12]",
            );
        }
        assert_eq!(
            FrameType::ArithmeticYuy2.compatible_pixel_kinds(),
            &[PixelKind::Yuy2],
        );
    }

    /// `accepts_pixel_kind` aligns with the existing
    /// `is_planar_yv12` / `is_packed_yuy2` predicates on the YUV
    /// frame families: a frame type is `is_planar_yv12` iff its
    /// compatible set is exactly `[Yv12]`, and is `is_packed_yuy2`
    /// iff its compatible set is exactly `[Yuy2]`.
    #[test]
    fn frame_type_accepts_pixel_kind_aligns_with_yuv_subclassifiers() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            let compat = ft.compatible_pixel_kinds();
            if ft.is_planar_yv12() {
                assert_eq!(compat, &[PixelKind::Yv12], "{ft:?} planar-YV12");
            }
            if ft.is_packed_yuy2() {
                assert_eq!(compat, &[PixelKind::Yuy2], "{ft:?} packed-YUY2");
            }
            // RGB-family arithmetic types and solid types are all
            // packed-RGB targets; their compatible set is the BGR(A)
            // pair.
            if ft.is_packed_rgb() || ft.is_solid() {
                assert_eq!(
                    compat,
                    &[PixelKind::Bgr24, PixelKind::Bgra32],
                    "{ft:?} packed-RGB / solid",
                );
            }
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

    /// `has_alpha_plane` matches exactly the two RGBA frame types
    /// per `spec/01` §2.2 row 9 + §2.3 row 8 + `spec/03` §4.3:
    /// [`ArithmeticRgba`](FrameType::ArithmeticRgba) (8) and
    /// [`SolidRgba`](FrameType::SolidRgba) (9). Every other frame
    /// type in the legal `1..=11` set is on the wire either RGB
    /// (no alpha plane), YV12 (no alpha plane per `spec/03` §4.4),
    /// YUY2 (no alpha plane per `spec/03` §4.4), or — for
    /// [`Uncompressed`](FrameType::Uncompressed) — a host-format-
    /// dependent raw passthrough where the presence of an alpha
    /// byte is a host-buffer property, not a wire-form property.
    #[test]
    fn frame_type_has_alpha_plane() {
        let rgba = [FrameType::ArithmeticRgba, FrameType::SolidRgba];
        for ft in rgba {
            assert!(
                ft.has_alpha_plane(),
                "{ft:?} should report has_alpha_plane (RGBA family)",
            );
        }
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            assert_eq!(
                ft.has_alpha_plane(),
                rgba.contains(&ft),
                "{ft:?} has_alpha_plane should match RGBA-family membership",
            );
        }
    }

    /// On every arithmetic frame type (i.e. every type for which
    /// [`FrameType::n_channels`] is non-zero), `has_alpha_plane`
    /// agrees with `n_channels() == 4`: the only arithmetic type
    /// with four wire-coded channels is
    /// [`ArithmeticRgba`](FrameType::ArithmeticRgba) (8), which is
    /// also the only arithmetic type carrying an alpha plane on
    /// the wire (`spec/03` §4.3). Pins the structural equivalence
    /// of the two accessors on the arithmetic-type subset.
    #[test]
    fn frame_type_has_alpha_plane_implies_four_channels_on_arithmetic_set() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            if ft.is_arithmetic() {
                let four_chan = ft.n_channels() == 4;
                assert_eq!(
                    ft.has_alpha_plane(),
                    four_chan,
                    "{ft:?}: on arithmetic types, has_alpha_plane must equal (n_channels == 4)",
                );
            }
        }
        // Direct check on the two arithmetic poles.
        assert!(FrameType::ArithmeticRgba.has_alpha_plane());
        assert_eq!(FrameType::ArithmeticRgba.n_channels(), 4);
        assert!(!FrameType::ArithmeticRgb24.has_alpha_plane());
        assert_eq!(FrameType::ArithmeticRgb24.n_channels(), 3);
    }

    /// `has_alpha_plane` aligns with `compatible_pixel_kinds` and
    /// `accepts_pixel_kind` in the expected way: every frame type
    /// reporting `has_alpha_plane = true` accepts the
    /// [`Bgra32`](crate::PixelKind::Bgra32) host buffer (so the
    /// decoded alpha plane has a host-side slot to land in), and
    /// the RGBA frame types' compatible-pixel-kinds slice always
    /// contains `Bgra32`. The converse does not hold — many
    /// non-RGBA frame types also accept `Bgra32` (they pad alpha
    /// with the constant `0xff` per `spec/03` §4 third bullet) —
    /// so this is a one-direction implication only.
    #[test]
    fn frame_type_has_alpha_plane_implies_bgra32_compatible() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            if ft.has_alpha_plane() {
                assert!(
                    ft.accepts_pixel_kind(PixelKind::Bgra32),
                    "{ft:?}: has_alpha_plane => accepts Bgra32 host buffer",
                );
                assert!(
                    ft.compatible_pixel_kinds().contains(&PixelKind::Bgra32),
                    "{ft:?}: has_alpha_plane => compatible_pixel_kinds contains Bgra32",
                );
            }
        }
    }

    /// `has_alpha_plane` is disjoint from the planar and packed-
    /// YUV sub-classifiers per `spec/03` §4.4 (YV12 / YUY2 have
    /// no alpha plane on the wire). Pins the orthogonality of the
    /// alpha-plane and YUV-family axes on the public accessor
    /// surface.
    #[test]
    fn frame_type_has_alpha_plane_disjoint_from_yuv_families() {
        for b in 1u8..=11 {
            let ft = FrameType::from_byte(b).unwrap();
            if ft.has_alpha_plane() {
                assert!(
                    !ft.is_planar_yv12(),
                    "{ft:?}: has_alpha_plane and is_planar_yv12 must be disjoint",
                );
                assert!(
                    !ft.is_packed_yuy2(),
                    "{ft:?}: has_alpha_plane and is_packed_yuy2 must be disjoint",
                );
            }
        }
    }
}
