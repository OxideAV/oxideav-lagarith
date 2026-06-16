# oxideav-lagarith

Pure-Rust Lagarith lossless video codec for the
[oxideav](https://github.com/OxideAV/oxideav-workspace) framework. Built
clean-room from the specification and trace documents under
`docs/video/lagarith/` only.

## Status

The decoder handles **every Lagarith frame type**, and a test-side
encoder mirrors the modern arithmetic paths. Decode is stateless per
frame (with a stateful wrapper for NULL "JUMP" frames). The modern
RGB(A) paths are byte-exact-validated against an independent third-party
decoder used strictly as a black-box binary oracle in fixture
generation (it never runs in CI; committed pins carry the captured
results).

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
`registry` Cargo feature and claims the `LAGS` FOURCC.

## Tests, benchmarks, fuzzing

- Unit + roundtrip tests cover every frame type and the predictor
  rules; cross-decoder pins (captured from a black-box binary oracle)
  exercise the modern RGB(A) paths byte-exactly without that oracle in
  CI. The header-`0x01..0x03` u32 length-field dispatch boundary
  (`spec/06` §1.4) is pinned at its exact edge values — `< n_pixels`
  takes call site A (pre-RLE length, prefix at byte 5); `>= n_pixels`
  diverts to the header-`0x00` Fibonacci fall-back; a `0` length field
  surfaces a clean `Error::Truncated`.
- A `libFuzzer` harness in `fuzz/` asserts panic-freedom on
  attacker-supplied payloads. The modern range coder rejects a
  malformed probability total that exceeds the working `range`
  (`q = range / total` → 0) as `Error::ProbabilityTotalExceedsRange`
  rather than dividing by zero (`spec/02` §5 / `spec/04` §5).
- Criterion benchmarks in `benches/decode.rs` time the decode hot path,
  and a SIMD-vs-scalar predictor bench tracks the `spec/06` §3.2 path.

### Known divergences from a byte-exact third-party-encoded oracle

A clean byte-exact parity test against a *proprietary-encoded* stream
awaits a fixture (the public sample set 404s). A residual normalisation
step in the raw-histogram → cumulative-frequency conversion is not yet
disassembled into clean-room spec, which is the remaining blocker for
parity on certain power-of-two sizes; the crate's own decoder
self-roundtrips those streams byte-exactly. Closing the gap is an
extraction-round deliverable.

## License

MIT — see [LICENSE](LICENSE).
