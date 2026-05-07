//! `oxideav-core` framework integration: codec registration plus
//! the `Decoder` impl wrapping [`crate::decode_frame`].
//!
//! Compiled only when the default-on `registry` Cargo feature is
//! enabled. Standalone consumers (`default-features = false`) skip
//! this module entirely.

#![cfg(feature = "registry")]

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecParameters, CodecRegistry, CodecTag, Decoder,
    Error as CoreError, Frame, Packet, Result as CoreResult, RuntimeContext, VideoFrame,
    VideoPlane,
};

use crate::decoder::{decode_frame, DecodedFrame, PixelKind};

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
        .with_lossless(true)
        .with_intra_only(true);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .tags([CodecTag::fourcc(b"LAGS")]),
    );
}

/// Unified entry point invoked by the macro-generated wrapper.
pub fn register(ctx: &mut RuntimeContext) {
    register_codecs(&mut ctx.codecs);
}

// ──────────────────────── Decoder impl ────────────────────────

fn make_decoder(params: &CodecParameters) -> CoreResult<Box<dyn Decoder>> {
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
