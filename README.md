# oxideav-lagarith

[![CI](https://github.com/OxideAV/oxideav-lagarith/actions/workflows/ci.yml/badge.svg)](https://github.com/OxideAV/oxideav-lagarith/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/oxideav-lagarith.svg)](https://crates.io/crates/oxideav-lagarith) [![docs.rs](https://docs.rs/oxideav-lagarith/badge.svg)](https://docs.rs/oxideav-lagarith) [![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Pure-Rust Lagarith lossless video codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework. Built
clean-room from the specification and trace documents under
`docs/video/lagarith/` only.

## Status

The decoder handles **every Lagarith frame type**, and the encoder is a
**public API** — [`encode_frame`] is the symmetric counterpart of
[`decode_frame`], accepting the same [`PixelKind`] host buffer and
emitting a single self-contained Lagarith frame that round-trips
byte-exactly. Both directions are wired into `oxideav-core`'s codec
registry (the `LAGS` `CodecInfo` carries `.with_decode()` **and**
`.with_encode()`), so framework consumers can drive encode and decode
end-to-end. The encoder produces every encodable type — the modern arithmetic families
(RGB24 type 2/4, RGBA type 8, YV12 type 10, YUY2 type 3,
reduced-resolution type 11), the legacy adaptive-CDF RGB path (type 7),
the literal / solid frames (types 1 / 5 / 6 / 9), and NULL "JUMP". A
machine-checked invariant confirms **every one of the nine modern
channel-header sub-forms the decoder accepts is encodable**, and an
exhaustive encode→decode matrix plus a 1900-iteration encoder fuzz loop
prove byte-exact self-roundtrip across every family, dimension class,
and data pattern. The YUY2 (type 3) encoder closes the
**odd-width** sub-form — it mirrors the decoder's floor-chroma
layout (`spec/03` §6.2), unpacking the trailing luma column with no
chroma counterpart and dropping the decoder-synthesised `0x80`
neutral tail slot — so odd widths (incl. the degenerate `W = 1` with
empty chroma planes) now self-roundtrip byte-exactly. Decode is
stateless per frame (with a stateful
wrapper for NULL "JUMP" frames).

**Round 459 lands vendor-corpus exactness.** The docs workspace staged
a 240-stream corpus produced by the **vendor's own encoder**
(`docs/video/lagarith/fixtures/`, 2026-09-12: 187 byte-exact
round-trip streams + a 3-frame NULL sequence + 52 degenerate
geometries at which the vendor codec is itself lossy), and the crate
now decodes **187/188** of the byte-exact class byte-exactly through
`decode_frame` (the one miss is a stream the vendor round-trips only
through its 24-bpp DIB-stride host layout) and reproduces the vendor
*decoder's* own output on **42/52** of the lossy class through
`decode_frame_vendor_layout` — which also reproduces all 188/188 of
the byte-exact class. The corpus is vendored with its digests and
pinned in CI (`tests/vendor_corpus.rs`); the round-8 spec errata it
came with (SIMD first-column Rule B, the RLE zero-count rule, the
pow2 model normaliser) are each pinned per stream on the vendor's own
bytes, and the one crate-side defect it exposed — the header-`0xff`
"solid plane" is a residual plane `{v, 0, 0, …}`, not a fill — is
fixed. On the encoder side **45/179** vendor inputs now encode to
bytes identical to the vendor's (every solid frame and every frame
whose channels the vendor codes bare-arithmetic; 80/195 same-header
channels are byte-identical) and no frame is larger than the
vendor's.

**Round 451 lands third-party decodability of our encoded streams.**
A black-box capture harness (`examples/blackbox_capture.rs`) drives a
27-case deterministic matrix — every emittable frame type × content
class × dimension parity — through an independent third-party decoder
used strictly as a black-box binary oracle (never in CI): **all
19 RGB24 / RGB32 / RGBA / YV12 / even-width-YUY2 cases decode
sample-exactly** in that oracle (arithmetic types 2 / 3 / 4 / 8 / 10,
solids 5 / 6 / 9, downscale-elected tables, unaligned widths,
non-power-of-two totals, and the round-127 "structured pattern" class
whose re-capture had been the standing open item). Types 1 / 7 / 11,
the NULL payload, and odd-width YUY2 are rejected by that oracle
build before decode (unsupported there). CI freezes all 26 captured
streams by hash (`tests/blackbox_encode_pins.rs`). Getting here
surfaced and fixed three wire-semantics divergences the
self-roundtrip suites could never see — the range coder's top-symbol
slack interval, the YV12-family first-column predictor rule, and the
complete YUY2 predictor (raw second row-0 luma sample, plain-L row-1
first chunk, 8-bit-wrapping median; see below) — and restricted the
per-channel election to the cross-validated header set.

### Frame-type coverage

| Frame type | Wire form |
| ---------- | --------- |
| 1 — Uncompressed | raw pixel data |
| 2 — Unaligned-RGB24 | arithmetic, `width % 4 != 0` |
| 3 — Arithmetic-YUY2 | packed → planar (Y / U / V planes) |
| 4 — Arithmetic-RGB24 / RGB32 | arithmetic, `width % 4 == 0` |
| 5 — Solid Grey | byte fill |
| 6 — Solid RGB | three-byte fill |
| 7 — Legacy RGB | adaptive-CDF + RLE-then-Fibonacci (`spec/07`) |
| 8 — Arithmetic-RGBA | four planes incl. alpha |
| 9 — Solid RGBA | four-byte fill |
| 10 — Arithmetic-YV12 | three-plane Y / V / U |
| 11 — Reduced-resolution | type 10 at half-W/H + 2× upscale |
| NULL ("JUMP") | zero-byte payload, replay previous frame |

## Decode pipeline

1. **Frame layout** (`spec/01`) — byte 0 is the frame-type selector;
   non-NULL frames carry an `(n_channels - 1) * 4` byte channel-offset
   table.
2. **Per-channel header dispatcher** (`spec/03` §2.1 + `spec/06` §1) —
   the header-`0xff` "solid plane" form is a *residual* plane
   `{v, 0, 0, …}` (zeroed plane, byte 1 stored at position 0) that
   runs through the predictor like any other channel (`spec/03` §2.1
   corrected blockquote; the YUY2 coordinator copies `Y[0]` into
   `Y[1]` first).
3. **Fibonacci probability prefix** (`spec/04`) — MSB-first Zeckendorf
   decode of the 256-entry frequency table with the zero-run subcode.
4. **Modern range coder** (`spec/02`) — TOP = 2^23, init range = 2^31,
   four-byte priming + flush, byte refill with cross-byte LSB rotation.
5. **Residual zero-run RLE escape** (`spec/05`) — `escape_len +
   LUT[supplement_byte]` zero runs, decoded lazily from the range
   coder until the plane is full: the escape fires on the
   `escape_len`-th consecutive zero and the very next symbol is the
   supplement (`spec/06` §2.3 / §2.6); the channel's u32 pre-RLE
   count is a dispatch hint, not the symbol budget.
6. **Spatial predictor** (`spec/03` §3) — left predictor on row 0,
   JPEG-LS clamped median on rows ≥ 1. The modern RGB(A) types (2 / 4 /
   8) and the legacy type-7 path use the **Rule B** first-column rule
   (`TL = plane[y-2][W-1]`; the docs' round-8 validation records the
   older "Strategy A on the SIMD path" reading as an erratum — every
   vendor RGB-family stream with `H >= 3` decodes only under Rule B,
   pinned per stream), while the YV12 / reduced-resolution
   families (10 / 11) use the round-451 oracle-recovered **Yuv**
   rule — row 1 predicts `L = plane[0][W-1]` (the `0x180009f30`
   carry enters the row holding `T`, so `MED(L, T, T) = L`), rows
   ≥ 2 take the Rule-B median — and YUY2 (type 3) uses its own
   recovered predictor: the luma plane stores its second row-0
   sample raw (the packed first macropixel seeds `Y0` and `Y1`),
   the first chunk of row 1 (4 luma / 2 chroma lanes) predicts
   plain `L`, and everywhere else the clamped-median **gradient
   wraps mod 256** before clamping (where RGB / YV12 clamp the
   signed gradient), with the Rule-B first column for rows ≥ 2.
   Both recoveries replace `spec/06` §3.8's "Strategy A everywhere"
   reading (flagged as an erratum candidate) and close the §6.4
   open item: the black-box oracle reconstructs YV12 **and**
   even-width YUY2 frames byte-exactly under these rules at every
   probed geometry/content class — and under no other candidate
   (the two families measurably do NOT share one predictor).
7. **Cross-plane decorrelation** (`spec/03` §4) — RGB families only:
   `R += G; B += G` post-prediction; alpha is stored raw.

## API

Stateless decode of a single frame:

```rust
use oxideav_lagarith::{decode_frame, PixelKind};

let decoded = decode_frame(&payload, width, height, PixelKind::Bgra32)?;
assert_eq!(decoded.pixels.len(), (width as usize) * (height as usize) * 4);
# Ok::<(), oxideav_lagarith::Error>(())
```

YV12 produces concatenated Y / V / U planes:

```rust
use oxideav_lagarith::{decode_frame, PixelKind};

let yv12 = decode_frame(&payload, width, height, PixelKind::Yv12)?;
assert_eq!(yv12.pixels.len(), PixelKind::Yv12.buffer_len(width, height));
# Ok::<(), oxideav_lagarith::Error>(())
```

`decode_frame_vendor_layout` decodes the same wire **as the vendor
decoder lays it into a host buffer**, reproducing three
host-integration behaviours of the vendor build at degenerate
geometries that the wire format does not define (`spec/06` §3.2 step
1 / §3.7 / §3.8 validated notes): 24-bpp rows on the Windows DIB
stride `(3W + 3) & !3` (zero pads, output cut to `3·W·H`), no `+= G`
recorrelation on single-row RGB32 output, and — below the YV12 vector
loop's size — a row-1 / column-0 `TL` seed read from the byte
preceding each plane. It is bit-identical to `decode_frame`
everywhere else; use it to compare against vendor-captured output at
such sizes, and `decode_frame` for everything a host actually wants.

Stateful decode that handles NULL ("JUMP") frames by replaying the
predecessor (`spec/01` §1.1):

```rust
use oxideav_lagarith::{Decoder, PixelKind};

let mut dec = Decoder::new();
let frame_a = dec.decode(&payload_a, width, height, PixelKind::Bgra32)?;
// Empty payload -> a clone of frame_a.
let frame_b = dec.decode(&[], width, height, PixelKind::Bgra32)?;
# Ok::<(), oxideav_lagarith::Error>(())
```

The `ChannelHeader` accessor classifies the modern per-plane
channel-header byte (frame types 2 / 3 / 4 / 8 / 10 / 11) into one of
five semantic forms — `BareArithmetic`, `ArithRle`, `Raw`, `RawRle`,
and `ConstantFill` — with `uses_arithmetic_body`,
`uses_rle_postprocess`, `rle_escape_len`, and a `to_byte` round-trip.
The `FrameType` enum also exposes structural accessors:
`wire_plane_roles()` (per-plane semantic role in wire order),
`wire_plane_pixel_counts(w, h)` (per-plane byte counts), and
`n_channels()`.

`oxideav-core` framework registration is gated on the default-on
`registry` Cargo feature and claims the `LAGS` FOURCC for **both** the
decoder and the encoder.

## Encode API

Stateless encode of a single frame, the symmetric counterpart of
`decode_frame`:

```rust
use oxideav_lagarith::{encode_frame, decode_frame, PixelKind};

let frame = encode_frame(&pixels, width, height, PixelKind::Bgra32)?;
// Round-trips byte-exactly back through the decoder.
let decoded = decode_frame(&frame, width, height, PixelKind::Bgra32)?;
assert_eq!(decoded.pixels, pixels);
# Ok::<(), oxideav_lagarith::Error>(())
```

`encode_frame` picks the **smallest** legal wire form automatically —
the per-family solid-colour fast path (`spec/01` §3.1; types 5 / 6 /
9), the modern arithmetic body (types 2 / 4 / 8 / 10 / 3 by `kind` and
`width % 4`), and a frame-level uncompressed (type 1) size guard
(`spec/01` §2.1). The choice is externally invisible: a conformant
decoder dispatches on byte 0, so every form decodes to the identical
pixels. A bad buffer length or zero dimension surfaces a clean
`Error::BadDimensions` rather than a panic. `encode_null()` produces
the zero-byte NULL ("JUMP") payload (`spec/01` §1.1).

Round 432 rebuilt the per-channel election around a **closed-form
cost model** (exact Fibonacci-prefix bits + entropy body estimate,
O(nonzero) per candidate):

- **Transmitted-model downscale election** — the probability-prefix
  table is an encoder-side model choice (the decoder rebuilds its
  model deterministically from the wire bytes, `spec/04` §6 +
  `provenance/52`), so the encoder scores an element-wise
  `max(1, freq >> d)` ladder and probes the elected rung with one
  real encode, keeping it only on a strict byte win. Measured on
  gradient+noise content: −0.36…−0.57% at 64×64, −0.16…−0.19% at
  640×480, never larger than the raw-histogram wire.
- **Full-capacity RLE escapes** — `spec/05` §5.3's algebraic
  inverse unlocks the 254/255 paddings the staged INV_LUT's index
  form cannot express; long zero runs now split per the §5.4
  canonical greedy emit (a 4096-zero stretch contracts 34 → 32
  bytes at `escape_len = 1`).
- **Candidate pruning + gating** — the three arith+RLE forms are
  ranked by the cost model and only the best is range-encoded, and
  any arithmetic pass whose estimate cannot beat the known-length
  raw forms (entropy is a lower bound on arithmetic output) is
  skipped outright. Byte-identical output on every size fixture;
  zero-heavy 640×480 encodes ~11% faster and full-random content
  ~68–72% faster (the type-1 fallback class skips the range coder
  entirely).

Through the framework, `CodecRegistry::first_encoder` yields a
`LagarithEncoder` (`oxideav_core::Encoder`): `send_frame` reassembles
the packed host buffer from a `VideoFrame`'s planes (stride padding
stripped; YV12's three `Y / V / U` planes concatenated), and
`receive_packet` emits one packet per frame carrying the source
frame's PTS. A frame byte-identical to its predecessor becomes the
zero-byte NULL ("JUMP") payload (`spec/01` §1.1) as a non-keyframe
packet — static scenes cost 0 payload bytes and the stateful decoder
replays the predecessor losslessly; distinct frames stay intra
keyframes. The host pixel format is read from
`CodecParameters::pixel_format` (`Bgr24`, `Bgra`, `Yuv420P`,
`Yuyv422`); unsupported formats are rejected at encoder construction.

The per-channel header election stays within the **interoperable
form set** `{0x00, 0x01..0x03, 0x04, 0xff}`. Since round 459 the
`0xff` "solid plane" form is elected for *any* solid residual plane
(`{v, 0, 0, …}`, and the YUY2 luma's `{v, v, 0, …}`), exactly as the
vendor encoder does (`07,ff,ff` / `ff,00,ff` / `ff,ff,ff` channel
shapes), and a 32-bpp host buffer whose alpha is `0xff` throughout is
encoded as the RGB24 family (types 2 / 4 / 5 / 6 — the vendor's own
treatment of 32-bpp input outside its RGBA mode), dropping the alpha
channel from the wire. The raw+RLE forms (`0x05..0x07`) remain
decodable and directly encodable (`encode_channel_raw_rle`) but are
withheld from the automatic election: the vendor corpus proves them
ordinary vendor wire (55 channels), yet the mainstream third-party
decoder rejects frames carrying them outright.

## Tests, benchmarks, fuzzing

- The **vendor corpus** (`tests/vendor_corpus/`, 240 streams produced
  by the vendor's own encoder from deterministic inputs, each with the
  SHA-256 the vendor decoder's output must hash to) is decoded in CI
  through the public entry points: streams outside a `KNOWN_GAPS`
  table must match, streams inside it must still miss (the table only
  shrinks by fixing the decoder), the scorecard is pinned (187/188
  byte-exact via `decode_frame`, 42/52 vendor-lossy via
  `decode_frame_vendor_layout`, 188/188 byte-exact via the latter),
  and every vendored input encodes through `encode_frame`,
  self-roundtrips, and is counted against the vendor's bytes
  (floor 45/179 identical). Per-stream pins on the vendor's bytes
  also discriminate the round-8 errata: Rule B vs Rule A on 12
  RGB-family + 3 YV12 streams, the lazy RLE decode consuming exactly
  the u32 on all 52 arith+RLE channels, and the pow2 normaliser vs a
  raw-total model on all 189 non-power-of-two channels.
  `examples/vendor_corpus.rs` runs the same comparison against the
  docs staging with per-byte diffs and an encoder-side per-channel
  report.
- Unit + roundtrip tests cover every frame type and the predictor
  rules; cross-decoder pins (captured from a black-box binary oracle)
  exercise the modern RGB(A) paths byte-exactly without that oracle in
  CI. The header-`0x01..0x03` u32 length-field dispatch boundary
  (`spec/06` §1.4) is pinned at its exact edge values — `< n_pixels`
  takes call site A (pre-RLE length, prefix at byte 5); `>= n_pixels`
  diverts to the header-`0x00` Fibonacci fall-back; a `0` length field
  surfaces a clean `Error::Truncated`.
- The **round-451 black-box capture matrix**
  (`examples/blackbox_capture.rs`, out of CI) muxes every emittable
  frame type into minimal `LAGS` AVIs and diffs the third-party
  oracle's raw output against the crate's own decode, classifying
  each case Exact / oracle-unsupported / documented-YUY2-gap;
  `tests/blackbox_encode_pins.rs` re-derives the same 26 streams in
  CI and pins their bytes by FNV-1a-64 hash plus self-roundtrip, so
  the oracle-validated wire cannot drift between captures.
- An **exhaustive encoder → decoder self-roundtrip matrix**
  (`encoder_exhaustive_matrix`) drives every encodable colour family
  through a full cross-product of *dimensions* (spanning the
  `width % 4` type-2/type-4 split, even/odd, power-of-two vs
  non-power-of-two plane pixel counts, and 1-row / 1-col edges) ×
  *data pattern* (random, gradient, zero-heavy, constant, two-symbol,
  sparse-impulse, stripe), asserting byte-exact recovery of the input
  on every cell (type 11 asserts fixed-point idempotence, since its
  downsample→upscale is lossy by construction). A capstone coverage
  test proves **all nine** legal modern channel-header sub-forms —
  `0x00`, `0x01`/`0x02`/`0x03`, `0x04`, `0x05`/`0x06`/`0x07`, `0xff` —
  are independently encodable and byte-exact-decodable, so "every wire
  type the decoder accepts is encodable" is a machine-checked
  invariant.
- A **decode-determinism property suite** pins that `decode_frame` is a
  pure function of its inputs: every modern family decodes
  byte-identically on a repeat call; 600 arbitrary/corrupt payloads ×
  all four host pixel formats each return the *same* `Result` on repeat
  (same `Ok` bytes or same `Err` variant, complementing the
  panic-freedom fuzz suites); and 64 consecutive NULL ("JUMP") frames
  through the stateful `Decoder` replay the keyframe with zero
  accumulated drift (`spec/01` §1.1).
- Two `libFuzzer`-style harnesses guard robustness from both ends. The
  decode-side harness in `fuzz/` (`cargo-fuzz`) asserts panic-freedom
  on attacker-supplied payloads through both `decode_frame` and
  `decode_frame_vendor_layout` (which must agree on `Ok`/`Err`, on
  output length, and byte-for-byte off the quirk geometries) plus the
  stateful NULL replay, seeded with 220 vendor-encoded streams — the
  modern range coder rejects a
  malformed probability total exceeding the working `range`
  (per-symbol quotient → 0) as `Error::ProbabilityTotalExceedsRange`
  rather than dividing by zero (`spec/02` §5 / `spec/04` §5). Its
  dimension selectors map onto `1..=64` at **both parities**, so the
  decoder's documented odd-dimension branches are in-corpus: the YV12
  `floor(W·H/4) != (W/2)·(H/2)` SPECGAP single-row chroma fallback
  (`spec/03` §6.1.1) and the YUY2 odd-width luma-tail / `0x80`
  neutral-chroma slot (`spec/03` §6.2). Because the fuzz binary is
  out-of-CI, the in-crate panic-freedom sweeps additionally drive the
  YV12/YUY2 dispatchers across a mixed-parity shape set (down to the
  degenerate single-pixel `1×1` edge with empty chroma planes), so the
  same odd-dimension geometry is panic-free-checked under CI. The
  encode-side counterpart (`encoder_fuzz_harness`, in-crate so it runs
  under CI) runs a deterministic-LCG high-iteration loop
  over the encoder's *input* space (random legal dimensions × a 4-level
  content-entropy knob): 1900 encode→decode roundtrips that must each
  neither panic nor diverge from byte-exact recovery, with failures
  reproducible from the printed `(family, w, h, content_seed)` tuple.
  A round-432 extension drives the public `encode_frame` over large
  planes (10k–30k pixels/plane) with flat-with-impulses and gradient
  content, so the downscale election, the candidate gate, and
  multi-escape full-capacity zero runs are all fuzz-covered, and
  three frozen scorer-decision pins alarm on cost-model drift.
- Criterion benchmarks in `benches/decode.rs` time the decode hot path,
  and a SIMD-vs-scalar predictor bench tracks the `spec/06` §3.2 path.
  `benches/encode.rs` is the encode-side counterpart: it times the
  public `encode_frame` across every host pixel family (rgb24 / rgba /
  yv12 / yuy2) on deterministically synthesised gradient+noise input,
  asserting an encode→decode round-trip before timing each case.
- Standalone profiling drivers in `examples/profile_decode.rs` and
  `examples/profile_encode.rs` loop the decode / encode hot path
  (a type-4 modern-arithmetic RGB24 frame) in a tight, harness-free
  loop so an external profiler (`perf`, `callgrind`, Instruments,
  `samply`) can attach with clean symbol attribution. Iteration count
  is a CLI arg (decode default 200k, encode default 50k); inputs are
  self-contained (the decode fixture is byte-identical to the bench's;
  the encode input is synthesised deterministically) and read no files.
  Run with `cargo run --release --example profile_decode -- <iters>`
  or `... --example profile_encode -- <iters>`.

### Decode coverage and the remaining cross-encoder-parity gap

Every documented colour mode decodes **sample-exactly** across the
fixture class — RGB24 (types 2 / 4), RGBA (8), YV12 (10),
reduced-resolution YV12 (11), YUY2 (3) and legacy RGB (7) — at both
power-of-two and non-power-of-two plane pixel counts. The round-338
`milestone_*` tests pin the non-pow2 sample-exact decode of every mode
as a single regression. Round 352 closes the **YV12 odd-dimension
SPECGAP** path on the encode side: when
`floor(W·H/4) != (W/2)·(H/2)` both `encode_arith_yv12` and
`decode_arith_yv12` fall through to the `spec/03` §6.1.1 single-row
chroma placeholder geometry, and the `arith_yv12_odd_dims_specgap
_roundtrip` test pins that the two halves use the identical breakdown
so the path self-roundtrips byte-exactly even though the per-row
chroma layout itself is a host-integration placeholder.

**Round 407 lands the recovered `lagarith.dll!0x180001050` model
normalizer** (`provenance/52`, closing `spec/02` §9 item 1 / `spec/04`
§9 item 2). The wire carries a raw byte-histogram table whose total is
the per-channel symbol count (`spec/04` §5, the `audit/01` §3.2
validation correction), but the reference never codes against it
directly: the recovered helper forces the model total to the smallest
power of two `>=` the raw sum (IEEE-754-double floor-rescale +
cumulative-sum correction over the nonzero low-128 slots, cursor
masked `& 0x7f`) and publishes `shift = log2(pow2)`, so the per-symbol
quotient is an exact `q = range >> shift` at **every** total.
`src/model.rs` implements the recovery and both coder directions build
their model through it (`Cdf::from_wire_frequencies`). At power-of-two
totals the normalizer is the identity and the coder is bit-identical
to the previous exact-division form — every oracle-captured
cross-decoder pin (`tests/reference_pins.rs`) holds unchanged — while
non-pow2 totals now follow the reference derivation. The round-398
`tables::recip_lut()` characterisation (naive reciprocal-multiply ==
exact division **iff** the total is a power of two) is exactly the
regime the normalizer guarantees, so LUT, division, and shift coincide
on every conformant model. The recovery also *explains* the round-127
pattern-sensitivity finding: structured residuals route channels to
the pre-RLE arithmetic headers whose totals are non-pow2 even at pow2
pixel counts, which is where the oracle (normalizing) and the crate's
old raw-total model diverged.

**Round 451 closes the cross-decoder re-capture** the round-407
normalizer had left open: the black-box oracle now reconstructs every
modern RGB(A)/YV12 stream the encoder emits byte-exactly, including
the structured-pattern and non-pow2-total classes (see Status). Two
further wire-semantics recoveries made that possible, both flagged as
spec erratum candidates in the round report: the modern range coder's
**top-symbol (0xff) slack-absorbing interval** (`spec/02` §5 Step B's
recovered `total·q` threshold leaves the documented "0xff fast path"
unreachable; the boundary is `cum[255]·q`) and the **Yuv first-column
predictor rule** for the 4:2:x families (`spec/06` §3.8 / §6.4).

Still open:

* **Odd-width YUY2 third-party validation**: the vendor encoder
  rejects `W % 4 != 0` YUY2 input and the third-party oracle rejects
  odd-width YUY2 frames outright, so the crate's `spec/03` §6.2
  floor-chroma odd-width form remains validated by self-roundtrip
  only.
* **24-bpp DIB pad bytes at `W >= 4`**: the vendor decoder's row-end
  vector store leaves values in the pad bytes of `W % 4 != 0`, `W >= 4`
  RGB24 rows that the docs did not capture; the 10 remaining
  vendor-lossy misses (`rgb24-5x7-*`, `rgb24-33x27-*`) differ only
  there (`spec/06` §3.2 step 1 validated note).
* **Vendor header heuristic**: the vendor's RLE-header choice
  (`spec/05` §9 item 2, out of the spec's scope) and this crate's
  transmitted-table downscale election account for every remaining
  encoder-side byte difference; no frame is larger than the vendor's.

Closed in round 459: the `spec/06` §5 header-`0xff` semantics (residual
plane, normative) and the standing proprietary-encoded-fixture item
(the vendor corpus is staged and pinned).

## License

MIT — see [LICENSE](LICENSE).
