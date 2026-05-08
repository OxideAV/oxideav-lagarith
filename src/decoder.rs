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

use crate::channel::decode_channel;
use crate::error::{Error, Result};
use crate::frame::{split_channels, FrameType};
use crate::predict::{apply_plane_inverse, cross_plane_decorrelate_rgb};

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
    let bpp = pixel_kind.packed_bpp().ok_or(Error::PixelFormatMismatch {
        frame_type: match shape {
            SolidShape::Grey => 5,
            SolidShape::Rgb => 6,
            SolidShape::Rgba => 9,
        },
    })?;
    let need = match shape {
        SolidShape::Grey => 2,
        SolidShape::Rgb => 4,
        SolidShape::Rgba => 5,
    };
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
    let mut pixels = Vec::with_capacity(n * bpp);
    for _ in 0..n {
        match pixel_kind {
            PixelKind::Bgr24 => {
                pixels.push(b);
                pixels.push(g);
                pixels.push(r);
            }
            PixelKind::Bgra32 => {
                pixels.push(b);
                pixels.push(g);
                pixels.push(r);
                pixels.push(a);
            }
            PixelKind::Yv12 | PixelKind::Yuy2 => {
                unreachable!("guarded by packed_bpp() above")
            }
        }
    }
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
    let n_pixels = width as usize * height as usize;
    let slices = split_channels(payload, 3)?;
    // `spec/01` §2.3 + `spec/03` §3.2 audit clarification: the wiki
    // labels the first channel "R" but it lands in output[+0] which
    // under Windows BGR memory order is the **B** byte.
    let mut plane_b = decode_channel(slices[0], n_pixels)?; // wire "R" -> output +0 (B)
    let mut plane_g = decode_channel(slices[1], n_pixels)?; // wire "G" -> output +1 (G)
    let mut plane_r = decode_channel(slices[2], n_pixels)?; // wire "B" -> output +2 (R)

    apply_plane_inverse(&mut plane_b, width as usize, height as usize);
    apply_plane_inverse(&mut plane_g, width as usize, height as usize);
    apply_plane_inverse(&mut plane_r, width as usize, height as usize);

    // `spec/03` §4: wire stores R-G and B-G. Output positions +0 and
    // +2 had G subtracted; restore via += G. Output position +1 (G)
    // is unchanged.
    cross_plane_decorrelate_rgb(&mut plane_b, &plane_g, &mut plane_r);

    let bpp = pixel_kind.packed_bpp().ok_or(Error::PixelFormatMismatch {
        frame_type: payload[0],
    })?;
    let mut pixels = Vec::with_capacity(n_pixels * bpp);
    for i in 0..n_pixels {
        match pixel_kind {
            PixelKind::Bgr24 => {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
            }
            PixelKind::Bgra32 => {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
                pixels.push(0xff);
            }
            PixelKind::Yv12 | PixelKind::Yuy2 => {
                unreachable!("guarded by packed_bpp() above")
            }
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
    let n_pixels = width as usize * height as usize;
    let slices = split_channels(payload, 4)?;
    let mut plane_b = decode_channel(slices[0], n_pixels)?; // wire R -> output +0 = B
    let mut plane_g = decode_channel(slices[1], n_pixels)?;
    let mut plane_r = decode_channel(slices[2], n_pixels)?; // wire B -> output +2 = R
    let mut plane_a = decode_channel(slices[3], n_pixels)?;

    apply_plane_inverse(&mut plane_b, width as usize, height as usize);
    apply_plane_inverse(&mut plane_g, width as usize, height as usize);
    apply_plane_inverse(&mut plane_r, width as usize, height as usize);
    apply_plane_inverse(&mut plane_a, width as usize, height as usize);

    cross_plane_decorrelate_rgb(&mut plane_b, &plane_g, &mut plane_r);
    // Alpha plane has no cross-plane decorrelation per `spec/03`
    // §4.3.

    let bpp = pixel_kind.packed_bpp().ok_or(Error::PixelFormatMismatch {
        frame_type: payload[0],
    })?;
    let mut pixels = Vec::with_capacity(n_pixels * bpp);
    for i in 0..n_pixels {
        match pixel_kind {
            PixelKind::Bgr24 => {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
            }
            PixelKind::Bgra32 => {
                pixels.push(plane_b[i]);
                pixels.push(plane_g[i]);
                pixels.push(plane_r[i]);
                pixels.push(plane_a[i]);
            }
            PixelKind::Yv12 | PixelKind::Yuy2 => {
                unreachable!("guarded by packed_bpp() above")
            }
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
    // chroma planes is W/2 × H/2 — but since `apply_plane_inverse`
    // only uses width × height = pixel-count and the per-row /
    // per-column offsets, integer-truncated sub-sample sizes
    // (W/2, H/2) match what the proprietary's predictor at
    // `lagarith.dll!0x180009f30` walks (spec/03 §6.1).
    apply_plane_inverse(&mut plane_y, w, h);
    let cw = w / 2;
    let ch = h / 2;
    if cw * ch == c_pixels {
        apply_plane_inverse(&mut plane_v, cw, ch);
        apply_plane_inverse(&mut plane_u, cw, ch);
    } else {
        // SPECGAP fallback: spec/03 §6.1.1 leaves the row/column
        // breakdown for odd-dimensioned chroma to host integration.
        // We treat the plane as a single row of `c_pixels` bytes —
        // bit-accurate for the cumulative-sum row-0 rule and a
        // best-effort placeholder for fractional rows. Tests in
        // round 2 use even dimensions only.
        apply_plane_inverse(&mut plane_v, c_pixels, 1);
        apply_plane_inverse(&mut plane_u, c_pixels, 1);
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

    apply_plane_inverse(&mut plane_y, w, h);
    apply_plane_inverse(&mut plane_u, cw, h);
    apply_plane_inverse(&mut plane_v, cw, h);

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
/// Width and height must both be at least 2 — anything smaller has
/// no valid half-resolution YV12 representation.
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
