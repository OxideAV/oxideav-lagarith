# oxideav-lagarith — bundled lookup tables

This directory mirrors the cleanroom-workspace extraction artefacts
at `docs/video/lagarith/tables/` so the crate compiles without a
checked-out workspace tree (CI builds the crate in isolation, per
`oxideav-magicyuv`'s pattern).

The source-of-truth extractor is `extract-luts.sh` in the workspace's
docs tree; both files here are byte-identical copies of that
script's output for binary SHA-256
`f90f371146ee0b762e4d0c95b336cad2dcff8802953e8667ed0b16389b1dd89c`
(x86-64 `lagarith.dll`).

## Files

- `01-residual-rle-decoder-lut.csv` — 256 × u32 forward LUT used by
  the decoder. `LUT[i] = 2*i` for `i < 128`, `511 - 2*i` for
  `i >= 128` (`spec/05` §3.2). Loaded at module-init by
  `tables::rle_fwd_lut()`.
- `02-residual-rle-encoder-inv-lut.csv` — 256 × u8 inverse LUT used
  by the test-only encoder. `LUT[INV_LUT[k]] = k - 2` for `k >= 2`,
  with `INV_LUT[0..=2] = 0` (`spec/05` §5.2). Loaded by
  `tables::rle_inv_lut()`.

The `tables` module asserts the algebraic relations above as unit
tests; if a future re-extraction would change the bytes the test
suite catches it.
