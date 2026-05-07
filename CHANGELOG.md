# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Round 2 — YV12 (frame type 10) and stateful NULL-frame replay.**
  - `FrameType::ArithmeticYv12` (frame-type byte `0x0a`) now
    decodes through the same channel-header dispatcher as the RGB
    family, with three independent planes (Y at `W * H`, V and U
    at `floor((W * H) / 4)` each), per-plane left + median
    predictor, and **no** cross-plane decorrelation per `spec/03`
    §6.1 + §4.4.
  - New `PixelKind::Yv12` variant — output buffer is the
    concatenated Y / V / U planes (the standard YV12 raw layout
    the proprietary decoder writes per `spec/03` §6.1). The
    `oxideav-core` framework `Decoder` impl splits this back into
    three `VideoPlane`s with their respective strides.
  - Stateful `Decoder` wrapper: `Decoder::decode(payload, ..)`
    accepts a zero-byte payload as a NULL frame ("JUMP") per
    `spec/01` §1.1 and replays the predecessor frame; the
    stateless `decode_frame` continues to surface NULL frames as
    `Error::NullFrame`. A standalone `decode_frame_with_prev`
    helper centralises the same replay rule for callers that
    don't want to carry state.
  - Two new error variants: `Error::NullFrameWithoutPredecessor`
    (NULL frame with no prior to replay) and
    `Error::PixelFormatMismatch` (host-requested pixel format
    doesn't match the wire frame type, e.g. asking for `Bgr24` on
    a YV12 frame).
  - Encoder helpers `encode_arith_yv12` and `encode_null` for the
    self-roundtrip suite.
  - +13 tests covering the YV12 path (4×4, 8×6, 16×16, all-solid
    planes, pixel-format mismatch, buffer-length parity) and the
    NULL-frame replay (helper-function, stateful-decoder,
    predecessor update, dimension mismatch, YV12 replay). 55
    tests total.

- **Round 1 — modern arithmetic-coded RGB family decoder.** Decodes
  Lagarith frame types 1 (Uncompressed), 2 (Unaligned-RGB24), 4
  (Arithmetic-RGB24), 5/6/9 (Solid Grey/RGB/RGBA), and 8
  (Arithmetic-RGBA) into BGR24 / BGRA32 host buffers. Pipeline
  ports:
  - Frame layout + per-pixel-format channel-offset table (`spec/01`).
  - Modern range coder with TOP=2^23 / init range=2^31 / four-byte
    init / cross-byte-LSB-rotated refill / four-byte flush tail
    (`spec/02`).
  - JPEG-LS clamped median + first-row left predictor + cross-plane
    G-pivot decorrelation reverse (`spec/03`).
  - Fibonacci-prefix probability table with Zeckendorf encoding +
    binary suffix (`spec/04`).
  - Residual zero-run RLE escape with the 256-entry permutation
    LUT loaded from `tables/01-residual-rle-decoder-lut.csv`
    (`spec/05`).
  - Channel-header dispatcher with the
    `0x00 / 0x01..0x03 / 0x04 / 0x05..0x07 / 0xff` sub-paths plus
    the u32-length-overflow fall-back to header-`0x00` style
    (`spec/06` §1).
- `oxideav-core` framework registration claiming the `LAGS` FOURCC
  via `CodecInfo::tags([CodecTag::fourcc(b"LAGS")])`.
- `cfg(test)` self-roundtrip encoder + 42-test suite covering each
  dispatcher path.

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch.

### Deferred

- Frame types 3 (YUY2), 7 (legacy RGB), and 11 (reduced-resolution)
  — surfaced as `Error::UnsupportedFrameType` for now. (Type 10
  YV12 landed in round 2; type 11 wire-format mirrors type 10 at
  half-resolution + 2× upscale per `spec/01` §2.4 — straightforward
  follow-on once an Auditor fixture exists.)
- The reciprocal-multiply LUT at RVA `0x1b9a0` is not used by the
  decoder (the cumulative-frequency search loop `spec/02` §5 invites
  is bit-equivalent and simpler).
