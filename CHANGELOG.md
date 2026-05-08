# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Round 7 — type-7 decoder defensive harness (`audit/12 §7.1`).**
  - `decode_legacy_channel` now runs the rare-symbol-cluster
    predicate (`is_rare_symbol_cluster`) over the transmitted 256-
    entry frequency table before building the CDF. When the
    signature matches (`freq[0] >= 0.95 * Σfreq` *and* ≥ 3 distinct
    nonzero bins each with `freq ∈ {1, 2}`), the decoder returns
    the new `Error::LegacyRareSymbolClusterUnsupported` variant
    rather than silently miscoding the body. Audit/12 §5..§6
    retracts spec/07 §3.4's flat-CDF allowance for this fixture
    class — the cleanroom's flat 257-entry CDF and the
    proprietary's pair-packed 513-entry CDF are *not*
    bit-equivalent here, so a *foreign* encoder's stream with this
    freq table would silently decode to the wrong residual sequence
    under our flat CDF.
  - The cleanroom's own encoder still applies *Strategy E* (round
    6) and re-routes such fixtures to type 1 *before* reaching the
    legacy range coder, so the guard is never invoked on
    self-roundtrip and the existing 104-test suite is unchanged.
    The guard exists for the case where downstream callers feed
    *foreign* type-7 streams into `decode_frame` — a hypothetical
    proprietary type-7 writer (the shipped proprietary build is
    decode-only per `spec/07 §6` / §9.2 item 8) or any third-party
    encoder.
  - Strategy F (full pair-packed 513-entry CDF refactor of
    `audit/12 §7.1`) remains parked: blocked on a proprietary-
    encoded type-7 fixture appearing at
    `samples.oxideav.org/lagarith/` (`audit/04 §5`; re-checked
    round 7 — still 404). Without an oracle a 150-200-LOC refactor
    risks regressing the 95/96 currently-passing type-7 cells.
  - +6 tests: rare-cluster freq table at `decode_legacy_channel`;
    3-vs-2 rare-bin boundary (must trigger / must not); freq[0]
    dominance below 95% (must not trigger); rare-cluster type-7
    frame at the public `decode_frame` surface; error-display
    mentions audit/12 + Strategy F; self-roundtrip smoke check.
    110 tests total (was 104).

- **Round 6 — Strategy E encoder integration (audit/12 §7.1).**
  - `encode_legacy_rgb` and `encode_legacy_rgb_rle` now run the
    `is_rare_symbol_cluster` predicate over the three residual
    planes (B', G, R') after predict + decorrelate. When any plane
    matches the rare-symbol-cluster signature (≥ 3 distinct rare
    bins `freq ∈ {1, 2}` within a histogram dominated by
    `freq[0] >= 0.95 * pixel_count`), the encoder skips type-7
    emission and falls through to a type-1 (uncompressed) frame.
    Type-1's roundtrip is byte-exact on every fixture per
    `spec/01 §2.1` / `audit/11 §4.5`, sidestepping the
    flat-CDF / pair-packed-CDF wire-format divergence that
    `audit/12 §5..§6` localised to this fixture class.
  - Strategy F (full pair-packed 513-entry CDF refactor) deferred
    — `audit/12 §7.1` recommends Strategy E because Strategy F's
    regression risk on the 95/96 currently-passing type-7 cells
    outweighs its benefit (type 7 is decode-only in the
    proprietary build per `spec/07 §6` / §9.2 item 8; no archival
    type-7 fixture exists per `audit/04 §5`).
  - +4 tests: rare-symbol-cluster routes near_flat 33×27 to
    type 1; same propagates through the RLE-then-Fibonacci
    sub-path for escape_len ∈ {1, 2, 3}; pattern-pixels and
    pure-solid plane fixtures stay on type 7 (negative cases).
  - 104 tests total (was 100).

- **Round 5 — type-7 spec-coverage extensions: Rule B + RLE-then-
  Fibonacci channel sub-path.**
  - **First-column predictor Rule B** for type-7 frames per
    `spec/07` §9.1 item 7b: row `y ≥ 2` first-column TL =
    `plane[y-2][W-1]` (linear-memory rule), falling back to Rule A
    for `y = 1` (no `y-2` row exists). The audit-resolved binary
    walk at `lagarith.dll!0x180001b00..0x180001c5d` shows no
    per-row state machine; Rule B matches the proprietary's SIMD
    predictor's reverse-engineered residuals bit-for-bit. Modern
    types 2 / 4 / 8 / 10 / 11 retain Rule A.
  - **Channel header `0x01..=0x03` RLE-then-Fibonacci sub-path**
    per `spec/07` §2.3 / §2.4. Wire layout: outer header byte
    (= escape_len ∈ {1, 2, 3}); u32 LE post-RLE byte count L
    (≤ 256); RLE-compressed input expanding to the L-byte
    Fibonacci-coded freq-table buffer; post-Fibonacci 1-byte
    reservation; legacy range-coder body. Fixes the prior
    `BadChannelHeader` rejection for outer headers in {0x01..0x03}.
  - **Audit/12 rare-symbol-cluster signature detector**
    (`is_rare_symbol_cluster`) — predicate hook for a future
    Strategy E encoder route-around (audit/12 §7.1: ≥ 3 distinct
    nonzero bins each with `freq ∈ {1, 2}` within a histogram
    dominated by `freq[0] >= 0.95 * pixel_count`). The
    pair-packed 513-entry CDF (Strategy F) is deferred per
    audit/12 §7.1's regression-risk recommendation; no archival
    type-7 fixture exists per audit/04 §5 to validate the full
    refactor.
  - Test-only `encode_legacy_channel_rle` / `encode_legacy_rgb_rle`
    helpers driving end-to-end self-roundtrip on the
    RLE-then-Fibonacci wire path for escape lengths 1, 2, and 3.
  - +17 tests: 4 Rule-B predictor unit tests (4×4, 11×7, y=1
    fallback to Rule A, divergence vs Rule A on row ≥ 2); 6
    type-7 RLE-then-Fib roundtrip tests (escape 1/2/3 at 4×4,
    8×8 sweep, solid plane); 2 type-7 Rule B frame-level
    roundtrips (5×4, 4×8); 5 rare-symbol-cluster detector unit
    tests covering the audit/12 canonical fixture, solid plane,
    too-few-rare-bins, no-dominant-zero, and high-freq neighbours.
  - 100 tests total (was 83).

- **Round 4 — legacy RGB (frame type 7, `spec/07` adaptive-CDF range
  coder).**
  - `FrameType::LegacyRgb` (frame-type byte `0x07`) now decodes
    pre-1.1.0 "Obsolete arithmetic coded RGB keyframe" frames per
    `spec/07`. The wire pipeline differs from the modern range
    coder of `spec/02` in two load-bearing ways: the probability
    distribution is **transmitted per-channel** as a 256-entry
    Fibonacci-coded frequency table (no zero-run sub-prefix —
    audit/03 §3.1 confirms the legacy decoder body has a clean
    one-int-per-symbol shape, distinct from the modern coder's
    spec/04 §3.3 path), and the decoder builds a **257-entry CDF
    on the fly** from the transmitted frequencies via the §3
    "pair-pack + rescale-to-pow2 + zigzag-residue + prefix-sum"
    pipeline.
  - New `legacy_range_coder` module: `decode_legacy_freq_table`,
    `build_legacy_cdf` (with zigzag residue distribution per
    `spec/07` §3.3), and `LegacyRangeDecoder` (binary-search
    descent over the 257-entry CDF — algebraically equivalent to
    the proprietary's eight-level binary-tree descent per
    `spec/07` §5.2 final paragraph). The init seed uses the
    audit-corrected 4-byte priming form per `spec/07` §9.1
    item 3.
  - 2-byte channel prefix (outer header `0x00` + inner codec-mode
    flag `0x00`) per `spec/07` §2.5; post-Fibonacci 1-byte
    reservation per audit/08 §3.2 / `spec/07` §9.1 item 7c —
    emitted by the encoder + skipped by the decoder **only when**
    the encoded bit stream length is a multiple of 8.
  - Test-only `LegacyRangeEncoder` + `encode_legacy_freq_table`
    for the round-4 self-roundtrip suite (the proprietary build
    is decode-only for type 7; the cleanroom honours
    `CLEANROOM-MANUAL §8` "Both directions, always" by shipping
    both halves). Round-4 encoder ships only the bare-Fibonacci
    `header == 0` path — `spec/07` §6.3 / §9.2 item 9 confirm
    header-0 is sufficient for round-trip correctness; the
    `header ∈ {0x01, 0x02, 0x03}` RLE-then-Fibonacci sub-path is
    decoder-undefined per §9.1 item 2.
  - +17 tests covering type-7 (4×4, 8×8, 16×12, unaligned width
    7×5, BGRA32 widening, solid-plane edge case, NULL replay,
    inner-codec-mode-flag rejection), the YUY2 odd-width
    floor-chroma layout (audit/00 §9.4 partial resolution + a
    raw-channel roundtrip exercising the 5×4 odd-width tail), and
    the legacy range-coder + Fibonacci freq-table self-roundtrip
    primitives. 83 tests total.
  - Spec gap noted: `samples.oxideav.org/lagarith/` returns HTTP
    404 at the time of round 4 — byte-exact validation against a
    proprietary-encoded AVI fixture remains an Auditor concern
    for a future round.

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

- Type-7 RLE-then-Fibonacci channel sub-path (`header ∈ {0x01,
  0x02, 0x03}` — surfaces `BadChannelHeader` in the round-4
  decoder). `spec/07` §9.2 item 9 explicitly notes header-0 is
  sufficient for round-trip correctness; the RLE-pre-decompressed
  freq-table path is decoder-undefined per §9.1 item 2.
- Byte-exact encoder validation against a proprietary-encoded AVI
  fixture — Auditor concern; `samples.oxideav.org/lagarith/`
  returned HTTP 404 at the time of round 4.
- The reciprocal-multiply LUT at RVA `0x1b9a0` is not used by the
  decoder (the cumulative-frequency search loop `spec/02` §5 invites
  is bit-equivalent and simpler).
- Type-7 byte-exact match against the proprietary's RuleB
  first-column predictor (`TL = plane[y-2][W-1]` for `y >= 2` per
  `spec/07` §9.1 item 7b). The round-4 decoder uses Rule A
  (`TL = L = plane[y-1][W-1]`) — the same rule types 2 / 4 use.
  Self-roundtrip is bit-perfect; the discrepancy only matters for
  byte-exact match against a proprietary-encoded type-7 fixture
  (none in tree).
