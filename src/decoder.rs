//! Top-level Lagarith decoder.
//!
//! Wires the per-frame dispatcher (frame-type byte → pixel-format
//! branch), the per-plane bitstream reader, the inverse median predictor
//! and the cross-plane RGB recombination into a [`oxideav_core::Decoder`]
//! implementation.

use std::collections::VecDeque;

use oxideav_core::{
    CodecId, CodecParameters, Decoder, Error, Frame, Packet, PixelFormat, Result, VideoFrame,
    VideoPlane,
};

use crate::frame_header::{FrameHeader, FrameType};
use crate::plane::decode_plane;
use crate::predictor::{unpredict_plane, PredictMode};

/// Build a [`LagarithDecoder`] from the supplied [`CodecParameters`].
///
/// Width and height **must** be set on `params` (Lagarith carries no
/// in-band picture-size header — the AVI `BITMAPINFOHEADER` is the
/// canonical source).
pub fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>> {
    let width = params
        .width
        .ok_or_else(|| Error::invalid("Lagarith: missing width in CodecParameters"))?;
    let height = params
        .height
        .ok_or_else(|| Error::invalid("Lagarith: missing height in CodecParameters"))?;
    if width == 0 || height == 0 {
        return Err(Error::invalid("Lagarith: zero width/height"));
    }

    // The pixel-format hint is advisory: the actual format is selected
    // per-frame by the first byte of the packet (see §3.1 of the trace
    // doc). We just remember whatever the container said so we can
    // double-check on the first frame and detect obvious mismatches.
    let hint = params.pixel_format;

    Ok(Box::new(LagarithDecoder {
        codec_id: CodecId::new(crate::CODEC_ID_STR),
        width: width as usize,
        height: height as usize,
        pix_fmt_hint: hint,
        pending: VecDeque::new(),
        eof: false,
    }))
}

struct LagarithDecoder {
    codec_id: CodecId,
    width: usize,
    height: usize,
    pix_fmt_hint: Option<PixelFormat>,
    pending: VecDeque<Packet>,
    eof: bool,
}

impl std::fmt::Debug for LagarithDecoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LagarithDecoder")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("pending", &self.pending.len())
            .finish()
    }
}

impl Decoder for LagarithDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> Result<()> {
        self.pending.push_back(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> Result<Frame> {
        let pkt = match self.pending.pop_front() {
            Some(p) => p,
            None => {
                return if self.eof {
                    Err(Error::Eof)
                } else {
                    Err(Error::NeedMore)
                }
            }
        };
        let vf = decode_packet(&pkt.data, self.width, self.height, self.pix_fmt_hint)?;
        let mut vf = vf;
        vf.pts = pkt.pts;
        Ok(Frame::Video(vf))
    }

    fn flush(&mut self) -> Result<()> {
        self.eof = true;
        Ok(())
    }

    fn reset(&mut self) -> Result<()> {
        self.pending.clear();
        self.eof = false;
        Ok(())
    }
}

/// Decode a single Lagarith packet (no PTS handling — that's the
/// caller's job). Public so tests can drive the decoder directly without
/// the `Packet` plumbing.
pub fn decode_packet(
    packet: &[u8],
    width: usize,
    height: usize,
    _pix_fmt_hint: Option<PixelFormat>,
) -> Result<VideoFrame> {
    let hdr = FrameHeader::parse(packet)?;
    match hdr.frametype {
        FrameType::SolidGray => decode_solid_gray(packet, width, height),
        FrameType::SolidColor => decode_solid_color(packet, width, height),
        FrameType::SolidRgba => decode_solid_rgba(packet, width, height),
        FrameType::ArithRgb24 | FrameType::URgb24 => {
            decode_three_plane_rgb(packet, &hdr, width, height, /*has_alpha=*/ false)
        }
        FrameType::ArithRgba => {
            decode_three_plane_rgb(packet, &hdr, width, height, /*has_alpha=*/ true)
        }
        FrameType::ArithYv12 => decode_yv12(packet, &hdr, width, height),
        FrameType::ArithYuy2 => Err(Error::unsupported(
            "Lagarith: ARITH_YUY2 (frametype 0x03) decode not yet implemented",
        )),
        FrameType::OldArithRgb => Err(Error::unsupported(
            "Lagarith: OLD_ARITH_RGB (frametype 0x07) is obsolete pre-1.1 stream",
        )),
        FrameType::ReducedRes => Err(Error::unsupported(
            "Lagarith: REDUCED_RES (frametype 0x0b) decode not yet implemented",
        )),
        FrameType::Raw => Err(Error::unsupported(
            "Lagarith: RAW (frametype 0x01) decode not yet implemented",
        )),
    }
}

// ───────────────────────── solid frame paths ─────────────────────────

/// `SOLID_GRAY` (0x05): one constant byte fills the whole picture.
/// Output as packed RGB24 with R=G=B=value.
fn decode_solid_gray(packet: &[u8], width: usize, height: usize) -> Result<VideoFrame> {
    let v = *packet
        .get(1)
        .ok_or_else(|| Error::invalid("Lagarith: SOLID_GRAY missing constant byte"))?;
    let pix = width.checked_mul(height).ok_or_else(overflow)?;
    let mut out = vec![0u8; pix * 3];
    for chunk in out.chunks_exact_mut(3) {
        chunk[0] = v;
        chunk[1] = v;
        chunk[2] = v;
    }
    packed_rgb_frame(out, width, /*bpp=*/ 3)
}

/// `SOLID_COLOR` (0x06): three constant bytes (B, G, R per the trace
/// doc's plane order). Output as packed RGB24.
fn decode_solid_color(packet: &[u8], width: usize, height: usize) -> Result<VideoFrame> {
    if packet.len() < 4 {
        return Err(Error::invalid(
            "Lagarith: SOLID_COLOR needs 3 constant bytes after frametype",
        ));
    }
    // Plane order on disk for RGB is (R, G, B) per §3.3 of the trace doc.
    let r = packet[1];
    let g = packet[2];
    let b = packet[3];
    let pix = width.checked_mul(height).ok_or_else(overflow)?;
    let mut out = Vec::with_capacity(pix * 3);
    for _ in 0..pix {
        out.push(r);
        out.push(g);
        out.push(b);
    }
    packed_rgb_frame(out, width, /*bpp=*/ 3)
}

/// `SOLID_RGBA` (0x09): four constant bytes — plane order on disk is
/// (R, G, B, A). Output as packed Rgba.
fn decode_solid_rgba(packet: &[u8], width: usize, height: usize) -> Result<VideoFrame> {
    if packet.len() < 5 {
        return Err(Error::invalid(
            "Lagarith: SOLID_RGBA needs 4 constant bytes after frametype",
        ));
    }
    let r = packet[1];
    let g = packet[2];
    let b = packet[3];
    let a = packet[4];
    let pix = width.checked_mul(height).ok_or_else(overflow)?;
    let mut out = Vec::with_capacity(pix * 4);
    for _ in 0..pix {
        out.push(r);
        out.push(g);
        out.push(b);
        out.push(a);
    }
    packed_rgb_frame(out, width, /*bpp=*/ 4)
}

// ───────────────── compressed-frame plumbing (R/G/B[/A]) ─────────────────

/// Decode a 3- or 4-plane RGB(A) frame. `has_alpha=true` for ARITH_RGBA;
/// otherwise we treat the frame as 3-plane RGB.
fn decode_three_plane_rgb(
    packet: &[u8],
    hdr: &FrameHeader,
    width: usize,
    height: usize,
    has_alpha: bool,
) -> Result<VideoFrame> {
    let plane_size = width.checked_mul(height).ok_or_else(overflow)?;
    // Plane bounds. Plane 0 starts right after the header; plane N's end
    // is plane (N+1)'s start, except for the last plane which ends at
    // the packet end.
    let p0_start = hdr.offset_plane0;
    let p1_start = hdr
        .offset_plane1
        .ok_or_else(|| Error::invalid("Lagarith: 3-plane RGB missing plane-1 offset"))?;
    let p2_start = hdr
        .offset_plane2
        .ok_or_else(|| Error::invalid("Lagarith: 3-plane RGB missing plane-2 offset"))?;
    let (p3_start, has_alpha_pkt) = if has_alpha {
        let s = hdr
            .offset_plane3
            .ok_or_else(|| Error::invalid("Lagarith: 4-plane RGB missing plane-3 offset"))?;
        (Some(s), true)
    } else {
        (None, false)
    };
    let p_end = packet.len();
    if !(p0_start <= p1_start && p1_start <= p2_start) {
        return Err(Error::invalid(
            "Lagarith: plane offset table is not monotonically increasing",
        ));
    }
    if let Some(p3) = p3_start {
        if p2_start > p3 {
            return Err(Error::invalid(
                "Lagarith: alpha plane offset precedes blue plane offset",
            ));
        }
    }

    // Plane 0 = R, Plane 1 = G, Plane 2 = B (per §3.3).
    let mut r = decode_plane(packet, p0_start, p1_start, width, height)?;
    let mut g = decode_plane(packet, p1_start, p2_start, width, height)?;
    let mut b = decode_plane(packet, p2_start, p3_start.unwrap_or(p_end), width, height)?;
    let mut a_opt = if let Some(p3) = p3_start {
        Some(decode_plane(packet, p3, p_end, width, height)?)
    } else {
        None
    };

    // Predict each non-solid plane independently.
    if !r.skip_predictor {
        unpredict_plane(&mut r.bytes, width, height, PredictMode::Rgb);
    }
    if !g.skip_predictor {
        unpredict_plane(&mut g.bytes, width, height, PredictMode::Rgb);
    }
    if !b.skip_predictor {
        unpredict_plane(&mut b.bytes, width, height, PredictMode::Rgb);
    }
    if let Some(ref mut a) = a_opt {
        if !a.skip_predictor {
            unpredict_plane(&mut a.bytes, width, height, PredictMode::Rgb);
        }
    }

    // RGB cross-plane recombination: R += G, B += G (per row).
    // (Alpha is NOT cross-decorrelated.) See §5.5.
    debug_assert_eq!(r.bytes.len(), plane_size);
    debug_assert_eq!(g.bytes.len(), plane_size);
    debug_assert_eq!(b.bytes.len(), plane_size);
    for i in 0..plane_size {
        r.bytes[i] = r.bytes[i].wrapping_add(g.bytes[i]);
        b.bytes[i] = b.bytes[i].wrapping_add(g.bytes[i]);
    }

    // Pack the planes to packed Rgb24 / Rgba in top-down row order. The
    // on-disk planes are stored bottom-up (negative stride per §7); the
    // entropy + predictor passes above produce them in their natural
    // top-down order because we never reverse the rows during decode.
    // The trace doc's "negative stride" caveat is FFmpeg-internal — it
    // sets the pointer to the last row and writes upward so the picture
    // ends up right-side-up in memory. Our planes are already top-down
    // because we never inverted them; we just pack here.
    //
    // Wait — the trace says "Lagarith's RGB rows are bottom-up". That
    // means the **first row of decoded data is the bottom of the
    // picture**. We must flip vertically when packing.
    let bpp = if has_alpha_pkt { 4 } else { 3 };
    let mut packed = vec![0u8; plane_size * bpp];
    for y in 0..height {
        let src_row = (height - 1 - y) * width;
        let dst_row = y * width * bpp;
        for x in 0..width {
            let i = src_row + x;
            let o = dst_row + x * bpp;
            packed[o] = r.bytes[i];
            packed[o + 1] = g.bytes[i];
            packed[o + 2] = b.bytes[i];
            if has_alpha_pkt {
                let a = a_opt.as_ref().unwrap();
                packed[o + 3] = a.bytes[i];
            }
        }
    }

    packed_rgb_frame(packed, width, bpp)
}

/// Decode an `ARITH_YV12` (frametype 0x0a) frame to planar `Yuv420P`.
fn decode_yv12(
    packet: &[u8],
    hdr: &FrameHeader,
    width: usize,
    height: usize,
) -> Result<VideoFrame> {
    // YV12 chroma is `(w+1)/2 × (h+1)/2`.
    let cw = width.div_ceil(2);
    let ch = height.div_ceil(2);

    let p0_start = hdr.offset_plane0;
    let p1_start = hdr
        .offset_plane1
        .ok_or_else(|| Error::invalid("Lagarith: YV12 missing plane-1 offset"))?;
    let p2_start = hdr
        .offset_plane2
        .ok_or_else(|| Error::invalid("Lagarith: YV12 missing plane-2 offset"))?;
    let p_end = packet.len();
    if !(p0_start <= p1_start && p1_start <= p2_start) {
        return Err(Error::invalid("Lagarith: YV12 plane offsets not monotonic"));
    }

    // Plane 0 = Y, Plane 1 = V (offset_gu), Plane 2 = U (offset_bv).
    // Note the V/U swap — see §3.3 of the trace doc.
    let mut y = decode_plane(packet, p0_start, p1_start, width, height)?;
    let mut v = decode_plane(packet, p1_start, p2_start, cw, ch)?;
    let mut u = decode_plane(packet, p2_start, p_end, cw, ch)?;

    if !y.skip_predictor {
        unpredict_plane(&mut y.bytes, width, height, PredictMode::Yuv);
    }
    if !u.skip_predictor {
        unpredict_plane(&mut u.bytes, cw, ch, PredictMode::Yuv);
    }
    if !v.skip_predictor {
        unpredict_plane(&mut v.bytes, cw, ch, PredictMode::Yuv);
    }

    // Emit as Yuv420P (Y, U, V planar).
    let frame = VideoFrame {
        pts: None,
        planes: vec![
            VideoPlane {
                stride: width,
                data: y.bytes,
            },
            VideoPlane {
                stride: cw,
                data: u.bytes,
            },
            VideoPlane {
                stride: cw,
                data: v.bytes,
            },
        ],
    };
    Ok(frame)
}

// ───────────────── helpers ─────────────────

fn packed_rgb_frame(data: Vec<u8>, width: usize, bpp: usize) -> Result<VideoFrame> {
    Ok(VideoFrame {
        pts: None,
        planes: vec![VideoPlane {
            stride: width * bpp,
            data,
        }],
    })
}

fn overflow() -> Error {
    Error::invalid("Lagarith: width*height overflow")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_rgba_black() {
        // The first frame of `2889-assassin_OL.avi` (per the trace doc):
        // SOLID_RGBA = 0x09, R=0, G=0, B=0, A=0.
        let pkt = [0x09u8, 0, 0, 0, 0];
        let vf = decode_packet(&pkt, 4, 2, None).unwrap();
        assert_eq!(vf.planes.len(), 1);
        let p = &vf.planes[0];
        assert_eq!(p.stride, 4 * 4);
        assert_eq!(p.data, vec![0u8; 4 * 2 * 4]);
    }

    #[test]
    fn solid_rgba_known_color() {
        // R=0x10, G=0x20, B=0x30, A=0x40 over a 2x2 picture.
        let pkt = [0x09u8, 0x10, 0x20, 0x30, 0x40];
        let vf = decode_packet(&pkt, 2, 2, None).unwrap();
        let p = &vf.planes[0];
        assert_eq!(
            p.data,
            vec![
                0x10, 0x20, 0x30, 0x40, // px (0,0)
                0x10, 0x20, 0x30, 0x40, // px (1,0)
                0x10, 0x20, 0x30, 0x40, // px (0,1)
                0x10, 0x20, 0x30, 0x40, // px (1,1)
            ]
        );
    }

    #[test]
    fn solid_color_rgb24() {
        // R=0xAA, G=0xBB, B=0xCC over a 3x1 picture.
        let pkt = [0x06u8, 0xAA, 0xBB, 0xCC];
        let vf = decode_packet(&pkt, 3, 1, None).unwrap();
        let p = &vf.planes[0];
        assert_eq!(p.stride, 9);
        assert_eq!(
            p.data,
            vec![0xAA, 0xBB, 0xCC, 0xAA, 0xBB, 0xCC, 0xAA, 0xBB, 0xCC]
        );
    }

    #[test]
    fn solid_gray_rgb24() {
        // grey 0x77 over a 2x2 picture.
        let pkt = [0x05u8, 0x77];
        let vf = decode_packet(&pkt, 2, 2, None).unwrap();
        let p = &vf.planes[0];
        assert_eq!(p.data, vec![0x77; 12]);
    }

    #[test]
    fn arith_rgba_unsupported_until_tables_land() {
        // A truthful arith-coded packet is too big to inline. Fake the
        // header fields enough that the dispatcher reaches plane decode
        // and then errors out on the RAC mode byte.
        // ARITH_RGBA, plane offsets (1085, 5746, 7790) — but a 32-byte
        // packet is enough to reach plane 0's mode byte and report
        // Unsupported.
        let mut pkt = vec![0x08u8];
        pkt.extend_from_slice(&[14, 0, 0, 0]); // gu = 14
        pkt.extend_from_slice(&[16, 0, 0, 0]); // bv = 16
        pkt.extend_from_slice(&[18, 0, 0, 0]); // a  = 18
        pkt.push(1); // plane 0 mode = 1 (arith) at offset 13
        pkt.push(0);
        pkt.push(1);
        pkt.push(0);
        pkt.push(1);
        pkt.push(0);
        // Need width*height plane bounds. Choose 1x1 to keep math trivial.
        let err = decode_packet(&pkt, 1, 1, None).unwrap_err();
        assert!(matches!(err, Error::Unsupported(_)));
    }
}
