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
wrapper for NULL "JUMP" frames). The modern RGB(A) paths are
byte-exact-validated against an independent third-party decoder used
strictly as a black-box binary oracle in fixture generation (it never
runs in CI; committed pins carry the captured results).

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
   families (3 / 10 / 11) use **Rule A** unconditionally (their
   chroma-subsampled plane widths are always 4-byte-aligned, so the
   predictor never takes the `width % 4` Rule-B branch — `spec/06` §3.8).
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

Through the framework, `CodecRegistry::first_encoder` yields a
`LagarithEncoder` (`oxideav_core::Encoder`): `send_frame` reassembles
the packed host buffer from a `VideoFrame`'s planes (stride padding
stripped; YV12's three `Y / V / U` planes concatenated), and
`receive_packet` emits one keyframe packet per frame. The host pixel
format is read from `CodecParameters::pixel_format` (`Bgr24`, `Bgra`,
`Yuv420P`, `Yuyv422`); unsupported formats are rejected at encoder
construction.

## Tests, benchmarks, fuzzing

- Unit + roundtrip tests cover every frame type and the predictor
  rules; cross-decoder pins (captured from a black-box binary oracle)
  exercise the modern RGB(A) paths byte-exactly without that oracle in
  CI. The header-`0x01..0x03` u32 length-field dispatch boundary
  (`spec/06` §1.4) is pinned at its exact edge values — `< n_pixels`
  takes call site A (pre-RLE length, prefix at byte 5); `>= n_pixels`
  diverts to the header-`0x00` Fibonacci fall-back; a `0` length field
  surfaces a clean `Error::Truncated`.
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
  (`q = range / total` → 0) as `Error::ProbabilityTotalExceedsRange`
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
  encode-side counterpart (`encoder_fuzz_harness`, in-crate because the
  encoder is test-only) runs a deterministic-LCG high-iteration loop
  over the encoder's *input* space (random legal dimensions × a 4-level
  content-entropy knob): 1900 encode→decode roundtrips that must each
  neither panic nor diverge from byte-exact recovery, with failures
  reproducible from the printed `(family, w, h, content_seed)` tuple.
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
power-of-two and non-power-of-two plane pixel counts. The modern range
coder narrows its interval with `q = range / total_freq` where
`total_freq` is the raw histogram sum (= the per-channel symbol count),
per `spec/02` §5's invariant box and `spec/04` §5 (the `audit/01` §3.2
validation correction: the wire carries a raw byte-histogram table whose
total is the pixel count, not the internal-only 524288-normalised LUT
total). That division is exact for any `total_freq`, so the
`raw-histogram → cumulative-frequency` derivation the proprietary loader
performs at `lagarith.dll!0x180001050` — including its shift exponent —
is fully covered for the **decoder** by `spec/04` §6 + §8 item 2 (the
auxiliary fields are deterministic post-processing of the raw freq[]
array) combined with `spec/02` §5's cumulative-search equivalent. The
round-338 `milestone_*` tests pin the non-pow2 sample-exact decode of
every mode as a single regression. Round 352 closes the **YV12
odd-dimension SPECGAP** path on the encode side: when
`floor(W·H/4) != (W/2)·(H/2)` both `encode_arith_yv12` and
`decode_arith_yv12` fall through to the `spec/03` §6.1.1 single-row
chroma placeholder geometry, and the `arith_yv12_odd_dims_specgap
_roundtrip` test pins that the two halves use the identical breakdown
so the path self-roundtrips byte-exactly even though the per-row
chroma layout itself is a host-integration placeholder.

What remains open is a clean byte-exact **cross-encoder parity** test
against a *proprietary-encoded* stream. It awaits a fixture (the public
sample set 404s); separately, the reciprocal-multiply quotient the
proprietary loader derives (`spec/02` §5, via the
`.rdata` table now bundled at
`tables/00-rangecoder-reciprocal-multiply-lut.csv`) can differ from the
crate's exact `q = range / total` at non-power-of-two totals. Round 398
makes this **machine-checked** rather than prose: `tables::recip_lut()`
loads the extracted 2048-entry table (numerically `floor(2^32 / i)`),
and three characterisation tests pin that a naive reciprocal-multiply
`(range * LUT[total]) >> 32` equals exact division across the operating
band `[2^23 + 1, 2^31]` **iff** `total` is a power of two, and diverges
by exactly 1 at every non-power-of-two total. This is why the byte-exact
cross-decoder pins (`tests/reference_pins.rs`) are held to power-of-two
pixel counts, and why the *exact* reference quotient at a
non-power-of-two total remains an open item (the `0x180001050`
cumulative-sum / shift derivation is not covered by the wire-format
spec — `spec/02` §9 item 1, `spec/04` §9 item 2). Matching the
proprietary **encoder** byte-for-byte on structured residuals is
therefore blocked on that open derivation *and* a proprietary-encoded
fixture; it is not a decode-spec gap. The crate's own encode→decode
round-trips all such streams byte-exactly.

## License

MIT — see [LICENSE](LICENSE).
