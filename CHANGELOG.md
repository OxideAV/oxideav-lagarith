# Changelog

All notable changes to this crate are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and
versioning follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- round 335 — **profiling driver `examples/profile_decode.rs`
  (depth-mode "profile" deliverable).** The crate is decode-saturated;
  the one depth-mode capability not yet present was a *standalone
  runnable* that loops the decode hot path long enough for an external
  sampling/instrumenting profiler (`perf record`, `valgrind
  --tool=callgrind`, macOS Instruments, `samply`, `dtrace`) to attach
  with clean symbol attribution. It complements the r301 Criterion
  bench (statistical timing, but wraps each iteration in a measurement
  harness that pollutes a profiler's view) and the r291 libFuzzer
  target (robustness, but mutates the input so each decode walks a
  different path). `profile_decode` decodes a single fixed, valid
  type-4 modern-arithmetic RGB24 frame (full pipeline: 3-channel
  offset table, channel-header dispatcher, Fibonacci prefix, modern
  range coder, RLE escape, JPEG-LS median predictor, RGB cross-plane
  decorrelation) in a tight, harness-free loop. The iteration count is
  a CLI arg (default 200_000); each decoded buffer is folded into an
  index-mixed accumulating checksum (`black_box`-fenced) so the
  optimiser cannot hoist or elide the decode. Fixture bytes are
  embedded inline (byte-identical to the bench's `FRAME_RGB24_64`,
  captured once from the crate's own reference encoder) — no committed
  fixture files, no `docs/` reads at run time.

### Tested

- round 326 — **pinned the header-`0x01..0x03` u32 length-field
  dispatch boundary (`spec/06` §1.4, closing the §6.2 open item).**
  The channel dispatcher (`channel::decode_channel`) reads the
  little-endian u32 at channel bytes 1..=4 and compares it against the
  plane pixel count: `< n_pixels` selects call site A (the field is a
  pre-RLE symbol-stream length; the Fibonacci prefix starts at byte 5),
  while `>= n_pixels` falls back to call site B (the four bytes are
  re-read as the leading bytes of a header-`0x00` Fibonacci prefix at
  byte 1, no RLE post-process). §6.2 flagged the exact-boundary values
  as documented-in-prose-but-not-cross-tested. Two new tests close
  that: `arith_rle_length_field_dispatch_boundary` round-trips the
  natural `< n_pixels` call-site-A path, then splices the length field
  to exactly `n_pixels` and to `n_pixels + 1` to assert the dispatcher
  diverts onto call site B (panic-free, output never silently equal to
  the RLE result it was diverted away from); and
  `arith_rle_zero_length_field_is_clean_error` confirms a `0` length
  field (call site A with a zero-symbol pre-RLE stream) surfaces a
  clean `Error::Truncated` from the output-driven `spec/05` §4.2 RLE
  expander rather than panicking. Lib unit-test count 311 → **313**.

### Fixed

- round 322 — **divide-by-zero panic in the modern range coder on a
  malformed probability total.** `RangeDecoder::decode_symbol`
  (`spec/02` §5) computes the per-symbol quotient `q = range / total`,
  then the generic Step-C path divides `low / q`. The wire `total`
  comes straight from the Fibonacci probability prefix
  (`fibonacci::decode_freq_table` → `Cdf::from_frequencies`), which
  accepts totals up to `u32::MAX` by design. A malformed prefix
  encoding a `total` greater than the working `range` (range ∈
  `[2^23 + 1, 2^31]` per `spec/02` §2) makes `q == 0`, so `low / q`
  panicked — a fuzzer-reachable crash in the live decode hot path
  (`channel.rs` header-`0x00`/`0x01..0x03` arithmetic bodies). The
  decoder now checks `q == 0` up front and surfaces the new
  `Error::ProbabilityTotalExceedsRange` wire error, restoring the
  panic-freedom guarantee. The `spec/04` §5 validation correction
  establishes the legitimate wire total = per-channel symbol count
  (≤ pixel count ≤ `range`), so the guard never false-triggers on a
  well-formed frame — proven by the new `decode_symbol_accepts_total_at_cap`
  test (`total == TOP` roundtrips cleanly) alongside
  `decode_symbol_rejects_total_above_range` (the panic regression).
  Lib unit-test count 309 → **311**.

### Added

- round 310 — **typed `FrameType::wire_plane_roles()` accessor +
  `WirePlaneRole` enum.** Extends the public `FrameType` enum (rounds
  242 / 253 / 261 / 262 / 308) with a structural accessor returning
  the semantic role of each decoded plane in **wire plane order** —
  the single source of truth for the channel-offset table's slot →
  channel mapping (`spec/01` §2.3) combined with the per-family plane
  ordering (`spec/03` §6.1 / §6.2 + §4). Returns
  `Some([Red, Green, Blue])` for the three packed-RGB families (2 / 4
  / 7), `Some([Red, Green, Blue, Alpha])` for `ArithmeticRgba` (8),
  `Some([Luma, ChromaV, ChromaU])` for `ArithmeticYv12` (10) and
  `ReducedResYv12` (11 — half-resolution YV12 body per `spec/01`
  §2.4), `Some([Luma, ChromaU, ChromaV])` for `ArithmeticYuy2` (3 —
  U before V on the wire), and `None` for the literal types
  (uncompressed + the three solid types) which carry no channel-offset
  table. The slice lines up element-for-element with `split_channels`
  output and complements round 308's `wire_plane_pixel_counts`
  (per-plane counts) by naming each plane's role. New public
  `WirePlaneRole` enum (`Red` / `Green` / `Blue` / `Alpha` / `Luma`
  / `ChromaU` / `ChromaV`) exported from the crate root. No
  wire-format or decode-behaviour change. 3 new unit tests pin the
  exact per-type role sequence
  (`wire_plane_roles_exact_sequence_per_type`), the
  `is_some() ⟺ is_arithmetic` + `roles.len() == n_channels` invariant
  (`wire_plane_roles_length_matches_n_channels`), and the RGB(A)
  decorrelation-pivot structure — Green at wire plane 1, Alpha last
  iff `has_alpha_plane`, YUV families carry only luma/chroma roles
  (`wire_plane_roles_rgb_family_structure`). Brings the lib unit-test
  count from 306 to **309**.

- round 301 — **Criterion decode benchmark harness
  (`benches/decode.rs`).** The crate is decode-saturated, so per the
  workspace "saturated → fuzz / bench / profile" cadence this is a
  depth-mode addition: a `cargo bench --bench decode` harness that
  feeds the public `decode_frame` one representative 64×64 compressed
  frame of each major pipeline (type-4 RGB24, type-8 RGBA, type-10
  YV12, type-3 YUY2, type-7 legacy, type-6 solid, type-1
  uncompressed). The frames are real reference-encoder output
  captured once and embedded inline as `const` byte arrays (no
  committed fixture files, no `docs/` reads at bench time); a
  pre-timing `is_ok()` assertion guards each embed. `criterion`
  joins as a `[dev-dependencies]` pin (`0.5`, matching the other
  benched OxideAV crates). No production code changes.

### Changed

- round 301 — README **stale-prose correction**: the SIMD-vs-scalar
  predictor section now states the packed-RGB(A) Rule-B choice on the
  authority of the binary-resolved `spec/07` §9.1 item 7b (which
  overrides the `spec/06` §3 narrative per the workspace
  Resolved-by-binary convention), and the YV12 / YUY2 /
  reduced-resolution families are documented as **Rule A
  unconditionally** (resolved round 295 from `spec/06` §3.8), not
  "pending a clean pin". The matching open-item tail entry is updated
  to mark the predictor-rule question resolved.

- round 295 — **YV12 / YUY2 / reduced-res first-column predictor
  rule spec-anchored to Rule A (`spec/06` §3.8).** The YV12 (type
  10), YUY2 (type 3), and reduced-resolution (type 11) decode paths
  — and their test-side encoder mirrors — now select
  `FirstColRule::A` (`TL = L = plane[y-1][W-1]`) **explicitly** at
  every plane-predictor call site, anchored in `spec/06` §3.8: the
  predictor at `lagarith.dll!0x180009f30` for those families takes
  the `TL = L` carry unconditionally because their
  chroma-subsampled plane widths are always 4-byte-aligned at the
  natural subsampling (no `width % 4` Rule-B branch). Previously the
  call sites relied on the bare `apply_plane_inverse` default, whose
  doc comment wrongly claimed Rule A was "not used by any shipping
  path"; this resolves the README's "YV12/YUY2 retain Rule A pending
  a clean ffmpeg pin" note from the spec rather than deferring it to
  a black-box oracle. Behaviour is unchanged (the bare wrapper
  already applied Rule A). The bare `apply_plane_inverse` /
  `apply_plane_forward` wrappers are now `#[cfg(test)]`-only. New
  `rule_a_yuv_first_column_inverts_itself` test pins the §3.8
  invariant on a multi-row plane where Rule A and Rule B diverge:
  Rule-A residuals round-trip losslessly and decoding them under
  Rule B corrupts rows ≥ 2.

### Fixed

- round 291 — **decode panic on overflowing probability table
  (fuzz finding).** A malformed Fibonacci probability prefix could
  decode per-symbol frequencies whose cumulative sum overflowed
  `u32`, hitting an `.expect("prob table overflow")` in
  `Cdf::from_frequencies` (`range_coder.rs`) and aborting the
  decode. The build now returns the new
  `Error::ProbabilityTableOverflow` instead of panicking, so a
  hostile frame surfaces a defined error like every other malformed
  input. Pinned by the `from_frequencies_rejects_overflow` and
  `from_frequencies_accepts_u32_max_total` unit tests plus a corpus
  regression seed. Found by the new `decode_lagarith` libFuzzer
  target (below).

### Added

- round 291 — **coverage-guided libFuzzer decode harness (`fuzz/`).**
  New `decode_lagarith` cargo-fuzz target drives arbitrary mutated
  bytes through the public `decode_frame` entry point across every
  wire-type decode path and all four `PixelKind` host formats,
  asserting the decoder always returns (`Ok`/`Err`) without
  panicking, aborting, integer-overflowing, or indexing out of
  bounds. Two leading selector bytes map to small even
  (chroma-safe) dimensions so the budget goes to logic paths rather
  than allocation. Hand-built seed corpus (solid grey/RGB/RGBA,
  uncompressed, NULL, bad-type) plus the overflow regression seed.
  Ran clean for 28.9M executions (300 s, 0 findings) after the fix
  above; a daily `fuzz.yml` workflow keeps it on a 30-minute budget.
  Decode-only by design — the clean-room wall bars any external
  reference codec as a cross-decode oracle.

- round 276 — **encoder frame-level solid-colour fast path
  (`spec/01` §3.1).** New test-side encoder wrappers
  `encode_arith_rgb24_or_solid` / `encode_arith_rgba_or_solid`
  implement the proprietary's solid-colour shortcut: a frame
  whose pixels are all identical is emitted as the 2-byte
  type-5 (Solid-Grey, input pixel's `R == G == B`), 4-byte
  type-6 (Solid-RGB) or 5-byte type-9 (Solid-RGBA) frame
  (`spec/01` §3 rows 5/6/9 + §2.2.2 totals), with the colour
  bytes copied from the input pixel unchanged per the §2.2.1
  encoder mirror (`lagarith.dll!0x180002c8c..0x180002cda` /
  `0x180002f8c..0x180002fc8`). The proprietary gates the
  shortcut on a post-encode compressed-size threshold (`0xf` at
  `lagarith.dll!0x180002c65` on the RGB path, `0x15` at
  `0x180002f7f` on the RGBA path); the wrappers gate on the
  exact-constancy predicate itself — the
  necessary-and-sufficient lossless condition the threshold
  proxies — and emit byte-identical wire on every genuinely
  solid frame. Non-solid input falls through to
  `encode_arith_rgb24` / `encode_arith_rgba` byte-identically;
  the RGBA path has no grey sub-shortcut (constant grey +
  opaque BGRA still emits type 9). 7 new tests
  (`frame_solid_fast_path` module) pin the exact wire shape per
  solid type, decode round-trips at aligned / unaligned / 1×1
  sizes, the grey-vs-RGB split, fall-through byte-identity on
  gradient + almost-solid fixtures, and the never-larger /
  strictly-smaller-on-solid size invariants.

- round 262 — **typed `FrameType` solid-frame wire-size
  accessors.** Extends the public `FrameType` enum (rounds 242 /
  253 / 261) with `solid_colour_byte_count() -> Option<usize>`
  returning the `spec/01` §2.2 bytes-consumed table (`Some(1)` /
  `Some(3)` / `Some(4)` for types 5 / 6 / 9; wire bytes map to
  output pixel positions +0.. per `spec/01` §2.2.1's BGR(A)
  memory convention) and `solid_wire_size() -> Option<usize>`
  returning the `spec/01` §2.2.2 "Solid-frame total payload
  sizes" table (`Some(2)` / `Some(4)` / `Some(5)` — the values
  the proprietary encoder commits to the codec context's
  compressed-size field at `+0x14` via immediate loads of `0x2`
  at `lagarith.dll!0x180002c84`, `0x4` at `0x180002ca3`, and
  `0x5` at `0x180002f88`). Both return `None` on every non-solid
  frame type (their wire size is input-dependent and determined
  by the frame chunk size per `spec/01` §2.1 last paragraph), so
  `is_some()` coincides exactly with `is_solid()`. The decoder's
  solid-frame minimum-length gate (`decode_solid`'s `need`
  value) now reads `FrameType::solid_wire_size`, making the
  typed accessor the single source of truth for the §2.2.2
  table (round-239 pattern). 4 new unit tests pin the per-byte
  values of both accessors across the full 1..=11 byte range +
  the `is_some ⟺ is_solid` coincidence
  (`frame_type_solid_colour_byte_count`), the structural
  identity `solid_wire_size = prefix_size +
  solid_colour_byte_count` on the solid set
  (`frame_type_solid_wire_size_matches_spec_table`), agreement
  with the three solid-frame encoder entry points on output
  length + byte-0 reclassification
  (`frame_type_solid_wire_size_matches_encoder_output_len`),
  and the decoder boundary contract — exactly `solid_wire_size`
  bytes decode, one byte fewer reports `Error::Truncated`
  (`frame_type_solid_wire_size_is_decoder_minimum_length`).
  Brings the total unit-test count from 288 to **292**.
- round 261 — **typed `FrameType::has_alpha_plane` accessor on the
  outermost wire byte.** Extends the public `FrameType` enum
  (rounds 242 / 253) with `has_alpha_plane() -> bool` returning
  `true` exactly for the two RGBA frame types — `ArithmeticRgba`
  (8) and `SolidRgba` (9). Type 8 (`spec/01` §2.3 row 8 +
  `spec/03` §4.3) splits four planes (R / G / B / A) on the wire
  with the alpha plane decoded independently (left predictor on
  row 0, JPEG-LS median on rows ≥ 1) and no cross-plane
  decorrelation against G; type 9 (`spec/01` §2.2 row 9) carries
  exactly four colour bytes (R / G / B / A) replicated to fill
  the host BGRA buffer. Type 1 (Uncompressed) is excluded — its
  wire body is the source pixel buffer in its source layout per
  `spec/01` §2.1, so the presence of an alpha byte is a host-
  format property; the three solid-RGB types (5 grey, 6 RGB)
  are excluded per `spec/03` §4 third bullet ("RGB32 has no
  alpha plane on the wire", filled to the constant `0xff` by
  `lagarith.dll!0x180009486`); types 3 / 10 / 11 are excluded
  per `spec/03` §4.4 (YUY2 / YV12 carry no alpha plane). Mirrors
  `PixelKind::has_alpha` (round 245) on the frame-type axis:
  `PixelKind::has_alpha` reports whether the host buffer reserves
  an alpha byte per pixel, this new accessor reports whether the
  wire form supplies one. 4 new unit tests pin the per-byte
  positive set across the full 1..=11 byte range
  (`frame_type_has_alpha_plane`), structural equivalence with
  `n_channels() == 4` on the arithmetic-type subset
  (`frame_type_has_alpha_plane_implies_four_channels_on_arithmetic_set`),
  the one-direction implication
  `has_alpha_plane => accepts(Bgra32) && Bgra32 ∈ compatible_pixel_kinds`
  (`frame_type_has_alpha_plane_implies_bgra32_compatible`), and
  disjointness with the planar / packed YUV sub-classifiers
  (`frame_type_has_alpha_plane_disjoint_from_yuv_families`).
  Brings the total unit-test count from 284 to **288**.

- round 257 — **typed `LegacyChannelHeader::prefix_size` accessor.**
  Extends the public `LegacyChannelHeader` enum with a structural
  `prefix_size` accessor returning the byte count the legacy
  (type-7) channel-data dispatcher consumes for header / metadata
  fields before the wire body proper begins: `2` for `BareFib`
  (outer header byte at offset 0 + inner codec-mode flag byte at
  offset 1 per `spec/07` §1.3 final paragraph + §2.5 second
  blockquote) and `5` for `RleThenFib` (outer header byte + 4-byte
  LE u32 post-RLE length field at offsets 1..5 per `spec/07` §2.1
  second bullet + §2.3 / §2.4). Equals the existing
  `freq_table_offset` on every legacy variant — every legacy
  channel-header form carries a Fibonacci-coded frequency table
  (directly on `BareFib`; via the post-RLE intermediate buffer on
  `RleThenFib`), so the wire body the dispatcher reads next always
  begins at the freq-table input-byte offset. Mirrors round-250
  `ChannelHeader::prefix_size` on the modern channel-header byte so
  the modern and legacy channel-header wire-form classifiers expose
  the same structural surface, and mirrors `FrameType::prefix_size`
  at the channel level. 3 new unit tests pin the per-byte values
  across the four-element accepted set
  (`legacy_channel_header_prefix_size`), equality of `prefix_size`
  and `freq_table_offset` on every legacy variant
  (`legacy_channel_header_prefix_size_equals_freq_table_offset`),
  and cross-form agreement on the RLE sub-path (modern `ArithRle(h)`
  and legacy `RleThenFib(h)` both report `prefix_size = 5`,
  `legacy_and_modern_prefix_size_agree_on_rle_subpath`). Brings the
  total unit-test count from 281 to **284**.

- round 253 — **typed `FrameType` × `PixelKind` compatibility
  relation accessor.** Extends the public `FrameType` enum with
  `accepts_pixel_kind(PixelKind) -> bool` (the predicate the per-
  frame-type decoders already enforce at function entry) and a
  complementary `compatible_pixel_kinds() -> &'static [PixelKind]`
  slice accessor returning the exact element-wise set of host
  pixel kinds the frame type accepts. Compatibility table is
  anchored in `spec/01` §2.1 (uncompressed accepts all four host
  pixel kinds — `Bgr24` / `Bgra32` / `Yv12` / `Yuy2` — the wire
  body is the host pixel buffer verbatim) + §2.2.1 (the three
  solid types accept `Bgr24` / `Bgra32` only — Windows BI_RGB BGR
  memory order) + §2.3 (the four packed-RGB arithmetic families
  pack into BGR(A) — accept `Bgr24` / `Bgra32`) + §2.4 +
  `spec/03` §6.1 (the YV12 family accepts `Yv12` only — planar Y
  / V / U region concatenation) + `spec/03` §6.2 (YUY2 accepts
  `Yuy2` only — packed Y0 U Y1 V macropixels). Mirrors the round-
  245 `PixelKind` partition on the frame-type axis. 5 new unit
  tests pin the full 11×4 acceptance table per (frame_type,
  pixel_kind) pair (`frame_type_accepts_pixel_kind_table`), non-
  emptiness (every frame type accepts at least one pixel kind —
  no structurally-unreachable frame types,
  `frame_type_accepts_pixel_kind_non_empty`), element-wise
  consistency between the two new accessors
  (`frame_type_accepts_pixel_kind_consistent_with_compatible_set`),
  the exact slice sequence returned by `compatible_pixel_kinds`
  so iteration order is part of the public contract
  (`frame_type_compatible_pixel_kinds_exact_sequence`), and
  alignment with the existing `is_planar_yv12` / `is_packed_yuy2`
  / `is_packed_rgb` / `is_solid` sub-classifiers
  (`frame_type_accepts_pixel_kind_aligns_with_yuv_subclassifiers`).
  Brings the total unit-test count from 276 to **281**.

- round 250 — **typed `ChannelHeader` structural accessors on the
  modern per-plane channel-header byte.** Extends the public
  `ChannelHeader` enum with `freq_table_offset() -> Option<usize>`
  (returns `Some(1)` for `BareArithmetic`, `Some(5)` for `ArithRle`,
  `None` for `Raw` / `RawRle` / `ConstantFill`) per `spec/06` §1.2
  call sites A + B + `spec/03` §2.1, and `prefix_size() -> usize`
  (`1` for every variant whose dispatcher only consumes the header
  byte before the wire body proper begins; `5` for `ArithRle` whose
  prefix carries the 4-byte u32 pre-RLE length field at bytes 1..4).
  Mirrors `LegacyChannelHeader::freq_table_offset` (the modern and
  legacy classifiers now expose the same structural surface) and
  mirrors `FrameType::prefix_size` at the channel level so
  downstream callers can compute byte offsets through both prefix
  layers without re-running the dispatcher. 4 new unit tests pin
  the two accessors per-byte across the full nine-element accepted
  set, consistency between `freq_table_offset` and `prefix_size`,
  and equivalence of `freq_table_offset.is_some()` with the
  existing `uses_arithmetic_body` predicate.

- round 245 — **typed `PixelKind` classification accessors on the
  host-side pixel-format selector.** Extends the existing public
  `PixelKind` enum (`Bgr24` / `Bgra32` / `Yv12` / `Yuy2`) with six
  semantic helpers grounded in `spec/01` §2.2..§2.3 + `spec/03`
  §6.1 / §6.2: `is_rgb_family` (matches `Bgr24` / `Bgra32` — the
  host targets for the modern arithmetic-coded RGB / RGBA families
  and the SOLID-RGB / SOLID-RGBA literals), `is_yuv_family` (matches
  `Yv12` / `Yuy2` — `spec/03` §6.1 / §6.2), `is_packed` (matches
  `Bgr24` / `Bgra32` / `Yuy2` — packed memory order; YUY2 is packed
  at the macropixel level per `spec/03` §6.2), `is_planar` (matches
  `Yv12` only — Y / V / U region concatenation per `spec/03` §6.1.1),
  `has_alpha` (matches `Bgra32` only — the alpha byte the decoder
  either fills with `0xff` for non-RGBA frame types or copies from
  the decoded alpha plane for RGBA frame type 8 / SOLID-RGBA type
  9), and `bytes_per_pixel` (`Some(3)` for `Bgr24`, `Some(4)` for
  `Bgra32`; `None` for the two YUV-family formats — the public
  face of the existing private `packed_bpp` helper). A companion
  `PixelKind::all()` enumerates the four recognised host formats
  in declaration order. The new surface lets downstream callers
  introspect a host-format choice without re-running the per-format
  dispatch in `decode_arith_rgb` / `decode_arith_yv12` /
  `decode_arith_yuy2`, mirroring the round 236 / 239 / 242 pattern
  that exposed the modern channel-header, legacy channel-header,
  and frame-type bytes via typed enums. 12 new unit tests in
  module `decoder::tests` cover (a) the four-element enumeration
  order of `PixelKind::all()`, (b) per-predicate membership for the
  four boolean accessors against the explicit positive sets, (c)
  the color-family partition invariant (`is_rgb_family` +
  `is_yuv_family` is exactly one for every variant), (d) the
  memory-layout partition invariant (`is_packed` + `is_planar` is
  exactly one for every variant), (e) the `bytes_per_pixel` value
  for each variant, (f) `bytes_per_pixel`-implies-`is_packed`
  consistency, (g) `buffer_len(w, h) == w * h * bpp` whenever
  `bytes_per_pixel` returns `Some(bpp)`, (h) `buffer_len` on
  `Yuy2` is `w * h * 2` per `spec/03` §6.2, and (i) `buffer_len`
  on `Yv12` matches the `n + 2 * (n / 4)` formula per `spec/03`
  §6.1.1 across multiple W / H pairs. Brings the total unit-test
  count from 259 to **272**. No wire-format change.

- round 242 — **typed `FrameType` classification accessors on the
  outermost wire byte (frame-type selector).** Extends the existing
  public `FrameType` enum with a `to_byte` round-trip (`from_byte ∘
  to_byte = id` on `1..=11`) plus eleven semantic helpers grounded
  in `spec/01`: `is_uncompressed` (type 1, §2.1), `is_solid`
  (types 5 / 6 / 9, §2.2), `is_arithmetic` (types 2 / 3 / 4 / 7
  / 8 / 10 / 11, §2.3), `is_legacy_decode_only` (type 7 per §3
  row 7 — encoder writes "(none)"), `is_reduced_resolution`
  (type 11, §2.4), `is_planar_yv12` (types 10 / 11), `is_packed_yuy2`
  (type 3), `is_packed_rgb` (types 2 / 4 / 7 / 8), and
  `is_produced_by_v64_encoder` (the eight types §3 enumerates with
  immediate-byte writes: 2 / 3 / 4 / 5 / 6 / 8 / 9 / 10 — excludes
  types 1, 7, 11). Two structural-size accessors expose the §2.3
  channel-offset prefix sizes: `prefix_size` (`1` for literal /
  solid types; `9` for 3-channel arithmetic; `13` for 4-channel
  RGBA arithmetic) and `channel_offset_table_size` (`prefix_size −
  1`). The new surface lets downstream callers introspect a parsed
  frame-type byte without re-running the per-type dispatcher in
  `decoder::decode_frame`, mirroring the round-236 / -239 pattern
  for the modern + legacy channel-header bytes. 13 new unit tests
  in module `frame::tests` cover (a) `to_byte` round-trip closure
  on `1..=11`, (b) the top-level-classes-partition invariant — the
  three uncompressed / solid / arithmetic predicates partition the
  accepted set without overlap or gap — (c) per-predicate
  membership for the seven boolean accessors against the explicit
  positive sets, (d) the arithmetic-sub-classes-partition invariant
  — every arithmetic frame type satisfies exactly one of
  {planar-YV12, packed-YUY2, packed-RGB} — (e) the §3 v64-encoder-
  produced set against its explicit positive + negative sets, (f)
  `prefix_size` + `channel_offset_table_size` match the §2.3 table
  (9 / 13 / 1 for arithmetic-3ch / RGBA-arithmetic / literal), and
  (g) `prefix_size` is consistent with the existing
  `pack_channels` / `split_channels` helpers (a frame packed at the
  canonical type byte has its first channel starting at offset
  `ft.prefix_size()`). Brings the total unit-test count from 246
  to **259**. No wire-format change.

- round 239 — **typed `LegacyChannelHeader` accessor on the legacy
  (type-7, adaptive-CDF RGB) per-plane channel-header byte.** A new
  public enum (`LegacyChannelHeader`) classifies the outer
  channel-header byte at offset 0 of every legacy (frame type 7)
  channel slice per `spec/07` §1.3 + §2.1 into its semantic wire
  form: `BareFib` (`0x00`, 2-byte channel prefix — outer header +
  inner codec-mode flag — followed by a Fibonacci-coded 256-entry
  frequency table at offset 2 and the legacy range-coder body) and
  `RleThenFib { escape_len }` (`0x01..=0x03`, 5-byte channel prefix
  — outer header + u32 LE post-RLE length field — followed by a
  `spec/05` zero-run-RLE-compressed Fibonacci-coded freq table at
  offset 5 with the per-channel escape length equal to the outer
  header byte). The legal set is strictly `{0x00, 0x01, 0x02, 0x03}`
  — disjoint from the modern (`ChannelHeader`) wire form set, which
  also accepts `0x04` raw, `0x05..=0x07` raw-with-RLE, and `0xff`
  constant-fill (`spec/03` §2.1 + `spec/06` §1.1). The surface
  exposes `from_byte`, `to_byte`, `uses_rle_pre_decompress`,
  `rle_escape_len`, and `freq_table_offset` so callers can
  introspect a parsed legacy wire header without re-running the
  dispatcher. The wire-level decoder (`decode_legacy_channel`) now
  classifies through the typed accessor, making it the single
  source of truth for the legal outer-header set so the public
  API and the dispatcher can't drift apart. Three new unit tests
  in module `channel::tests` cover (a) full byte-classification
  for the four accepted bytes including `escape_len` extraction on
  the three RLE-bearing values plus correct `freq_table_offset`
  for both variants, (b) rejection of representative out-of-range
  bytes (`0x04`, `0x05`, `0x06`, `0x07`, `0x08`, `0x10`, `0x7f`,
  `0x80`, `0xfe`, `0xff`) as `Error::BadChannelHeader` —
  explicitly including the modern-only headers — and (c)
  `from_byte` → `to_byte` round-trip closure on every accepted
  byte. Total test count rises from 242 to 245 (registry).

- round 236 — **typed `ChannelHeader` accessor on the modern per-plane
  channel-header byte.** A new public enum classifies the
  channel-header byte at offset 0 of every modern arithmetic-coded
  channel slice (frame types 2, 3, 4, 8, 10, 11) per `spec/03` §2.1 +
  `spec/06` §1.1 into its semantic wire form: `BareArithmetic`
  (`0x00`, Fibonacci prefix + arithmetic body, no RLE), `ArithRle`
  (`0x01..=0x03`, arithmetic body + `spec/05` zero-run RLE
  post-process with `escape_len = header`), `Raw` (`0x04`, literal
  bytes), `RawRle` (`0x05..=0x07`, literal bytes + RLE with
  `escape_len = header - 4`), and `ConstantFill` (`0xff`). The legacy
  (type 7) channel header uses a disjoint, narrower set per
  `spec/07` §1.3 + §2.3 and is **not** covered by this enum (see
  `decode_legacy_channel`). The surface exposes `from_byte`,
  `to_byte`, `uses_arithmetic_body`, `uses_rle_postprocess`, and
  `rle_escape_len` for callers that need to introspect a parsed wire
  header without re-running the dispatcher. Three new unit tests in
  module `channel::tests` cover (a) full byte-classification for the
  nine accepted bytes including escape-length extraction on the two
  RLE-bearing ranges, (b) rejection of representative out-of-range
  bytes (`0x08`, `0x09`, `0x10`, `0x80`, `0xfe`) as
  `Error::BadChannelHeader`, and (c) `from_byte` → `to_byte`
  round-trip closure on every accepted byte. Total test count rises
  from 239 to 242 (registry).

- round 229 — **type-7 (legacy adaptive-CDF RGB) frame-level type-1
  (uncompressed) size guard.** A fifth `*_or_uncompressed` public
  encoder entry point — `encode_legacy_rgb_or_uncompressed` — extends
  the round-222 modern-arithmetic size guard to the legacy fork. The
  wrapper computes both wire forms (`encode_legacy_rgb` + the
  equivalent `encode_uncompressed(pixels)` form per `spec/01` §2.1)
  and returns the shorter, tie-breaking in favour of the legacy form
  so already-compressing inputs stay byte-identical to the existing
  `encode_legacy_rgb` output. The Strategy E (`audit/12` §7.1) rare-
  symbol-cluster diversion already inside `encode_legacy_rgb` is
  preserved by construction: when it fires, `encode_legacy_rgb`
  returns a type-1 frame and the size guard's `raw == legacy` tie-
  breaks to that already-type-1 wire byte-identically. The size guard
  is the orthogonal **size-based** axis: even on histograms that
  clear Strategy E's wire-correctness signature, the bare-Fibonacci
  form's 9-byte channel-offset preamble + per-channel adaptive-CDF
  prefix + range-coder body can exceed the `1 + W*H*3`-byte raw
  payload on tiny / high-entropy inputs. The fallback is decoder-
  orthogonal: byte 0 = `0x01` routes the memcpy helper per
  `spec/01` §2.1 + the dispatch table at §1, so the type-1
  substitute decodes byte-exactly against any decoder that accepts
  type 7. 5 new tests in module `legacy_frame_uncompressed_size_guard`
  cover: (a) never-larger size invariant across `4×4`..`32×32` with
  three LCG seeds per size plus a smooth-gradient fixture, (b)
  decode-correct round-trip through the wrapper, (c) positive
  selector-fires pin showing `4×4` random input routes to byte
  0 = `0x01` and the wire equals `encode_uncompressed(pixels)`
  byte-identically, (d) tie-break-favours-legacy pin on `32×32`
  smooth gradient where the legacy wire is strictly shorter than
  the raw payload, and (e) a Strategy E composability pin on a
  `16×16` rare-symbol-cluster fixture demonstrating that the guard
  preserves the pre-existing type-1 frame byte-identically (the size
  guard composes with the wire-correctness diversion rather than
  masking it). Total test count rises from 231 to 236 (no-default) /
  234 to 239 (registry).

- round 222 — **frame-level type-1 (uncompressed) size guard.** Four
  new public encoder entry points wrap each modern arithmetic frame
  encoder with a never-larger comparison against the equivalent
  `encode_uncompressed(pixels)` form (`spec/01` §2.1):
  `encode_arith_rgb24_or_uncompressed`,
  `encode_arith_yv12_or_uncompressed`,
  `encode_arith_yuy2_or_uncompressed`, and
  `encode_arith_rgba_or_uncompressed`. Each computes both wire
  forms and returns the shorter, tie-breaking in favour of the
  arithmetic form so already-compressing inputs stay byte-identical
  to the existing `encode_arith_*` output. The fallback is
  decoder-orthogonal: byte 0 = `0x01` routes through the memcpy
  helper at `lagarith.dll!0x18000555a` per `spec/01` §2.1 +
  the dispatch table at §1, so a type-1 substitute is wire-format-
  compatible with every conformant decoder. The 64-bit proprietary
  encoder does not emit type 1 (`spec/01` §3 row 1), making the
  guard a strict structural improvement over the proprietary's own
  emission path on inputs where arithmetic overhead exceeds the raw
  payload (the 9-byte channel-offset preamble + per-channel
  Fibonacci freq table + range-coder body easily exceed the
  `1 + W*H*bpp`-byte raw payload at small frame sizes or
  high-entropy / random-pixel inputs). 13 new tests in module
  `frame_uncompressed_size_guard` cover: (a) never-larger size
  invariant across `4×4` through `32×32` for all four pixel
  families with three LCG seeds per size plus a smooth-gradient
  fixture, (b) decode-correct round-trip from every wrapper through
  `decode_frame`, (c) positive selector-fires pin showing the
  wrapper picks the type-1 branch on `4×4` random-pixel inputs and
  the wire equals `encode_uncompressed(pixels)` byte-identically,
  and (d) a tie-break-favours-arithmetic pin on `32×32` smooth
  gradient RGB24 where the arithmetic wire is strictly shorter and
  the guarded wire must equal the unguarded `encode_arith_rgb24`
  output. Total test count rises from 218 to 231 (no-default) /
  221 to 234 (registry).

### Changed

- round 216 — **packed-RGB(A) pack-loop branch hoist.** Lifted the
  per-pixel `match pixel_kind` arm out of the BGR(A) pack loops of
  `decode_arith_rgb` (types 2 / 4), `decode_arith_rgba` (type 8),
  `decode_legacy_rgb` (type 7), and `decode_solid` (types 5 / 6 / 9)
  so the dispatch fires once per call rather than once per pixel
  (`pixel_kind` is invariant after the existing early `packed_bpp()`
  validation). `decode_solid` additionally swaps the per-pixel
  `Vec::push` push-loop for `vec![0u8; n * bpp]` + a chunked-write
  fill. The plane-A `Option::expect` site in `decode_arith_rgba`
  also moves out of the hot loop body, so the round-211 lazy alpha-
  decode invariant (`Some(_)` on Bgra32, `None` on Bgr24) is
  enforced exactly once per call rather than once per pixel. Output
  byte sequence is unchanged on every input; verified by the
  existing 215 lib + 7 ffmpeg pin tests passing without modification.

### Added

- round 216 — **packed-RGB(A) pack-loop byte-layout pins.** 6 new
  tests in module `pack_loop_byte_layout_pins` lock the layout
  invariants the round-216 hoist must preserve:
  `arith_rgb_bgra32_pack_alpha_is_opaque_constant` (RGB-coded Bgra32
  must fill alpha with 0xff and the BGR triplet must match the
  Bgr24 decode of the same wire bytes),
  `arith_rgba_bgra32_pack_carries_real_alpha` (RGBA-coded Bgra32
  carries the wire alpha, not the opaque constant, verified with a
  per-pixel gradient that excludes both 0x00 and 0xff),
  `legacy_rgb_bgra32_pack_alpha_is_opaque_constant` (same as the
  modern RGB pin, on type 7),
  `solid_frames_pack_loop_byte_layout` (types 5 / 6 / 9 each pack
  the correct BGR(A) tuple to every pixel under both host kinds),
  `solid_frames_pack_loop_buffer_length` (chunked-write `vec![0u8;
  n * bpp]` sizing matches `PixelKind::buffer_len`), and
  `planar_frames_reject_packed_pixel_kinds_unchanged` (YV12 / YUY2
  still surface `PixelFormatMismatch` for `Bgr24` / `Bgra32`,
  confirming the hoist did not accidentally route them through the
  packed-RGB packer). Brings lib tests from 215 to 221, total to
  **228** (221 lib + 7 ffmpeg pins).
- round 211 — lazy alpha-plane decode in `decode_arith_rgba` for the
  `PixelKind::Bgr24` host buffer, plus early pixel-kind validation
  on the modern `decode_arith_rgb` / `decode_arith_rgba` and legacy
  `decode_legacy_rgb` paths. Spec-grounded by `spec/03` §4.3 ("alpha
  plane has no cross-plane decorrelation interaction") combined with
  `spec/04` §5 item 5 ("channels are compressed independently"): when
  the host requests `Bgr24` for a type-8 RGBA frame the alpha bytes
  are discarded at the pack step regardless, so the decoder now
  skips the entire fourth-channel dispatch (Fibonacci probability
  prefix + modern range coder + optional RLE expansion + predictor
  inverse) for that case. The pre-existing `packed_bpp().ok_or`
  pixel-kind guard moves from the post-decode pack site to the
  function entry, so a `Yv12` / `Yuy2` host buffer for an RGB-family
  frame surfaces `PixelFormatMismatch` before any per-channel
  arithmetic decode work (was: after 3 / 4 full plane decodes). 3
  new tests (`arith_rgba_bgr24_matches_bgra32_bgr_portion`,
  `arith_rgba_bgr24_skips_alpha_plane_decode`,
  `arith_rgb_family_early_rejects_planar_pixel_kind`) pin the
  behaviour from both directions: the BGR portion must match the
  BGR-of-Bgra32 byte-for-byte; a frame whose only corruption is in
  the alpha channel body must decode cleanly as Bgr24 and surface
  `BadChannelHeader` as Bgra32. 215 lib + 7 ffmpeg pin tests pass
  (+3 vs. round 204's 212).
- round 204 — randomised encoder→decoder self-roundtrip property
  suite (9 new tests, module `encoder_random_roundtrip_property`).
  Every modern arithmetic family (`encode_arith_rgb24` /
  `encode_arith_rgba` / `encode_arith_yv12` / `encode_arith_yuy2`)
  plus the legacy type-7 path (`encode_legacy_rgb`) is driven with
  deterministic LCG-seeded random pixel buffers across 3 seeds × 4
  representative `(W, H)` pairs per family that span both selector
  branches (RGB24 `width % 4 == 0` vs. unaligned; YUY2 / YV12
  chroma sub-sampling alignment), plus a wider 8-seed cross-sweep
  at 8×8 for each modern type. Each test asserts strict byte
  equality between input pixel buffer and decoded `Image::pixels` —
  a stronger correctness pin than the existing fixed-pattern
  roundtrip fixtures (which use a single `i * 73 + 11` gradient
  and would miss encoder fast-path asymmetries that fire only on
  rare residual distributions). Reduced-resolution type 11 is
  excluded by construction (2× nearest-neighbour downsample →
  upsample is lossy; only the fixed-point round-trips, pinned by
  `reduced_res_roundtrip_*`). 219 unit + integration tests pass
  after the addition (+9 vs. round 198's 210).
- round 198 — deeper channel-body fuzz: single-bit XOR, multi-byte burst
  flip (`0xff` / `0x00` / `0x55` / `0xaa`, N ∈ {2,3,4}), and
  insertion/deletion shift sweeps on valid 8×8 encoded frames across
  types 3 / 4 / 7 / 8 / 10 / 11. Targets the channel-body decoders
  (Fibonacci prefix, modern range-coder normalisation, legacy
  adaptive-CDF) at single-bit granularity below round 192's
  byte-extremes fuzz; same no-panic invariant.

## [0.0.1](https://github.com/OxideAV/oxideav-lagarith/releases/tag/v0.0.1) - 2026-05-30

### Other

- round 192 — truncation + single-byte-flip fuzz harness on valid encoded frames
- round 187 — reduced-resolution (type 11) host-dimension guard
- round 181 — defensive harness for malformed-input no-panic invariants
- round 174 — per-frame-type header-form selector flip
- round 15 — legacy-fork per-channel header-form selection
- round 14 — per-channel header-form selection across all 8 wire forms
- round 13 — modern probability-model write path: q>=1 frequency rescale
- round 12 — encoder spec/02 §6.3 final-flush FF-chain bulk-fill
- round 127 — extend ffmpeg pin set to 7 pow2 sizes + pattern-sensitivity characterisation
- round 124 — modern RGB(A) predictor Rule B + ffmpeg pins
- update is_rare_symbol_cluster doc for Strategy F
- round 96 — pair-packed 513-entry CDF decode (Strategy F)
- round 11 — encoder-side spec/02 §5 Step-C freqs[] cache
- round 10 — encoder-side spec/02 §5 Step-B fast path + cache Option→bool refactor
- round 9 — encoder-side spec/02 §5 Step-A fast path + FF-chain bulk flush
- round 8 — spec/02 §5 three-way fast path + 2-byte refill window
- round-7 test count + decoder-side defensive harness mention
- Round 7: type-7 decoder defensive harness (audit/12 §7.1)
- Round 6: Strategy E encoder integration (audit/12 §7.1)
- Round 5: type 7 Rule B predictor + RLE-then-Fibonacci channel sub-path
- Round 4: type 7 (legacy RGB / spec/07 adaptive-CDF range coder)
- Round 3: YUY2 (type 3), reduced-resolution (type 11), SIMD parity
- Round 2: YV12 (frame type 10) + stateful NULL-frame replay
- bundle RLE LUT CSVs into crate to fix CI build
- Round 1: clean-room rebuild — modern arithmetic-coded RGB family decoder
- Round 0 — clean-room rebuild scaffold (orphan master)

### Added

- **Round 192 — truncation + single-byte-flip fuzz harness on valid
  encoded frames (12 new tests).** Closes the gap between round 181's
  hand-constructed malformed inputs (one fixture per documented
  failure variant) and round 181's random-byte sweep (statistically
  unlikely to look like a valid `(escape_len, supplement_byte)` RLE
  pair / Fibonacci-prefix bit stream / channel-offset table). The
  new module
  `roundtrip_tests::decoder_truncation_fuzz` encodes one valid frame
  per frame-type the test encoder covers (1, 3, 4, 5, 6, 7, 8, 9,
  10, 11) and then walks every truncated prefix `frame[..k]` for
  `k ∈ 1..frame.len()` against all four pixel kinds, asserting the
  decoder returns `Ok(_)` or `Err(_)` without panicking (including
  in debug builds where the predictor / RLE / range-coder modules
  carry `debug_assert!` invariants). For each frame the test also
  runs a single-byte-flip pass at every 7th offset (7 is coprime
  with the channel-header / Fibonacci-prefix / RLE byte strides) to
  `0x00` and `0xff`, same no-panic invariant. Truncations that land
  strictly inside the frame-type byte + channel-offset table
  (`k ∈ 1..=8` for 3-plane frames, `1..=12` for the 4-plane RGBA
  frame, `1..frame.len()` for solid + uncompressed types) are
  additionally pinned to `Error::Truncated` — the dispatcher
  contract for an incomplete prefix per `spec/01` §2.3. Channel-body
  truncations are allowed to surface any `Err(_)` variant the
  channel decoder chooses, because a truncated body can look like a
  legal but shorter Fibonacci-coded plane. Two further tests cover
  the stateful `Decoder` and `decode_frame_with_prev` paths against
  truncated primer frames + mismatched-shape `prev`-frame state,
  pinning the invariant that a failed primer must not leave a
  half-initialised `prev` slot a subsequent NULL replay would
  dereference. 197 unit tests pass after the addition (+12 vs.
  round 187's 185).
- **Round 187 — reduced-resolution (type 11) host-dimension guard (6
  new tests).** `decode_reduced_res` now rejects host width/height
  pairs that aren't multiples of 4 with `Error::BadDimensions` before
  any wire bytes are consulted. Per `spec/01` §2.4 the type-11 wire
  body is a type-10 (YV12) frame at half-W/half-H followed by a 2×
  nearest-neighbour upscale onto the host's full-resolution YV12
  buffer. For the upscaler to land output samples on the integer
  pixel grid the host W and H must each be even (`width = 2 *
  half_w`, `height = 2 * half_h`), and for the embedded half-res
  YV12 chroma plane (`spec/03` §6.1 4:2:0 sub-sampling) the
  half_w and half_h must each *also* be even — i.e. host W and H
  each a multiple of 4. The previous bound checked only `half_w >=
  1 && half_h >= 1`, which let odd-dimensioned malformed inputs
  flow into `upscale_plane_2x` against a `(src_w, dst_w)` pair the
  helper's `debug_assert!(dst_w == src_w * 2)` invariant doesn't
  hold — `debug_assert!` panic in debug builds, silent zeroing of
  the chroma planes in release. The new bound surfaces the
  mismatch as `Error::BadDimensions` up-front, restoring the
  defensive-harness contract (`Round 181`) for the type-11 path.
  Tests cover odd widths (1, 3, 5, 7, 9, 13, 15, 17), odd heights
  (same set), widths `≡ 2 mod 4` (2, 6, 10, 14, 18, 22 — even but
  `(W/2) % 2 == 1` so the chroma sub-plane lands at fractional
  `(W/4)` columns), heights `≡ 2 mod 4` (same set), zero
  width/height (still surfaced as `BadDimensions` by the top-level
  guard), and a positive pin that multiples-of-4 still flow into
  the body parser (so a future regression that over-tightens to
  multiples-of-8 is caught). 192 tests pass after the addition.
- **Round 181 — decoder defensive harness (22 new tests).** Production-
  path robustness sweep against the public `decode_frame` /
  `decode_frame_with_prev` / `Decoder::decode` surface: every
  documented failure mode in `crate::error::Error` is exercised with
  a minimum-shape malformed fixture constructed in-line from the
  spec-defined layout fields (no encoder path involved — these tests
  target the decoder against arbitrary on-wire bytes, the actual
  production attack surface). Coverage spans the `spec/01` §1.2
  frame-type byte (`BadFrameType` for byte `0` and bytes `12..=255`,
  `NullFrame` for empty payload, `BadDimensions` for zero-W/H input),
  the `spec/01` §2.3 channel-offset table (`Truncated` short tables,
  `OffsetOutOfRange` past-EOF and descending offsets), the `spec/03`
  §2.1 channel-header dispatcher (`Truncated` short `0x01..=0x03`
  u32-length / short `0x04` raw / short `0xff` fill channels;
  `BadChannelHeader` for bytes `0x08..=0xfe`), the uncompressed and
  solid frame-type sub-paths (`Truncated` for short bodies,
  `PixelFormatMismatch` when a planar pixel kind is asked of a
  packed-RGB / solid frame), and the `spec/01` §1.1 NULL-frame replay
  contract (`NullFrameWithoutPredecessor` on the stateful `Decoder`'s
  first frame and `decode_frame_with_prev` with `prev=None`;
  `PixelFormatMismatch{frame_type:0}` on a NULL replay with
  mismatched dimensions or pixel kind). Two deterministic-LCG-seeded
  no-panic sweeps round out the harness: `random_payload_no_panic_sweep`
  exercises every frame-type byte (`0..=12`) × three seeds × eight
  lengths × four pixel kinds; `random_channel_bodies_no_panic_sweep`
  exercises random per-channel bodies behind a valid type-4 offset
  table re-routed through the type-3 / -7 / -10 dispatchers. Every
  probe returns `Result`, none panics. Test count moves from 164 →
  186 (157 → 179 in the unit-test bin + 7 unchanged integration
  tests in `tests/ffmpeg_pins.rs`). No production-code changes — the
  harness pins the existing decoder's defensive behaviour so any
  regression that introduces a panic on a malformed channel surfaces
  here as a concrete test failure rather than as a host-process
  crash.

- **Round 174 — per-frame-type header-form selector flip + frame-level
  size-delta pins.** Round 14 (round 138) added `encode_channel_best`,
  the eight-form per-channel selector across the wire forms
  `decode_channel` accepts (`spec/03` §2.1 + `spec/06` §1.7), and
  pinned its per-channel never-worse guarantee, but left every
  modern frame encoder calling `encode_channel_simple` (the
  two-candidate `0x00` / `0x04` form) pending a per-frame-type
  benchmark fixture. Round 174 flips every modern frame encoder
  (`encode_arith_rgb24` / `encode_arith_yv12` / `encode_arith_yuy2`
  / `encode_arith_rgba`, plus `encode_arith_reduced_res`
  transitively via `_yv12`) to call `encode_channel_best`
  per-plane. Round 141's analogue switch on the type-7 side is
  applied symmetrically: `encode_legacy_rgb` now calls
  `encode_legacy_channel_best` per-channel; on every realistic
  histogram the selector picks bare-Fib (header `0x00`) byte-
  identically to `encode_legacy_channel` per the
  `legacy_best_always_picks_bare_on_realistic_inputs` pin, so the
  wire stays byte-identical for our fixtures while gaining a
  forward path for any future Fibonacci variant the spec adds
  that emits zero bytes.

  Six new frame-level **never-larger** pins cover
  `encode_arith_rgb24` / `_yv12` / `_yuy2` / `_rgba` / `_reduced_res`
  against a hand-constructed `encode_channel_simple`-pipeline
  reference frame, plus a `legacy_rgb_best_pipeline_byte_identical_on_realistic_input`
  pin that guards the type-7 byte-identity. A channel-level
  `channel_best_strictly_smaller_than_simple_at_64k_zero_heavy`
  pin guards the actual size-delta direction: on a 65,536-symbol
  ~95%-zero post-gradient-dominant fixture (`spec/06` §6.4 profile),
  the selector picks header `0x01` (Fibonacci-prefixed arithmetic
  over the zero-run-contracted symbol stream) and saves **53
  bytes** vs. `encode_channel_simple` (3784 → 3731 bytes, a 1.4%
  reduction at this fixture). The crossover from "bare-Fib wins" to
  "RLE wins" sits around `n_symbols ≈ 65536` for this profile —
  smaller planes the bare-Fibonacci form already encodes near
  optimally, so the +4-byte u32 length field of `spec/07` §2.3 is
  not yet amortised on the smaller fixtures the existing roundtrip
  suite covers.

  Wire compatibility unchanged: the per-channel choice is decoder-
  blind (`spec/03` §2.1 dispatcher routes on byte 0 alone), the
  ffmpeg pins in `tests/ffmpeg_pins.rs` decode wires that this
  crate produced before the flip and still pass, and the existing
  157-test self-roundtrip suite remains byte-exact on every
  fixture.

- **Round 141 (encoder round 15) — legacy-fork per-channel
  header-form selection for type-7 (`encode_legacy_channel_best`
  + frame-level `encode_legacy_rgb_best`, `spec/07` §6.3).** The
  selector enumerates the four wire forms `decode_legacy_channel`
  accepts — bare-Fibonacci (`0x00`, `spec/07` §2.5 / §6.3) plus
  the three RLE-then-Fibonacci variants (`0x01..0x03`, one per
  `escape_len ∈ {1, 2, 3}`, `spec/07` §2.3 / §2.4) — and returns
  the shortest. Tie-breaker preserves the bare-Fibonacci form
  byte-identically. Strategy E (`audit/12 §7.1`) propagates
  through the frame-level wrapper unchanged.

  Empirical correction to `spec/07` §6.3's "compression
  trade-off" framing: with the proprietary's bit-packed Fibonacci
  layout (`spec/07` §2.2), the encoded freq-table byte stream
  produces **zero `0x00` bytes** on every realistic histogram
  probed (dense / sparse / two-symbol / biased-mid-band). The RLE
  escape (`spec/05`) requires runs of `0x00` *bytes* to fire, so
  it cannot shrink the freq-table buffer; the three
  RLE-then-Fibonacci candidates end up `+4 bytes` longer than
  bare-Fib (the u32 length field of `spec/07` §2.3 is dead
  weight). The selector therefore picks header `0x00` on every
  realistic input — making it a **never-worse defensive
  guarantee** plus a forward path for any future Fibonacci
  variant the spec adds that does emit zero bytes. The cleanroom
  encoder's wire output is byte-identical against the existing
  legacy roundtrip suite.

  8 new tests cover: never-larger-than-bare-Fib, every-sub-path-
  roundtrips through `decode_legacy_channel`, legal-header-only
  emission (`0x00..=0x03`), the tie-breaker keeps bare-Fib bytes
  byte-identical, frame-level roundtrip on 4×4 / 8×8 / 16×12
  BGR24, frame-level never-larger than `encode_legacy_rgb`,
  Strategy E propagation through the new frame wrapper, and the
  empirical bare-only-wins pin across the four fixture profiles.

- **Round 138 (encoder round 14) — per-channel header-form
  selection (`encode_channel_best`) covering all 8 wire forms
  the dispatcher accepts (`spec/03` §2.1, `spec/06` §1.7 + §2.7).**
  The new selector encodes the plane through every legal form
  (`0xff` solid-fill, `0x00` Fibonacci+arith, `0x01..0x03`
  Fibonacci+arith+pre-RLE, `0x04` raw memcpy, `0x05..0x07`
  raw+RLE) and returns the shortest. Headers `0x05..0x07` are
  new on the encoder side, exposed via the standalone
  `encode_channel_raw_rle` primitive; headers `0x01..0x03` were
  already implemented in `encode_channel_arith_rle` but were not
  in the auto-selection set. The `spec/06` §1.5 fall-back rule
  (pre-RLE count `>= n_pixels`) is enforced — illegal candidates
  are skipped, never emitted.

  Rationale: the proprietary encoder at `lagarith.dll!0x18000c500`
  picks between these forms per-channel (`spec/06` §2.8). The
  cleanroom encoder cannot reproduce the proprietary heuristic
  byte-exactly without the disassembled selector (out of scope —
  the byte-exact path is the Auditor-blocked probability-loader
  question), but a candidate-enumerate-then-min selector is
  guaranteed-legal *and* guaranteed-shortest per the spec's wire-
  format invariants: a decoder reads byte 0, routes to the matching
  sub-path, and recovers the same plane regardless of which form
  the encoder picked. Replacing `encode_channel_simple` with
  `encode_channel_best` in frame-encoder call sites cannot regress
  self-roundtrip correctness; it can only shorten output.

  Measured size delta on a representative post-gradient residual
  fixture (1900 zeros + 100 Laplacian-tail non-zero bytes, the
  canonical Lagarith profile per `spec/06` §6.4): the round-13
  `encode_channel_simple` produces 143 bytes; `encode_channel_best`
  produces 90 bytes (header `0x01`, arith+RLE at escape_len=1) =
  **37% smaller**. The frame-encoder call sites continue to use
  `encode_channel_simple` for now — flipping each frame type
  individually is the next bounded step once a per-frame-type
  benchmark fixture is wired so the size-delta can be measured per
  type rather than per channel.

  Five new tests cover:
  * `best_never_larger_than_simple` — the candidate-min selector
    cannot produce a longer wire than the 2-candidate selector
    (`encode_channel_simple`) across the fixture set, by
    construction.
  * `best_roundtrips_through_decoder` — every selected wire form
    decodes back to the input plane via the existing
    `decode_channel` dispatcher.
  * `best_picks_rle_form_on_zero_heavy_plane` — on the canonical
    95%-zero residual the selector picks `0x01..0x07` (RLE-bearing),
    never raw `0x04`.
  * `best_beats_raw_on_flat_with_zero_runs` — on a flat histogram
    with a single moderate zero run, the selector beats raw `0x04`
    by routing through `0x05..0x07`.
  * `best_size_delta_on_residual_profile` — pins the >= 10% gain
    measured above so a future Fibonacci-prefix or rescale rework
    can't silently regress the new selector below the no-RLE
    baseline.
  * `raw_rle_channel_roundtrips` — the new `encode_channel_raw_rle`
    primitive roundtrips at every `escape_len ∈ {1, 2, 3}`.

### Changed

- **Round 135 (encoder round 13) — modern probability-model write
  path: transmitted-frequency rescale to the `q >= 1` operating
  range (`spec/02` §2 / §5, `spec/04` §5 / §6).** The per-channel
  modern-coder encode path (`encode_channel_simple` header-0x00 and
  `encode_channel_arith_rle` header-0x01..0x03) now rescales the
  raw byte-histogram through `rescale_to_max_total` before it builds
  the CDF and the Fibonacci prefix, so the transmitted total stays
  `<= MAX_MODERN_TOTAL = TOP (0x800000)`.

  Rationale: `spec/02` §5 starts every symbol with `q = range /
  total`; `spec/02` §2 floors the post-renormalisation `range` at
  `TOP + 1`, so a transmitted `total > TOP` drives `q` to zero and
  the coder produces `range = 0` (and a divide-by-zero in the
  decoder's Step-C `low / q`). `spec/04` §5 documents that the
  proprietary loader normalises the histogram for exactly this
  `q >= 1` guarantee; its validation correction establishes the
  *wire* still carries a raw histogram for the fixtures probed —
  every one well under `TOP`. The rescale therefore **no-ops for
  every sub-TOP plane** (the common case), keeping the wire
  byte-identical to the raw-histogram form, and engages only for
  planes whose symbol count exceeds `TOP` (> ~8.39M residuals — a
  single 4K+ plane), which the prior encoder could not encode at
  all. The rescale is `floor(freq[s] * cap / sum)` clamped up to
  `1` for every nonzero slot (so no used symbol drops out of the
  table), with any residual overshoot trimmed from the largest
  slots, never below `1`. Encoder and decoder build the identical
  CDF from the transmitted rescaled table (`spec/04` §6), so the
  arithmetic coder stays exact — only the probability model
  changes, never the symbol→byte mapping.

  Five new tests in `src/encoder.rs`: `rescale_noop_when_total_fits`
  (verbatim passthrough under the cap), `rescale_caps_total_and_
  preserves_nonzero` (total `<= cap` + nonzero-preservation on a
  dominant-plus-rare-tail histogram), `rescale_small_cap_overshoot_
  trim` (256 equal slots forced through the trim path),
  `rescale_capped_channel_roundtrip` (small-cap end-to-end modern-
  wire self-roundtrip across five caps), and
  `rescale_production_cap_large_plane_roundtrip` (a genuine
  `total > TOP` ~8.4M-residual plane that round-trips byte-exactly
  at the production cap — the prior encoder would have broken on
  it). Test count: 131 → 136 (library) + 7 (integration). Still
  self-roundtrip-only; byte-exact-vs-proprietary remains
  Auditor/Extractor-blocked on the un-disassembled probability
  loader at `0x180001050`.

- **Round 132 (encoder round 12) — `spec/02` §6.3 final-flush
  FF-chain bulk-fill.** `RangeEncoder::finish()`'s pre-tail
  pending-FF chain drain now uses `Vec::resize` (one bounds
  check + one memset) instead of `pending_ffs` individual
  `Vec::push` calls. This is the §6.3 final-flush analogue of
  the round-9 hot-path `Vec::resize` for the per-`shift_low`
  FF-chain commit, completing the structural pattern: no
  per-FF push loops anywhere in the encoder. The on-wire
  bytes are unchanged — the same `cache` (or `cache+1`) head
  + N×fill (`0x00` on a carry, `0xff` otherwise) + four-byte
  tail per `spec/02` §6.3 — so the flush is bit-identical to
  the proprietary's primitive.

  On a short-channel + Step-B-heavy fixture (512 channels ×
  128 symbols, 200 reps, release build, macOS aarch64) the
  round-12 encoder delivers ~305-311 MSym/s vs. ~298-310
  MSym/s for the per-FF push reference (timed side-by-side in
  the same bench) = ~1.00-1.03× speedup, default-on. The
  delta is modest because realistic `pending_ffs` chains are
  short (3-15 bytes); the value of the round is structural
  pattern completion + a byte-equivalence regression guard.

  Two new tests in `src/range_coder.rs`:
  `rangecoder_finish_resize_byte_equiv_to_push_loop` encodes
  a 0xff-dominant stream through both the production
  `Vec::resize` form AND a `finish_via_push_loop` helper
  modelling the pre-round-12 per-FF push form, asserting byte
  equality (covers the §6.3 flush bit-identity guard).
  `rangecoder_encode_throughput_finish_heavy` times both
  forms side-by-side on a 65,536-symbol short-channel
  workload (512 chans × 128 syms) under `LAGARITH_BENCH=1`,
  reporting the RESIZE/PUSH speedup ratio for direct
  comparability with the Step-A / Step-B / Step-C bench
  numbers. Test count: 129 → 131 (library) + 7 (integration).

### Added

- **Round 127 — extended ffmpeg cross-decoder pin set + pattern-
  sensitivity characterisation.** The pin file now carries four new
  random-seeded committed frames in addition to round 124's three:
  RGB24 4×4 + 8×16 and RGBA 4×4 + 8×8, all built from the same LCG
  pixel source (`state * 0x9e37_79b9 + 0x12345`, seed `0xdeadbeef`,
  high byte per pixel) and all verified to decode byte-for-byte
  through ffmpeg's `lagarith` decoder via a minimal `LAGS`-coded AVI
  wrapper. Seven total pins now run in CI without ffmpeg.

  Round 127 also empirically characterises the residual gap: the
  crate encoder's compatibility with ffmpeg is **pattern-sensitive
  even within the power-of-two-pixel-count regime**. The structured
  `i * 73 + 11 → bit-slice` test pattern (used by the existing
  self-roundtrip tests) at the same pow2 sizes that the random
  patterns sweep cleanly produces ffmpeg-divergent frames (e.g.
  ~40% byte match at 16×16, single-byte off-by-N residuals
  scattered through the planes). The crate's own decoder
  self-roundtrips both pattern classes byte-exactly, so the
  divergence sits on ffmpeg's side of the wire-format interpretation.
  The most likely root cause is the channel-prefix probability-loader
  at `lagarith.dll!0x180001050` (referenced from `spec/02` §5 and
  `spec/04` §5/§6 but **not disassembled into cleanroom spec**),
  which converts the wire's raw frequency histogram into the
  internal cumulative + shift-exponent struct the modern range
  coder consumes. ffmpeg's implementation almost certainly mirrors a
  normalisation step that the crate's encoder/decoder pair collapses
  to identity (`q = range / total` with raw `total`).

  A prototype encoder-side `rescale_to_pow2` fix was explored that
  converted non-pow2 channel totals to the smallest power of two
  before encoding (matching the legacy coder's `spec/07` §3.2
  approach) — this closed the non-pow2 5×5 / 3×3 frame gap exactly
  but did not address the pattern-sensitive pow2 cases, confirming
  the two issues share the same un-disassembled normalisation root.
  The prototype is not landed (no spec-derived rationale for changing
  the wire format from raw to rescaled freqs without the
  `0x180001050` reference); the gap remains documented for the
  Extractor round.

### Fixed

- **Round 124 — modern arithmetic RGB(A) first-column predictor
  corrected from Rule A to Rule B.** The modern RGB24 / RGB32
  (types 2 / 4) and RGBA (type 8) decode + encode paths now use
  the **Rule B** first-column-of-row predictor
  (`TL = plane[y-2][W-1]` for `y >= 2`, `spec/06` §3.2) instead of
  Rule A. The cleanroom's audit/01 §9.1 had left the Rule A vs
  Rule B dispatch open because a horizontal-ramp fixture makes the
  two rules degenerate (first column constant ⇒ `TL == T`). A
  black-box differential decode against the independent ffmpeg
  `lagarith` decoder — feeding it `LAGS`-wrapped frames built under
  each rule — resolves it: ffmpeg reproduces the original pixels
  byte-exactly only for Rule B encodes (every power-of-two
  pixel-count RGB24 / RGB32 / RGBA frame tested). The prior Rule A
  decode produced wrong pixels for real Lagarith RGB streams.

### Added

- **Round 124 — ffmpeg cross-decoder byte-exact pins.**
  `tests/ffmpeg_pins.rs` commits three RGB24 (8×8, 16×16) and RGBA
  (16×16) frames produced by the crate encoder and verified to
  decode to their original pixels through ffmpeg's `lagarith`
  decoder (used purely as a black-box oracle). The pins are
  committed so the regression runs in CI without ffmpeg; they
  guard against any reversion of the modern path back to Rule A.

- **Round 96 — pair-packed 513-entry CDF decode path (legacy
  type 7, `spec/07` §3.1 + §3.4 audit-corrected; audit/12 §7.1
  "Strategy F").**
  - `build_legacy_pair_packed_cdf` constructs the proprietary's
    pair-packed 513-entry CDF: the rescaled frequencies are
    interleaved with sentinel-`1` inter-symbol gaps as
    `(freq'[c], 1)` and prefix-summed, so symbol `c`'s bounds sit
    at `cdf[2c]` / `cdf[2c+1]` and the full span is `total + 256`.
    The shared `spec/07` §3.2..§3.3 rescale + zigzag-residue front
    end is factored into `rescale_freq`, used by both the flat and
    pair-packed builders.
  - `LegacyRangeDecoder::new_pair_packed` + a `CdfLayout` dispatch
    decode against the pair-packed CDF via the `spec/07` §5.2
    even-index binary descent, with the same
    `total = next_pow2(Σfreq)` divisor. Because the pair-packed
    lower bounds span `[0, total + 256)` while the §5.1
    `symbol_index` is capped at `total - 1`, high-index rare
    symbols are unreachable — reproducing the proprietary's
    documented rare-symbol mis-decode (audit/12 §3.6 — `0xc0`
    decodes as `0xff`).
  - `channel::decode_legacy_channel` now routes streams matching
    the rare-symbol-cluster signature
    (`is_rare_symbol_cluster`) through the pair-packed path
    instead of returning `Error::LegacyRareSymbolClusterUnsupported`.
    Our own encoder never produces such streams (Strategy E routes
    them to type 1), so this path serves foreign / proprietary-
    encoded type-7 streams. The error variant is retained for API
    stability and genuinely-undecodable edge cases.
  - +4 tests: the audit/12 §5 worked-example boundary shifts
    (`1081 / 1085 / 1215`), pair-packed CDF layout vs. the flat
    form, length-correct pair-packed decode that avoids the
    unreachable symbols, and a public-API `decode_frame` decode of
    a rare-cluster type-7 frame. The five round-7 "defensive
    harness" refusal tests are rewritten to assert the new decode
    behaviour. Full byte-exact proprietary parity still awaits a
    fixture oracle (`samples.oxideav.org/lagarith/`, 404 per
    audit/04 §5).

- **Round 11 — encoder-side range-coder Step-C `freqs[]` cache
  (`spec/02` §5).**
  - `Cdf` now caches `freqs: [u32; 256]` where `freqs[s] = cum[s+1]
    - cum[s]`. The encoder's `spec/02` §5 Step-C arm (fired
    whenever the symbol is neither `0` nor `255`) hoists its
    `cum[s+1] - cum[s]` two-read + subtract to `from_frequencies`
    time; the hot path then loads `lo = cdf.lo(s)` and `freq =
    cdf.freq(s)` in parallel and the `range = freq * q` multiply
    no longer waits on a subtract. Bit-identical to the round-10
    `cum[]`-array form (verified by
    `rangecoder_step_c_encode_bit_equiv_to_generic`, which
    encodes the same mid-band stream through both paths and
    asserts byte equality).
  - The Cdf struct's small scalar fields (`freq0`, `total`) are
    reordered to offset 0 so a Step-A-dominant workload keeps
    them on the first cache line; the `freqs[]` array lands
    after `cum[]` so it does not contend with `freq0` for cache
    set on the dominant path.
  - **Throughput delta**: 65,536-symbol Step-C-heavy encode
    fixture (99% mid-band symbols `1..=254`, 200 reps, release
    build, macOS aarch64) — round-10 baseline ~225 MSym/s →
    round-11 ~244 MSym/s = **1.08×** on Step-C-dominant
    workloads. Step-A and Step-B benches stay within run-to-run
    noise of round 10 (~334 vs. ~333 MSym/s on Step-A; ~333 vs.
    ~327 MSym/s on Step-B) — the new cache field does not
    regress the dominant paths. Default-on, no feature gate.
  - +5 tests: Step-C-dominant encode self-roundtrip
    (`rangecoder_encode_step_c_dominant_roundtrip`); Step-C
    bit-equivalence guard
    (`rangecoder_step_c_encode_bit_equiv_to_generic`); Step-C
    encode throughput bench
    (`rangecoder_encode_throughput_step_c_heavy`; functional
    check, timing only printed under `LAGARITH_BENCH=1`);
    Laplacian-residual roundtrip on a `{0, 1, 254, 255}`-heavy
    distribution (`rangecoder_encode_laplacian_residual_roundtrip`);
    `freqs[]` cache layout regression guard
    (`cdf_freq_matches_array_form`). 126 tests total (was 121).
  - **Step-A1 / Step-B1 prototype reverted**: dedicated `s == 1`
    (small-positive `+1` residual) and `s == 254` (small-negative
    `-1` residual on the unsigned-wrap side) fast paths were
    tried and benched against mixed-distribution streams — they
    hurt the dominant Step-A path more than they helped the
    secondary symbols (extra branches in the hot loop dropped
    Step-A heavy from ~340 MSym/s to ~299 MSym/s, -12%). The
    `encode_symbol` dispatcher deliberately falls through to
    Step-C for s ∈ 1..=254 instead of growing the if-chain.
    Documented as a round-11 NOTE inside `encode_symbol`.

- **Round 10 — encoder-side range-coder Step-B fast path
  + cache-slot Option→bool refactor (`spec/02` §5/§6.2).**
  - `RangeEncoder::encode_symbol` now implements the symmetric
    **Step-B** fast path for `s == 255` (the high-sentinel symbol
    the decoder already short-circuits per `spec/02` §5 Step B).
    For `s == 255` the generic update reads `lo = cum[255]` and
    `hi = cum[256] = total`, so the cached `Cdf::cum_top`
    (= `cum[255]`) and `Cdf::total` directly drive the update
    without indexing the 257-entry cumulative array:
    `low += cum_top * q; range = (total - cum_top) * q`.
    Bit-identical to the round-9 Step-C path on 0xff (verified by
    `rangecoder_step_b_encode_bit_equiv_to_generic`, which encodes
    the same 0xff-dominant stream through Step-B and through an
    inline Step-C-only reference and asserts byte equality).
  - `RangeEncoder::shift_low` swaps `cache: Option<u8>` for
    `cache: u8 + started: bool`. The hot inner body now issues one
    `bool` check instead of the `Option::take()` discharge the
    optimiser couldn't elide across the carry / defer /
    steady-state branch arms. The arithmetic is unchanged (same
    `c` / `c+1` cache byte, same `0x00` / `0xff` fill, same
    low-mask) so the wire stays bit-identical to the proprietary's
    cache-then-FF-chain emission per `spec/02` §6.2 — verified by
    the new `rangecoder_shift_low_started_byte_equiv_to_option`
    self-roundtrip and the long-standing `rangecoder_roundtrip_wide`
    decoder test.
  - **Throughput delta**: 65,536-symbol Step-B-heavy encode
    fixture (94% 0xff symbols, same total mass + same residual
    shape as the Step-A bench), 200 reps, release build,
    macOS aarch64 — round-9 baseline ~305 MSym/s → round-10
    ~327 MSym/s = **1.07×** on Step-B-dominant workloads. The
    Step-A-dominant bench is within run-to-run noise of the
    round-9 number (~333 vs. ~336 MSym/s) — Step-B does not
    regress Step-A. Default-on, no feature gate.
  - +4 tests: Step-B-dominant encode self-roundtrip
    (`rangecoder_encode_step_b_dominant_roundtrip`); Step-B
    bit-equivalence guard
    (`rangecoder_step_b_encode_bit_equiv_to_generic`); Step-B
    encode throughput bench
    (`rangecoder_encode_throughput_step_b_heavy`; functional
    check, timing only printed under `LAGARITH_BENCH=1`); cache
    Option→bool roundtrip regression
    (`rangecoder_shift_low_started_byte_equiv_to_option`).
    121 tests total (was 117).

- **Round 9 — encoder-side range-coder hot-path optimisation
  (`spec/02` §5, symmetric to round 8's decoder).**
  - `RangeEncoder::encode_symbol` now implements the Step-A
    symbol-0 fast path: for `s == 0` the generic Step-C
    arithmetic collapses to a no-op `low += cum[0]*q = 0` plus
    `range = freq0 * q`, so the optimised path skips the two
    `Cdf::lo()` reads and the `wrapping_add` of zero. Lagarith
    residuals after gradient prediction are dominated by symbol 0
    (`spec/06` §6.4: `freq[0] >= 0.95 * pixel_count`), so the
    Step-A check is the dominant case and short-circuits the
    generic indirection. The wire bytes are bit-identical to the
    generic path (verified by `rangecoder_step_a_encode_bit_equiv_to_generic`,
    which encodes the same stream through Step-A and through an
    inline Step-C-only reference and asserts byte equality).
  - `RangeEncoder::shift_low` now commits the `pending_ffs` chain
    with a single `Vec::resize` (one bounds check + one bulk
    memset) instead of `pending_ffs` individual `Vec::push` calls.
    Same `c` / `c+1` cache byte, same `0x00` / `0xff` fill, same
    low-mask, so the wire stays bit-identical to the proprietary's
    cache-then-FF-chain emission per `spec/02` §6.2.
  - **Throughput delta**: 65,536-symbol signal-heavy encode
    fixture (94% zeros — same shape as the round-8 decoder
    bench), 200 reps, release build, macOS aarch64 — baseline
    179 MSym/s → optimised 330 MSym/s = **1.84×**. Default-on,
    no feature gate (over the 1.3× threshold for unconditional
    landing).
  - +3 tests: Step-A-dominant encode self-roundtrip
    (`rangecoder_encode_step_a_dominant_roundtrip`); Step-A
    bit-equivalence guard
    (`rangecoder_step_a_encode_bit_equiv_to_generic`) that re-runs
    the same input through an inline generic-only path and asserts
    byte equality; signal-heavy encode throughput
    (`rangecoder_encode_throughput_signal_heavy`; functional
    check, timing only printed under `LAGARITH_BENCH=1`). 117
    tests total (was 114).

- **Round 8 — modern range-coder hot-path optimisation (`spec/02` §5).**
  - `RangeDecoder::decode_symbol` now implements the three-way fast
    path of `spec/02` §5: Step A (symbol 0, `low < cum[1] * q`),
    Step B (symbol 0xff slack-band sentinel, `low >= total * q`,
    update `low -= total*q; range -= total*q`), Step C (generic
    cumulative search). Step A short-circuits the 9-iteration
    binary search on the dominant case — Lagarith residuals after
    gradient prediction land in symbol 0 ~94-96% of the time
    (`spec/06` §6.4: `freq[0] >= 0.95 * pixel_count`), so this
    branch fires almost every iteration of the hot loop.
  - `Cdf` now caches `freq0 = cum[1]` and `total = cum[256]`
    inline on the struct so the fast paths read them with no array
    indexing per symbol.
  - `RangeDecoder::renormalise` reshapes the per-iteration bytewise
    refill into a 2-byte slice window (`src.get(c..c+2)`) so the
    optimiser hoists a single bounds compare per loop iteration
    instead of two `Option`-unwraps. The arithmetic is unchanged
    (still byte-at-a-time per `spec/02` §4) so output is
    bit-identical to the proprietary.
  - **Throughput delta**: 65,536-symbol signal-heavy fixture
    (94% zeros), 200 reps, release build, macOS aarch64 —
    baseline 37.4 MSym/s → optimised 161.3 MSym/s = **4.31×**.
    Far above the 1.3× threshold for default-on; no feature
    gate needed.
  - +4 tests: step-A-dominant histogram, step-B-hits histogram,
    renormalise tail-saturation, signal-heavy throughput
    (functional check; timing only printed under
    `LAGARITH_BENCH=1`). 114 tests total (was 110).

- **Round 7 — type-7 decoder defensive harness (`audit/12 §7.1`).**
  - `decode_legacy_channel` now runs the rare-symbol-cluster
    predicate (`is_rare_symbol_cluster`) over the transmitted 256-
    entry frequency table before building the CDF. When the
    signature matches (`freq[0] >= 0.95 * Σfreq` *and* ≥ 3 distinct
    nonzero bins each with `freq ∈ {1, 2}`), the decoder returns
    the new `Error::LegacyRareSymbolClusterUnsupported` variant
    rather than silently miscoding the body. Audit/12 §5..§6
    retracts spec/07 §3.4's flat-CDF allowance for this fixture
    class — the cleanroom's flat 257-entry CDF and the
    proprietary's pair-packed 513-entry CDF are *not*
    bit-equivalent here, so a *foreign* encoder's stream with this
    freq table would silently decode to the wrong residual sequence
    under our flat CDF.
  - The cleanroom's own encoder still applies *Strategy E* (round
    6) and re-routes such fixtures to type 1 *before* reaching the
    legacy range coder, so the guard is never invoked on
    self-roundtrip and the existing 104-test suite is unchanged.
    The guard exists for the case where downstream callers feed
    *foreign* type-7 streams into `decode_frame` — a hypothetical
    proprietary type-7 writer (the shipped proprietary build is
    decode-only per `spec/07 §6` / §9.2 item 8) or any third-party
    encoder.
  - Strategy F (full pair-packed 513-entry CDF refactor of
    `audit/12 §7.1`) remains parked: blocked on a proprietary-
    encoded type-7 fixture appearing at
    `samples.oxideav.org/lagarith/` (`audit/04 §5`; re-checked
    round 7 — still 404). Without an oracle a 150-200-LOC refactor
    risks regressing the 95/96 currently-passing type-7 cells.
  - +6 tests: rare-cluster freq table at `decode_legacy_channel`;
    3-vs-2 rare-bin boundary (must trigger / must not); freq[0]
    dominance below 95% (must not trigger); rare-cluster type-7
    frame at the public `decode_frame` surface; error-display
    mentions audit/12 + Strategy F; self-roundtrip smoke check.
    110 tests total (was 104).

- **Round 6 — Strategy E encoder integration (audit/12 §7.1).**
  - `encode_legacy_rgb` and `encode_legacy_rgb_rle` now run the
    `is_rare_symbol_cluster` predicate over the three residual
    planes (B', G, R') after predict + decorrelate. When any plane
    matches the rare-symbol-cluster signature (≥ 3 distinct rare
    bins `freq ∈ {1, 2}` within a histogram dominated by
    `freq[0] >= 0.95 * pixel_count`), the encoder skips type-7
    emission and falls through to a type-1 (uncompressed) frame.
    Type-1's roundtrip is byte-exact on every fixture per
    `spec/01 §2.1` / `audit/11 §4.5`, sidestepping the
    flat-CDF / pair-packed-CDF wire-format divergence that
    `audit/12 §5..§6` localised to this fixture class.
  - Strategy F (full pair-packed 513-entry CDF refactor) deferred
    — `audit/12 §7.1` recommends Strategy E because Strategy F's
    regression risk on the 95/96 currently-passing type-7 cells
    outweighs its benefit (type 7 is decode-only in the
    proprietary build per `spec/07 §6` / §9.2 item 8; no archival
    type-7 fixture exists per `audit/04 §5`).
  - +4 tests: rare-symbol-cluster routes near_flat 33×27 to
    type 1; same propagates through the RLE-then-Fibonacci
    sub-path for escape_len ∈ {1, 2, 3}; pattern-pixels and
    pure-solid plane fixtures stay on type 7 (negative cases).
  - 104 tests total (was 100).

- **Round 5 — type-7 spec-coverage extensions: Rule B + RLE-then-
  Fibonacci channel sub-path.**
  - **First-column predictor Rule B** for type-7 frames per
    `spec/07` §9.1 item 7b: row `y ≥ 2` first-column TL =
    `plane[y-2][W-1]` (linear-memory rule), falling back to Rule A
    for `y = 1` (no `y-2` row exists). The audit-resolved binary
    walk at `lagarith.dll!0x180001b00..0x180001c5d` shows no
    per-row state machine; Rule B matches the proprietary's SIMD
    predictor's reverse-engineered residuals bit-for-bit. Modern
    types 2 / 4 / 8 / 10 / 11 retain Rule A.
  - **Channel header `0x01..=0x03` RLE-then-Fibonacci sub-path**
    per `spec/07` §2.3 / §2.4. Wire layout: outer header byte
    (= escape_len ∈ {1, 2, 3}); u32 LE post-RLE byte count L
    (≤ 256); RLE-compressed input expanding to the L-byte
    Fibonacci-coded freq-table buffer; post-Fibonacci 1-byte
    reservation; legacy range-coder body. Fixes the prior
    `BadChannelHeader` rejection for outer headers in {0x01..0x03}.
  - **Audit/12 rare-symbol-cluster signature detector**
    (`is_rare_symbol_cluster`) — predicate hook for a future
    Strategy E encoder route-around (audit/12 §7.1: ≥ 3 distinct
    nonzero bins each with `freq ∈ {1, 2}` within a histogram
    dominated by `freq[0] >= 0.95 * pixel_count`). The
    pair-packed 513-entry CDF (Strategy F) is deferred per
    audit/12 §7.1's regression-risk recommendation; no archival
    type-7 fixture exists per audit/04 §5 to validate the full
    refactor.
  - Test-only `encode_legacy_channel_rle` / `encode_legacy_rgb_rle`
    helpers driving end-to-end self-roundtrip on the
    RLE-then-Fibonacci wire path for escape lengths 1, 2, and 3.
  - +17 tests: 4 Rule-B predictor unit tests (4×4, 11×7, y=1
    fallback to Rule A, divergence vs Rule A on row ≥ 2); 6
    type-7 RLE-then-Fib roundtrip tests (escape 1/2/3 at 4×4,
    8×8 sweep, solid plane); 2 type-7 Rule B frame-level
    roundtrips (5×4, 4×8); 5 rare-symbol-cluster detector unit
    tests covering the audit/12 canonical fixture, solid plane,
    too-few-rare-bins, no-dominant-zero, and high-freq neighbours.
  - 100 tests total (was 83).

- **Round 4 — legacy RGB (frame type 7, `spec/07` adaptive-CDF range
  coder).**
  - `FrameType::LegacyRgb` (frame-type byte `0x07`) now decodes
    pre-1.1.0 "Obsolete arithmetic coded RGB keyframe" frames per
    `spec/07`. The wire pipeline differs from the modern range
    coder of `spec/02` in two load-bearing ways: the probability
    distribution is **transmitted per-channel** as a 256-entry
    Fibonacci-coded frequency table (no zero-run sub-prefix —
    audit/03 §3.1 confirms the legacy decoder body has a clean
    one-int-per-symbol shape, distinct from the modern coder's
    spec/04 §3.3 path), and the decoder builds a **257-entry CDF
    on the fly** from the transmitted frequencies via the §3
    "pair-pack + rescale-to-pow2 + zigzag-residue + prefix-sum"
    pipeline.
  - New `legacy_range_coder` module: `decode_legacy_freq_table`,
    `build_legacy_cdf` (with zigzag residue distribution per
    `spec/07` §3.3), and `LegacyRangeDecoder` (binary-search
    descent over the 257-entry CDF — algebraically equivalent to
    the proprietary's eight-level binary-tree descent per
    `spec/07` §5.2 final paragraph). The init seed uses the
    audit-corrected 4-byte priming form per `spec/07` §9.1
    item 3.
  - 2-byte channel prefix (outer header `0x00` + inner codec-mode
    flag `0x00`) per `spec/07` §2.5; post-Fibonacci 1-byte
    reservation per audit/08 §3.2 / `spec/07` §9.1 item 7c —
    emitted by the encoder + skipped by the decoder **only when**
    the encoded bit stream length is a multiple of 8.
  - Test-only `LegacyRangeEncoder` + `encode_legacy_freq_table`
    for the round-4 self-roundtrip suite (the proprietary build
    is decode-only for type 7; the cleanroom honours
    `CLEANROOM-MANUAL §8` "Both directions, always" by shipping
    both halves). Round-4 encoder ships only the bare-Fibonacci
    `header == 0` path — `spec/07` §6.3 / §9.2 item 9 confirm
    header-0 is sufficient for round-trip correctness; the
    `header ∈ {0x01, 0x02, 0x03}` RLE-then-Fibonacci sub-path is
    decoder-undefined per §9.1 item 2.
  - +17 tests covering type-7 (4×4, 8×8, 16×12, unaligned width
    7×5, BGRA32 widening, solid-plane edge case, NULL replay,
    inner-codec-mode-flag rejection), the YUY2 odd-width
    floor-chroma layout (audit/00 §9.4 partial resolution + a
    raw-channel roundtrip exercising the 5×4 odd-width tail), and
    the legacy range-coder + Fibonacci freq-table self-roundtrip
    primitives. 83 tests total.
  - Spec gap noted: `samples.oxideav.org/lagarith/` returns HTTP
    404 at the time of round 4 — byte-exact validation against a
    proprietary-encoded AVI fixture remains an Auditor concern
    for a future round.

- **Round 3 — YUY2 (frame type 3) and reduced-resolution (frame
  type 11).**
  - `FrameType::ArithmeticYuy2` (frame-type byte `0x03`) now decodes
    via the same channel-header dispatcher as the YV12 path, with
    three planes — Y at `W * H`, U and V at `(W / 2) * H` each —
    per-plane left + median predictor, no cross-plane
    decorrelation. The wire is **planar** Y/U/V (note: U before V,
    unlike YV12); the output ([`PixelKind::Yuy2`]) is **packed**
    `Y0 U Y1 V` per pair of pixels at columns `2k, 2k+1`
    (`spec/03` §6.2).
  - `FrameType::ReducedResYv12` (frame-type byte `0x0b`) decodes
    the body as a half-W/half-H YV12 frame and 2× nearest-neighbour
    upscales each plane (luma + V + U) onto the host's full-
    resolution `PixelKind::Yv12` buffer (`spec/01` §2.4 +
    audit/00-report.md §9.1).
  - New `PixelKind::Yuy2` variant (16-bpp packed, 2 bytes per
    pixel).
  - Encoder helpers `encode_arith_yuy2` and `encode_arith_reduced_res`
    for the self-roundtrip suite (the round-3 encoder remains
    self-roundtrip-only — byte-exact validation against the
    proprietary encoder is an Auditor concern; see the
    `SPECGAP-encoder-byte-exact` test marker).
  - **SIMD-vs-scalar predictor parity** documented per `spec/06`
    §3.5 / §3.6: the crate's scalar predictor implements
    Strategy A (`TL = L = plane[y-1][W-1]`), which is
    *carry-equivalent* to the proprietary's SIMD inner-loop AND
    matches the proprietary's scalar predictor for every
    `(width, frame-type)` pair. No separate SIMD path is needed
    for byte-equivalent residual streams.
  - +11 tests covering the YUY2 path (4×4, 8×6, 16×16, pixel-
    format mismatch, buffer-length parity), the reduced-resolution
    path (8×8, 16×16, pixel-format mismatch, hand-rolled 2×
    upscale parity), the SIMD-vs-scalar predictor parity, and the
    byte-exact-encoder SPECGAP marker. 66 tests total.

- **Round 2 — YV12 (frame type 10) and stateful NULL-frame replay.**
  - `FrameType::ArithmeticYv12` (frame-type byte `0x0a`) now
    decodes through the same channel-header dispatcher as the RGB
    family, with three independent planes (Y at `W * H`, V and U
    at `floor((W * H) / 4)` each), per-plane left + median
    predictor, and **no** cross-plane decorrelation per `spec/03`
    §6.1 + §4.4.
  - New `PixelKind::Yv12` variant — output buffer is the
    concatenated Y / V / U planes (the standard YV12 raw layout
    the proprietary decoder writes per `spec/03` §6.1). The
    `oxideav-core` framework `Decoder` impl splits this back into
    three `VideoPlane`s with their respective strides.
  - Stateful `Decoder` wrapper: `Decoder::decode(payload, ..)`
    accepts a zero-byte payload as a NULL frame ("JUMP") per
    `spec/01` §1.1 and replays the predecessor frame; the
    stateless `decode_frame` continues to surface NULL frames as
    `Error::NullFrame`. A standalone `decode_frame_with_prev`
    helper centralises the same replay rule for callers that
    don't want to carry state.
  - Two new error variants: `Error::NullFrameWithoutPredecessor`
    (NULL frame with no prior to replay) and
    `Error::PixelFormatMismatch` (host-requested pixel format
    doesn't match the wire frame type, e.g. asking for `Bgr24` on
    a YV12 frame).
  - Encoder helpers `encode_arith_yv12` and `encode_null` for the
    self-roundtrip suite.
  - +13 tests covering the YV12 path (4×4, 8×6, 16×16, all-solid
    planes, pixel-format mismatch, buffer-length parity) and the
    NULL-frame replay (helper-function, stateful-decoder,
    predecessor update, dimension mismatch, YV12 replay). 55
    tests total.

- **Round 1 — modern arithmetic-coded RGB family decoder.** Decodes
  Lagarith frame types 1 (Uncompressed), 2 (Unaligned-RGB24), 4
  (Arithmetic-RGB24), 5/6/9 (Solid Grey/RGB/RGBA), and 8
  (Arithmetic-RGBA) into BGR24 / BGRA32 host buffers. Pipeline
  ports:
  - Frame layout + per-pixel-format channel-offset table (`spec/01`).
  - Modern range coder with TOP=2^23 / init range=2^31 / four-byte
    init / cross-byte-LSB-rotated refill / four-byte flush tail
    (`spec/02`).
  - JPEG-LS clamped median + first-row left predictor + cross-plane
    G-pivot decorrelation reverse (`spec/03`).
  - Fibonacci-prefix probability table with Zeckendorf encoding +
    binary suffix (`spec/04`).
  - Residual zero-run RLE escape with the 256-entry permutation
    LUT loaded from `tables/01-residual-rle-decoder-lut.csv`
    (`spec/05`).
  - Channel-header dispatcher with the
    `0x00 / 0x01..0x03 / 0x04 / 0x05..0x07 / 0xff` sub-paths plus
    the u32-length-overflow fall-back to header-`0x00` style
    (`spec/06` §1).
- `oxideav-core` framework registration claiming the `LAGS` FOURCC
  via `CodecInfo::tags([CodecTag::fourcc(b"LAGS")])`.
- `cfg(test)` self-roundtrip encoder + 42-test suite covering each
  dispatcher path.

### Changed

- Clean-room rebuild from a fresh orphan `master`. The previous
  implementation was retired by the OxideAV docs audit dated
  2026-05-06; the prior history is preserved on the `old` branch.

### Deferred

- Type-7 RLE-then-Fibonacci channel sub-path (`header ∈ {0x01,
  0x02, 0x03}` — surfaces `BadChannelHeader` in the round-4
  decoder). `spec/07` §9.2 item 9 explicitly notes header-0 is
  sufficient for round-trip correctness; the RLE-pre-decompressed
  freq-table path is decoder-undefined per §9.1 item 2.
- Byte-exact encoder validation against a proprietary-encoded AVI
  fixture — Auditor concern; `samples.oxideav.org/lagarith/`
  returned HTTP 404 at the time of round 4.
- The reciprocal-multiply LUT at RVA `0x1b9a0` is not used by the
  decoder (the cumulative-frequency search loop `spec/02` §5 invites
  is bit-equivalent and simpler).
- Type-7 byte-exact match against the proprietary's RuleB
  first-column predictor (`TL = plane[y-2][W-1]` for `y >= 2` per
  `spec/07` §9.1 item 7b). The round-4 decoder uses Rule A
  (`TL = L = plane[y-1][W-1]`) — the same rule types 2 / 4 use.
  Self-roundtrip is bit-perfect; the discrepancy only matters for
  byte-exact match against a proprietary-encoded type-7 fixture
  (none in tree).
