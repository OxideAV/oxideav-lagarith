# oxideav-lagarith

Pure-Rust Lagarith lossless video codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework.

## Status

**Rounds 1+2 — arithmetic-coded RGB+YV12 + NULL-frame replay.** This
`master` branch is the clean-room rebuild against the strict-
isolation cleanroom workspace at
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
| 3 — Arithmetic-YUY2 | packed→planar | deferred |
| 4 — Arithmetic-RGB24 / RGB32 | arithmetic, `width % 4 == 0` | 1 |
| 5 — Solid Grey | byte fill | 1 |
| 6 — Solid RGB | three-byte fill | 1 |
| 7 — Legacy RGB (decode-only) | pre-1.1.0 prob format | deferred |
| 8 — Arithmetic-RGBA | four planes incl. alpha | 1 |
| 9 — Solid RGBA | four-byte fill | 1 |
| 10 — Arithmetic-YV12 | three-plane Y/V/U | **2** |
| 11 — Reduced-resolution | type 10 + 2× upscale | deferred |
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
   left predictor on row 0; JPEG-LS clamped median on rows ≥ 1
   with the `TL = L = plane[y-1][W-1]` first-column rule.
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

## Tests

55 unit + integration tests cover the range coder, Fibonacci
prefix, RLE escape, predictor + decorrelation, channel-header
dispatcher, the channel-header `0x01..=0x03` arithmetic-with-RLE
path and the `0x05..=0x07` raw-RLE path, an end-to-end encode →
decode round-trip for each of types 1, 2, 4, 5, 6, 8, 9, 10
(round 2 YV12), and the NULL-frame ("JUMP") replay path through
both the `Decoder` wrapper and the `decode_frame_with_prev` helper.
