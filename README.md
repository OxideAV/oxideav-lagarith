# oxideav-lagarith

Pure-Rust [Lagarith](https://lags.leetcode.net) lossless video decoder for the
[oxideav](https://github.com/OxideAV) framework.

Lagarith ("LAGS") is Ben Greenwood's mid-2000s lossless video codec, designed
as a successor to Huffyuv with a range coder replacing the Huffman stage.
Streams are AVI-wrapped and each frame is intra-only.

## Status

Reverse-engineered clean-room from the behavioural trace at
`docs/video/lagarith/lagarith-trace-reverse-engineering.md`. No upstream
codec source was consulted.

### Implemented

- Frame-header parser for 3-plane and 4-plane variants.
- Frame-type dispatcher (`0x01`..`0x0b`).
- `SOLID_GRAY` / `SOLID_COLOR` / `SOLID_RGBA` (frame types `0x05` /
  `0x06` / `0x09`) — bit-exact decode against ffmpeg.
- Per-plane `SOLID_PLANE` (`esc_count == 0xff`) shortcut.
- Per-plane `UNCOMPRESSED` (`esc_count == 4`) plane reader.
- Median predictor with the 9-bit-gradient quirk and per-row
  bootstrap (left-only on row 0, RGB-row-1 collapse, full median
  thereafter).
- RGB cross-plane recombination (`R += G; B += G;` per row, alpha
  untouched).
- Bottom-up emission of packed `Rgb24` / `Rgba` for RGB(A) frames
  (Lagarith stores RGB rows bottom-up).

### Not implemented

The 53-entry sparse VLC for probability magnitudes and the 256-entry
probability array used by the range coder are part of the codec
specification and are intentionally **not** transcribed in the trace
doc — they would require reading the upstream decoder source. Range-
coded plane modes (`esc_count` ∈ `{1,2,3}`) and the zero-run-only
modes (`{5,6,7}`) therefore return `Error::Unsupported`. Once those
tables are in the docs, range-coded RGB / RGBA / YV12 / YUY2 decode
can land on top of the existing predictor + cross-plane pipeline.

The RAW (`0x01`), U_RGB24 (`0x02`), OLD_ARITH_RGB (`0x07`), and
REDUCED_RES (`0x0b`) frame types are also stubbed.

## Pixel formats

Lagarith produces packed RGB output (the trace doc's `gbrp` /
`gbrap` is FFmpeg's planar internal layout — we re-pack to the
canonical `Rgb24` / `Rgba` `oxideav_core::PixelFormat` variants):

| Frame type      | Output `PixelFormat` |
|-----------------|----------------------|
| `ARITH_RGB24`   | `Rgb24`              |
| `U_RGB24`       | `Rgb24`              |
| `SOLID_GRAY`    | `Rgb24`              |
| `SOLID_COLOR`   | `Rgb24`              |
| `ARITH_RGBA`    | `Rgba`               |
| `SOLID_RGBA`    | `Rgba`               |
| `ARITH_YV12`    | `Yuv420P`            |
| `ARITH_YUY2`    | `Yuv422P`            |

## License

MIT — see [LICENSE](LICENSE).
