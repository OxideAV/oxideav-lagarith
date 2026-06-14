//! Frame-level decoder API. Rounds 1, 2, and 3 coverage:
//!
//! - **Uncompressed** (frame type 1): raw pixel data verbatim.
//! - **Solid frames** (types 5, 6, 9): byte fill.
//! - **Arithmetic RGB24** (types 2, 4 with bit-depth 24): per-plane
//!   range-coded decode + cross-plane G-pivot decorrelation reverse
//!   + spatial predictor reverse, packed BGR into the output.
//! - **Arithmetic RGB32** (type 4 with bit-depth 32): same as RGB24
//!   plus a constant `0xff` alpha byte.
//! - **Arithmetic RGBA** (type 8): four planes including alpha; G
//!   decorrelation applies to R and B only.
//! - **Arithmetic YV12** (type 10, round 2): three planes — Y at
//!   `W * H`, V and U at `(W * H) / 4` each — with per-plane left +
//!   median predictor and **no** cross-plane decorrelation
//!   (`spec/03` §6.1, §4.4). Output layout is concatenated
//!   Y / V / U planes (`PixelKind::Yv12`).
//! - **Arithmetic YUY2** (type 3, round 3): three planes — Y at
//!   `W * H`, U and V at `(W / 2) * H` each — per `spec/03` §6.2.
//!   Predictor is identical to YV12 (per-plane, no cross-plane
//!   decorrelation). The wire is **planar** (Y, U, V), the output
//!   ([`PixelKind::Yuy2`]) is **packed** (`Y0 U Y1 V` per pair of
//!   pixels at columns `2k, 2k+1`).
//! - **Reduced-resolution YV12** (type 11, round 3): per `spec/01`
//!   §2.4 + `spec/03` §6.1, the wire body is byte-identical to a
//!   type-10 (YV12) frame at half-W / half-H; the decoder
//!   reconstructs that half-resolution YV12 image and 2× nearest-
//!   neighbour upscales each of the three planes (luma + V + U) onto
//!   the host's full-resolution `PixelKind::Yv12` output buffer.
//!   The 64-bit Lagarith encoder does not produce type 11; it
//!   exists for backwards compatibility with the i386 build.
//! - **NULL frame / JUMP** (round 2): zero-byte payload signals
//!   "frame unchanged from predecessor" (`spec/01` §1.1). The
//!   stateless [`decode_frame`] reports [`Error::NullFrame`]; the
//!   stateful [`Decoder`] wrapper (or [`decode_frame_with_prev`])
//!   replays the predecessor frame as required.

use crate::channel::{decode_channel, decode_legacy_channel};
use crate::error::{Error, Result};
use crate::frame::{split_channels, FrameType};
use crate::predict::{apply_plane_inverse_with_rule, cross_plane_decorrelate_rgb, FirstColRule};

/// What pixel format the caller wants the decoder to produce.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum PixelKind {
    /// 24-bpp BGR (Windows `BI_RGB` 24-bpp DIB convention).
    Bgr24,
    /// 32-bpp BGRA, alpha = 0xff for non-RGBA frames.
    Bgra32,
    /// 12-bpp planar YV12 — Y plane (`W * H` bytes) followed by V
    /// (`(W * H) / 4` bytes) followed by U (`(W * H) / 4` bytes).
    /// `spec/03` §6.1 plane order.
    Yv12,
    /// 16-bpp packed YUY2 — `Y0 U Y1 V` per pair of pixels at
    /// columns `2k, 2k+1`. Buffer length is `W * H * 2`.
    /// `spec/03` §6.2 packed-pixel layout.
    Yuy2,
}

impl PixelKind {
    /// Byte length of the decoded buffer for this pixel format at
    /// the given dimensions.
    pub fn buffer_len(self, width: u32, height: u32) -> usize {
        let n = width as usize * height as usize;
        match self {
            Self::Bgr24 => n * 3,
            Self::Bgra32 => n * 4,
            // YV12: Y + V + U with chroma at quarter resolution.
            // Chroma is `floor((W * H) / 4)` per `spec/03` §6.1.1.
            Self::Yv12 => n + 2 * (n / 4),
            // YUY2: packed `Y0 U Y1 V` — 2 bytes per pixel.
            Self::Yuy2 => n * 2,
        }
    }

    /// `Some(bytes-per-pixel)` for packed RGB pixel formats; `None`
    /// for planar formats like YV12 where bytes-per-pixel is not a
    /// single integer. YUY2 is packed but at the macropixel level
    /// — bytes-per-pixel is 2 in aggregate but unevenly divided
    /// between luma and chroma — the unpack helper handles it.
    fn packed_bpp(self) -> Option<usize> {
        match self {
            Self::Bgr24 => Some(3),
            Self::Bgra32 => Some(4),
            Self::Yv12 | Self::Yuy2 => None,
        }
    }

    /// `true` for the RGB-family host pixel formats — `Bgr24`
    /// (24-bpp BGR, Windows `BI_RGB` 24-bpp DIB) and `Bgra32`
    /// (32-bpp BGRA). These are the host targets the decoder packs
    /// the modern arithmetic-coded RGB / RGBA frame families
    /// (`spec/01` §2.3 rows 2 / 4 / 7 / 8) — and the SOLID-RGB(A)
    /// literal frame families (`spec/01` §2.2 rows 6 / 9) — back
    /// into after the per-plane decode and inverse G-pivot
    /// decorrelation (`spec/03` §4).
    pub fn is_rgb_family(self) -> bool {
        matches!(self, Self::Bgr24 | Self::Bgra32)
    }

    /// `true` for the YUV-family host pixel formats — `Yv12`
    /// (12-bpp planar Y / V / U per `spec/03` §6.1) and `Yuy2`
    /// (16-bpp packed `Y0 U Y1 V` per `spec/03` §6.2). These are
    /// the host targets the decoder produces for the YV12 (frame
    /// type 10), reduced-resolution YV12 (type 11), and YUY2 (type
    /// 3) wire forms. Neither YUY2 nor YV12 applies any cross-plane
    /// decorrelation (`spec/03` §4.4).
    pub fn is_yuv_family(self) -> bool {
        matches!(self, Self::Yv12 | Self::Yuy2)
    }

    /// `true` for the packed-pixel host formats — `Bgr24`, `Bgra32`,
    /// and `Yuy2`. Packed formats interleave all components of one
    /// pixel (or one macropixel for `Yuy2`) contiguously in the
    /// output buffer; planar formats keep each component in a
    /// separate region. The YUY2 case is packed at the macropixel
    /// level (`Y0 U Y1 V`, 2 bytes / pixel in aggregate per
    /// `spec/03` §6.2).
    pub fn is_packed(self) -> bool {
        matches!(self, Self::Bgr24 | Self::Bgra32 | Self::Yuy2)
    }

    /// `true` for the planar host pixel formats — `Yv12` only. The
    /// `Yv12` output layout is three concatenated plane regions in
    /// Y / V / U order (`spec/03` §6.1.1). All other host formats
    /// — `Bgr24`, `Bgra32`, `Yuy2` — are packed.
    pub fn is_planar(self) -> bool {
        matches!(self, Self::Yv12)
    }

    /// `true` for host pixel formats that carry an explicit alpha
    /// channel — `Bgra32` only. For `Bgra32` the decoder writes
    /// `0xff` into the alpha byte of every output pixel when the
    /// source frame type lacks alpha (the modern RGB24 / YV12 /
    /// YUY2 / SOLID-RGB / SOLID-GREY / legacy-RGB families) and
    /// the decoded alpha plane when the source is RGBA (`spec/01`
    /// §2.3 row 8 + §2.2 row 9 SOLID-RGBA).
    pub fn has_alpha(self) -> bool {
        matches!(self, Self::Bgra32)
    }

    /// `Some(bytes-per-pixel)` for host pixel formats whose
    /// bytes-per-pixel is a single integer at the pixel level —
    /// `Bgr24` (= 3) and `Bgra32` (= 4). `None` for `Yv12` (planar,
    /// fractional aggregate) and for `Yuy2` (packed at the
    /// macropixel level — 2 bytes / pixel in aggregate but
    /// unevenly distributed across the four bytes of the macropixel).
    /// This is the public face of the existing `packed_bpp` helper.
    pub fn bytes_per_pixel(self) -> Option<usize> {
        self.packed_bpp()
    }

    /// The four host pixel formats this build's decoder recognises,
    /// in the order they appear on the public enum (`Bgr24`,
    /// `Bgra32`, `Yv12`, `Yuy2`). Useful for exhaustively driving
    /// the per-format decode dispatch in tests / fixtures.
    pub fn all() -> [Self; 4] {
        [Self::Bgr24, Self::Bgra32, Self::Yv12, Self::Yuy2]
    }
}

/// Decoded frame payload.
#[derive(Debug, Clone)]
pub struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub pixel_kind: PixelKind,
    /// Pixel data, row-major, tightly packed (`width * bpp` per row).
    pub pixels: Vec<u8>,
}

/// Decode one Lagarith-encoded frame given its payload bytes
/// (everything from the AVI `00dc` chunk after the chunk header)
/// and the host's expected dimensions / pixel format.
///
/// Stateless — surfaces zero-byte payloads as [`Error::NullFrame`].
/// For NULL-frame ("JUMP") handling that replays the predecessor
/// frame, use [`Decoder`] or [`decode_frame_with_prev`].
pub fn decode_frame(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    if payload.is_empty() {
        return Err(Error::NullFrame);
    }
    if width == 0 || height == 0 {
        return Err(Error::BadDimensions { width, height });
    }
    let frame_type = FrameType::from_byte(payload[0])?;

    match frame_type {
        FrameType::Uncompressed => decode_uncompressed(payload, width, height, pixel_kind),
        FrameType::SolidGrey => decode_solid(payload, width, height, pixel_kind, SolidShape::Grey),
        FrameType::SolidRgb => decode_solid(payload, width, height, pixel_kind, SolidShape::Rgb),
        FrameType::SolidRgba => decode_solid(payload, width, height, pixel_kind, SolidShape::Rgba),
        FrameType::ArithmeticRgb24 | FrameType::UnalignedRgb24 => {
            decode_arith_rgb(payload, width, height, pixel_kind)
        }
        FrameType::LegacyRgb => decode_legacy_rgb(payload, width, height, pixel_kind),
        FrameType::ArithmeticRgba => {
            // RGBA frames produce 4 planes; if the host asked for
            // BGR24 we drop alpha after the decode.
            decode_arith_rgba(payload, width, height, pixel_kind)
        }
        FrameType::ArithmeticYv12 => decode_arith_yv12(payload, width, height, pixel_kind),
        FrameType::ArithmeticYuy2 => decode_arith_yuy2(payload, width, height, pixel_kind),
        FrameType::ReducedResYv12 => decode_reduced_res(payload, width, height, pixel_kind),
    }
}

/// Decode one frame with optional predecessor-frame state for
/// NULL-frame ("JUMP") replay per `spec/01` §1.1.
///
/// - Non-NULL payloads decode normally and the result replaces the
///   predecessor.
/// - A zero-byte payload returns a clone of `prev` if `prev` is
///   `Some`; otherwise [`Error::NullFrameWithoutPredecessor`].
///
/// The non-stateful [`decode_frame`] rejects all NULL frames; this
/// helper centralises the state-management contract for callers
/// that don't want to carry the [`Decoder`] wrapper.
pub fn decode_frame_with_prev(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
    prev: Option<&DecodedFrame>,
) -> Result<DecodedFrame> {
    if payload.is_empty() {
        return match prev {
            Some(p) => {
                // `spec/01` §1.1: "presumably the frame is unchanged
                // from the previous frame". Cross-check the
                // (width, height, pixel_kind) tuple before replaying;
                // a NULL frame applied to a different surface is a
                // host-integration error, not a wire-format event.
                if p.width != width || p.height != height || p.pixel_kind != pixel_kind {
                    return Err(Error::PixelFormatMismatch { frame_type: 0 });
                }
                Ok(p.clone())
            }
            None => Err(Error::NullFrameWithoutPredecessor),
        };
    }
    decode_frame(payload, width, height, pixel_kind)
}

/// Stateful Lagarith frame decoder.
///
/// Tracks the last successfully decoded frame so that subsequent
/// NULL-frame ("JUMP") payloads can be expanded to a clone of the
/// predecessor per `spec/01` §1.1. The stateless [`decode_frame`]
/// rejects NULL frames; this wrapper accepts them once a predecessor
/// is present.
#[derive(Debug, Default, Clone)]
pub struct Decoder {
    /// Most recent successfully decoded frame, or `None` before any
    /// frame has been decoded.
    prev: Option<DecodedFrame>,
}

impl Decoder {
    /// Construct a fresh stateful decoder with no predecessor frame.
    pub fn new() -> Self {
        Self { prev: None }
    }

    /// Decode one frame, applying the NULL-frame replay rule when
    /// the payload is empty.
    pub fn decode(
        &mut self,
        payload: &[u8],
        width: u32,
        height: u32,
        pixel_kind: PixelKind,
    ) -> Result<DecodedFrame> {
        let frame = decode_frame_with_prev(payload, width, height, pixel_kind, self.prev.as_ref())?;
        self.prev = Some(frame.clone());
        Ok(frame)
    }

    /// Drop any cached predecessor frame (e.g. on stream seek).
    pub fn reset(&mut self) {
        self.prev = None;
    }

    /// Read-only view of the cached predecessor frame, if any.
    pub fn previous(&self) -> Option<&DecodedFrame> {
        self.prev.as_ref()
    }
}

#[derive(Debug, Copy, Clone)]
enum SolidShape {
    Grey,
    Rgb,
    Rgba,
}

fn decode_uncompressed(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    // Uncompressed frames are byte-for-byte the host's pixel format;
    // any of `Bgr24`, `Bgra32`, or `Yv12` is fine here (the buffer
    // length differs but the wire payload is the same shape).
    let n = pixel_kind.buffer_len(width, height);
    if payload.len() < 1 + n {
        return Err(Error::Truncated {
            context: "uncompressed frame body",
        });
    }
    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels: payload[1..1 + n].to_vec(),
    })
}

fn decode_solid(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
    shape: SolidShape,
) -> Result<DecodedFrame> {
    let frame_type = match shape {
        SolidShape::Grey => FrameType::SolidGrey,
        SolidShape::Rgb => FrameType::SolidRgb,
        SolidShape::Rgba => FrameType::SolidRgba,
    };
    let bpp = pixel_kind.packed_bpp().ok_or(Error::PixelFormatMismatch {
        frame_type: frame_type.to_byte(),
    })?;
    // `spec/01` §2.2.2 solid-frame total payload size (2 / 4 / 5
    // bytes); `FrameType::solid_wire_size` is the single source of
    // truth for the table.
    let need = frame_type
        .solid_wire_size()
        .expect("solid frame types have a fixed wire size");
    if payload.len() < need {
        return Err(Error::Truncated {
            context: "solid-frame colour bytes",
        });
    }
    // `spec/01` §2.2.1: wire byte 1 -> output[+0], byte 2 ->
    // output[+1], byte 3 -> output[+2] (Windows BI_RGB BGR).
    let (b, g, r, a) = match shape {
        SolidShape::Grey => (payload[1], payload[1], payload[1], 0xff),
        SolidShape::Rgb => (payload[1], payload[2], payload[3], 0xff),
        SolidShape::Rgba => (payload[1], payload[2], payload[3], payload[4]),
    };
    let n = width as usize * height as usize;
    // Pack into output (round 216 — single hoisted-branch pack loop +
    // bulk-fill). The solid-frame payload writes the same 3 or 4 byte
    // tuple to every pixel; `Vec::resize` / chunked-write turns the
    // per-pixel push loop into a bulk memset-then-stripe over the
    // pre-sized buffer. Output byte sequence is unchanged.
    let mut pixels: Vec<u8>;
    match pixel_kind {
        PixelKind::Bgr24 => {
            pixels = vec![0u8; n * 3];
            for px in pixels.chunks_exact_mut(3) {
                px[0] = b;
                px[1] = g;
                px[2] = r;
            }
        }
        PixelKind::Bgra32 => {
            pixels = vec![0u8; n * 4];
            for px in pixels.chunks_exact_mut(4) {
                px[0] = b;
                px[1] = g;
                px[2] = r;
                px[3] = a;
            }
        }
        PixelKind::Yv12 | PixelKind::Yuy2 => {
            unreachable!("guarded by packed_bpp() above")
        }
    }
    // `bpp` is the per-pixel byte width pinned by `packed_bpp()` for
    // sanity-check parity with the previous form; the chunked writes
    // above sized the buffer identically.
    debug_assert_eq!(pixels.len(), n * bpp);
    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels,
    })
}

fn decode_arith_rgb(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    // Early pixel-kind validation — fail before decoding any channel
    // bytes when the host asks for a planar buffer for a packed
    // arithmetic-RGB frame. Same `packed_bpp().ok_or` shape as the
    // post-decode guard kept at the pack site (which now becomes a
    // structural no-op for the BGR / BGRA pack loop), but moved
    // before the three Fibonacci + range-coder + predictor passes.
    let bpp = pixel_kind.packed_bpp().ok_or(Error::PixelFormatMismatch {
        frame_type: payload[0],
    })?;
    let n_pixels = width as usize * height as usize;
    let slices = split_channels(payload, 3)?;
    // `spec/01` §2.3 + `spec/03` §3.2 audit clarification: the wiki
    // labels the first channel "R" but it lands in output[+0] which
    // under Windows BGR memory order is the **B** byte.
    let mut plane_b = decode_channel(slices[0], n_pixels)?; // wire "R" -> output +0 (B)
    let mut plane_g = decode_channel(slices[1], n_pixels)?; // wire "G" -> output +1 (G)
    let mut plane_r = decode_channel(slices[2], n_pixels)?; // wire "B" -> output +2 (R)

    // First-column-of-row rule: **Rule B** (`TL = plane[y-2][W-1]`
    // for `y >= 2`). This is the `spec/06` §3.2 "linear-memory TL"
    // rule the proprietary's SIMD predictor walks; it is shared by
    // the modern RGB(A) arithmetic path. The audit/01 §9.1 dispatch
    // question (Rule A vs Rule B) was open in the cleanroom because
    // a horizontal-ramp fixture makes the two rules degenerate (the
    // first column is constant down its column, so `TL == T` and
    // both rules reduce to `T`). A black-box differential decode
    // against the independent ffmpeg `lagarith` decoder — fed our
    // own `LAGS`-wrapped frames built under each rule — resolves it:
    // ffmpeg reproduces the original pixels byte-exactly only for
    // **Rule B** encodes (every power-of-two pixel-count RGB24 /
    // RGB32 / RGBA frame tested). Rule A mis-decodes the same
    // streams. See `tests/ffmpeg_pins.rs`.
    apply_plane_inverse_with_rule(
        &mut plane_b,
        width as usize,
        height as usize,
        FirstColRule::B,
    );
    apply_plane_inverse_with_rule(
        &mut plane_g,
        width as usize,
        height as usize,
        FirstColRule::B,
    );
    apply_plane_inverse_with_rule(
        &mut plane_r,
        width as usize,
        height as usize,
        FirstColRule::B,
    );

    // `spec/03` §4: wire stores R-G and B-G. Output positions +0 and
    // +2 had G subtracted; restore via += G. Output position +1 (G)
    // is unchanged.
    cross_plane_decorrelate_rgb(&mut plane_b, &plane_g, &mut plane_r);

    // Pack into output (round 216 — single hoisted-branch pack loop).
    // `pixel_kind` is loop-invariant after the early `packed_bpp()`
    // validation, so dispatch once on it and run a tight per-pixel
    // loop in each branch. The previous form had a per-pixel `match
    // pixel_kind` in the hot loop; rustc inlined the branch but the
    // generated code still re-tested the discriminant on every
    // iteration (the compiler conservatively assumed the loop body
    // could change the value). Output byte sequence is unchanged —
    // each branch emits the same bytes in the same order as the
    // round-211 form.
    let mut pixels = Vec::with_capacity(n_pixels * bpp);
    match pixel_kind {
        PixelKind::Bgr24 => {
            for i in 0..n_pixels {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
            }
        }
        PixelKind::Bgra32 => {
            for i in 0..n_pixels {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
                pixels.push(0xff);
            }
        }
        PixelKind::Yv12 | PixelKind::Yuy2 => {
            unreachable!("guarded by early packed_bpp() above")
        }
    }
    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels,
    })
}

fn decode_arith_rgba(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    // Validate the host pixel-kind tier up-front. If the host asked
    // for a YV12 / YUY2 buffer for an RGBA-coded frame, fail before
    // doing any channel-decode work — the alternative (decode all
    // four planes then error in the pack loop) wastes the bulk of the
    // CPU time on input the caller already mis-configured.
    let bpp = pixel_kind.packed_bpp().ok_or(Error::PixelFormatMismatch {
        frame_type: payload[0],
    })?;
    let n_pixels = width as usize * height as usize;
    let slices = split_channels(payload, 4)?;
    let mut plane_b = decode_channel(slices[0], n_pixels)?; // wire R -> output +0 = B
    let mut plane_g = decode_channel(slices[1], n_pixels)?;
    let mut plane_r = decode_channel(slices[2], n_pixels)?; // wire B -> output +2 = R
                                                            // **Lazy alpha decode** (round 211). The alpha plane has no
                                                            // cross-plane decorrelation interaction (`spec/03` §4.3 — alpha
                                                            // is stored raw) and is only read at the final pack step for
                                                            // `PixelKind::Bgra32`. When the host asked for `Bgr24` the alpha
                                                            // bytes are discarded, so we skip the entire fourth-channel
                                                            // dispatch (Fibonacci probability prefix + modern range coder +
                                                            // optional RLE expansion + predictor inverse) for that case.
                                                            // Spec-grounded: spec/03 §4.3's "alpha plane is unchanged" plus
                                                            // spec/04 §5 item 5's "channels are compressed independently"
                                                            // guarantee independence — discarding the alpha slice carries no
                                                            // side-effect on the other three planes.
    let plane_a_opt = if matches!(pixel_kind, PixelKind::Bgra32) {
        let mut plane_a = decode_channel(slices[3], n_pixels)?;
        apply_plane_inverse_with_rule(
            &mut plane_a,
            width as usize,
            height as usize,
            FirstColRule::B,
        );
        Some(plane_a)
    } else {
        None
    };

    // **Rule B** first-column-of-row predictor — ffmpeg-confirmed for
    // the modern RGBA arithmetic path; see `decode_arith_rgb`.
    apply_plane_inverse_with_rule(
        &mut plane_b,
        width as usize,
        height as usize,
        FirstColRule::B,
    );
    apply_plane_inverse_with_rule(
        &mut plane_g,
        width as usize,
        height as usize,
        FirstColRule::B,
    );
    apply_plane_inverse_with_rule(
        &mut plane_r,
        width as usize,
        height as usize,
        FirstColRule::B,
    );

    cross_plane_decorrelate_rgb(&mut plane_b, &plane_g, &mut plane_r);
    // Alpha plane has no cross-plane decorrelation per `spec/03`
    // §4.3.

    // Pack into output (round 216 — single hoisted-branch pack loop).
    // Same shape as `decode_arith_rgb`; the Bgra32 arm here additionally
    // pulls a real alpha byte from `plane_a_opt` (round 211's lazy
    // alpha decode guarantees `Some(_)` on Bgra32 and `None` on Bgr24).
    let mut pixels = Vec::with_capacity(n_pixels * bpp);
    match pixel_kind {
        PixelKind::Bgr24 => {
            for i in 0..n_pixels {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
            }
        }
        PixelKind::Bgra32 => {
            // Bgra32 dispatch decoded the alpha plane above (round 211
            // lazy alpha-decode guard); the `expect` here is
            // structurally enforced by the same `matches!` guard.
            let plane_a = plane_a_opt
                .as_ref()
                .expect("Bgra32 path always decodes alpha");
            for i in 0..n_pixels {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
                pixels.push(plane_a[i]);
            }
        }
        PixelKind::Yv12 | PixelKind::Yuy2 => {
            unreachable!("guarded by packed_bpp() above")
        }
    }
    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels,
    })
}

/// Decode an arithmetic YV12 frame (type 10).
///
/// Per `spec/03` §6.1: three planes — Y at full `W * H`, V and U at
/// `floor((W * H) / 4)` each — decoded by the channel-header
/// dispatcher (same as RGB) and then **independently** processed by
/// the per-plane left + median predictor (no cross-plane
/// decorrelation, per `spec/03` §4.4). Plane order on the wire is
/// **Y, V, U** — `frame.bytes[1..5]` is the V offset, `frame.bytes[5..9]`
/// is the U offset, Y starts at byte 9. Output is concatenated
/// Y / V / U planes (same on-disk layout as the standard YV12 raw
/// pixel buffer the decoder writes per `spec/03` §6.1).
fn decode_arith_yv12(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    if pixel_kind != PixelKind::Yv12 {
        return Err(Error::PixelFormatMismatch {
            frame_type: payload[0],
        });
    }
    let w = width as usize;
    let h = height as usize;
    let y_pixels = w * h;
    // `spec/03` §6.1.1 audit-resolved: chroma plane size is the
    // integer truncation `floor((W * H) / 4)`. For odd W or H,
    // some output positions therefore have no chroma sample on
    // the wire — host integration concern, not a wire-format event.
    let c_pixels = y_pixels / 4;

    let slices = split_channels(payload, 3)?;
    // Wire plane order: Y at slice 0 (W*H), V at slice 1
    // (floor(W*H/4)), U at slice 2 (floor(W*H/4)).
    let mut plane_y = decode_channel(slices[0], y_pixels)?;
    let mut plane_v = decode_channel(slices[1], c_pixels)?;
    let mut plane_u = decode_channel(slices[2], c_pixels)?;

    // Per-plane spatial predictor reverse. Width/height for the
    // chroma planes is W/2 × H/2 — but since the predictor only uses
    // width × height = pixel-count and the per-row / per-column
    // offsets, integer-truncated sub-sample sizes (W/2, H/2) match
    // what the proprietary's predictor at `lagarith.dll!0x180009f30`
    // walks (spec/03 §6.1). The first-column-of-row rule is
    // **Rule A** (`TL = L = plane[y-1][W-1]`) per spec/06 §3.8: the
    // YV12 plane widths are always 4-byte-aligned at the natural
    // chroma subsampling, so the predictor's dispatch is
    // unconditional (no `width % 4` Rule-B branch).
    apply_plane_inverse_with_rule(&mut plane_y, w, h, FirstColRule::A);
    let cw = w / 2;
    let ch = h / 2;
    if cw * ch == c_pixels {
        apply_plane_inverse_with_rule(&mut plane_v, cw, ch, FirstColRule::A);
        apply_plane_inverse_with_rule(&mut plane_u, cw, ch, FirstColRule::A);
    } else {
        // SPECGAP fallback: spec/03 §6.1.1 leaves the row/column
        // breakdown for odd-dimensioned chroma to host integration.
        // We treat the plane as a single row of `c_pixels` bytes —
        // bit-accurate for the cumulative-sum row-0 rule and a
        // best-effort placeholder for fractional rows. Tests in
        // round 2 use even dimensions only.
        apply_plane_inverse_with_rule(&mut plane_v, c_pixels, 1, FirstColRule::A);
        apply_plane_inverse_with_rule(&mut plane_u, c_pixels, 1, FirstColRule::A);
    }

    let mut pixels = Vec::with_capacity(y_pixels + 2 * c_pixels);
    pixels.extend_from_slice(&plane_y);
    pixels.extend_from_slice(&plane_v);
    pixels.extend_from_slice(&plane_u);

    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels,
    })
}

/// Decode an arithmetic YUY2 frame (type 3, round 3).
///
/// Per `spec/03` §6.2 the wire is **planar** (Y, U, V plane order —
/// note the swap relative to YV12) where Y is `W * H` bytes and each
/// of U / V is `(W / 2) * H` bytes. The channel-offset table maps
/// plane[0] = Y, plane[1] = U, plane[2] = V. The predictor is
/// per-plane left + median (no cross-plane decorrelation, like
/// YV12 — `spec/03` §4.4). The output ([`PixelKind::Yuy2`]) is
/// **packed**: for each pair of pixels at columns `2k, 2k+1` of row
/// `y`,
///
/// ```text
/// out[(y, 2k    ) * 2 + 0] = Y[y, 2k    ]
/// out[(y, 2k    ) * 2 + 1] = U[y, k]
/// out[(y, 2k + 1) * 2 + 0] = Y[y, 2k + 1]
/// out[(y, 2k + 1) * 2 + 1] = V[y, k]
/// ```
fn decode_arith_yuy2(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    if pixel_kind != PixelKind::Yuy2 {
        return Err(Error::PixelFormatMismatch {
            frame_type: payload[0],
        });
    }
    let w = width as usize;
    let h = height as usize;
    let y_pixels = w * h;
    // Per `spec/03` §6.2 chroma planes are W/2 wide and H tall (4:2:2
    // sub-sampling). For odd widths the encoder truncates one luma
    // column to the chroma macropixel — we mirror that.
    let cw = w / 2;
    let c_pixels = cw * h;

    let slices = split_channels(payload, 3)?;
    let mut plane_y = decode_channel(slices[0], y_pixels)?;
    let mut plane_u = decode_channel(slices[1], c_pixels)?;
    let mut plane_v = decode_channel(slices[2], c_pixels)?;

    // First-column-of-row rule is **Rule A** per spec/06 §3.8 — the
    // YUY2 chroma plane width (W/2) is always 4-byte-aligned at 4:2:2
    // subsampling, so the `0x180009f30` predictor takes the
    // unconditional `TL = L = plane[y-1][W-1]` carry (no `width % 4`
    // Rule-B branch).
    apply_plane_inverse_with_rule(&mut plane_y, w, h, FirstColRule::A);
    apply_plane_inverse_with_rule(&mut plane_u, cw, h, FirstColRule::A);
    apply_plane_inverse_with_rule(&mut plane_v, cw, h, FirstColRule::A);

    // Pack Y/U/V into the YUY2 output. The output is a `W * H * 2`
    // byte buffer; we emit one full row at a time. Odd-width tail
    // gets a chroma neutral (0x80) byte to keep the macropixel
    // boundary aligned (matches the reference impl).
    let mut pixels = vec![0u8; y_pixels * 2];
    for y in 0..h {
        let y_row = y * w;
        let c_row = y * cw;
        let out_row = y * w * 2;
        for k in 0..cw {
            pixels[out_row + 4 * k] = plane_y[y_row + 2 * k];
            pixels[out_row + 4 * k + 1] = plane_u[c_row + k];
            pixels[out_row + 4 * k + 2] = plane_y[y_row + 2 * k + 1];
            pixels[out_row + 4 * k + 3] = plane_v[c_row + k];
        }
        if w % 2 == 1 {
            // Odd-width tail: last luma sample with neutral chroma.
            let last_x = w - 1;
            pixels[out_row + 2 * last_x] = plane_y[y_row + last_x];
            pixels[out_row + 2 * last_x + 1] = 0x80;
        }
    }
    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels,
    })
}

/// Decode a reduced-resolution frame (type 11, round 3).
///
/// Per `spec/01` §2.4 (audit-resolved at `audit/00-report.md` §9.1)
/// the wire body is **byte-identical to a type-10 (YV12) frame at
/// half-W / half-H**. The decoder reconstructs that half-resolution
/// YV12 image by re-interpreting byte 0 as `0x0a` and dispatching
/// through the regular YV12 path, then 2× nearest-neighbour
/// upscales every plane (luma, V, U) onto the full-resolution
/// output buffer.
///
/// Host width and height must both be **multiples of 4**. The 2×
/// nearest-neighbour upscale per `spec/01` §2.4 requires even host
/// dimensions to land output samples on the integer pixel grid (the
/// proprietary's upscaler at `lagarith.dll!0x18000ca90` /
/// `0x18000cd40` writes 2×2 output blocks per input byte, which
/// presupposes `width = 2 * half_w` and `height = 2 * half_h`).
/// Beyond that, the half-resolution YV12 body itself carries 4:2:0
/// chroma at quarter resolution (`spec/03` §6.1), so the half-W and
/// half-H must each also be even — i.e. the host W and H must each
/// be a multiple of 4 — otherwise the 2× upscaler reads from a
/// `(half_w/2) × (half_h/2)` chroma plane it cannot tile into the
/// host-buffer's `(W/2) × (H/2)` chroma slot. Non-multiple-of-4 host
/// dimensions are rejected with [`Error::BadDimensions`]; the same
/// error is returned for sub-2 host dimensions where the half-res
/// YV12 wouldn't carry any luma at all.
fn decode_reduced_res(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    if pixel_kind != PixelKind::Yv12 {
        return Err(Error::PixelFormatMismatch {
            frame_type: payload[0],
        });
    }
    // Host W/H must be a multiple of 4 (see fn-doc): only then are
    // the 2× upscale and the embedded half-res YV12 chroma both
    // integer-aligned. Reject everything else up-front so the
    // downstream `upscale_plane_2x` invocations cannot land on
    // mismatched `(src_w, dst_w)` tiles (which in release would
    // silently zero the chroma planes and in debug would
    // `debug_assert!` panic — neither is a reasonable response to
    // malformed input).
    if width % 4 != 0 || height % 4 != 0 {
        return Err(Error::BadDimensions { width, height });
    }
    let half_w = width / 2;
    let half_h = height / 2;
    if half_w < 1 || half_h < 1 {
        return Err(Error::BadDimensions { width, height });
    }
    // Re-route byte 0 to the YV12 decoder. Build a fresh payload
    // with byte 0 = 0x0a; the rest is unchanged.
    let mut sub = Vec::with_capacity(payload.len());
    sub.push(10);
    sub.extend_from_slice(&payload[1..]);
    let half = decode_arith_yv12(&sub, half_w, half_h, PixelKind::Yv12)?;

    // 2× upscale each plane (Y, V, U) by nearest-neighbour
    // duplication into a 2W × 2H output. (audit-resolved at
    // §9.1: the proprietary's upscaler at
    // `lagarith.dll!0x18000ca90` / `0x18000cd40` writes each input
    // byte to a 2×2 output block, which is the standard nearest-
    // neighbour 2× upsample.)
    let small_y = half_w as usize * half_h as usize;
    let small_cw = (half_w as usize) / 2;
    let small_ch = (half_h as usize) / 2;
    let small_c = small_cw * small_ch;
    debug_assert_eq!(half.pixels.len(), small_y + 2 * small_c);
    let big_w = width as usize;
    let big_h = height as usize;
    let big_y = big_w * big_h;
    let big_cw = big_w / 2;
    let big_ch = big_h / 2;
    let big_c = big_cw * big_ch;
    let mut out = vec![0u8; big_y + 2 * big_c];

    upscale_plane_2x(
        &half.pixels[..small_y],
        half_w as usize,
        half_h as usize,
        &mut out[..big_y],
        big_w,
    );
    upscale_plane_2x(
        &half.pixels[small_y..small_y + small_c],
        small_cw,
        small_ch,
        &mut out[big_y..big_y + big_c],
        big_cw,
    );
    upscale_plane_2x(
        &half.pixels[small_y + small_c..],
        small_cw,
        small_ch,
        &mut out[big_y + big_c..],
        big_cw,
    );

    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels: out,
    })
}

/// Decode a **legacy RGB** frame (type 7, round 4) per `spec/07`.
///
/// Per `spec/07` §1.1 type 7 is dispatched by the same RGB
/// coordinator as types 2 / 4 — three planes (R, G, B) at 24 bpp.
/// The 2 × u32 channel-offset header is identical to spec/01 §2.3.
/// What differs is the per-channel entropy decode: each channel's
/// payload is a Fibonacci-coded freq table + 1-byte reservation +
/// legacy range-coder body (`spec/07` §2..§5), not the modern
/// channel-header dispatcher of `spec/06` §1.
///
/// After decoding the three plane residuals the predictor +
/// cross-plane decorrelation pipeline runs **identically** to types
/// 2 / 4 (`spec/07` §7.2: "type 7 reuses the same predictor and the
/// same RGB cross-plane decorrelation as types 2 / 4"). The output
/// is packed BGR(A) into the host's pixel buffer.
fn decode_legacy_rgb(
    payload: &[u8],
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
) -> Result<DecodedFrame> {
    // Same early-validate pattern as `decode_arith_rgb` (round 211):
    // a Yv12 / Yuy2 host buffer for a legacy-RGB frame is a host-
    // integration error; fail before the three legacy range-coder
    // decodes which are the dominant cost of this path.
    let bpp = pixel_kind.packed_bpp().ok_or(Error::PixelFormatMismatch {
        frame_type: payload[0],
    })?;
    let n_pixels = width as usize * height as usize;
    let slices = split_channels(payload, 3)?;
    // Same plane-order convention as types 2 / 4: wire R -> output
    // +0 (B), wire G -> output +1, wire B -> output +2 (R).
    let mut plane_b = decode_legacy_channel(slices[0], n_pixels)?;
    let mut plane_g = decode_legacy_channel(slices[1], n_pixels)?;
    let mut plane_r = decode_legacy_channel(slices[2], n_pixels)?;

    // `spec/07` §9.1 item 7b: type 7 uses **Rule B** (`TL =
    // plane[y-2][W-1]` for `y >= 2`) for the first-column-of-row
    // predictor, not Rule A. Rule B matches the proprietary's
    // SIMD predictor's linear-memory walk. For `y == 1` Rule B
    // falls back to Rule A (no `y - 2` row exists).
    apply_plane_inverse_with_rule(
        &mut plane_b,
        width as usize,
        height as usize,
        FirstColRule::B,
    );
    apply_plane_inverse_with_rule(
        &mut plane_g,
        width as usize,
        height as usize,
        FirstColRule::B,
    );
    apply_plane_inverse_with_rule(
        &mut plane_r,
        width as usize,
        height as usize,
        FirstColRule::B,
    );

    cross_plane_decorrelate_rgb(&mut plane_b, &plane_g, &mut plane_r);

    // Pack into output (round 216 — single hoisted-branch pack loop).
    // Type-7 legacy uses the same BGR(A) pack shape as the modern
    // arithmetic-RGB family above. `pixel_kind` is loop-invariant
    // after the early `packed_bpp()` validation, so dispatch once.
    let mut pixels = Vec::with_capacity(n_pixels * bpp);
    match pixel_kind {
        PixelKind::Bgr24 => {
            for i in 0..n_pixels {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
            }
        }
        PixelKind::Bgra32 => {
            for i in 0..n_pixels {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
                pixels.push(0xff);
            }
        }
        PixelKind::Yv12 | PixelKind::Yuy2 => {
            unreachable!("guarded by early packed_bpp() above")
        }
    }
    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels,
    })
}

/// 2× nearest-neighbour upsample: read `src` as a `src_w × src_h`
/// plane, write each input byte to a 2×2 output block in `dst`
/// (which the caller has sized at `(2 * src_w) * (2 * src_h)`).
fn upscale_plane_2x(src: &[u8], src_w: usize, src_h: usize, dst: &mut [u8], dst_w: usize) {
    debug_assert_eq!(src.len(), src_w * src_h);
    debug_assert_eq!(dst.len(), dst_w * src_h * 2);
    debug_assert_eq!(dst_w, src_w * 2);
    for y in 0..src_h {
        let src_row = y * src_w;
        let dst_row_top = (2 * y) * dst_w;
        let dst_row_bot = (2 * y + 1) * dst_w;
        for x in 0..src_w {
            let v = src[src_row + x];
            dst[dst_row_top + 2 * x] = v;
            dst[dst_row_top + 2 * x + 1] = v;
            dst[dst_row_bot + 2 * x] = v;
            dst[dst_row_bot + 2 * x + 1] = v;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `PixelKind::all()` enumerates the four host pixel formats in
    /// the order they appear on the public enum: `Bgr24`, `Bgra32`,
    /// `Yv12`, `Yuy2`.
    #[test]
    fn pixel_kind_all_enumerates_four_variants() {
        let v = PixelKind::all();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0], PixelKind::Bgr24);
        assert_eq!(v[1], PixelKind::Bgra32);
        assert_eq!(v[2], PixelKind::Yv12);
        assert_eq!(v[3], PixelKind::Yuy2);
    }

    /// `is_rgb_family` matches exactly `Bgr24` and `Bgra32` per
    /// `spec/01` §2.3 (modern RGB / RGBA wire) + `spec/01` §2.2
    /// (SOLID-RGB / SOLID-RGBA literal). All other host formats are
    /// YUV-family.
    #[test]
    fn pixel_kind_is_rgb_family() {
        let rgbs = [PixelKind::Bgr24, PixelKind::Bgra32];
        for pk in rgbs {
            assert!(pk.is_rgb_family(), "{pk:?} should be is_rgb_family");
        }
        for pk in PixelKind::all() {
            assert_eq!(pk.is_rgb_family(), rgbs.contains(&pk), "{pk:?}");
        }
    }

    /// `is_yuv_family` matches exactly `Yv12` and `Yuy2` per
    /// `spec/03` §6.1 / §6.2. All other host formats are RGB-family.
    #[test]
    fn pixel_kind_is_yuv_family() {
        let yuvs = [PixelKind::Yv12, PixelKind::Yuy2];
        for pk in yuvs {
            assert!(pk.is_yuv_family(), "{pk:?} should be is_yuv_family");
        }
        for pk in PixelKind::all() {
            assert_eq!(pk.is_yuv_family(), yuvs.contains(&pk), "{pk:?}");
        }
    }

    /// The two color-family predicates partition the accepted set
    /// of host pixel formats: every `PixelKind` belongs to exactly
    /// one of {RGB-family, YUV-family}.
    #[test]
    fn pixel_kind_color_family_predicates_partition_set() {
        for pk in PixelKind::all() {
            let r = pk.is_rgb_family() as u8;
            let y = pk.is_yuv_family() as u8;
            assert_eq!(r + y, 1, "exactly one color family must hold for {pk:?}");
        }
    }

    /// `is_packed` matches exactly `Bgr24`, `Bgra32`, and `Yuy2`
    /// per `spec/03` §6.2 (YUY2 packed `Y0 U Y1 V`). Only `Yv12` is
    /// planar.
    #[test]
    fn pixel_kind_is_packed() {
        let packed = [PixelKind::Bgr24, PixelKind::Bgra32, PixelKind::Yuy2];
        for pk in packed {
            assert!(pk.is_packed(), "{pk:?} should be is_packed");
        }
        for pk in PixelKind::all() {
            assert_eq!(pk.is_packed(), packed.contains(&pk), "{pk:?}");
        }
    }

    /// `is_planar` matches `Yv12` only per `spec/03` §6.1.1
    /// (concatenated Y / V / U regions).
    #[test]
    fn pixel_kind_is_planar() {
        assert!(PixelKind::Yv12.is_planar());
        for pk in [PixelKind::Bgr24, PixelKind::Bgra32, PixelKind::Yuy2] {
            assert!(!pk.is_planar(), "{pk:?} should not be is_planar");
        }
    }

    /// The two memory-layout predicates partition the accepted set
    /// of host pixel formats: every `PixelKind` belongs to exactly
    /// one of {packed, planar}.
    #[test]
    fn pixel_kind_layout_predicates_partition_set() {
        for pk in PixelKind::all() {
            let p = pk.is_packed() as u8;
            let l = pk.is_planar() as u8;
            assert_eq!(p + l, 1, "exactly one layout must hold for {pk:?}");
        }
    }

    /// `has_alpha` matches `Bgra32` only. The other three host
    /// formats — `Bgr24` (no alpha byte), `Yv12` (no alpha plane),
    /// `Yuy2` (no alpha component) — report `false`.
    #[test]
    fn pixel_kind_has_alpha() {
        assert!(PixelKind::Bgra32.has_alpha());
        for pk in [PixelKind::Bgr24, PixelKind::Yv12, PixelKind::Yuy2] {
            assert!(!pk.has_alpha(), "{pk:?} should not have alpha");
        }
    }

    /// `bytes_per_pixel` returns `Some(3)` for `Bgr24`, `Some(4)`
    /// for `Bgra32`, and `None` for both YUV-family formats (`Yv12`
    /// is planar with fractional aggregate; `Yuy2` is packed at the
    /// macropixel level — 2 bytes / pixel in aggregate but unevenly
    /// distributed across the 4-byte macropixel).
    #[test]
    fn pixel_kind_bytes_per_pixel() {
        assert_eq!(PixelKind::Bgr24.bytes_per_pixel(), Some(3));
        assert_eq!(PixelKind::Bgra32.bytes_per_pixel(), Some(4));
        assert_eq!(PixelKind::Yv12.bytes_per_pixel(), None);
        assert_eq!(PixelKind::Yuy2.bytes_per_pixel(), None);
    }

    /// `bytes_per_pixel` and `is_packed` are consistent: whenever
    /// `bytes_per_pixel` returns `Some(_)`, `is_packed` is true (the
    /// converse does not hold — `Yuy2` is packed but has no
    /// single-integer bytes-per-pixel because its bytes split
    /// unevenly across the four-byte macropixel).
    #[test]
    fn pixel_kind_bytes_per_pixel_implies_packed() {
        for pk in PixelKind::all() {
            if pk.bytes_per_pixel().is_some() {
                assert!(
                    pk.is_packed(),
                    "{pk:?} with bytes_per_pixel should be is_packed"
                );
            }
        }
    }

    /// When `bytes_per_pixel` returns `Some(bpp)`, the
    /// `buffer_len(w, h)` accessor matches `w * h * bpp` exactly.
    /// This anchors the per-format buffer-sizing rule to the
    /// per-pixel byte count for the packed RGB-family formats.
    #[test]
    fn pixel_kind_buffer_len_matches_bytes_per_pixel() {
        for (w, h) in [(1u32, 1u32), (4, 4), (16, 9), (640, 480)] {
            for pk in PixelKind::all() {
                if let Some(bpp) = pk.bytes_per_pixel() {
                    assert_eq!(
                        pk.buffer_len(w, h),
                        (w as usize) * (h as usize) * bpp,
                        "{pk:?} {w}x{h}"
                    );
                }
            }
        }
    }

    /// `buffer_len` on `Yuy2` is `w * h * 2` per `spec/03` §6.2
    /// (packed `Y0 U Y1 V`, 2 bytes / pixel in aggregate). This
    /// anchors the YUY2 buffer rule even though `bytes_per_pixel`
    /// returns `None` for it (the four bytes of the macropixel
    /// split unevenly across two adjacent pixels).
    #[test]
    fn pixel_kind_buffer_len_yuy2_is_two_bytes_per_pixel() {
        for (w, h) in [(2u32, 1u32), (4, 4), (640, 480)] {
            assert_eq!(
                PixelKind::Yuy2.buffer_len(w, h),
                (w as usize) * (h as usize) * 2,
                "Yuy2 {w}x{h}"
            );
        }
    }

    /// `buffer_len` on `Yv12` matches `n + 2 * (n / 4)` per
    /// `spec/03` §6.1.1 (Y plane at `W * H`; V and U each at
    /// `floor((W * H) / 4)`). For even-W/even-H frames this is
    /// the canonical `n * 3 / 2` formula.
    #[test]
    fn pixel_kind_buffer_len_yv12_matches_spec_6_1_1() {
        for (w, h) in [(2u32, 2u32), (4, 4), (16, 16), (640, 480)] {
            let n = (w as usize) * (h as usize);
            assert_eq!(PixelKind::Yv12.buffer_len(w, h), n + 2 * (n / 4));
        }
    }
}
