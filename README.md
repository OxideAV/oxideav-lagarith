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

**Round 451 lands third-party decodability of our encoded streams.**
A black-box capture harness (`examples/blackbox_capture.rs`) drives a
27-case deterministic matrix — every emittable frame type × content
class × dimension parity — through an independent third-party decoder
used strictly as a black-box binary oracle (never in CI): **all
17 RGB24 / RGB32 / RGBA / YV12 cases decode sample-exactly** in that
oracle (arithmetic types 2 / 4 / 8 / 10, solids 5 / 6 / 9,
downscale-elected tables, unaligned widths, non-power-of-two totals,
and the round-127 "structured pattern" class whose re-capture had been
the standing open item). Types 1 / 7 / 11 and the NULL payload are
rejected by that oracle build before decode (unsupported there);
YUY2 remains a documented partial (`spec/06` §6.4). CI freezes all 26
captured streams by hash (`tests/blackbox_encode_pins.rs`). Getting
here surfaced and fixed two wire-semantics divergences the
self-roundtrip suites could never see — the range coder's top-symbol
slack interval and the YUV-family first-column predictor rule (see
below) — and restricted the per-channel election to the
cross-validated header set.

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
2. **Per-channel header dispatcher** (`spec/03` §2.1 + `spec/06` §1).
3. **Fibonacci probability prefix** (`spec/04`) — MSB-first Zeckendorf
   decode of the 256-entry frequency table with the zero-run subcode.
4. **Modern range coder** (`spec/02`) — TOP = 2^23, init range = 2^31,
   four-byte priming + flush, byte refill with cross-byte LSB rotation.
5. **Residual zero-run RLE escape** (`spec/05`) — `escape_len +
   LUT[supplement_byte]` zero runs.
6. **Spatial predictor** (`spec/03` §3) — left predictor on row 0,
   JPEG-LS clamped median on rows ≥ 1. The modern RGB(A) types (2 / 4 /
   8) and the legacy type-7 path use the **Rule B** first-column rule
   (`TL = plane[y-2][W-1]`), while the YV12 / YUY2 / reduced-resolution
   families (3 / 10 / 11) use the round-451 oracle-recovered **Yuv**
   rule — row 1 predicts `L = plane[0][W-1]` (the `0x180009f30`
   carry enters the row holding `T`, so `MED(L, T, T) = L`), rows
   ≥ 2 take the Rule-B median. This replaces `spec/06` §3.8's
   "Strategy A everywhere" reading (flagged as an erratum candidate)
   and closes the §6.4 open item for YV12: the black-box oracle
   reconstructs YV12 frames byte-exactly under this rule at every
   probed geometry/content class, and under no other candidate.
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

Since round 451 the per-channel header election stays within the
**cross-validated form set** `{0x00, 0x01..0x03, 0x04,
0xff-zero-fill}`: the raw+RLE forms (`0x05..0x07`) and the
nonzero-constant fill decode divergently (or not at all) in the
black-box oracle, and the docs' encoder-mirror sections document
vendor emission only for `0x00..0x03` — every form remains decodable
and directly encodable (`encode_channel_raw_rle`), the automatic
election just never emits a wire whose third-party decode is
unconfirmed.

## Tests, benchmarks, fuzzing

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
  on attacker-supplied payloads — the modern range coder rejects a
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

* **YUY2 luma-path carry semantics** (`spec/06` §6.4, now the last
  predictor gap): black-box probes additionally show a raw second
  row-0 luma sample, an 8-bit-wrapping median gradient, and a
  zeroed-TL first-chunk region on row 1; a candidate model matching
  gradient/zero-heavy content exactly still diverges on full-random
  content, so the recovery is incomplete and the crate deliberately
  ships only the YV12-confirmed rule. Needs the §6.4 byte-walk of the
  YUY2 coordinator/predictor (`0x180004ec0` / `0x180009f30`).
* **`spec/06` §5 header-`0xff` semantics**: the oracle fills the
  plane with no predictor pass, the checklist's step 8 implies one;
  the encoder sidesteps the conflict (zero fill only) until a
  vendor-encoded fixture or a re-derivation arbitrates.
* **Proprietary-encoded fixture** — still the one artefact that would
  arbitrate every remaining conflict directly against the vendor
  bitstream. Per the docs staging of 2026-08-10 this is resolved as
  an **operator upload** to the project fixture host (no research
  path remains). The i386 `fistp` rounding caveat that fixture was
  once needed for is now settled statically (audit/15, 2026-07-31):
  both official builds truncate, so the rescale + residue path is
  architecture-independent and the crate's x86-64-truncation model
  covers both.

## License

MIT — see [LICENSE](LICENSE).
