//! Frame-level decoder API. Round 1 supports:
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
}

impl PixelKind {
    fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Bgr24 => 3,
            Self::Bgra32 => 4,
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
    let bpp = pixel_kind.bytes_per_pixel();
    let n = width as usize * height as usize * bpp;
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
    let bpp = pixel_kind.bytes_per_pixel();
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

    let bpp = pixel_kind.bytes_per_pixel();
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

    let bpp = pixel_kind.bytes_per_pixel();
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
        }
    }
    Ok(DecodedFrame {
        width,
        height,
        pixel_kind,
        pixels,
    })
}
