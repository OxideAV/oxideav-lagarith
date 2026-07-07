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

- `00-rangecoder-reciprocal-multiply-lut.csv` — 2048 × u32
  reciprocal-multiply table used by the modern range coder's generic
  symbol-search path (`spec/02` §5 step C). Numerically
  `LUT[i] = floor(2^32 / i)` for `i >= 2`; `LUT[0] = 0` and
  `LUT[1] = 0xffffffff` (the exact `2^32` overflows a `u32`). Loaded
  at module-init by `tables::recip_lut()`. The crate's own decoder
  does **not** consult this table — it runs the `spec/02` §5
  invariant-box cumulative search with exact `q = range / total`
  instead — but the table is bundled and characterised here because
  its numeric form pins **why** the crate's byte-exact cross-decoder
  pins (`tests/reference_pins.rs`) are held to power-of-two pixel
  counts: a naive reciprocal-multiply `(range * LUT[total]) >> 32`
  coincides with exact division only when `total` is a power of two.
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
