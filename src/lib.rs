//! Pure-Rust **Lagarith ("LAGS")** lossless intra-only video decoder.
//!
//! Lagarith (Ben Greenwood, mid-2000s) is the AVI-wrapped successor to
//! Huffyuv: same three-corner gradient median predictor, but a range
//! coder with per-plane probability headers replaces the byte-aligned
//! Huffman stage, and a per-frame format byte selects the pixel format
//! at decode time. See `docs/video/lagarith/lagarith-trace-reverse-
//! engineering.md` for the bitstream description we implemented from.
//!
//! ## What's implemented
//!
//! * Frame-header parser for 3-plane and 4-plane variants.
//! * `SOLID_GRAY` (0x05), `SOLID_COLOR` (0x06), `SOLID_RGBA` (0x09)
//!   constant-frame paths — bit-exact decode.
//! * Per-plane `SOLID_PLANE` (`esc_count == 0xFF`) shortcut.
//! * Per-plane `UNCOMPRESSED` (`esc_count == 4`) plane reader.
//! * Median predictor with the **9-bit-gradient quirk** (the one most-
//!   often silently wrong in independent decoders) and per-row
//!   bootstrap (left-only on row 0, RGB row-1 collapse, full median
//!   thereafter).
//! * RGB cross-plane recombination (`R += G; B += G;` per row, alpha
//!   untouched).
//! * Bottom-up emission to packed `Rgb24` / `Rgba` (Lagarith's RGB rows
//!   are stored bottom-up).
//! * `ARITH_YV12` plumbing (Y, U, V planar — note the V/U slot swap
//!   relative to I420 — sets up the planes; range-coded entropy is
//!   blocked, see below).
//!
//! ## What's not implemented
//!
//! The 53-entry sparse VLC for probability magnitudes and the 256-entry
//! probability rescale array are part of the codec specification and
//! intentionally **not** transcribed in the trace doc — they would
//! require reading the upstream decoder source. Range-coded plane modes
//! (`esc_count` ∈ `{1,2,3}`) and the zero-run-only modes (`{5,6,7}`)
//! therefore return `Error::Unsupported` from
//! [`plane::decode_plane`]. Once those tables land in
//! `docs/video/lagarith/`, range-coded RGB / RGBA / YV12 / YUY2 decode
//! can land on top of the existing predictor + cross-plane pipeline.
//!
//! `ARITH_YUY2` (0x03), `OLD_ARITH_RGB` (0x07), `REDUCED_RES` (0x0b)
//! and `RAW` (0x01) are also stubbed.

#![allow(clippy::needless_range_loop)]
#![deny(missing_debug_implementations)]

pub mod decoder;
pub mod frame_header;
pub mod plane;
pub mod predictor;

pub use decoder::{decode_packet, make_decoder};

use oxideav_core::{
    CodecCapabilities, CodecId, CodecInfo, CodecRegistry, CodecTag, DecoderFactory,
};

/// Stable codec-id string used by the registry. Matches the lower-case
/// FOURCC convention of FFmpeg's "lagarith" identifier.
pub const CODEC_ID_STR: &str = "lagarith";

/// Factory value for use in [`CodecInfo::decoder`].
pub const DECODER_FACTORY: DecoderFactory = make_decoder;

/// Construct a [`CodecId`] for the Lagarith codec.
pub fn lagarith_codec_id() -> CodecId {
    CodecId::new(CODEC_ID_STR)
}

/// Register the Lagarith decoder with a [`CodecRegistry`]. Claims the
/// AVI FOURCC `LAGS` (the only FOURCC Lagarith uses in the wild).
pub fn register(reg: &mut CodecRegistry) {
    let caps = CodecCapabilities::video("lagarith_sw")
        .with_lossless(true)
        .with_intra_only(true)
        .with_max_size(65535, 65535);
    reg.register(
        CodecInfo::new(CodecId::new(CODEC_ID_STR))
            .capabilities(caps)
            .decoder(make_decoder)
            .tag(CodecTag::fourcc(b"LAGS")),
    );
}
