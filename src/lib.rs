//! Pure-Rust Lagarith lossless video codec (decoder + encoder).
//!
//! **Rounds 1..5 of the clean-room rebuild.** Implements the modern
//! arithmetic-coded RGB24 / RGB32 / RGBA decode pipeline plus the
//! Uncompressed (type 1) and Solid (types 5 / 6 / 9) literal frames
//! (round 1); the YV12 (type 10) multi-plane arithmetic decode path
//! and a stateful [`Decoder`] wrapper that replays NULL ("JUMP")
//! frames per `spec/01` §1.1 (round 2); the YUY2 (type 3)
//! packed→planar pipeline + reduced-resolution (type 11) — a
//! half-W/half-H YV12 body with a 2× nearest-neighbour upscale on
//! decode (round 3); the **legacy RGB** (type 7, pre-1.1.0)
//! adaptive-CDF range coder per `spec/07` (round 4); and round 5's
//! type-7 spec-coverage extensions — **Rule B** first-column
//! predictor (`spec/07` §9.1 item 7b) and the **RLE-then-Fibonacci**
//! channel sub-path (header `0x01..=0x03`, `spec/07` §2.3 / §2.4).
//! All against the strict-isolation cleanroom workspace at
//! `docs/video/lagarith/`.
//!
//! ## Pipeline (arithmetic-coded RGB family)
//!
//! 1. **Frame layout** ([`spec/01`]): byte 0 is the frame-type
//!    selector; non-NULL frames carry an `(n_channels - 1) * 4`
//!    byte channel-offset table next. RGB24/RGB32 use 3 channels;
//!    RGBA uses 4.
//! 2. **Per-channel header dispatcher** ([`spec/03` §2.1] +
//!    [`spec/06` §1]): the channel's first byte selects between
//!    arithmetic decode, raw memcpy, RLE-only, and solid-fill.
//! 3. **Fibonacci probability prefix** ([`spec/04`]): bit-stream
//!    (MSB-first) decode of the 256-entry frequency table.
//! 4. **Modern range coder** ([`spec/02`]): TOP = 2^23, init range
//!    = 2^31, four-byte priming with a 31-bit init state, byte
//!    refill with cross-byte LSB rotation, four-byte flush tail.
//! 5. **Residual zero-run RLE escape** ([`spec/05`]): post-process
//!    expansion of `escape_len + LUT[supplement_byte]`-zero runs.
//! 6. **Spatial predictor** ([`spec/03` §3]): left for row 0,
//!    JPEG-LS clamped median for rows ≥ 1, with the **Rule B**
//!    `TL = plane[y-2][W-1]` (for `y ≥ 2`) first-column rule —
//!    oracle-confirmed for the modern RGB(A) types (round 124).
//! 7. **Cross-plane decorrelation** ([`spec/03` §4]): RGB families
//!    only — `R += G; B += G` after spatial prediction; alpha
//!    plane (RGBA) is unchanged.
//!
//! ## Public API
//!
//! - [`decode_frame`] — decode one Lagarith-encoded frame's bytes.
//! - [`encode_frame`] — encode one raw frame into Lagarith bytes; the
//!   symmetric counterpart of [`decode_frame`], with automatic
//!   smallest-wire-form (solid / arithmetic / uncompressed)
//!   selection. Round-trips byte-exactly back through `decode_frame`.
//! - [`encode_null`] — the zero-byte NULL ("JUMP") payload
//!   (`spec/01` §1.1).
//! - [`PixelKind`] — host-side pixel format selector
//!   (`Bgr24` / `Bgra32` / `Yv12` / `Yuy2`) shared by both directions.
//! - [`Error`] / [`Result`] — crate-local error type.
//!
//! ## Cargo features
//!
//! - **`registry`** (default): wire the crate into `oxideav-core`'s
//!   codec registry.
//!
//! [`spec/01`]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/01-frame-data-layout.md
//! [`spec/02`]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/02-range-coder-framing.md
//! [`spec/03` §2.1]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/03-channel-decorrelation-and-predictors.md
//! [`spec/03` §3]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/03-channel-decorrelation-and-predictors.md
//! [`spec/03` §4]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/03-channel-decorrelation-and-predictors.md
//! [`spec/04`]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/04-fibonacci-probability-prefix.md
//! [`spec/05`]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/05-rle-escape-bit-format.md
//! [`spec/06` §1]: https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/06-simd-predictor-rle-entropy-channel-dispatcher.md

#![forbid(unsafe_code)]

mod channel;
mod decoder;
mod encoder;
mod error;
mod fibonacci;
mod frame;
mod legacy_range_coder;
mod model;
mod predict;
mod range_coder;
#[cfg(feature = "registry")]
pub mod registry;
mod rle;
mod tables;

#[cfg(test)]
mod roundtrip_tests;

pub use crate::channel::{ChannelHeader, LegacyChannelHeader};
pub use crate::decoder::{decode_frame, decode_frame_with_prev, DecodedFrame, Decoder, PixelKind};
pub use crate::encoder::{encode_frame, encode_null};
pub use crate::error::{Error, Result};
pub use crate::frame::{FrameType, WirePlaneRole};

// Framework integration — only when the `registry` feature is on.
// `make_decoder` / `make_encoder` are the dual-API convention's direct
// factory endpoints, exposed alongside the `register!` registry path.
#[cfg(feature = "registry")]
pub use crate::registry::{make_decoder, make_encoder, register, register_codecs, CODEC_ID_STR};

#[cfg(feature = "registry")]
oxideav_core::register!("oxideav-lagarith", register);
