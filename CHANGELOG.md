# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Round 3 — YUY2 (frame type 3) and reduced-resolution (frame
  type 11).**
  - `FrameType::ArithmeticYuy2` (frame-type byte `0x03`) now decodes
    via the same channel-header dispatcher as the YV12 path, with
    three planes — Y at `W * H`, U and V at `(W / 2) * H` each —
    per-plane left + median predictor, no cross-plane
    decorrelation. The wire is **planar** Y/U/V (note: U before V,
    unlike YV12); the output ([`PixelKind::Yuy2`]) is **packed**
    `Y0 U Y1 V` per pair of pixels at columns `2k, 2k+1`
    (`spec/03` §6.2).
  - `FrameType::ReducedResYv12` (frame-type byte `0x0b`) decodes
    the body as a half-W/half-H YV12 frame and 2× nearest-neighbour
    upscales each plane (luma + V + U) onto the host's full-
    resolution `PixelKind::Yv12` buffer (`spec/01` §2.4 +
    audit/00-report.md §9.1).
  - New `PixelKind::Yuy2` variant (16-bpp packed, 2 bytes per
    pixel).
  - Encoder helpers `encode_arith_yuy2` and `encode_arith_reduced_res`
    for the self-roundtrip suite (the round-3 encoder remains
    self-roundtrip-only — byte-exact validation against the
    proprietary encoder is an Auditor concern; see the
    `SPECGAP-encoder-byte-exact` test marker).
  - **SIMD-vs-scalar predictor parity** documented per `spec/06`
    §3.5 / §3.6: the crate's scalar predictor implements
    Strategy A (`TL = L = plane[y-1][W-1]`), which is
    *carry-equivalent* to the proprietary's SIMD inner-loop AND
    matches the proprietary's scalar predictor for every
    `(width, frame-type)` pair. No separate SIMD path is needed
    for byte-equivalent residual streams.
  - +11 tests covering the YUY2 path (4×4, 8×6, 16×16, pixel-
    format mismatch, buffer-length parity), the reduced-resolution
    path (8×8, 16×16, pixel-format mismatch, hand-rolled 2×
    upscale parity), the SIMD-vs-scalar predictor parity, and the
    byte-exact-encoder SPECGAP marker. 66 tests total.

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

- Frame type 7 (legacy RGB, pre-1.1.0 adaptive-CDF range coder per
  `spec/07`) remains surfaced as `Error::UnsupportedFrameType`.
  Round 4 candidate.
- Byte-exact encoder validation against a proprietary-encoded AVI
  fixture — Auditor concern; no fixture currently in tree
  (`samples.oxideav.org` not provisioned).
- The reciprocal-multiply LUT at RVA `0x1b9a0` is not used by the
  decoder (the cumulative-frequency search loop `spec/02` §5 invites
  is bit-equivalent and simpler).
