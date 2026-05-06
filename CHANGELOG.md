# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.0.2](https://github.com/OxideAV/oxideav-lagarith/compare/v0.0.1...v0.0.2) - 2026-05-06

### Other

- prepend retirement notice (docs audit 2026-05-06)

## [0.0.1](https://github.com/OxideAV/oxideav-lagarith/compare/v0.0.0...v0.0.1) - 2026-05-03

### Other

- replace never-match regex with semver_check = false

- Initial scaffold:
  - Frame-header parser (1-byte frametype + 4 / 8 / 12-byte plane offset
    table).
  - Frame-type dispatcher (`0x01`..`0x0b`).
  - `SOLID_GRAY` (0x05), `SOLID_COLOR` (0x06), `SOLID_RGBA` (0x09)
    constant-frame paths — bit-exact decode.
  - Per-plane `SOLID_PLANE` (`esc_count == 0xff`) shortcut and per-plane
    `UNCOMPRESSED` (`esc_count == 4`) reader.
  - Inverse median predictor with the 9-bit-gradient quirk and per-row
    bootstrap (RGB row-1 collapse, YUV row-1 with `TL = row0[0]`).
  - RGB cross-plane recombination (`R += G; B += G;` per row, alpha
    untouched).
  - Bottom-up emission to packed `Rgb24` / `Rgba`.
- Codec registry hook: `register(&mut CodecRegistry)` claims AVI FOURCC
  `LAGS`.
- Range-coded entropy (`esc_count` ∈ `{1,2,3,5,6,7}`) returns
  `Error::Unsupported`. The 53-entry probability-magnitude VLC and the
  256-entry probability-rescale array are not yet in the trace doc and
  cannot be transcribed clean-room without them.
