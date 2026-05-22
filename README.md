# oxideav-lagarith

Pure-Rust Lagarith lossless video codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Rounds 1..5 — arithmetic-coded RGB / YV12 / YUY2 + NULL-frame
replay + reduced-resolution + legacy RGB (type 7) with Rule B
first-column predictor + RLE-then-Fibonacci channel sub-path.**
This `master` branch is the clean-room rebuild against the
strict-isolation cleanroom workspace at
[`docs/video/lagarith/`](https://github.com/OxideAV/docs/tree/master/video/lagarith).
The previous implementation was retired by the OxideAV docs audit
dated 2026-05-06 (see
[`AUDIT-2026-05-06.md`](https://github.com/OxideAV/docs/blob/master/AUDIT-2026-05-06.md));
the prior history is preserved on the `old` branch for archival
but is forbidden input.

## Coverage

| Frame type | Wire form | Round |
| ---------- | --------- | ----- |
| 1 — Uncompressed | raw pixel data | 1 |
| 2 — Unaligned-RGB24 | arithmetic, `width % 4 != 0` | 1 |
| 3 — Arithmetic-YUY2 | packed→planar (Y/U/V planes) | **3** |
| 4 — Arithmetic-RGB24 / RGB32 | arithmetic, `width % 4 == 0` | 1 |
| 5 — Solid Grey | byte fill | 1 |
| 6 — Solid RGB | three-byte fill | 1 |
| 7 — Legacy RGB | adaptive-CDF + Rule B + RLE-then-Fib (`spec/07`) | **4–5** |
| 8 — Arithmetic-RGBA | four planes incl. alpha | 1 |
| 9 — Solid RGBA | four-byte fill | 1 |
| 10 — Arithmetic-YV12 | three-plane Y/V/U | **2** |
| 11 — Reduced-resolution | type 10 at half-W/H + 2× upscale | **3** |
| NULL ("JUMP") | zero-byte payload, replay previous | **2** |

## Pipeline

1. **Frame layout** ([`spec/01`](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/01-frame-data-layout.md)):
   byte 0 is the frame-type selector; non-NULL frames carry an
   `(n_channels - 1) * 4` byte channel-offset table next.
2. **Per-channel header dispatcher** ([`spec/03` §2.1](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/03-channel-decorrelation-and-predictors.md)
   + [`spec/06` §1](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/06-simd-predictor-rle-entropy-channel-dispatcher.md)).
3. **Fibonacci probability prefix** ([`spec/04`](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/04-fibonacci-probability-prefix.md)):
   MSB-first bit-stream Zeckendorf decode of the 256-entry frequency
   table, with the second-Fibonacci zero-run subcode.
4. **Modern range coder** ([`spec/02`](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/02-range-coder-framing.md)):
   TOP = 2^23, init range = 2^31, four-byte priming with the 31-bit
   init state, byte refill with cross-byte LSB rotation, four-byte
   flush tail.
5. **Residual zero-run RLE escape** ([`spec/05`](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/05-rle-escape-bit-format.md)):
   `escape_len + LUT[supplement_byte]`-zero runs, with the LUT
   loaded from `tables/01-residual-rle-decoder-lut.csv`.
6. **Spatial predictor** ([`spec/03` §3](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/03-channel-decorrelation-and-predictors.md)):
   left predictor on row 0; JPEG-LS clamped median on rows ≥ 1.
   The first-column-of-row rule depends on frame type — modern
   types (2/4/8/10/11) use **Rule A** (`TL = L = plane[y-1][W-1]`);
   the legacy type-7 path uses **Rule B**
   (`TL = plane[y-2][W-1]` for `y ≥ 2`) per `spec/07` §9.1 item 7b.
7. **Cross-plane decorrelation** ([`spec/03` §4](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/03-channel-decorrelation-and-predictors.md)):
   RGB families only — `R += G; B += G` post-prediction; alpha is
   stored raw.

## API

Stateless decode of a single frame:

```rust
use oxideav_lagarith::{decode_frame, PixelKind};

let decoded = decode_frame(&payload, width, height, PixelKind::Bgra32)?;
assert_eq!(decoded.pixels.len(), (width as usize) * (height as usize) * 4);
```

YV12 produces concatenated Y / V / U planes:

```rust
use oxideav_lagarith::{decode_frame, PixelKind};

let yv12 = decode_frame(&payload, width, height, PixelKind::Yv12)?;
assert_eq!(yv12.pixels.len(), PixelKind::Yv12.buffer_len(width, height));
```

Stateful decode that handles NULL ("JUMP") frames by replaying the
predecessor (`spec/01` §1.1):

```rust
use oxideav_lagarith::{Decoder, PixelKind};

let mut dec = Decoder::new();
let frame_a = dec.decode(&payload_a, width, height, PixelKind::Bgra32)?;
// Empty payload -> a clone of frame_a.
let frame_b = dec.decode(&[], width, height, PixelKind::Bgra32)?;
```

`oxideav-core` framework registration is gated on the default-on
`registry` Cargo feature and claims the `LAGS` FOURCC.

## Performance

The modern range-coder hot path implements the three-way fast
path of `spec/02` §5: Step A (symbol 0, fires when `low < cum[1]
* q`), Step B (symbol 0xff slack-band sentinel, fires when
`low >= total * q`), and Step C (generic cumulative search).
Lagarith residuals after gradient prediction are dominated by
symbol 0 (`spec/06` §6.4: `freq[0] >= 0.95 * pixel_count`), so
Step A short-circuits the 9-iteration binary search on the
overwhelming majority of symbols. The `renormalise` refill loop
uses a 2-byte-window slice access so the optimiser hoists a
single bounds compare per loop iteration. On a 65,536-symbol
signal-heavy fixture (94% zeros, 200 reps, release build, macOS
aarch64) the round-8 hot path delivers **161 MSym/s vs. 37
MSym/s** for the round-7 baseline = **4.31× speedup**, default-on
(no feature flag).

Round 9 mirrors the same Step-A fast path on the **encode**
side: for `s == 0` the generic `low += cum[0]*q + range =
(cum[1] - cum[0]) * q` update collapses to `range = freq0 * q`
(the `low += 0` is elided), skipping two `cum[]` reads + the
wrapping_add per dominant symbol. The `shift_low` pending-FF
chain is also flushed via a single `Vec::resize` (one bulk
memset) instead of `pending_ffs` individual pushes. On the same
64k signal-heavy fixture (200 reps, release build, macOS
aarch64) the round-9 encode hot path delivers **330 MSym/s vs.
179 MSym/s** for the round-8 baseline = **1.84× speedup**,
default-on. The Step-A path is algebraically a no-op vs.
generic Step-C; the `rangecoder_step_a_encode_bit_equiv_to_generic`
test re-encodes the same input through both paths and asserts
byte equality.

Round 10 adds the symmetric encoder **Step-B** fast path for
`s == 255` (the high-sentinel that the decoder already
short-circuits per `spec/02` §5 Step B). The 257-entry `cum[]`
array reads collapse to a `cum_top` cached field
(`Cdf::cum_top = cum[255]`) + the existing `total`; the update
becomes `low += cum_top * q; range = (total - cum_top) * q`.
The `shift_low` cache slot is also refactored from
`Option<u8>` to `u8 + started: bool` so the hot inner branch is
a single bool check instead of `Option::take()`. On a 65,536-
symbol Step-B-heavy fixture (94% 0xff, same total mass as
round 9, 200 reps, release build, macOS aarch64) the round-10
encoder delivers **~327 MSym/s vs. ~305 MSym/s** for the
round-9 baseline = **1.07× speedup** on Step-B-dominant
workloads. The Step-A bench stays at ~333 MSym/s (within noise
of round 9's ~336 MSym/s) — Step-B does not regress Step-A.
Bit-identity vs. the generic Step-C path is verified by
`rangecoder_step_b_encode_bit_equiv_to_generic`; the Option→
bool cache refactor is regression-guarded by
`rangecoder_shift_low_started_byte_equiv_to_option`.

Round 11 lands the **Step-C `freqs[]` cache** on the encoder
side. The §5 Step-C arm dominates whenever the symbol is
neither `0` nor `255` (the surviving ~5% of post-gradient
residuals spreads across symbols `1..=254`); for those cases
the generic update reads `lo = cum[s]` *and* `hi = cum[s+1]`
*and* computes `hi - lo` before the `range = (hi - lo) * q`
multiply. Round 11 hoists the subtraction to `from_frequencies`
time by caching `freqs[s] = cum[s+1] - cum[s]` on the `Cdf`
struct; the Step-C hot path then loads `lo = cdf.lo(s)` and
`freq = cdf.freq(s)` in parallel and the `range = freq * q`
multiply no longer waits on a subtract. On a 65,536-symbol
Step-C-heavy fixture (99% mid-band symbols `1..=254`, 200 reps,
release build, macOS aarch64) the round-11 encoder delivers
**~244 MSym/s vs. ~225 MSym/s** for the round-10 baseline =
**1.08× speedup** on Step-C-dominant workloads. Step-A and
Step-B benches stay within run-to-run noise of round 10 (~334
vs. ~333 MSym/s on Step-A; ~333 vs. ~327 MSym/s on Step-B) —
the new cache field does not regress the dominant paths.
Bit-identity vs. the round-10 `cum[]`-array Step-C form is
verified by `rangecoder_step_c_encode_bit_equiv_to_generic`,
which encodes the same mid-band stream through both paths and
asserts byte equality. The `freqs[s]` cache layout itself is
regression-guarded by `cdf_freq_matches_array_form`. Dedicated
Step-A1 (`s == 1`) / Step-B1 (`s == 254`) fast paths were
prototyped and reverted: the extra branches in the hot loop
regressed the dominant Step-A path more than they helped the
secondary symbols (the post-gradient Laplacian residual is
sharp enough that symbols `±1` are an order of magnitude
rarer than symbol `0`).

## Pair-packed 513-entry CDF (legacy type 7)

The type-7 legacy range coder builds its probability model two ways
(`spec/07` §3.4):

* The **flat 257-entry CDF** is the cleanroom's self-roundtrip form —
  the encoder (`encode_legacy_rgb`) builds the same flat CDF, so its
  streams decode byte-exactly.
* The **pair-packed 513-entry CDF** is the proprietary's form
  (`spec/07` §3.1 + §3.4 audit-corrected): the rescaled frequencies
  are interleaved with sentinel-`1` inter-symbol gaps as
  `(freq'[c], 1)` and prefix-summed, so symbol `c`'s bounds sit at
  `cdf[2c]` / `cdf[2c+1]` and the full span is `total + 256`. The
  decoder addresses it via the `spec/07` §5.2 even-index binary
  descent with the same `total = next_pow2(Σfreq)` divisor.

Audit/12 §5..§6 proved the two are **not** bit-equivalent for the
rare-symbol-cluster fixture class (`freq[0] >= 0.95 * Σfreq` plus ≥ 3
distinct nonzero bins with `freq ∈ {1, 2}`): the sentinel gaps push
high-index rare symbols' lower bounds past `total`, so they become
unreachable and the proprietary mis-decodes them (audit/12 §3.6 —
`0xc0` decodes as `0xff`). The decoder selects the pair-packed path
for streams matching this signature (which our own encoder never
produces — Strategy E routes them to type 1), reproducing the
proprietary decode bit-faithfully. The pair-packed construction +
addressing are implemented clean-room from the spec and unit-tested
against the audit/12 §5 worked example (boundaries `1081 / 1085 /
1215`); full *byte-exact* parity against a real proprietary-encoded
type-7 stream still awaits a fixture oracle
(`samples.oxideav.org/lagarith/`, 404 per audit/04 §5).

## Tests

129 unit + integration tests cover the range coder, Fibonacci
prefix, RLE escape, predictor + decorrelation, channel-header
dispatcher, the channel-header `0x01..=0x03` arithmetic-with-RLE
path and the `0x05..=0x07` raw-RLE path, an end-to-end encode →
decode round-trip for each of types 1, 2, 3 (round 3 YUY2), 4,
5, 6, 7 (round 4 legacy RGB; round 5 Rule B + RLE-then-Fibonacci
channel sub-path), 8, 9, 10 (round 2 YV12), 11 (round 3
reduced-resolution), the legacy adaptive-CDF range coder + its
Fibonacci freq-table primitives (round 4), the audit/12
rare-symbol-cluster signature detector (round 5; round-6 encoder
Strategy E), the round-96 **pair-packed 513-entry CDF** decode
path (Strategy F) — its layout vs. the flat form, the audit/12 §5
worked-example boundary shifts, and length-correct decode through
both the channel decoder and `decode_frame` — the YUY2
odd-width floor-chroma layout (audit/00 §9.4), and the NULL-frame
("JUMP") replay path through both the `Decoder` wrapper and the
`decode_frame_with_prev` helper.

### SIMD-vs-scalar predictor (`spec/06` §3.5)

For frame types 2 / 4 / 8 / 10 / 11 the crate's predictor
implements **Rule A / Strategy A** (`TL = L = plane[y-1][W-1]`
for every row `y >= 1`, per `spec/06` §3.6), which is
*carry-equivalent* to the proprietary's SIMD inner-loop. No
separate SIMD code path is therefore required for byte-
equivalent output on these types.

For frame type 7 (legacy RGB) the predictor uses **Rule B**
(`TL = plane[y-2][W-1]` for `y >= 2`, falls back to Rule A for
`y = 1`) per `spec/07` §9.1 item 7b, mirroring the proprietary's
linear-memory walk through the SIMD predictor on the legacy
path.

### Byte-exact encoder validation — SPECGAP

Rounds 1..4 continue the self-roundtrip-only contract. Byte-exact
validation against the proprietary encoder requires either a
proprietary-encoded AVI fixture (we don't carry one in tree —
`docs/video/lagarith/reference/binaries/` only holds the DLL
black-box, not encoded video) or an AVI sample from
`samples.oxideav.org` (returned HTTP 404 at the time of round 4).
This is an Auditor concern for a future round.
