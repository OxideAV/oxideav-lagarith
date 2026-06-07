# oxideav-lagarith

Pure-Rust Lagarith lossless video codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Round 253 — typed `FrameType` × `PixelKind` compatibility relation
accessor.** Extends the public `FrameType` enum (rounds 242 / 245)
with `accepts_pixel_kind(PixelKind) -> bool` and a complementary
`compatible_pixel_kinds() -> &'static [PixelKind]` slice accessor
anchored in `spec/01` §2.1 / §2.2.1 / §2.3 / §2.4 + `spec/03` §6.1 /
§6.2. The compatibility table — uncompressed (type 1) accepts all
four host pixel kinds (`spec/01` §2.1's "RGB24 / RGB32 / RGBA / YUY2
/ YV12 with no Lagarith transformation applied"); the three solid
types (5 / 6 / 9) and four packed-RGB arithmetic types (2 / 4 / 7 /
8) accept the BGR-family pair (`Bgr24` / `Bgra32`, `spec/01` §2.2.1
+ §2.3); the YV12 family (10 / 11) accepts `Yv12` only (`spec/03`
§6.1 + `spec/01` §2.4); YUY2 (3) accepts `Yuy2` only (`spec/03`
§6.2) — is the predicate the per-frame-type decoders already enforce
at function entry, via the literal pixel-kind matches in
`decode_arith_yv12` / `decode_arith_yuy2` / `decode_reduced_res` and
the `PixelKind::bytes_per_pixel` (`packed_bpp`) gate in
`decode_solid` / `decode_arith_rgb` / `decode_arith_rgba` /
`decode_legacy_rgb`. The new accessor lets downstream callers
introspect the relation without re-running the dispatcher or
interrogating `Error::PixelFormatMismatch` failures, and structurally
mirrors the round-245 `PixelKind` partition (`is_rgb_family` /
`is_yuv_family` / `is_packed` / `is_planar` / `has_alpha` /
`bytes_per_pixel`) on the frame-type axis. 5 new unit tests pin the
full 11 × 4 acceptance table per (frame_type, pixel_kind) pair,
non-emptiness (every frame type accepts at least one pixel kind —
no orphan rows), element-wise consistency between the two accessors,
the exact slice sequence returned by `compatible_pixel_kinds` (so
iteration order is part of the public contract), and alignment with
the existing `is_planar_yv12` / `is_packed_yuy2` / `is_packed_rgb`
sub-classifiers. Brings the total unit-test count from 276 to **281**.

**Round 250 — typed `ChannelHeader` structural accessors on the
modern per-plane channel-header byte.** Extends the public
`ChannelHeader` enum (rounds 236 / 242) with two new structural
accessors anchored in `spec/03` §2.1 + `spec/06` §1.2:
`freq_table_offset` returning `Some(1)` for `BareArithmetic` (the
no-length-field path — `spec/06` §1.2 call site B; Fibonacci
prefix begins at channel-data byte 1), `Some(5)` for `ArithRle`
(the §1.4-precondition path — call site A; Fibonacci prefix begins
at byte 5 after the 4-byte u32 pre-RLE length field at bytes 1..4),
and `None` for the three non-arithmetic forms `Raw` / `RawRle` /
`ConstantFill` which carry no Fibonacci-coded freq table; and
`prefix_size` returning the channel-prefix byte count the
dispatcher consumes before the wire-body proper begins (`1` for
every variant that consumes only the header byte; `5` for
`ArithRle`'s header + u32 length field). Mirrors the round-239
`LegacyChannelHeader::freq_table_offset` shape so the modern and
legacy channel-header wire-form classifiers expose the same
structural surface, and mirrors `FrameType::prefix_size` at the
channel level — the two `prefix_size` accessors together let
downstream callers compute byte offsets through both prefix layers
(frame-level channel-offset table + per-channel header machinery)
without re-running the dispatcher. 4 new unit tests pin the two
accessors' values per-byte across the full nine-element accepted
set, consistency of `freq_table_offset` with `prefix_size` (when
present, the offset equals the prefix size; when absent, the
prefix size is 1), and equivalence of `freq_table_offset.is_some()`
with the existing `uses_arithmetic_body` predicate.

**Round 242 — typed `FrameType` classification accessors on the
outermost wire byte.** Extends the public `FrameType` enum with a
`to_byte` round-trip (`from_byte ∘ to_byte = id` on the legal set
`1..=11`) plus 11 spec-grounded semantic helpers anchored in
`spec/01`: `is_uncompressed` (type 1, §2.1), `is_solid` (types
5 / 6 / 9, §2.2), `is_arithmetic` (types 2 / 3 / 4 / 7 / 8 / 10 /
11, §2.3), `is_legacy_decode_only` (type 7 per §3 row 7 — the
encoder-side cross-check writes "(none)"), `is_reduced_resolution`
(type 11, §2.4), `is_planar_yv12` (types 10 / 11), `is_packed_yuy2`
(type 3), `is_packed_rgb` (types 2 / 4 / 7 / 8), and
`is_produced_by_v64_encoder` (the eight types §3 enumerates with
immediate-byte writes: 2 / 3 / 4 / 5 / 6 / 8 / 9 / 10 — exclusion
of types 1, 7, 11 reflects the §3 table's "(none)" rows). Two
structural-size accessors expose the §2.3 channel-offset prefix
sizes: `prefix_size` (`1` for literal / solid types; `9` for
3-channel arithmetic; `13` for 4-channel RGBA arithmetic) and
`channel_offset_table_size` (= `prefix_size − 1`). The new
surface lets downstream callers introspect a parsed frame-type
byte without re-running the per-type dispatcher in
`decoder::decode_frame`, mirroring the round-236 / -239 pattern
for the modern (`ChannelHeader`) and legacy (`LegacyChannelHeader`)
channel-header bytes. 13 new unit tests pin `to_byte` round-trip
closure, the top-level-classes partition invariant (uncompressed
/ solid / arithmetic — every accepted byte satisfies exactly one),
per-predicate membership against explicit positive sets for the
seven boolean accessors, the arithmetic-sub-classes partition
invariant (every arithmetic frame type satisfies exactly one of
{planar-YV12, packed-YUY2, packed-RGB}), the v64-encoder-produced
set, `prefix_size` matching the §2.3 table, and `prefix_size`
consistency with the existing `pack_channels` / `split_channels`
helpers. Brings the total unit-test count from 246 to **259**.

**Round 239 — typed `LegacyChannelHeader` accessor on the legacy
(type-7) per-plane channel-header byte.** A new public enum
classifies the outer channel-header byte of every type-7 channel
slice per `spec/07` §1.3 + §2.1 into its semantic wire form:
`BareFib` (`0x00`, 2-byte channel prefix — outer header + inner
codec-mode flag — followed by a Fibonacci-coded 256-entry
frequency table at offset 2) and `RleThenFib { escape_len }`
(`0x01..=0x03`, 5-byte channel prefix — outer header + u32 LE
post-RLE length field — followed by a `spec/05` zero-run-RLE-
compressed Fibonacci-coded freq table at offset 5 with the
per-channel escape length equal to the outer header byte). The
legal outer-header set is strictly `{0x00, 0x01, 0x02, 0x03}` —
disjoint from the modern (`ChannelHeader`) wire form set, which
also accepts `0x04` raw, `0x05..=0x07` raw-with-RLE, and `0xff`
constant-fill (`spec/03` §2.1 + `spec/06` §1.1). The surface
exposes `from_byte`, `to_byte`, `uses_rle_pre_decompress`,
`rle_escape_len`, and `freq_table_offset` so callers can
introspect a parsed legacy wire header without re-running the
dispatcher. The decoder dispatcher (`decode_legacy_channel`) now
classifies through the typed accessor, making it the single
source of truth for the legal set. Three new unit tests pin
byte-classification on the four accepted bytes, rejection of
ten representative out-of-range bytes including the modern-only
headers (`0x04..=0x07`, `0xff`), and `from_byte` → `to_byte`
round-trip closure.

**Rounds 1..5 — arithmetic-coded RGB / YV12 / YUY2 + NULL-frame
replay + reduced-resolution + legacy RGB (type 7) with Rule B
first-column predictor + RLE-then-Fibonacci channel sub-path.
Round 124 — modern RGB(A) first-column predictor corrected to
**Rule B** (was Rule A), byte-exact against the independent ffmpeg
`lagarith` decoder for power-of-two-pixel RGB24/RGB32/RGBA frames.
Round 127 — extends the ffmpeg cross-decoder pin set to seven
power-of-two pixel sizes (RGB24 4×4 / 8×8 / 8×16 / 16×16 and RGBA
4×4 / 8×8 / 16×16), characterises the residual **pattern-sensitive
divergence** that survives the pow2 selection, and attributes it to
the un-disassembled probability-loader at `lagarith.dll!0x180001050`
(see `tests/ffmpeg_pins.rs` module docs).
Round 211 — **lazy alpha-plane decode**: `decode_arith_rgba` now
skips the fourth-channel arithmetic body (Fibonacci prefix + modern
range coder + RLE + predictor) when the host buffer is `Bgr24`,
spec-grounded by `spec/03` §4.3 (no cross-plane decorrelation for
alpha) + `spec/04` §5 item 5 (channels compressed independently).
Pixel-kind validation also moves to function entry on the modern and
legacy RGB families, so `Yv12` / `Yuy2` host buffers for RGB-coded
frames now surface `PixelFormatMismatch` before any per-channel
decode work (3 new tests pin both halves of the contract).
Round 229 — **type-7 (legacy adaptive-CDF RGB) frame-level type-1
size guard**: a fifth `*_or_uncompressed` entry point
(`encode_legacy_rgb_or_uncompressed`) extends the round-222 modern-
arithmetic guard to the legacy fork. Round 222 already routes the
modern arithmetic families through a never-larger comparison against
`encode_uncompressed(pixels)`; round 229 adds the orthogonal axis
for type 7: even when the type-7 residual histograms clear the
`audit/12` §7.1 Strategy E (rare-symbol-cluster) wire-correctness
diversion, the bare-Fibonacci form's 9-byte channel-offset preamble
+ per-channel adaptive-CDF prefix + range-coder body can exceed the
`1 + W*H*3`-byte raw payload on tiny / high-entropy inputs. The
selector returns the shorter wire, tie-breaking in favour of the
legacy form so already-compressing inputs stay byte-identical to
`encode_legacy_rgb`. The fallback is decoder-orthogonal (`spec/01`
§2.1 + the dispatch table at §1: byte 0 = `0x01` routes the memcpy
helper). When `encode_legacy_rgb` itself already returned type 1
via Strategy E, the tie-break preserves that frame byte-identically
(the size guard composes cleanly with the wire-correctness diversion
rather than masking it). 5 new tests in module
`legacy_frame_uncompressed_size_guard` cover the never-larger
invariant across `4×4`..`32×32` × three LCG seeds per size + a
smooth-gradient fixture, decode-correct round-trip through the
wrapper, a positive selector-fires pin (`4×4` random inputs route
to byte 0 = `0x01`, byte-identical to `encode_uncompressed`), a
tie-break-favours-legacy pin (`32×32` smooth gradient), and a
Strategy E composability pin (a `16×16` rare-symbol-cluster fixture
shows the guard preserves the pre-existing type-1 frame byte-
identically).

Round 222 — **frame-level type-1 (uncompressed) size guard**: four
new encoder entry points
(`encode_arith_rgb24_or_uncompressed`, `encode_arith_yv12_or_uncompressed`,
`encode_arith_yuy2_or_uncompressed`, `encode_arith_rgba_or_uncompressed`)
wrap each modern arithmetic frame encoder with a never-larger
comparison against the equivalent `encode_uncompressed(pixels)` form
(`spec/01` §2.1). The selector returns the shorter wire, tie-breaking
in favour of the arithmetic form so already-compressing inputs stay
byte-identical to the existing `encode_arith_*` output. The fallback
is decoder-orthogonal: byte 0 = `0x01` routes through the memcpy
helper at `lagarith.dll!0x18000555a`, so a type-1 substitute is wire-
format-compatible with every conformant decoder. The 64-bit
proprietary encoder does not produce type 1 in the wild
(`spec/01` §3 row 1), so the guard is a strict structural
improvement on inputs where arithmetic overhead exceeds the raw
payload — the 9-byte (RGB24 / YV12 / YUY2) or 13-byte (RGBA)
channel-offset preamble + per-channel Fibonacci freq tables + range-
coder bodies easily exceed the `1 + W*H*bpp`-byte raw payload at
small frame sizes or random / high-entropy pixel inputs. 13 new
tests in module `frame_uncompressed_size_guard` cover the never-
larger invariant across `4×4`..`32×32` × four pixel families with
three LCG seeds per size + a smooth-gradient fixture, decode-correct
round-trip through every wrapper, a positive selector-fires pin (on
`4×4` random inputs the wrapper picks byte 0 = `0x01` and emits
exactly `encode_uncompressed(pixels)`), and a tie-break-favours-
arithmetic pin (on `32×32` smooth gradient RGB24 the guarded wire
equals the unguarded `encode_arith_rgb24` output byte-identically).

Round 216 — **packed-RGB(A) pack-loop branch hoist**: the per-pixel
`match pixel_kind` arm inside the BGR(A) pack loops of
`decode_arith_rgb` (types 2 / 4), `decode_arith_rgba` (type 8),
`decode_legacy_rgb` (type 7), and `decode_solid` (types 5 / 6 / 9)
is hoisted outside the loop so dispatch fires once per call instead
of once per pixel. `decode_solid` additionally swaps the per-pixel
`Vec::push` push-loop for a `vec![0u8; n * bpp]` + chunked-write
form (one allocation + one bounds-check pair per pixel rather than
one per byte). Output byte sequence unchanged; round 211's lazy
alpha-decode invariant is preserved structurally (the `plane_a_opt
.expect("Bgra32 path always decodes alpha")` moves out of the loop
body so the matches!-guard relationship is still single-source).
Six new tests in module `pack_loop_byte_layout_pins` pin the
invariants the refactor must keep: Bgra32 alpha is 0xff on RGB-only
frames (types 2 / 4 / 6 / 7), wire-driven on RGBA frames (types 8
/ 9), Bgr24/Bgra32 BGR triplets match across pixel kinds for the
same source frame, solid-frame buffer length matches
`PixelKind::buffer_len`, and planar (YV12 / YUY2) frames still
reject packed pixel kinds.**
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
   The first-column-of-row rule is **Rule B**
   (`TL = plane[y-2][W-1]` for `y ≥ 2`, Rule A fallback at `y = 1`)
   for the modern RGB(A) types (2/4/8) and the legacy type-7 path
   alike (`spec/06` §3.2 / `spec/07` §9.1 item 7b). Rule B was
   confirmed for the modern types byte-exactly against the
   independent ffmpeg `lagarith` decoder (round 124); the prior
   Rule A choice mis-decoded real streams. YV12/YUY2 retain Rule A
   pending a clean ffmpeg pin.
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

Round 236 adds a typed [`ChannelHeader`] accessor for the modern
per-plane channel-header byte (frame types 2, 3, 4, 8, 10, 11) per
[`spec/03` §2.1](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/03-channel-decorrelation-and-predictors.md)
and [`spec/06` §1.1](https://github.com/OxideAV/docs/blob/master/video/lagarith/spec/06-simd-predictor-rle-entropy-channel-dispatcher.md).
It classifies the wire-level byte into one of five semantic forms —
`BareArithmetic` (`0x00`), `ArithRle { escape_len }`
(`0x01..=0x03`), `Raw` (`0x04`), `RawRle { escape_len }`
(`0x05..=0x07`), and `ConstantFill` (`0xff`) — with helpers
`uses_arithmetic_body`, `uses_rle_postprocess`, `rle_escape_len`,
and a `to_byte` round-trip for callers that want to introspect a
parsed wire header without re-running the dispatcher. Out-of-range
bytes return `Error::BadChannelHeader`. The legacy (type 7)
channel-header byte uses a disjoint, narrower set per `spec/07`
§1.3 + §2.3 and is **not** covered by this accessor.

```rust
use oxideav_lagarith::{ChannelHeader, Error};

assert_eq!(
    ChannelHeader::from_byte(0x06).unwrap(),
    ChannelHeader::RawRle { escape_len: 2 },
);
assert!(matches!(
    ChannelHeader::from_byte(0x10),
    Err(Error::BadChannelHeader(0x10)),
));
```

`oxideav-core` framework registration is gated on the default-on
`registry` Cargo feature and claims the `LAGS` FOURCC.

[`ChannelHeader`]: https://docs.rs/oxideav-lagarith/latest/oxideav_lagarith/enum.ChannelHeader.html

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

Round 12 lands the **`spec/02` §6.3 final-flush FF-chain
bulk-fill** on the encoder side: `RangeEncoder::finish()`
drained its pre-tail pending-FF chain with a `for _ in
0..pending_ffs { buf.push(...) }` loop, the last per-FF push
loop the encoder retained after round 9 replaced the equivalent
per-iteration `shift_low` loop with `Vec::resize`. Round 12
applies the same `Vec::resize` form to `finish()`: one bounds
check + one memset for the carry / steady-state fill bytes
(`0x00` on a carry, `0xff` otherwise), removing the last
N-push site from the encoder close-out. The on-wire bytes are
unchanged — same `cache` (or `cache+1`) head, same N×fill
chain, same four-byte tail per `spec/02` §6.3 — so the four-
byte tail layout stays bit-identical to the proprietary's
flush primitive. On a short-channel + Step-B-heavy fixture
(512 channels × 128 symbols, 200 reps, release build, macOS
aarch64) the round-12 encoder delivers **~305-311 MSym/s vs.
~298-310 MSym/s** for the per-FF push reference (timed
side-by-side in the same bench) = **~1.00-1.03× speedup**,
default-on. The delta is modest because realistic `pending_ffs`
chains are short (3-15 bytes), but the optimisation completes
the structural pattern (no per-FF push loops anywhere in the
encoder) and is byte-identical: the
`rangecoder_finish_resize_byte_equiv_to_push_loop` test
re-encodes the same 0xff-dominant stream through both forms
and asserts byte equality. The Step-A / Step-B / Step-C
benches are unchanged (the round-12 change only affects
`finish()`).

Round 174 lands the **per-frame-type call-site flip** that round 14
left as a follow-up step. Every modern frame encoder
(`encode_arith_rgb24` / `encode_arith_yv12` / `encode_arith_yuy2`
/ `encode_arith_rgba`, plus `encode_arith_reduced_res` transitively
via `_yv12`) now calls `encode_channel_best` per-plane instead of
`encode_channel_simple`. The type-7 analogue is applied
symmetrically: `encode_legacy_rgb` now routes per-channel through
`encode_legacy_channel_best`. The per-channel choice is decoder-
blind (`spec/03` §2.1 dispatcher routes on byte 0 alone), so the
wire stays decode-compatible with every conformant decoder; only
the size can change. Six new frame-level **never-larger** pins
cover each modern frame type + reduced-res against a hand-
constructed `encode_channel_simple`-pipeline reference frame. A
channel-level `channel_best_strictly_smaller_than_simple_at_64k_zero_heavy`
pin guards the size-delta direction: on a 65,536-symbol ~95%-zero
post-gradient-dominant fixture (`spec/06` §6.4 profile), the
selector picks header `0x01` (Fibonacci-prefixed arithmetic over
the zero-run-contracted symbol stream) and saves **53 bytes** vs.
`encode_channel_simple` (3784 → 3731 bytes, **1.4% reduction**).
The crossover from "bare-Fib wins" to "RLE wins" sits around
`n_symbols ≈ 65536` for this profile — smaller planes the bare-
Fibonacci form already encodes near-optimally, so the +4-byte u32
length field of `spec/07` §2.3 is not yet amortised on the smaller
fixtures the existing roundtrip suite covers. On type-7, the
selector picks the bare-Fibonacci form byte-identically to
`encode_legacy_channel` on every realistic histogram (per the
`legacy_best_always_picks_bare_on_realistic_inputs` pin), so the
production wire bytes for type-7 frames are unchanged today; the
flip is a **structural never-larger guarantee** plus a forward path
for any future Fibonacci variant the spec adds that emits zero
bytes. The ffmpeg pins in `tests/ffmpeg_pins.rs` decode wires that
this crate produced before the flip and still pass — wire-format
compatibility unchanged.

Round 15 (round 141) lands the **legacy-fork analogue**:
**per-channel header-form selection for type-7 (legacy RGB)**
(`encode_legacy_channel_best` + frame-level `encode_legacy_rgb_best`).
The selector enumerates the four wire forms `decode_legacy_channel`
accepts — bare-Fibonacci (`0x00`, `spec/07` §2.5 / §6.3) plus the
three RLE-then-Fibonacci variants (`0x01..0x03` one per
`escape_len ∈ {1, 2, 3}`, `spec/07` §2.3 / §2.4) — and returns
the shortest. Strategy E (`audit/12 §7.1`) propagates through the
frame-level wrapper. Empirical correction to `spec/07` §6.3's
"compression trade-off" framing: with the proprietary's bit-packed
Fibonacci layout, the encoded freq table produces **zero `0x00`
bytes** on every realistic histogram probed (dense, sparse,
two-symbol, biased-mid-band), so the RLE escape has nothing to
swallow and the three RLE-then-Fib candidates end up `+4 bytes`
longer than bare-Fib (the u32 length field is dead weight). The
selector therefore picks header `0x00` on every realistic input,
ties broken in favour of bare-Fib (byte-identical to the previous
`encode_legacy_channel` output — no roundtrip suite churn). The
new selector is therefore a **never-worse defensive guarantee**
+ a forward path for any future Fibonacci variant the spec adds
that does emit zero bytes; the `legacy_best_always_picks_bare_on_realistic_inputs`
test pins the current empirical invariant so a Fibonacci-encoder
tweak that begins emitting zero bytes surfaces as a deliberate
failure here. 8 new tests cover never-larger-than-bare, every-
sub-path-roundtrips, legal-header-only emission, the tie-breaker
keeps the bare-Fib bytes byte-identical, frame-level roundtrip on
4×4 / 8×8 / 16×12 BGR24, frame-level never-larger, Strategy E
propagation, and the empirical bare-only-wins pin.

Round 14 lands **per-channel header-form selection
(`encode_channel_best`)**: the encoder now considers every wire
form the decoder dispatcher accepts (`spec/03` §2.1 + `spec/06`
§1.7) — `0xff` solid-fill, `0x00` Fibonacci+arith, `0x01..0x03`
Fibonacci+arith with pre-RLE contraction, `0x04` raw memcpy, and
`0x05..0x07` raw bytes with RLE post-processing — encodes the
plane through each legal one, and returns the shortest. Headers
`0x05..0x07` are new on the encoder side (exposed as
`encode_channel_raw_rle`); headers `0x01..0x03` were already
implemented in `encode_channel_arith_rle` but were not in the
auto-selection set. The `spec/06` §1.5 fall-back rule (pre-RLE
count `>= n_pixels` reverts dispatcher to header-`0x00`
semantics) is enforced — illegal candidates are skipped, never
emitted. Picking among legal forms is purely an encoder choice:
a decoder reads byte 0, routes to the matching sub-path, and
recovers the same plane regardless. So replacing
`encode_channel_simple` with `encode_channel_best` cannot regress
self-roundtrip correctness; it can only shrink output. On a
representative post-gradient Lagarith residual (1900 zeros + 100
Laplacian-tail non-zero bytes, the canonical profile per
`spec/06` §6.4) the round-14 selector produces **90 bytes vs.
143 bytes** for the round-13 two-candidate selector
(`encode_channel_simple`, headers `0x00` / `0x04` only) =
**37% smaller wire**, with header `0x01` (arith + RLE escape_len=1)
winning. The frame-encoder call sites continue to use
`encode_channel_simple` for now — flipping each frame type
individually is the next bounded step once a per-frame-type
benchmark fixture is wired so the size-delta can be measured per
frame type rather than per channel. The new fixture-based
selector tests cover every selected form's roundtrip, the
zero-heavy / flat-with-runs pinned preferences, the never-larger-
than-simple invariant, and the 10%+ measured gain.

Round 13 lands the **modern probability-model write path**: the
per-channel encode now rescales the raw byte-histogram so the
**transmitted total stays inside the coder's `q >= 1` operating
range** before building the CDF and the Fibonacci prefix. `spec/02`
§5 starts every symbol with `q = range / total` and `spec/02` §2
floors the renormalised `range` at `TOP + 1`, so a transmitted
`total > TOP (0x800000)` collapses `q` to zero and breaks the
coder. `spec/04` §5 documents the proprietary loader normalising
the histogram for exactly this guarantee; its validation correction
shows the wire still carries a raw histogram for every probed
fixture — all far below `TOP`. So the rescale **no-ops for every
sub-`TOP` plane** (the wire stays byte-identical to the
raw-histogram form) and engages only for planes whose symbol count
exceeds `TOP` (> ~8.39M residuals — a single 4K+ plane), which the
prior encoder could not encode at all. The rescale is
`floor(freq * cap / sum)` clamped up to `1` per nonzero slot (no
used symbol drops out) with overshoot trimmed from the largest
slots; encoder and decoder build the identical CDF from the
transmitted rescaled table (`spec/04` §6), so the coder stays
exact. Five new tests cover the passthrough, the cap +
nonzero-preservation invariants, the overshoot-trim path, a
small-cap end-to-end self-roundtrip, and a genuine `total > TOP`
~8.4M-residual plane that round-trips byte-exactly at the
production cap.

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

249 unit + integration tests cover the range coder, Fibonacci
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
`decode_frame_with_prev` helper, and **seven**
ffmpeg-cross-validated byte-exact pins for the modern
arithmetic RGB24 (4×4, 8×8, 8×16, 16×16) and RGBA (4×4, 8×8, 16×16)
paths ([`tests/ffmpeg_pins.rs`](tests/ffmpeg_pins.rs); round 124
added the first three pins under structured patterns, round 127
extended the set with random-seeded patterns across non-square
and smaller pow2 sizes). Round 174 adds **six frame-level
never-larger pins** for the four modern frame encoders +
reduced-res + type-7 byte-identity, plus a channel-level
`channel_best_strictly_smaller_than_simple_at_64k_zero_heavy`
crossover pin (53-byte / 1.4% saving at the documented fixture).
Round 181 adds a **decoder defensive harness** (22 new tests) that
systematically feeds malformed inputs through the production
`decode_frame` / `decode_frame_with_prev` / `Decoder::decode`
surface and asserts each surfaces the documented `Err(_)` variant
rather than panicking: empty payloads (`NullFrame`), zero
dimensions (`BadDimensions`), out-of-range frame-type bytes
(`BadFrameType` for `0` and `12..=255`), truncated uncompressed
bodies and solid-fill colour bytes (`Truncated`), planar pixel
formats requested against packed-RGB / solid frames
(`PixelFormatMismatch`), short / descending / past-EOF channel-
offset tables (`Truncated` + `OffsetOutOfRange`), short and
unknown channel-header bytes through the `spec/03` §2.1
dispatcher (`Truncated` + `BadChannelHeader`), and the stateful
NULL-frame replay invariants (`NullFrameWithoutPredecessor` +
`PixelFormatMismatch{frame_type:0}`). Two deterministic-LCG-seeded
no-panic sweeps exercise (a) random byte streams across every
frame-type byte (`0..=12`) × three seeds × eight lengths × four
pixel kinds and (b) random per-channel bodies behind a valid
type-4 offset table re-routed through the type-3 / -7 / -10
dispatchers — every probe returns `Result`, none panics. Round
187 extends the harness with **6 reduced-resolution (type 11)
dimension-guard tests**: `decode_reduced_res` now rejects host
W/H pairs that aren't multiples of 4 with `Error::BadDimensions`
before any wire bytes are consulted (per `spec/01` §2.4 the 2×
nearest-neighbour upscale requires `W = 2 * half_w` / `H = 2 *
half_h` and the embedded half-res YV12 chroma sub-sampling
requires `half_w` / `half_h` each even — i.e. host W / H each
multiples of 4). The previous bound let odd dimensions flow into
`upscale_plane_2x`, which `debug_assert!`-panicked in debug and
silently zeroed chroma planes in release. Tests pin odd widths,
odd heights, widths `≡ 2 mod 4`, heights `≡ 2 mod 4`, zero
dimensions, and a positive pin that multiples-of-4 still flow
into the body parser. Round 192 layers a **truncation +
single-byte-flip fuzz** (12 new tests, module
`decoder_truncation_fuzz`) on **valid** encoded frames — closes
the gap between round 181's hand-constructed malformed fixtures
(one per documented `Err(_)` variant) and round 181's random-byte
sweep (statistically unlikely to look like a valid
`(escape_len, supplement_byte)` RLE pair or a Fibonacci-coded
prefix). Each test encodes one valid frame per frame-type the
test encoder covers (1, 3, 4, 5, 6, 7, 8, 9, 10, 11), then walks
every truncated prefix `frame[..k]` for `k ∈ 1..frame.len()`
across all four pixel kinds asserting no-panic + (for prefixes
strictly inside the `spec/01` §2.3 channel-offset table)
`Error::Truncated`. Each frame is also single-byte-flipped at
every 7th offset to `0x00` and `0xff` (7 coprime with channel-
header / Fibonacci-prefix / RLE byte strides so the flip pass
does not align to any one structural feature). The stateful
`Decoder` and `decode_frame_with_prev` paths are exercised
against truncated primers + a mismatched-shape `prev`-frame,
pinning the invariant that a failed primer must not leave a
half-initialised `prev` slot a subsequent NULL replay would
dereference. Round 198 adds a **deeper channel-body fuzz**
(6 new tests, module `decoder_deep_fuzz`) layered on top of
round 192's prefix-sweep: at `8×8` (where the channel body
dominates the byte budget — round 192's `4×4` frames bottom out
at ~5-20 body bytes per plane) every body offset is single-bit-
XOR-flipped at each of the 8 bit positions, then burst-flipped
with `N ∈ {2,3,4}` consecutive bytes set to one of `0xff` /
`0x00` / `0x55` / `0xaa` (the alternating-bit fills are new vs.
r192's two-extreme `0x00` / `0xff` vocabulary), then shifted by
±1 body byte (delete or insert a `0x00`) at every 11th offset.
The bit-flip axis probes the MSB-first Fibonacci-prefix decoder
(`spec/04`) and the modern range coder's normalisation loop
(`spec/02` §5) at single-bit-granularity that two-value byte
sweeps are blind to; the burst axis reaches multi-byte length
decoders that walk through well-formed prefixes into the
corrupted region; the shift axis tests decoders that implicitly
assume aligned reads (range coder 4-byte priming, Fibonacci
prefix's bit-granular reads crossing byte boundaries). Covers
frame types 3 / 4 / 7 / 8 / 10 / 11. Same no-panic invariant
the round-181 / 192 sweeps assert; 210 unit + integration tests
pass after the addition. Round 204 adds a **randomised
encoder→decoder self-roundtrip property suite** (9 new tests,
module `encoder_random_roundtrip_property`) — the orthogonal
strict-byte-equality invariant on the encoder side. Every modern
arithmetic family (`encode_arith_rgb24` / `encode_arith_rgba` /
`encode_arith_yv12` / `encode_arith_yuy2`) plus the legacy
type-7 path (`encode_legacy_rgb`) is driven with deterministic
LCG-seeded random pixel buffers across 3 seeds × 4 representative
`(W, H)` pairs per family, plus a wider 8-seed cross-sweep at the
canonical 8×8 size for each modern type. The four `(W, H)` pairs
per family span both selector branches (RGB24 `width % 4 == 0`
vs. unaligned; YUY2 / YV12 chroma sub-sampling alignment). Each
test asserts strict byte equality between the input pixel buffer
and the decoded `Image::pixels` — a stronger correctness pin than
the existing fixed-pattern roundtrip fixtures (which use a single
`i * 73 + 11` gradient and would miss encoder fast-path
asymmetries that fire only on rare residual distributions —
`spec/02` §5 Step-A `s == 0` and Step-B `s == 255` short-circuits
in particular). Reduced-resolution type 11 is excluded by
construction (its 2× downsample → upsample is lossy; only the
fixed-point round-trips, pinned by `reduced_res_roundtrip_*`).
219 unit + integration tests pass after the addition. Round 216
brings the total to 228 with the new packed-RGB(A) pack-loop
byte-layout pins. Round 222 adds the frame-level type-1 (uncompressed)
size guard module (`frame_uncompressed_size_guard`) with 13 new pins,
bringing the total to 241. Round 229 extends the size guard to type 7
(`legacy_frame_uncompressed_size_guard`) with 5 new pins, bringing the
total to 246. Round 242 adds 13 typed-`FrameType`-accessor pins in
module `frame::tests`, bringing the total to **259**.

### SIMD-vs-scalar predictor (`spec/06` §3.2)

For frame types 2 / 4 / 8 the crate's predictor implements
**Rule B** (`TL = plane[y-2][W-1]` for `y >= 2`, Rule A fallback
at `y = 1`, per `spec/06` §3.2), the linear-memory walk
*carry-equivalent* to the proprietary's SIMD inner-loop. No
separate SIMD code path is required for byte-equivalent output.
Frame type 7 (legacy RGB) uses the same Rule B (`spec/07` §9.1
item 7b). YV12 (type 10) / YUY2 (type 3) / reduced-resolution
(type 11) retain Rule A pending a clean ffmpeg pin (their planar
scan order interacts with the DIB flip differently — see
"Cross-decoder validation" below).

### Cross-decoder validation — ffmpeg black-box oracle

Round 124 closes the long-standing byte-exact-validation SPECGAP
for the modern RGB(A) path. ffmpeg ships a `lagarith` **decoder**
(no encoder), so the crate's own encoder output is wrapped in a
minimal `LAGS`-coded AVI and decoded by `ffmpeg -f rawvideo`; for
every power-of-two-pixel-count RGB24 / RGB32 / RGBA frame tested,
ffmpeg reproduces the original pixels byte-for-byte (after the
positive-`biHeight` bottom-up flip). This independently confirms
the wire format and resolved the open audit/01 §9.1 first-column
dispatch question in favour of **Rule B** — the prior Rule A
choice mis-decoded these streams in ffmpeg. The committed pins in
`tests/ffmpeg_pins.rs` run in CI without ffmpeg.

Open items (out of scope per workspace policy, no GitHub issue):
(a) **Pattern-sensitive ffmpeg divergence at pow2 sizes** — the
crate's encoder, when fed structured residual patterns (e.g.
`i * 73 + 11 → bit-slice`), produces frames at *power-of-two* pixel
sizes that ffmpeg's lagarith decoder mis-decodes by ~40-60% even
though the crate's own decoder self-roundtrips them byte-exactly.
Random-seeded byte patterns at the same sizes decode cleanly in
ffmpeg (verified across 4×4, 4×8, 8×8, 8×16, 16×16, 32×32 for both
RGB24 and RGBA, three seeds each). Most-likely root: the wire's
raw-histogram → cumulative-frequency + shift-exponent conversion at
`lagarith.dll!0x180001050` (referenced from `spec/02` §5 + `spec/04`
§5/§6, **not disassembled into cleanroom spec**); ffmpeg's
implementation almost certainly mirrors a normalisation step our
encoder/decoder collapse to identity. Closing the gap is an
Extractor-round deliverable (disassemble + spec `0x180001050`).
(b) Non-pow2 pixel counts compound (a) with a related pow2-total
gap (rescale-to-next-pow2 partial-fix confirmed in r127 but not
byte-exact); same docs gap blocks both. (c) YV12 / YUY2 planar
scan-order vs the DIB flip needs a clean ffmpeg pin — round 127
established YV12/YUY2 self-roundtrip is independent of Rule A vs
Rule B, so the gap is downstream of the predictor. (d) ffmpeg does
not implement the type-1 Uncompressed path, so it cannot oracle
that type. Byte-exact parity against a *proprietary-encoded* AVI
still awaits a fixture (`samples.oxideav.org/lagarith/`, 404 per
audit/04 §5).
