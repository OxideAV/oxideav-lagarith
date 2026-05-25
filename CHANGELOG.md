# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Round 135 (encoder round 13) — modern probability-model write
  path: transmitted-frequency rescale to the `q >= 1` operating
  range (`spec/02` §2 / §5, `spec/04` §5 / §6).** The per-channel
  modern-coder encode path (`encode_channel_simple` header-0x00 and
  `encode_channel_arith_rle` header-0x01..0x03) now rescales the
  raw byte-histogram through `rescale_to_max_total` before it builds
  the CDF and the Fibonacci prefix, so the transmitted total stays
  `<= MAX_MODERN_TOTAL = TOP (0x800000)`.

  Rationale: `spec/02` §5 starts every symbol with `q = range /
  total`; `spec/02` §2 floors the post-renormalisation `range` at
  `TOP + 1`, so a transmitted `total > TOP` drives `q` to zero and
  the coder produces `range = 0` (and a divide-by-zero in the
  decoder's Step-C `low / q`). `spec/04` §5 documents that the
  proprietary loader normalises the histogram for exactly this
  `q >= 1` guarantee; its validation correction establishes the
  *wire* still carries a raw histogram for the fixtures probed —
  every one well under `TOP`. The rescale therefore **no-ops for
  every sub-TOP plane** (the common case), keeping the wire
  byte-identical to the raw-histogram form, and engages only for
  planes whose symbol count exceeds `TOP` (> ~8.39M residuals — a
  single 4K+ plane), which the prior encoder could not encode at
  all. The rescale is `floor(freq[s] * cap / sum)` clamped up to
  `1` for every nonzero slot (so no used symbol drops out of the
  table), with any residual overshoot trimmed from the largest
  slots, never below `1`. Encoder and decoder build the identical
  CDF from the transmitted rescaled table (`spec/04` §6), so the
  arithmetic coder stays exact — only the probability model
  changes, never the symbol→byte mapping.

  Five new tests in `src/encoder.rs`: `rescale_noop_when_total_fits`
  (verbatim passthrough under the cap), `rescale_caps_total_and_
  preserves_nonzero` (total `<= cap` + nonzero-preservation on a
  dominant-plus-rare-tail histogram), `rescale_small_cap_overshoot_
  trim` (256 equal slots forced through the trim path),
  `rescale_capped_channel_roundtrip` (small-cap end-to-end modern-
  wire self-roundtrip across five caps), and
  `rescale_production_cap_large_plane_roundtrip` (a genuine
  `total > TOP` ~8.4M-residual plane that round-trips byte-exactly
  at the production cap — the prior encoder would have broken on
  it). Test count: 131 → 136 (library) + 7 (integration). Still
  self-roundtrip-only; byte-exact-vs-proprietary remains
  Auditor/Extractor-blocked on the un-disassembled probability
  loader at `0x180001050`.

- **Round 132 (encoder round 12) — `spec/02` §6.3 final-flush
  FF-chain bulk-fill.** `RangeEncoder::finish()`'s pre-tail
  pending-FF chain drain now uses `Vec::resize` (one bounds
  check + one memset) instead of `pending_ffs` individual
  `Vec::push` calls. This is the §6.3 final-flush analogue of
  the round-9 hot-path `Vec::resize` for the per-`shift_low`
  FF-chain commit, completing the structural pattern: no
  per-FF push loops anywhere in the encoder. The on-wire
  bytes are unchanged — the same `cache` (or `cache+1`) head
  + N×fill (`0x00` on a carry, `0xff` otherwise) + four-byte
  tail per `spec/02` §6.3 — so the flush is bit-identical to
  the proprietary's primitive.

  On a short-channel + Step-B-heavy fixture (512 channels ×
  128 symbols, 200 reps, release build, macOS aarch64) the
  round-12 encoder delivers ~305-311 MSym/s vs. ~298-310
  MSym/s for the per-FF push reference (timed side-by-side in
  the same bench) = ~1.00-1.03× speedup, default-on. The
  delta is modest because realistic `pending_ffs` chains are
  short (3-15 bytes); the value of the round is structural
  pattern completion + a byte-equivalence regression guard.

  Two new tests in `src/range_coder.rs`:
  `rangecoder_finish_resize_byte_equiv_to_push_loop` encodes
  a 0xff-dominant stream through both the production
  `Vec::resize` form AND a `finish_via_push_loop` helper
  modelling the pre-round-12 per-FF push form, asserting byte
  equality (covers the §6.3 flush bit-identity guard).
  `rangecoder_encode_throughput_finish_heavy` times both
  forms side-by-side on a 65,536-symbol short-channel
  workload (512 chans × 128 syms) under `LAGARITH_BENCH=1`,
  reporting the RESIZE/PUSH speedup ratio for direct
  comparability with the Step-A / Step-B / Step-C bench
  numbers. Test count: 129 → 131 (library) + 7 (integration).

### Added

- **Round 127 — extended ffmpeg cross-decoder pin set + pattern-
  sensitivity characterisation.** The pin file now carries four new
  random-seeded committed frames in addition to round 124's three:
  RGB24 4×4 + 8×16 and RGBA 4×4 + 8×8, all built from the same LCG
  pixel source (`state * 0x9e37_79b9 + 0x12345`, seed `0xdeadbeef`,
  high byte per pixel) and all verified to decode byte-for-byte
  through ffmpeg's `lagarith` decoder via a minimal `LAGS`-coded AVI
  wrapper. Seven total pins now run in CI without ffmpeg.

  Round 127 also empirically characterises the residual gap: the
  crate encoder's compatibility with ffmpeg is **pattern-sensitive
  even within the power-of-two-pixel-count regime**. The structured
  `i * 73 + 11 → bit-slice` test pattern (used by the existing
  self-roundtrip tests) at the same pow2 sizes that the random
  patterns sweep cleanly produces ffmpeg-divergent frames (e.g.
  ~40% byte match at 16×16, single-byte off-by-N residuals
  scattered through the planes). The crate's own decoder
  self-roundtrips both pattern classes byte-exactly, so the
  divergence sits on ffmpeg's side of the wire-format interpretation.
  The most likely root cause is the channel-prefix probability-loader
  at `lagarith.dll!0x180001050` (referenced from `spec/02` §5 and
  `spec/04` §5/§6 but **not disassembled into cleanroom spec**),
  which converts the wire's raw frequency histogram into the
  internal cumulative + shift-exponent struct the modern range
  coder consumes. ffmpeg's implementation almost certainly mirrors a
  normalisation step that the crate's encoder/decoder pair collapses
  to identity (`q = range / total` with raw `total`).

  A prototype encoder-side `rescale_to_pow2` fix was explored that
  converted non-pow2 channel totals to the smallest power of two
  before encoding (matching the legacy coder's `spec/07` §3.2
  approach) — this closed the non-pow2 5×5 / 3×3 frame gap exactly
  but did not address the pattern-sensitive pow2 cases, confirming
  the two issues share the same un-disassembled normalisation root.
  The prototype is not landed (no spec-derived rationale for changing
  the wire format from raw to rescaled freqs without the
  `0x180001050` reference); the gap remains documented for the
  Extractor round.

### Fixed

- **Round 124 — modern arithmetic RGB(A) first-column predictor
  corrected from Rule A to Rule B.** The modern RGB24 / RGB32
  (types 2 / 4) and RGBA (type 8) decode + encode paths now use
  the **Rule B** first-column-of-row predictor
  (`TL = plane[y-2][W-1]` for `y >= 2`, `spec/06` §3.2) instead of
  Rule A. The cleanroom's audit/01 §9.1 had left the Rule A vs
  Rule B dispatch open because a horizontal-ramp fixture makes the
  two rules degenerate (first column constant ⇒ `TL == T`). A
  black-box differential decode against the independent ffmpeg
  `lagarith` decoder — feeding it `LAGS`-wrapped frames built under
  each rule — resolves it: ffmpeg reproduces the original pixels
  byte-exactly only for Rule B encodes (every power-of-two
  pixel-count RGB24 / RGB32 / RGBA frame tested). The prior Rule A
  decode produced wrong pixels for real Lagarith RGB streams.

### Added

- **Round 124 — ffmpeg cross-decoder byte-exact pins.**
  `tests/ffmpeg_pins.rs` commits three RGB24 (8×8, 16×16) and RGBA
  (16×16) frames produced by the crate encoder and verified to
  decode to their original pixels through ffmpeg's `lagarith`
  decoder (used purely as a black-box oracle). The pins are
  committed so the regression runs in CI without ffmpeg; they
  guard against any reversion of the modern path back to Rule A.

- **Round 96 — pair-packed 513-entry CDF decode path (legacy
  type 7, `spec/07` §3.1 + §3.4 audit-corrected; audit/12 §7.1
  "Strategy F").**
  - `build_legacy_pair_packed_cdf` constructs the proprietary's
    pair-packed 513-entry CDF: the rescaled frequencies are
    interleaved with sentinel-`1` inter-symbol gaps as
    `(freq'[c], 1)` and prefix-summed, so symbol `c`'s bounds sit
    at `cdf[2c]` / `cdf[2c+1]` and the full span is `total + 256`.
    The shared `spec/07` §3.2..§3.3 rescale + zigzag-residue front
    end is factored into `rescale_freq`, used by both the flat and
    pair-packed builders.
  - `LegacyRangeDecoder::new_pair_packed` + a `CdfLayout` dispatch
    decode against the pair-packed CDF via the `spec/07` §5.2
    even-index binary descent, with the same
    `total = next_pow2(Σfreq)` divisor. Because the pair-packed
    lower bounds span `[0, total + 256)` while the §5.1
    `symbol_index` is capped at `total - 1`, high-index rare
    symbols are unreachable — reproducing the proprietary's
    documented rare-symbol mis-decode (audit/12 §3.6 — `0xc0`
    decodes as `0xff`).
  - `channel::decode_legacy_channel` now routes streams matching
    the rare-symbol-cluster signature
    (`is_rare_symbol_cluster`) through the pair-packed path
    instead of returning `Error::LegacyRareSymbolClusterUnsupported`.
    Our own encoder never produces such streams (Strategy E routes
    them to type 1), so this path serves foreign / proprietary-
    encoded type-7 streams. The error variant is retained for API
    stability and genuinely-undecodable edge cases.
  - +4 tests: the audit/12 §5 worked-example boundary shifts
    (`1081 / 1085 / 1215`), pair-packed CDF layout vs. the flat
    form, length-correct pair-packed decode that avoids the
    unreachable symbols, and a public-API `decode_frame` decode of
    a rare-cluster type-7 frame. The five round-7 "defensive
    harness" refusal tests are rewritten to assert the new decode
    behaviour. Full byte-exact proprietary parity still awaits a
    fixture oracle (`samples.oxideav.org/lagarith/`, 404 per
    audit/04 §5).

- **Round 11 — encoder-side range-coder Step-C `freqs[]` cache
  (`spec/02` §5).**
  - `Cdf` now caches `freqs: [u32; 256]` where `freqs[s] = cum[s+1]
    - cum[s]`. The encoder's `spec/02` §5 Step-C arm (fired
    whenever the symbol is neither `0` nor `255`) hoists its
    `cum[s+1] - cum[s]` two-read + subtract to `from_frequencies`
    time; the hot path then loads `lo = cdf.lo(s)` and `freq =
    cdf.freq(s)` in parallel and the `range = freq * q` multiply
    no longer waits on a subtract. Bit-identical to the round-10
    `cum[]`-array form (verified by
    `rangecoder_step_c_encode_bit_equiv_to_generic`, which
    encodes the same mid-band stream through both paths and
    asserts byte equality).
  - The Cdf struct's small scalar fields (`freq0`, `total`) are
    reordered to offset 0 so a Step-A-dominant workload keeps
    them on the first cache line; the `freqs[]` array lands
    after `cum[]` so it does not contend with `freq0` for cache
    set on the dominant path.
  - **Throughput delta**: 65,536-symbol Step-C-heavy encode
    fixture (99% mid-band symbols `1..=254`, 200 reps, release
    build, macOS aarch64) — round-10 baseline ~225 MSym/s →
    round-11 ~244 MSym/s = **1.08×** on Step-C-dominant
    workloads. Step-A and Step-B benches stay within run-to-run
    noise of round 10 (~334 vs. ~333 MSym/s on Step-A; ~333 vs.
    ~327 MSym/s on Step-B) — the new cache field does not
    regress the dominant paths. Default-on, no feature gate.
  - +5 tests: Step-C-dominant encode self-roundtrip
    (`rangecoder_encode_step_c_dominant_roundtrip`); Step-C
    bit-equivalence guard
    (`rangecoder_step_c_encode_bit_equiv_to_generic`); Step-C
    encode throughput bench
    (`rangecoder_encode_throughput_step_c_heavy`; functional
    check, timing only printed under `LAGARITH_BENCH=1`);
    Laplacian-residual roundtrip on a `{0, 1, 254, 255}`-heavy
    distribution (`rangecoder_encode_laplacian_residual_roundtrip`);
    `freqs[]` cache layout regression guard
    (`cdf_freq_matches_array_form`). 126 tests total (was 121).
  - **Step-A1 / Step-B1 prototype reverted**: dedicated `s == 1`
    (small-positive `+1` residual) and `s == 254` (small-negative
    `-1` residual on the unsigned-wrap side) fast paths were
    tried and benched against mixed-distribution streams — they
    hurt the dominant Step-A path more than they helped the
    secondary symbols (extra branches in the hot loop dropped
    Step-A heavy from ~340 MSym/s to ~299 MSym/s, -12%). The
    `encode_symbol` dispatcher deliberately falls through to
    Step-C for s ∈ 1..=254 instead of growing the if-chain.
    Documented as a round-11 NOTE inside `encode_symbol`.

- **Round 10 — encoder-side range-coder Step-B fast path
  + cache-slot Option→bool refactor (`spec/02` §5/§6.2).**
  - `RangeEncoder::encode_symbol` now implements the symmetric
    **Step-B** fast path for `s == 255` (the high-sentinel symbol
    the decoder already short-circuits per `spec/02` §5 Step B).
    For `s == 255` the generic update reads `lo = cum[255]` and
    `hi = cum[256] = total`, so the cached `Cdf::cum_top`
    (= `cum[255]`) and `Cdf::total` directly drive the update
    without indexing the 257-entry cumulative array:
    `low += cum_top * q; range = (total - cum_top) * q`.
    Bit-identical to the round-9 Step-C path on 0xff (verified by
    `rangecoder_step_b_encode_bit_equiv_to_generic`, which encodes
    the same 0xff-dominant stream through Step-B and through an
    inline Step-C-only reference and asserts byte equality).
  - `RangeEncoder::shift_low` swaps `cache: Option<u8>` for
    `cache: u8 + started: bool`. The hot inner body now issues one
    `bool` check instead of the `Option::take()` discharge the
    optimiser couldn't elide across the carry / defer /
    steady-state branch arms. The arithmetic is unchanged (same
    `c` / `c+1` cache byte, same `0x00` / `0xff` fill, same
    low-mask) so the wire stays bit-identical to the proprietary's
    cache-then-FF-chain emission per `spec/02` §6.2 — verified by
    the new `rangecoder_shift_low_started_byte_equiv_to_option`
    self-roundtrip and the long-standing `rangecoder_roundtrip_wide`
    decoder test.
  - **Throughput delta**: 65,536-symbol Step-B-heavy encode
    fixture (94% 0xff symbols, same total mass + same residual
    shape as the Step-A bench), 200 reps, release build,
    macOS aarch64 — round-9 baseline ~305 MSym/s → round-10
    ~327 MSym/s = **1.07×** on Step-B-dominant workloads. The
    Step-A-dominant bench is within run-to-run noise of the
    round-9 number (~333 vs. ~336 MSym/s) — Step-B does not
    regress Step-A. Default-on, no feature gate.
  - +4 tests: Step-B-dominant encode self-roundtrip
    (`rangecoder_encode_step_b_dominant_roundtrip`); Step-B
    bit-equivalence guard
    (`rangecoder_step_b_encode_bit_equiv_to_generic`); Step-B
    encode throughput bench
    (`rangecoder_encode_throughput_step_b_heavy`; functional
    check, timing only printed under `LAGARITH_BENCH=1`); cache
    Option→bool roundtrip regression
    (`rangecoder_shift_low_started_byte_equiv_to_option`).
    121 tests total (was 117).

- **Round 9 — encoder-side range-coder hot-path optimisation
  (`spec/02` §5, symmetric to round 8's decoder).**
  - `RangeEncoder::encode_symbol` now implements the Step-A
    symbol-0 fast path: for `s == 0` the generic Step-C
    arithmetic collapses to a no-op `low += cum[0]*q = 0` plus
    `range = freq0 * q`, so the optimised path skips the two
    `Cdf::lo()` reads and the `wrapping_add` of zero. Lagarith
    residuals after gradient prediction are dominated by symbol 0
    (`spec/06` §6.4: `freq[0] >= 0.95 * pixel_count`), so the
    Step-A check is the dominant case and short-circuits the
    generic indirection. The wire bytes are bit-identical to the
    generic path (verified by `rangecoder_step_a_encode_bit_equiv_to_generic`,
    which encodes the same stream through Step-A and through an
    inline Step-C-only reference and asserts byte equality).
  - `RangeEncoder::shift_low` now commits the `pending_ffs` chain
    with a single `Vec::resize` (one bounds check + one bulk
    memset) instead of `pending_ffs` individual `Vec::push` calls.
    Same `c` / `c+1` cache byte, same `0x00` / `0xff` fill, same
    low-mask, so the wire stays bit-identical to the proprietary's
    cache-then-FF-chain emission per `spec/02` §6.2.
  - **Throughput delta**: 65,536-symbol signal-heavy encode
    fixture (94% zeros — same shape as the round-8 decoder
    bench), 200 reps, release build, macOS aarch64 — baseline
    179 MSym/s → optimised 330 MSym/s = **1.84×**. Default-on,
    no feature gate (over the 1.3× threshold for unconditional
    landing).
  - +3 tests: Step-A-dominant encode self-roundtrip
    (`rangecoder_encode_step_a_dominant_roundtrip`); Step-A
    bit-equivalence guard
    (`rangecoder_step_a_encode_bit_equiv_to_generic`) that re-runs
    the same input through an inline generic-only path and asserts
    byte equality; signal-heavy encode throughput
    (`rangecoder_encode_throughput_signal_heavy`; functional
    check, timing only printed under `LAGARITH_BENCH=1`). 117
    tests total (was 114).

- **Round 8 — modern range-coder hot-path optimisation (`spec/02` §5).**
  - `RangeDecoder::decode_symbol` now implements the three-way fast
    path of `spec/02` §5: Step A (symbol 0, `low < cum[1] * q`),
    Step B (symbol 0xff slack-band sentinel, `low >= total * q`,
    update `low -= total*q; range -= total*q`), Step C (generic
    cumulative search). Step A short-circuits the 9-iteration
    binary search on the dominant case — Lagarith residuals after
    gradient prediction land in symbol 0 ~94-96% of the time
    (`spec/06` §6.4: `freq[0] >= 0.95 * pixel_count`), so this
    branch fires almost every iteration of the hot loop.
  - `Cdf` now caches `freq0 = cum[1]` and `total = cum[256]`
    inline on the struct so the fast paths read them with no array
    indexing per symbol.
  - `RangeDecoder::renormalise` reshapes the per-iteration bytewise
    refill into a 2-byte slice window (`src.get(c..c+2)`) so the
    optimiser hoists a single bounds compare per loop iteration
    instead of two `Option`-unwraps. The arithmetic is unchanged
    (still byte-at-a-time per `spec/02` §4) so output is
    bit-identical to the proprietary.
  - **Throughput delta**: 65,536-symbol signal-heavy fixture
    (94% zeros), 200 reps, release build, macOS aarch64 —
    baseline 37.4 MSym/s → optimised 161.3 MSym/s = **4.31×**.
    Far above the 1.3× threshold for default-on; no feature
    gate needed.
  - +4 tests: step-A-dominant histogram, step-B-hits histogram,
    renormalise tail-saturation, signal-heavy throughput
    (functional check; timing only printed under
    `LAGARITH_BENCH=1`). 114 tests total (was 110).

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
