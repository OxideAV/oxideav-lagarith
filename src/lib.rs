//! Pure-Rust Lagarith lossless video decoder.
//!
//! **Rounds 1+2+3 of the clean-room rebuild.** Implements the modern
//! arithmetic-coded RGB24 / RGB32 / RGBA decode pipeline plus the
//! Uncompressed (type 1) and Solid (types 5 / 6 / 9) literal frames
//! (round 1); the YV12 (type 10) multi-plane arithmetic decode path
//! and a stateful [`Decoder`] wrapper that replays NULL ("JUMP")
//! frames per `spec/01` §1.1 (round 2); and the YUY2 (type 3)
//! packed→planar pipeline + reduced-resolution (type 11) — a
//! half-W/half-H YV12 body with a 2× nearest-neighbour upscale on
//! decode (round 3). All against the strict-isolation cleanroom
//! workspace at `docs/video/lagarith/`. Frame type 7 (legacy RGB,
//! pre-1.1.0 adaptive-CDF range coder per `spec/07`) remains
//! deferred to a future round.
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
//!    JPEG-LS clamped median for rows ≥ 1, with the
//!    `TL = L = plane[y-1][W-1]` first-column rule.
//! 7. **Cross-plane decorrelation** ([`spec/03` §4]): RGB families
//!    only — `R += G; B += G` after spatial prediction; alpha
//!    plane (RGBA) is unchanged.
//!
//! ## Public API
//!
//! - [`decode_frame`] — decode one Lagarith-encoded frame's bytes.
//! - [`PixelKind`] — host-side pixel format selector
//!   (`Bgr24` / `Bgra32`).
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
#[cfg(test)]
mod encoder;
mod error;
mod fibonacci;
mod frame;
mod predict;
mod range_coder;
#[cfg(feature = "registry")]
pub mod registry;
mod rle;
mod tables;

#[cfg(test)]
mod roundtrip_tests;

pub use crate::decoder::{decode_frame, decode_frame_with_prev, DecodedFrame, Decoder, PixelKind};
pub use crate::error::{Error, Result};
pub use crate::frame::FrameType;

// Framework integration — only when the `registry` feature is on.
#[cfg(feature = "registry")]
pub use crate::registry::{register, register_codecs, CODEC_ID_STR};

#[cfg(feature = "registry")]
oxideav_core::register!("oxideav-lagarith", register);
