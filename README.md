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

## Tests

114 unit + integration tests cover the range coder, Fibonacci
prefix, RLE escape, predictor + decorrelation, channel-header
dispatcher, the channel-header `0x01..=0x03` arithmetic-with-RLE
path and the `0x05..=0x07` raw-RLE path, an end-to-end encode →
decode round-trip for each of types 1, 2, 3 (round 3 YUY2), 4,
5, 6, 7 (round 4 legacy RGB; round 5 Rule B + RLE-then-Fibonacci
channel sub-path), 8, 9, 10 (round 2 YV12), 11 (round 3
reduced-resolution), the legacy adaptive-CDF range coder + its
Fibonacci freq-table primitives (round 4), the audit/12
rare-symbol-cluster signature detector (round 5; round-6 encoder
Strategy E + round-7 decoder defensive harness via the new
`Error::LegacyRareSymbolClusterUnsupported` variant), the YUY2
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
