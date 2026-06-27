//! `oxideav-core` framework integration: codec registration plus
//! the `Decoder` impl wrapping [`crate::decode_frame`].
//!
//! Compiled only when the default-on `registry` Cargo feature is
//! enabled. Standalone consumers (`default-features = false`) skip
//! this module entirely.

#![cfg(feature = "registry")]

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, CodecTag, Decoder,
    Encoder, Error as CoreError, Frame, Packet, PixelFormat, Result as CoreResult, RuntimeContext,
    TimeBase, VideoFrame, VideoPlane,
};

use crate::decoder::{decode_frame, DecodedFrame, PixelKind};
use crate::encoder::encode_frame;

/// Canonical codec id. `oxideav-meta::register_all` calls
/// `crate::__oxideav_entry`, which delegates here.
pub const CODEC_ID_STR: &str = "lagarith";

/// Register the Lagarith codec with `reg`.
///
/// The native FOURCC is `LAGS` (`spec/01` §4) — that's the byte
/// sequence the encoder writes to a `BITMAPINFOHEADER`'s
/// `biCompression` field and that the decoder validates against
/// (`spec/01` §5). Registering it here lets the AVI demuxer route a
/// `LAGS` chunk straight through `CodecResolver` without a hand-
/// maintained FourCC table.
pub fn register_codecs(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("lagarith_sw")
        .with_decode()
        .with_encode()
        .with_lossless(true)
        .with_intra_only(true);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .encoder(make_encoder)
            .tags([CodecTag::fourcc(b"LAGS")]),
    );
}

/// Unified entry point invoked by the macro-generated wrapper.
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
}

// ──────────────────────── Decoder impl ────────────────────────

/// Direct decoder factory (the dual-API convention's historical
/// endpoint). Builds a boxed [`Decoder`] for `params`; the host pixel
/// format defaults to BGRA32. Re-exported as
/// [`crate::make_decoder`].
pub fn make_decoder(params: &CodecParameters) -> CoreResult<Box<dyn Decoder>> {
    let width = params.width.unwrap_or(0);
    let height = params.height.unwrap_or(0);
    let pixel_kind = PixelKind::Bgra32;
    Ok(Box::new(LagarithDecoder {
        codec_id: params.codec_id.clone(),
        width,
        height,
        pixel_kind,
        pending: None,
        eof: false,
    }))
}

struct LagarithDecoder {
    codec_id: CodecId,
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
    pending: Option<Packet>,
    eof: bool,
}

impl Decoder for LagarithDecoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn send_packet(&mut self, packet: &Packet) -> CoreResult<()> {
        if self.pending.is_some() {
            return Err(CoreError::other(
                "oxideav-lagarith: receive_frame must be called before sending another packet",
            ));
        }
        self.pending = Some(packet.clone());
        Ok(())
    }

    fn receive_frame(&mut self) -> CoreResult<Frame> {
        let Some(pkt) = self.pending.take() else {
            return if self.eof {
                Err(CoreError::Eof)
            } else {
                Err(CoreError::NeedMore)
            };
        };
        if self.width == 0 || self.height == 0 {
            return Err(CoreError::invalid(
                "oxideav-lagarith: width/height must be set on CodecParameters",
            ));
        }
        let frame = decode_frame(&pkt.data, self.width, self.height, self.pixel_kind)
            .map_err(|e| CoreError::invalid(format!("oxideav-lagarith: {e}")))?;
        Ok(Frame::Video(map_to_video_frame(frame, pkt.pts)))
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.eof = true;
        Ok(())
    }
}

fn map_to_video_frame(frame: DecodedFrame, pts: Option<i64>) -> VideoFrame {
    match frame.pixel_kind {
        PixelKind::Bgr24 | PixelKind::Bgra32 => {
            let bpp = if frame.pixel_kind == PixelKind::Bgra32 {
                4
            } else {
                3
            };
            let stride = frame.width as usize * bpp;
            VideoFrame {
                pts,
                planes: vec![VideoPlane {
                    stride,
                    data: frame.pixels,
                }],
            }
        }
        PixelKind::Yv12 => {
            // YV12: split the concatenated Y / V / U buffer back
            // into three `VideoPlane`s with their respective strides.
            let w = frame.width as usize;
            let h = frame.height as usize;
            let y_pixels = w * h;
            let c_pixels = y_pixels / 4;
            let cw = w / 2;
            let mut data = frame.pixels;
            let plane_y: Vec<u8> = data.drain(..y_pixels).collect();
            let plane_v: Vec<u8> = data.drain(..c_pixels).collect();
            let plane_u: Vec<u8> = data.drain(..c_pixels).collect();
            VideoFrame {
                pts,
                planes: vec![
                    VideoPlane {
                        stride: w,
                        data: plane_y,
                    },
                    VideoPlane {
                        stride: cw,
                        data: plane_v,
                    },
                    VideoPlane {
                        stride: cw,
                        data: plane_u,
                    },
                ],
            }
        }
        PixelKind::Yuy2 => {
            // YUY2: packed `Y0 U Y1 V` macropixels, 2 bytes per pixel
            // — surface as a single packed plane (the most common
            // host integration). Stride is `W * 2` bytes.
            let stride = frame.width as usize * 2;
            VideoFrame {
                pts,
                planes: vec![VideoPlane {
                    stride,
                    data: frame.pixels,
                }],
            }
        }
    }
}

// ──────────────────────── Encoder impl ────────────────────────

/// Map a core [`PixelFormat`] to the Lagarith [`PixelKind`] whose
/// host buffer layout the encoder understands. Returns `None` for
/// formats Lagarith has no frame type for.
///
/// The four supported families are the ones `spec/01` §3 enumerates a
/// frame type for: packed `Bgr24` (types 2/4/5/6), packed `Bgra`
/// (types 8/9), planar `Yuv420P` ≈ YV12 (type 10), and packed
/// `Yuyv422` ≈ YUY2 (type 3). `None` ⇒ the factory rejects the
/// parameter set so a caller gets a clear error instead of a silent
/// mis-encode.
fn pixel_kind_from_format(fmt: PixelFormat) -> Option<PixelKind> {
    match fmt {
        PixelFormat::Bgr24 => Some(PixelKind::Bgr24),
        PixelFormat::Bgra => Some(PixelKind::Bgra32),
        PixelFormat::Yuv420P => Some(PixelKind::Yv12),
        PixelFormat::Yuyv422 => Some(PixelKind::Yuy2),
        _ => None,
    }
}

/// Direct encoder factory (the dual-API convention's historical
/// endpoint). Builds a boxed [`Encoder`] for `params`; the host pixel
/// format is read from `CodecParameters::pixel_format` (defaulting to
/// BGRA32) and an unsupported format is rejected here. Re-exported as
/// [`crate::make_encoder`].
pub fn make_encoder(params: &CodecParameters) -> CoreResult<Box<dyn Encoder>> {
    let width = params.width.unwrap_or(0);
    let height = params.height.unwrap_or(0);
    if width == 0 || height == 0 {
        return Err(CoreError::invalid(
            "oxideav-lagarith: encoder requires non-zero width/height on CodecParameters",
        ));
    }
    // Default to BGRA32 (the decoder's default host format) when the
    // caller did not pin a pixel format.
    let pixel_kind = match params.pixel_format {
        Some(fmt) => pixel_kind_from_format(fmt).ok_or_else(|| {
            CoreError::invalid(format!(
                "oxideav-lagarith: unsupported pixel format {fmt:?} \
                 (supported: Bgr24, Bgra, Yuv420P, Yuyv422)"
            ))
        })?,
        None => PixelKind::Bgra32,
    };
    let mut out_params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
    out_params.width = Some(width);
    out_params.height = Some(height);
    out_params.pixel_format = params.pixel_format;
    out_params.tag = Some(CodecTag::fourcc(b"LAGS"));
    Ok(Box::new(LagarithEncoder {
        codec_id: CodecId::new(CODEC_ID_STR),
        width,
        height,
        pixel_kind,
        out_params,
        pending: None,
        eof: false,
    }))
}

struct LagarithEncoder {
    codec_id: CodecId,
    width: u32,
    height: u32,
    pixel_kind: PixelKind,
    out_params: CodecParameters,
    pending: Option<Vec<u8>>,
    eof: bool,
}

impl LagarithEncoder {
    /// Reassemble the packed Lagarith host buffer (`pixel_kind`
    /// layout) from a [`VideoFrame`]'s planes — the inverse of the
    /// decoder's [`map_to_video_frame`] split. Strips any stride
    /// padding so the buffer is exactly `pixel_kind.buffer_len(w, h)`
    /// bytes, the layout [`encode_frame`] documents.
    fn pack_planes(&self, vf: &VideoFrame) -> CoreResult<Vec<u8>> {
        let w = self.width as usize;
        let h = self.height as usize;
        // Copy `row_bytes` bytes per row out of a (possibly
        // stride-padded) plane into a tight buffer.
        let tight = |plane: &VideoPlane, row_bytes: usize, rows: usize| -> CoreResult<Vec<u8>> {
            if plane.stride < row_bytes {
                return Err(CoreError::invalid(format!(
                    "oxideav-lagarith: plane stride {} below row width {row_bytes}",
                    plane.stride
                )));
            }
            if plane.data.len() < plane.stride * rows {
                return Err(CoreError::invalid(format!(
                    "oxideav-lagarith: plane has {} bytes, expected at least stride*rows = {}",
                    plane.data.len(),
                    plane.stride * rows
                )));
            }
            let mut out = Vec::with_capacity(row_bytes * rows);
            for r in 0..rows {
                let start = r * plane.stride;
                out.extend_from_slice(&plane.data[start..start + row_bytes]);
            }
            Ok(out)
        };
        match self.pixel_kind {
            PixelKind::Bgr24 => {
                expect_planes(vf, 1)?;
                tight(&vf.planes[0], w * 3, h)
            }
            PixelKind::Bgra32 => {
                expect_planes(vf, 1)?;
                tight(&vf.planes[0], w * 4, h)
            }
            PixelKind::Yuy2 => {
                expect_planes(vf, 1)?;
                tight(&vf.planes[0], w * 2, h)
            }
            PixelKind::Yv12 => {
                // Three planes, wire order Y / V / U (the layout the
                // decoder emits and `encode_arith_yv12` consumes).
                expect_planes(vf, 3)?;
                let cw = w / 2;
                let ch = h / 2;
                let mut buf = tight(&vf.planes[0], w, h)?;
                buf.extend(tight(&vf.planes[1], cw, ch)?);
                buf.extend(tight(&vf.planes[2], cw, ch)?);
                Ok(buf)
            }
        }
    }
}

fn expect_planes(vf: &VideoFrame, want: usize) -> CoreResult<()> {
    if vf.planes.len() != want {
        return Err(CoreError::invalid(format!(
            "oxideav-lagarith: encoder expected {want} plane(s), got {}",
            vf.planes.len()
        )));
    }
    Ok(())
}

impl Encoder for LagarithEncoder {
    fn codec_id(&self) -> &CodecId {
        &self.codec_id
    }

    fn output_params(&self) -> &CodecParameters {
        &self.out_params
    }

    fn send_frame(&mut self, frame: &Frame) -> CoreResult<()> {
        if self.pending.is_some() {
            return Err(CoreError::other(
                "oxideav-lagarith: receive_packet must be called before sending another frame",
            ));
        }
        let vf = match frame {
            Frame::Video(v) => v,
            _ => {
                return Err(CoreError::invalid(
                    "oxideav-lagarith: encoder expected a video frame",
                ));
            }
        };
        let pixels = self.pack_planes(vf)?;
        let bytes = encode_frame(&pixels, self.width, self.height, self.pixel_kind)
            .map_err(|e| CoreError::invalid(format!("oxideav-lagarith: {e}")))?;
        self.pending = Some(bytes);
        Ok(())
    }

    fn receive_packet(&mut self) -> CoreResult<Packet> {
        match self.pending.take() {
            // Every Lagarith frame is intra-only (the codec is
            // lossless and stateless across frames — `spec/00`), so
            // each emitted packet is a keyframe.
            Some(bytes) => Ok(Packet::new(0, TimeBase::new(1, 1), bytes).with_keyframe(true)),
            None => {
                if self.eof {
                    Err(CoreError::Eof)
                } else {
                    Err(CoreError::NeedMore)
                }
            }
        }
    }

    fn flush(&mut self) -> CoreResult<()> {
        self.eof = true;
        Ok(())
    }
}

// ──────────────────────── tests ────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use oxideav_core::{CodecId, CodecParameters, MediaType, Packet, ProbeContext, TimeBase};

    #[test]
    fn register_via_runtime_context_installs_codec() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let codec_id = CodecId::new(CODEC_ID_STR);
        assert!(ctx.codecs.has_decoder(&codec_id));
    }

    #[test]
    fn register_claims_lags_fourcc() {
        let mut reg = CodecRegistry::new();
        register_codecs(&mut reg);
        let tag = CodecTag::fourcc(b"LAGS");
        let resolved = reg
            .resolve_tag_ref(&ProbeContext::new(&tag))
            .map(|c| c.as_str());
        assert_eq!(resolved, Some(CODEC_ID_STR));
    }

    /// The dual-API direct factory endpoints (`make_decoder` /
    /// `make_encoder`) are usable without the registry, and the codecs
    /// they build round-trip a frame.
    #[test]
    fn direct_factories_roundtrip() {
        use oxideav_core::PixelFormat;
        let (w, h) = (4u32, 4u32);
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.media_type = MediaType::Video;
        params.width = Some(w);
        params.height = Some(h);
        params.pixel_format = Some(PixelFormat::Bgra);

        let pixels: Vec<u8> = (0..(w * h * 4)).map(|i| (i * 11 % 251) as u8).collect();

        let mut enc = make_encoder(&params).expect("make_encoder");
        enc.send_frame(&Frame::Video(VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: (w * 4) as usize,
                data: pixels.clone(),
            }],
        }))
        .unwrap();
        let pkt = enc.receive_packet().unwrap();

        let mut dec = make_decoder(&params).expect("make_decoder");
        dec.send_packet(&pkt).unwrap();
        match dec.receive_frame().unwrap() {
            Frame::Video(v) => assert_eq!(v.planes[0].data, pixels),
            other => panic!("expected video frame, got {other:?}"),
        }
    }

    #[test]
    fn register_installs_encoder() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let codec_id = CodecId::new(CODEC_ID_STR);
        assert!(ctx.codecs.has_encoder(&codec_id));
    }

    /// Round-trip a BGRA32 frame all the way through the framework
    /// `Encoder` → `Decoder` pair and confirm byte-exact recovery.
    #[test]
    fn end_to_end_encode_decode_bgra() {
        use oxideav_core::PixelFormat;
        let (w, h) = (8u32, 4u32);
        // Deterministic gradient content (compresses; not solid).
        let mut pixels = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                pixels.extend_from_slice(&[
                    (x * 7) as u8,
                    (y * 13) as u8,
                    (x.wrapping_add(y) * 3) as u8,
                    0xff,
                ]);
            }
        }

        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.media_type = MediaType::Video;
        params.width = Some(w);
        params.height = Some(h);
        params.pixel_format = Some(PixelFormat::Bgra);

        let mut enc = ctx.codecs.first_encoder(&params).expect("first_encoder");
        let vf = VideoFrame {
            pts: Some(0),
            planes: vec![VideoPlane {
                stride: (w * 4) as usize,
                data: pixels.clone(),
            }],
        };
        enc.send_frame(&Frame::Video(vf)).unwrap();
        let pkt = enc.receive_packet().unwrap();
        assert!(pkt.is_keyframe());
        // Second receive without a new frame is NeedMore.
        assert!(matches!(
            enc.receive_packet(),
            Err(oxideav_core::Error::NeedMore)
        ));

        // Decode the produced packet back through the framework.
        let mut dec = ctx.codecs.first_decoder(&params).expect("first_decoder");
        // first_decoder defaults the host format to BGRA32, matching.
        dec.send_packet(&pkt).unwrap();
        let frame = dec.receive_frame().unwrap();
        match frame {
            Frame::Video(v) => {
                assert_eq!(v.planes.len(), 1);
                assert_eq!(v.planes[0].data, pixels);
            }
            other => panic!("expected video frame, got {other:?}"),
        }
    }

    /// YV12 (3-plane) round-trip through the framework encoder, with a
    /// stride-padded luma plane to exercise the `pack_planes`
    /// stride-stripping path.
    #[test]
    fn end_to_end_encode_decode_yv12_padded_stride() {
        use oxideav_core::PixelFormat;
        let (w, h) = (8u32, 4u32);
        let ylen = (w * h) as usize;
        let clen = ylen / 4;
        let y: Vec<u8> = (0..ylen).map(|i| (i * 3 % 251) as u8).collect();
        let v: Vec<u8> = (0..clen).map(|i| (i * 5 % 251) as u8).collect();
        let u: Vec<u8> = (0..clen).map(|i| (i * 7 % 251) as u8).collect();

        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.media_type = MediaType::Video;
        params.width = Some(w);
        params.height = Some(h);
        params.pixel_format = Some(PixelFormat::Yuv420P);

        let mut enc = ctx.codecs.first_encoder(&params).expect("first_encoder");
        // Pad the luma plane stride by 4 bytes/row; the encoder must
        // strip the padding before encoding.
        let pad = 4usize;
        let mut y_padded = Vec::with_capacity((w as usize + pad) * h as usize);
        for r in 0..h as usize {
            y_padded.extend_from_slice(&y[r * w as usize..(r + 1) * w as usize]);
            y_padded.extend(std::iter::repeat_n(0u8, pad));
        }
        let vf = VideoFrame {
            pts: Some(0),
            planes: vec![
                VideoPlane {
                    stride: w as usize + pad,
                    data: y_padded,
                },
                VideoPlane {
                    stride: (w / 2) as usize,
                    data: v.clone(),
                },
                VideoPlane {
                    stride: (w / 2) as usize,
                    data: u.clone(),
                },
            ],
        };
        enc.send_frame(&Frame::Video(vf)).unwrap();
        let pkt = enc.receive_packet().unwrap();

        let decoded = decode_frame(&pkt.data, w, h, PixelKind::Yv12).unwrap();
        let mut expected = y.clone();
        expected.extend_from_slice(&v);
        expected.extend_from_slice(&u);
        assert_eq!(decoded.pixels, expected);
    }

    /// An unsupported pixel format is rejected at `make_encoder` time
    /// with a clear error rather than a silent mis-encode.
    #[test]
    fn encoder_rejects_unsupported_pixel_format() {
        use oxideav_core::PixelFormat;
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.media_type = MediaType::Video;
        params.width = Some(4);
        params.height = Some(4);
        params.pixel_format = Some(PixelFormat::Gray8);
        assert!(ctx.codecs.first_encoder(&params).is_err());
    }

    #[test]
    fn pixel_kind_mapping_is_exhaustive_for_supported() {
        use oxideav_core::PixelFormat;
        assert_eq!(
            pixel_kind_from_format(PixelFormat::Bgr24),
            Some(PixelKind::Bgr24)
        );
        assert_eq!(
            pixel_kind_from_format(PixelFormat::Bgra),
            Some(PixelKind::Bgra32)
        );
        assert_eq!(
            pixel_kind_from_format(PixelFormat::Yuv420P),
            Some(PixelKind::Yv12)
        );
        assert_eq!(
            pixel_kind_from_format(PixelFormat::Yuyv422),
            Some(PixelKind::Yuy2)
        );
        assert_eq!(pixel_kind_from_format(PixelFormat::Gray8), None);
    }

    #[test]
    fn end_to_end_decode_solid_grey() {
        let mut ctx = RuntimeContext::new();
        register(&mut ctx);
        let mut params = CodecParameters::video(CodecId::new(CODEC_ID_STR));
        params.media_type = MediaType::Video;
        params.width = Some(4);
        params.height = Some(2);
        let mut dec = ctx.codecs.first_decoder(&params).expect("first_decoder");
        // Type-5 solid grey, value 0x77.
        let pkt = Packet::new(0, TimeBase::new(1, 30), vec![5, 0x77]);
        dec.send_packet(&pkt).unwrap();
        let frame = dec.receive_frame().unwrap();
        match frame {
            Frame::Video(v) => {
                // 4 wide × 2 high × 4 bpp BGRA = 32 bytes
                assert_eq!(v.planes.len(), 1);
                assert_eq!(v.planes[0].data.len(), 32);
                for chunk in v.planes[0].data.chunks_exact(4) {
                    assert_eq!(chunk, &[0x77, 0x77, 0x77, 0xff]);
                }
            }
            other => panic!("expected video frame, got {other:?}"),
        }
    }
}
