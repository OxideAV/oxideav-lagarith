//! Per-channel header dispatcher per `spec/03` §2.1 + `spec/06` §1.
//!
//! Decodes a single compressed channel slice into `n_pixels`
//! residuals. The channel-header byte at offset 0 selects the
//! sub-path:
//!
//! | Header | Wire form |
//! | ------ | --------- |
//! | `0x00` | Fibonacci prefix at offset 1 + arithmetic body. No RLE. Range coder produces `n_pixels` symbols. |
//! | `0x01..=0x03` | u32 length at offsets 1..4 (only when `< n_pixels`); Fibonacci prefix at offset 5; arithmetic body produces `u32` pre-RLE symbols. Post-process RLE-expand with `escape_len = header` to fill `n_pixels`. The "u32 ≥ n_pixels" fall-back diverts to header-`0x00` style. |
//! | `0x04` | `n_pixels` raw bytes at offset 1. No entropy. |
//! | `0x05..=0x07` | Raw bytes at offset 1, post-processed with RLE escape `escape_len = header - 4`. |
//! | `0xff` | Constant fill: byte at offset 1 replicated `n_pixels` times. |

use crate::error::{Error, Result};
use crate::fibonacci;
use crate::legacy_range_coder::{
    build_legacy_cdf, build_legacy_pair_packed_cdf, decode_legacy_freq_table,
    is_rare_symbol_cluster, LegacyRangeDecoder,
};
use crate::range_coder::{Cdf, RangeDecoder};
use crate::rle;

/// Typed accessor on the per-plane channel-header byte used by the
/// modern arithmetic-coded RGB / RGBA / YV12 / YUY2 frame types
/// (frame types 2, 3, 4, 8, 10, 11) per `spec/03` §2.1 + `spec/06`
/// §1.1.
///
/// The wire-level dispatcher partitions the seven accepted header
/// values into four sub-paths: bare arithmetic (`0x00`),
/// arithmetic with inline RLE post-processing
/// (`0x01..=0x03`, escape_len = header), raw plane (`0x04`),
/// raw with RLE post-processing (`0x05..=0x07`, escape_len =
/// header - 4), and constant-fill (`0xff`).
///
/// All other byte values are out of range and rejected as
/// [`Error::BadChannelHeader`] by [`decode_channel`].
///
/// Note that the legacy (type 7) channel-header byte uses a
/// disjoint, narrower set (`0x00` + `0x01..=0x03`) per `spec/07`
/// §1.3 + §2.3; see [`decode_legacy_channel`].
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum ChannelHeader {
    /// `0x00` — Fibonacci-coded probability prefix at offset 1
    /// followed by the modern arithmetic body. No post-process RLE.
    /// The decoder emits `n_pixels` 8-bit residuals.
    BareArithmetic,
    /// `0x01..=0x03` — Fibonacci-coded probability prefix at offset
    /// 5 followed by the modern arithmetic body that emits
    /// `pre_rle_len` symbols, then `spec/05` zero-run RLE expansion
    /// with `escape_len = header` to fill `n_pixels`. The
    /// `pre_rle_len` 32-bit field at offsets 1..4 governs the
    /// "u32 ≥ n_pixels" fall-back per `spec/06` §1.4 (rerouted to
    /// the [`BareArithmetic`](Self::BareArithmetic) shape with the
    /// prefix beginning at offset 1, not offset 5).
    ArithRle {
        /// Zero-run escape length, range `1..=3`.
        escape_len: u8,
    },
    /// `0x04` — `n_pixels` literal residual bytes at offsets
    /// `1..(1 + n_pixels)`. No entropy, no RLE.
    Raw,
    /// `0x05..=0x07` — literal residual bytes at offset 1
    /// post-processed by `spec/05` zero-run RLE expansion with
    /// `escape_len = header - 4`. No entropy.
    RawRle {
        /// Zero-run escape length, range `1..=3`.
        escape_len: u8,
    },
    /// `0xff` — constant-fill: the byte at offset 1 is replicated
    /// `n_pixels` times. The plane carries exactly two bytes on the
    /// wire (header + fill).
    ConstantFill,
}

impl ChannelHeader {
    /// Classify a channel-header byte per `spec/03` §2.1 +
    /// `spec/06` §1.1.
    ///
    /// Returns [`Error::BadChannelHeader`] for any value outside
    /// the seven-element accepted set
    /// `{0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0xff}`.
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            0x00 => Ok(Self::BareArithmetic),
            0x01..=0x03 => Ok(Self::ArithRle { escape_len: b }),
            0x04 => Ok(Self::Raw),
            0x05..=0x07 => Ok(Self::RawRle { escape_len: b - 4 }),
            0xff => Ok(Self::ConstantFill),
            other => Err(Error::BadChannelHeader(other)),
        }
    }

    /// Wire-level header byte that this variant maps back to.
    /// Round-trips with [`from_byte`](Self::from_byte) on every
    /// accepted input.
    pub fn to_byte(self) -> u8 {
        match self {
            Self::BareArithmetic => 0x00,
            Self::ArithRle { escape_len } => escape_len,
            Self::Raw => 0x04,
            Self::RawRle { escape_len } => escape_len + 4,
            Self::ConstantFill => 0xff,
        }
    }

    /// `true` if the header's wire form runs the modern arithmetic
    /// body (`BareArithmetic` or `ArithRle`). Used by encoder /
    /// decoder paths that gate on "needs a Fibonacci probability
    /// prefix".
    pub fn uses_arithmetic_body(self) -> bool {
        matches!(self, Self::BareArithmetic | Self::ArithRle { .. })
    }

    /// `true` if the header's wire form post-processes its output
    /// through the `spec/05` zero-run RLE expander (`ArithRle` or
    /// `RawRle`).
    pub fn uses_rle_postprocess(self) -> bool {
        matches!(self, Self::ArithRle { .. } | Self::RawRle { .. })
    }

    /// `Some(escape_len)` when the header's wire form carries a
    /// `spec/05` zero-run RLE escape length; `None` otherwise. The
    /// returned value is always in `1..=3`.
    pub fn rle_escape_len(self) -> Option<u8> {
        match self {
            Self::ArithRle { escape_len } | Self::RawRle { escape_len } => Some(escape_len),
            _ => None,
        }
    }

    /// `Some(offset)` when the header's wire form carries a
    /// `spec/04` Fibonacci-coded 256-entry frequency table — the
    /// returned offset is the byte position relative to channel
    /// byte 0 at which the Fibonacci bit stream begins.
    ///
    /// Per `spec/06` §1.2 the per-channel dispatcher takes one of
    /// two pointer-paths into the probability-table loader at
    /// `lagarith.dll!0x1800012b0`:
    ///
    /// * **Call site B** (the no-length-field path) — used by
    ///   `BareArithmetic`. The Fibonacci prefix begins at
    ///   **channel-data byte 1**, immediately after the header
    ///   byte.
    /// * **Call site A** (the §1.4-precondition path) — used by
    ///   `ArithRle` when the u32 pre-RLE length at channel bytes
    ///   1..5 is strictly less than the channel's pixel count. The
    ///   Fibonacci prefix begins at **channel-data byte 5**, after
    ///   the 4-byte u32 length field.
    ///
    /// Returns `None` for the three non-arithmetic wire forms —
    /// `Raw` (literal bytes at byte 1), `RawRle` (RLE-compressed
    /// bytes at byte 1), and `ConstantFill` (a single fill byte at
    /// byte 1) — which carry no Fibonacci-coded freq table by
    /// construction (`spec/03` §2.1 rows for headers `0x04` and
    /// `0x05..=0x07` plus the `0xff` solid-fill row).
    ///
    /// Note: when the `ArithRle` u32 length field at channel bytes
    /// 1..4 happens to be `>= n_pixels`, the `spec/06` §1.4
    /// fall-back rerouts the dispatch to the `BareArithmetic` shape
    /// with the Fibonacci prefix beginning at byte 1 — i.e. the
    /// effective freq-table offset on that runtime branch is `1`,
    /// not `5`. This accessor reports the static-by-header value
    /// only, so callers that need the runtime-resolved offset must
    /// re-check the u32 length field themselves; see
    /// [`decode_channel`] for the dispatcher's full rule.
    ///
    /// Mirrors [`LegacyChannelHeader::freq_table_offset`] —
    /// the legacy (type-7) channel-header byte uses a disjoint,
    /// narrower set with its own offset choices (`spec/07` §1.3 +
    /// §2.1). The two accessors share the same shape so callers can
    /// reason about either wire form uniformly.
    pub fn freq_table_offset(self) -> Option<usize> {
        match self {
            Self::BareArithmetic => Some(1),
            Self::ArithRle { .. } => Some(5),
            Self::Raw | Self::RawRle { .. } | Self::ConstantFill => None,
        }
    }

    /// Channel-prefix size in bytes — the number of leading bytes
    /// of the channel data that the dispatcher consumes for
    /// header / metadata fields before the wire-body proper
    /// begins.
    ///
    /// Per `spec/03` §2.1 + `spec/06` §1.2:
    ///
    /// * `1` for `BareArithmetic` (header only — the Fibonacci
    ///   prefix begins at byte 1).
    /// * `5` for `ArithRle` (header + 4-byte u32 pre-RLE
    ///   symbol-stream length at bytes 1..5 — the Fibonacci
    ///   prefix begins at byte 5; `spec/06` §1.2 call site A).
    /// * `1` for `Raw` (header only — the literal plane bytes
    ///   begin at byte 1).
    /// * `1` for `RawRle` (header only — the RLE-compressed plane
    ///   bytes begin at byte 1).
    /// * `1` for `ConstantFill` (header only — the fill byte sits
    ///   at byte 1).
    ///
    /// Equal to `1 + freq_table_offset().unwrap_or(1) - 1` for
    /// every variant: a structural restatement of the dispatcher's
    /// two-call-sites pointer arithmetic. Equivalently, this is
    /// the offset within the channel slice at which the wire body
    /// content `decode_channel` reads next begins — the Fibonacci
    /// prefix for the arithmetic forms, or the post-header
    /// payload bytes for the literal forms.
    ///
    /// Mirrors [`FrameType::prefix_size`] (which reports the
    /// channel-offset-table prefix size at the *frame* level) at
    /// the channel level. Note: when the `ArithRle` u32 length
    /// field is `>= n_pixels` the `spec/06` §1.4 fall-back rerouts
    /// the dispatcher to the `BareArithmetic` shape with prefix
    /// size `1`; this accessor reports the static-by-header value
    /// only, matching the same convention as
    /// [`freq_table_offset`](Self::freq_table_offset).
    pub fn prefix_size(self) -> usize {
        match self {
            Self::BareArithmetic | Self::Raw | Self::RawRle { .. } | Self::ConstantFill => 1,
            Self::ArithRle { .. } => 5,
        }
    }
}

/// Typed accessor on the per-plane channel-header byte used by the
/// **legacy type-7 (adaptive-CDF RGB)** frame type per `spec/07`
/// §1.3 + §2.1.
///
/// The legacy channel-header dispatcher accepts a strictly narrower
/// byte set than the modern dispatcher: `{0x00, 0x01, 0x02, 0x03}`.
/// Under outer header `0x00` the wire form carries a 1-byte
/// **inner codec-mode flag** at offset 1 (`spec/07` §1.3 final
/// paragraph + §2.5 second blockquote): the proprietary binary's
/// builder at `lagarith.dll!0x180001f60` performs a second dispatch
/// on this byte and only the `0x00` value (bare Fibonacci) appears
/// in the encoder's observed output; non-zero values select an
/// undocumented RLE-then-Fibonacci sub-path that is rejected as
/// [`Error::BadChannelHeader`] by [`decode_legacy_channel`].
///
/// All other outer-header values (everything outside
/// `{0x00, 0x01, 0x02, 0x03}`) are out of range and rejected as
/// [`Error::BadChannelHeader`].
///
/// Note that this set is disjoint from the modern
/// [`ChannelHeader`] set: the legacy fork has no `0x04` raw, no
/// `0x05..=0x07` raw-with-RLE, and no `0xff` constant-fill — those
/// wire forms exist only on the modern path (`spec/03` §2.1 +
/// `spec/06` §1.1). See [`decode_legacy_channel`] for the wire
/// dispatcher this enum classifies.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub enum LegacyChannelHeader {
    /// `0x00` — bare Fibonacci wire (`spec/07` §2.1 first bullet):
    /// 2-byte channel prefix (outer header + inner codec-mode flag),
    /// Fibonacci-coded 256-entry frequency table at offset 2, then
    /// the legacy range-coder body. The inner codec-mode flag at
    /// channel-data offset 1 must be `0x00`; non-zero values are
    /// rejected (`spec/07` §1.3 final paragraph).
    BareFib,
    /// `0x01..=0x03` — RLE-then-Fibonacci wire (`spec/07` §2.1
    /// second bullet + §2.3 / §2.4): outer header at offset 0,
    /// u32 LE post-RLE byte count at offsets 1..4, RLE-compressed
    /// Fibonacci-coded frequency table at offset 5, then the
    /// legacy range-coder body. The outer header byte doubles as
    /// the per-channel zero-run escape length.
    RleThenFib {
        /// Zero-run escape length, range `1..=3`. Equal to the
        /// outer header byte value.
        escape_len: u8,
    },
}

impl LegacyChannelHeader {
    /// Classify a legacy (type-7) outer channel-header byte per
    /// `spec/07` §1.3 + §2.1.
    ///
    /// Returns [`Error::BadChannelHeader`] for any value outside
    /// the four-element accepted set `{0x00, 0x01, 0x02, 0x03}`.
    pub fn from_byte(b: u8) -> Result<Self> {
        match b {
            0x00 => Ok(Self::BareFib),
            0x01..=0x03 => Ok(Self::RleThenFib { escape_len: b }),
            other => Err(Error::BadChannelHeader(other)),
        }
    }

    /// Wire-level outer-header byte that this variant maps back to.
    /// Round-trips with [`from_byte`](Self::from_byte) on every
    /// accepted input.
    pub fn to_byte(self) -> u8 {
        match self {
            Self::BareFib => 0x00,
            Self::RleThenFib { escape_len } => escape_len,
        }
    }

    /// `true` if the header's wire form runs the
    /// pre-Fibonacci-table RLE decompressor (`spec/07` §2.4):
    /// only [`RleThenFib`](Self::RleThenFib).
    pub fn uses_rle_pre_decompress(self) -> bool {
        matches!(self, Self::RleThenFib { .. })
    }

    /// `Some(escape_len)` when the header's wire form carries a
    /// per-channel zero-run RLE escape length (always equal to the
    /// outer header byte). `None` for [`BareFib`](Self::BareFib).
    /// Returned value is in `1..=3`.
    pub fn rle_escape_len(self) -> Option<u8> {
        match self {
            Self::RleThenFib { escape_len } => Some(escape_len),
            Self::BareFib => None,
        }
    }

    /// Byte offset at which the Fibonacci-coded 256-entry
    /// frequency table begins, relative to the channel-data
    /// pointer (channel byte 0):
    ///
    /// * `2` for [`BareFib`](Self::BareFib) — channel header byte
    ///   + inner codec-mode flag (`spec/07` §1.3 + §2.5).
    /// * `5` for [`RleThenFib`](Self::RleThenFib) — channel header
    ///   byte + 4-byte u32 post-RLE length field (`spec/07` §2.1
    ///   second bullet).
    ///
    /// Note: on the `RleThenFib` path the Fibonacci table is
    /// decoded out of the **post-RLE intermediate buffer**, not
    /// directly from the channel byte stream; this offset names
    /// where the RLE input bytes begin.
    pub fn freq_table_offset(self) -> usize {
        match self {
            Self::BareFib => 2,
            Self::RleThenFib { .. } => 5,
        }
    }

    /// Channel-prefix size in bytes — the number of leading bytes
    /// of the channel data that the legacy (type-7) dispatcher
    /// consumes for header / metadata fields before the wire body
    /// proper begins.
    ///
    /// Per `spec/07` §1.3 + §2.1:
    ///
    /// * `2` for [`BareFib`](Self::BareFib) — outer channel-header
    ///   byte at offset 0 + inner codec-mode flag byte at offset 1
    ///   (`spec/07` §1.3 final paragraph + §2.5 second blockquote;
    ///   the proprietary's histogram-and-CDF builder at
    ///   `lagarith.dll!0x180001f60` consumes the inner flag byte
    ///   before reading the Fibonacci-coded frequency table that
    ///   begins at channel-data byte 2).
    /// * `5` for [`RleThenFib`](Self::RleThenFib) — outer
    ///   channel-header byte at offset 0 + 4-byte little-endian
    ///   u32 post-RLE length field at offsets 1..5 (`spec/07` §2.1
    ///   second bullet + §2.3 / §2.4; the RLE-compressed
    ///   Fibonacci-coded frequency-table input bytes begin at
    ///   channel-data byte 5).
    ///
    /// On both legacy variants the value equals
    /// [`freq_table_offset`](Self::freq_table_offset): every
    /// legacy channel-header form carries a Fibonacci-coded
    /// frequency table (directly on the `BareFib` path; via the
    /// post-RLE intermediate buffer on the `RleThenFib` path), so
    /// the wire body the dispatcher reads next always begins at
    /// the freq-table input-byte offset. The two accessors are
    /// thus interchangeable on the legacy path but kept as two
    /// distinct names so callers reasoning about the channel
    /// layout structurally — and callers reasoning about the
    /// Fibonacci-decode entry point semantically — see the same
    /// surface as on the modern path
    /// ([`ChannelHeader::prefix_size`] /
    /// [`ChannelHeader::freq_table_offset`]).
    ///
    /// Mirrors [`ChannelHeader::prefix_size`] — the modern
    /// channel-header byte exposes the same accessor name at the
    /// channel level so callers can compute channel-data offsets
    /// uniformly across the two wire forms. Mirrors
    /// [`FrameType::prefix_size`](crate::frame::FrameType::prefix_size)
    /// — the frame-level channel-offset-table prefix size — at
    /// the channel-prefix level; together the two `prefix_size`
    /// accessors let downstream callers compute byte offsets
    /// through both prefix layers (frame-level + channel-level)
    /// of the type-7 legacy decode without re-running the
    /// dispatcher.
    pub fn prefix_size(self) -> usize {
        match self {
            Self::BareFib => 2,
            Self::RleThenFib { .. } => 5,
        }
    }
}

/// Decode one channel into a `Vec<u8>` of `n_pixels` residuals.
/// Returns the residuals; the predictor pipeline runs separately.
pub(crate) fn decode_channel(channel: &[u8], n_pixels: usize) -> Result<Vec<u8>> {
    if channel.is_empty() {
        return Err(Error::Truncated {
            context: "channel header byte",
        });
    }
    let header = channel[0];
    match header {
        0x00 => decode_arith_no_rle(channel, 1, n_pixels),
        0x01..=0x03 => {
            // `spec/06` §1.4: read u32 at bytes 1..4. If >=
            // n_pixels, fall back to header-0 dispatch (Fibonacci
            // prefix begins at byte 1, no RLE).
            if channel.len() < 5 {
                return Err(Error::Truncated {
                    context: "header 0x01..0x03 u32 length field",
                });
            }
            let u32_field =
                u32::from_le_bytes([channel[1], channel[2], channel[3], channel[4]]) as usize;
            if u32_field >= n_pixels {
                decode_arith_no_rle(channel, 1, n_pixels)
            } else {
                let escape_len = header as usize;
                decode_arith_rle(channel, 5, u32_field, n_pixels, escape_len)
            }
        }
        0x04 => {
            if channel.len() < 1 + n_pixels {
                return Err(Error::Truncated {
                    context: "header 0x04 raw plane data",
                });
            }
            Ok(channel[1..1 + n_pixels].to_vec())
        }
        0x05..=0x07 => {
            let escape_len = (header - 4) as usize;
            let body = &channel[1..];
            let (out, _) = rle::expand_raw(body, escape_len, n_pixels)?;
            Ok(out)
        }
        0xff => {
            if channel.len() < 2 {
                return Err(Error::Truncated {
                    context: "header 0xff fill byte",
                });
            }
            Ok(vec![channel[1]; n_pixels])
        }
        other => Err(Error::BadChannelHeader(other)),
    }
}

/// Decode an arithmetic-coded channel (no RLE).
fn decode_arith_no_rle(channel: &[u8], prefix_offset: usize, n_pixels: usize) -> Result<Vec<u8>> {
    if channel.len() < prefix_offset {
        return Err(Error::Truncated {
            context: "channel data shorter than prefix offset",
        });
    }
    // `spec/06` §1.3 empty-channel short-circuit: header=0 + four
    // zero bytes at positions 1..4 means "leave plane at zero".
    if prefix_offset == 1 && channel.len() >= 5 {
        let u32_field = u32::from_le_bytes([channel[1], channel[2], channel[3], channel[4]]);
        if u32_field == 0 {
            return Ok(vec![0u8; n_pixels]);
        }
    }
    let prefix = &channel[prefix_offset..];
    let (freq, prefix_bytes) = fibonacci::decode_freq_table(prefix)?;
    // The wire carries the RAW histogram (`spec/04` §5); the coder's
    // model is the `0x180001050`-normalized power-of-two-total form
    // (`provenance/52`), derived here exactly as the reference's
    // model-init helper does after the prefix parse.
    let cdf = Cdf::from_wire_frequencies(&freq)?;

    let body_offset = prefix_offset + prefix_bytes;
    if channel.len() < body_offset {
        return Err(Error::Truncated {
            context: "arithmetic body offset past channel end",
        });
    }
    let body = &channel[body_offset..];
    let mut dec = RangeDecoder::new(body)?;
    let mut symbols = Vec::with_capacity(n_pixels);
    for _ in 0..n_pixels {
        symbols.push(dec.decode_symbol(&cdf)?);
    }
    Ok(symbols)
}

/// Decode one **type-7 (legacy RGB)** channel into a `Vec<u8>` of
/// `n_pixels` residuals per `spec/07` §1.3 / §2.5.
///
/// ## Header == 0x00 — bare Fibonacci wire
///
/// | offset | bytes | meaning |
/// | ------ | ----- | ------- |
/// | 0      | 1     | outer channel-header byte (= 0x00) |
/// | 1      | 1     | inner codec-mode flag (= 0x00 for bare Fibonacci) |
/// | 2..    | N     | Fibonacci-coded 256-entry freq table |
/// | 2 + N  | 0..1  | post-Fibonacci 1-byte reservation (audit/08 §3.2) |
/// | 2+N+R  | …     | legacy range-coder body |
///
/// where `N = byte_advance_count` of the Fibonacci helper and
/// `R = 1` iff the bit stream ended on a byte boundary (the
/// reservation byte is *only* present in the byte-aligned case).
///
/// ## Header ∈ `{0x01, 0x02, 0x03}` — RLE-then-Fibonacci wire
/// (per `spec/07` §2.3 / §2.4)
///
/// | offset | bytes | meaning |
/// | ------ | ----- | ------- |
/// | 0      | 1     | outer channel-header byte (= escape_len) |
/// | 1..5   | 4     | u32 LE post-RLE byte count L (≤ 256) |
/// | 5..    | M     | RLE-compressed input expanding to L bytes |
/// | 5 + M  | 0..1  | post-Fibonacci 1-byte reservation |
/// | …      | …     | legacy range-coder body |
///
/// The RLE decompressor of `spec/05` (with the same per-channel
/// escape_len = header byte, range 1..3) expands `M` input bytes
/// into an `L`-byte intermediate buffer; the Fibonacci-coded 256-
/// entry freq table is decoded out of that buffer.
pub(crate) fn decode_legacy_channel(channel: &[u8], n_pixels: usize) -> Result<Vec<u8>> {
    if channel.is_empty() {
        return Err(Error::Truncated {
            context: "legacy type-7 channel header",
        });
    }
    // Classify via the typed accessor so the wire dispatcher and
    // the [`LegacyChannelHeader`] public API share a single source
    // of truth for the legal outer-header set (`spec/07` §2.1).
    match LegacyChannelHeader::from_byte(channel[0])? {
        LegacyChannelHeader::BareFib => decode_legacy_bare_fib(channel, n_pixels),
        LegacyChannelHeader::RleThenFib { escape_len } => {
            decode_legacy_rle_then_fib(channel, n_pixels, escape_len as usize)
        }
    }
}

/// Header == 0x00 path: bare Fibonacci freq table immediately
/// after the 2-byte channel prefix.
fn decode_legacy_bare_fib(channel: &[u8], n_pixels: usize) -> Result<Vec<u8>> {
    if channel.len() < 2 {
        return Err(Error::Truncated {
            context: "legacy type-7 channel prefix",
        });
    }
    let inner = channel[1];
    if inner != 0x00 {
        // Inner codec-mode flag != 0 selects an RLE-then-Fibonacci
        // sub-path *under* outer header 0x00 — not in the binary's
        // observed encoder paths and not produced by our clean-room
        // encoder. Surface as an error rather than silently mis-
        // decoding (`spec/07` §1.3 final paragraph + §2.5 audit
        // blockquote).
        return Err(Error::BadChannelHeader(inner));
    }
    let fib_src = &channel[2..];
    let (freq, fib_bytes, fib_aligned) = decode_legacy_freq_table(fib_src)?;
    let body_offset = 2 + fib_bytes + if fib_aligned { 1 } else { 0 };
    legacy_decode_body(channel, body_offset, &freq, n_pixels)
}

/// Header ∈ {0x01, 0x02, 0x03} path: u32 length field at offset +1,
/// RLE-compressed bytes at offset +5 expanding to an `L`-byte
/// Fibonacci-coded freq-table buffer.
fn decode_legacy_rle_then_fib(
    channel: &[u8],
    n_pixels: usize,
    escape_len: usize,
) -> Result<Vec<u8>> {
    debug_assert!((1..=3).contains(&escape_len));
    if channel.len() < 5 {
        return Err(Error::Truncated {
            context: "legacy type-7 RLE-then-Fib u32 length field",
        });
    }
    let post_rle_len =
        u32::from_le_bytes([channel[1], channel[2], channel[3], channel[4]]) as usize;
    // The RLE-expanded buffer is a `spec/05` zero-run-escaped form of
    // a 256-byte (or shorter) Fibonacci-coded freq table. Per `spec/07`
    // §2.4 the proprietary's stack buffer is 256 bytes; the
    // length field is the post-RLE byte count.
    if post_rle_len == 0 || post_rle_len > 256 {
        return Err(Error::BadChannelHeader(escape_len as u8));
    }
    let rle_in = &channel[5..];
    // `expand_raw` is output-driven: it consumes only as many input
    // bytes as needed to fill `post_rle_len` output bytes. The post-
    // RLE buffer is the Fibonacci-coded freq table.
    let (fib_buffer, rle_in_consumed) = rle::expand_raw(rle_in, escape_len, post_rle_len)?;
    let (freq, fib_bytes, fib_aligned) = decode_legacy_freq_table(&fib_buffer)?;
    let _ = fib_bytes; // freq table fits within the post-RLE buffer
                       // by construction; the Fib helper consumes
                       // `fib_bytes` of it which is <= post_rle_len.
    let body_offset = 5 + rle_in_consumed + if fib_aligned { 1 } else { 0 };
    legacy_decode_body(channel, body_offset, &freq, n_pixels)
}

/// Common tail: build the CDF, run the legacy range-coder body to
/// produce `n_pixels` residual bytes.
///
/// **CDF-layout selection (round 96, audit/12 §7.1 Strategy F).**
/// Two CDF construction schemes coexist (`spec/07` §3.4):
///
/// * The **flat 257-entry CDF** ([`build_legacy_cdf`]) is the
///   cleanroom's self-roundtrip form — our own encoder
///   (`encoder.rs`) builds the same flat CDF, so its streams decode
///   byte-exactly through this path.
/// * The **pair-packed 513-entry CDF** ([`build_legacy_pair_packed_cdf`])
///   is the proprietary's form, with sentinel-`1` inter-symbol gaps.
///   Audit/12 §5..§6 proved the two are *not* bit-equivalent for the
///   rare-symbol-cluster fixture class (`freq[0] >= 0.95 * Σfreq` and
///   ≥ 3 distinct nonzero bins with `freq ∈ {1, 2}`): the sentinel
///   gaps shift rare symbols' boundaries past `total`, so the
///   proprietary decoder mis-decodes them (audit/12 §3.6).
///
/// [`is_rare_symbol_cluster`] selects the layout. A stream that hits
/// the signature was **not** produced by our encoder (Strategy E
/// re-routes such fixtures to type 1 before they reach the legacy
/// range coder), so it is a foreign / proprietary-encoded type-7
/// stream — we decode it through the pair-packed path to match the
/// proprietary decoder bit-for-bit. Common-case streams use the flat
/// path. Both share the same `spec/07` §4..§5 range-coder state
/// machine; only the CDF addressing differs.
fn legacy_decode_body(
    channel: &[u8],
    body_offset: usize,
    freq: &[u32; 256],
    n_pixels: usize,
) -> Result<Vec<u8>> {
    if channel.len() < body_offset {
        return Err(Error::Truncated {
            context: "legacy type-7 channel body offset past end",
        });
    }
    let body = &channel[body_offset..];
    let mut dec = if is_rare_symbol_cluster(freq) {
        // Proprietary-encoded rare-symbol-cluster stream: decode
        // against the pair-packed 513-entry CDF (audit/12 §7.1
        // Strategy F) to reproduce the proprietary decode bit-for-bit.
        let (pair_cdf, total) = build_legacy_pair_packed_cdf(freq)?;
        LegacyRangeDecoder::new_pair_packed(body, pair_cdf, total)?
    } else {
        let (cdf, total) = build_legacy_cdf(freq)?;
        LegacyRangeDecoder::new(body, cdf, total)?
    };
    let mut out = Vec::with_capacity(n_pixels);
    for _ in 0..n_pixels {
        out.push(dec.decode_byte()?);
    }
    Ok(out)
}

/// Decode an arithmetic-coded channel with inline RLE: produce
/// `pre_rle_symbol_count` symbols, then post-process the symbol
/// sequence with the residual zero-run RLE escape (`spec/05`) to
/// fill an `n_pixels`-sized plane buffer.
fn decode_arith_rle(
    channel: &[u8],
    prefix_offset: usize,
    pre_rle_symbol_count: usize,
    n_pixels: usize,
    escape_len: usize,
) -> Result<Vec<u8>> {
    debug_assert!((1..=3).contains(&escape_len));
    if channel.len() < prefix_offset {
        return Err(Error::Truncated {
            context: "channel data shorter than prefix offset",
        });
    }
    let prefix = &channel[prefix_offset..];
    let (freq, prefix_bytes) = fibonacci::decode_freq_table(prefix)?;
    // Same wire-raw -> normalized-model derivation as the no-RLE
    // path: the pre-RLE symbol histogram on the wire is normalized
    // to a power-of-two total before any symbol is decoded.
    let cdf = Cdf::from_wire_frequencies(&freq)?;

    let body_offset = prefix_offset + prefix_bytes;
    if channel.len() < body_offset {
        return Err(Error::Truncated {
            context: "arithmetic body offset past channel end",
        });
    }
    let body = &channel[body_offset..];
    let mut dec = RangeDecoder::new(body)?;
    let mut symbols = Vec::with_capacity(pre_rle_symbol_count);
    for _ in 0..pre_rle_symbol_count {
        symbols.push(dec.decode_symbol(&cdf)?);
    }

    // Post-process: the symbol sequence is the same form `expand_raw`
    // consumes (escape_len consecutive zeros + supplement byte, etc.).
    let (plane, _) = rle::expand_raw(&symbols, escape_len, n_pixels)?;
    Ok(plane)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_header_byte_classification() {
        // Bare arithmetic.
        assert_eq!(
            ChannelHeader::from_byte(0x00).unwrap(),
            ChannelHeader::BareArithmetic
        );

        // Arithmetic + RLE, escape_len = 1..=3.
        for h in 0x01u8..=0x03 {
            let ch = ChannelHeader::from_byte(h).unwrap();
            assert_eq!(ch, ChannelHeader::ArithRle { escape_len: h });
            assert!(ch.uses_arithmetic_body());
            assert!(ch.uses_rle_postprocess());
            assert_eq!(ch.rle_escape_len(), Some(h));
            assert_eq!(ch.to_byte(), h);
        }

        // Raw plane (no entropy, no RLE).
        let raw = ChannelHeader::from_byte(0x04).unwrap();
        assert_eq!(raw, ChannelHeader::Raw);
        assert!(!raw.uses_arithmetic_body());
        assert!(!raw.uses_rle_postprocess());
        assert_eq!(raw.rle_escape_len(), None);
        assert_eq!(raw.to_byte(), 0x04);

        // Raw + RLE, escape_len = header - 4.
        for h in 0x05u8..=0x07 {
            let ch = ChannelHeader::from_byte(h).unwrap();
            assert_eq!(ch, ChannelHeader::RawRle { escape_len: h - 4 });
            assert!(!ch.uses_arithmetic_body());
            assert!(ch.uses_rle_postprocess());
            assert_eq!(ch.rle_escape_len(), Some(h - 4));
            assert_eq!(ch.to_byte(), h);
        }

        // Constant fill.
        let fill = ChannelHeader::from_byte(0xff).unwrap();
        assert_eq!(fill, ChannelHeader::ConstantFill);
        assert!(!fill.uses_arithmetic_body());
        assert!(!fill.uses_rle_postprocess());
        assert_eq!(fill.rle_escape_len(), None);
        assert_eq!(fill.to_byte(), 0xff);
    }

    #[test]
    fn channel_header_rejects_out_of_range_bytes() {
        // `spec/06` §1.1: anything not in {0x00..=0x07, 0xff} is
        // out-of-range. Spot-check the boundaries plus a handful of
        // mid-range values that aren't legal headers.
        for b in [0x08u8, 0x09, 0x10, 0x80, 0xfe] {
            assert!(
                matches!(
                    ChannelHeader::from_byte(b),
                    Err(Error::BadChannelHeader(x)) if x == b,
                ),
                "byte 0x{b:02x} should be rejected as BadChannelHeader"
            );
        }
    }

    #[test]
    fn channel_header_roundtrip_to_byte() {
        // Every accepted header byte round-trips losslessly.
        for b in (0x00u8..=0x07).chain(std::iter::once(0xffu8)) {
            let ch = ChannelHeader::from_byte(b).unwrap();
            assert_eq!(ch.to_byte(), b);
            // And re-classifying the round-tripped byte returns the
            // same variant.
            assert_eq!(ChannelHeader::from_byte(ch.to_byte()).unwrap(), ch);
        }
    }

    #[test]
    fn legacy_channel_header_byte_classification() {
        // `spec/07` §2.1 first bullet — outer header 0x00 is the
        // bare-Fibonacci wire (2-byte prefix; freq table at offset 2).
        let bare = LegacyChannelHeader::from_byte(0x00).unwrap();
        assert_eq!(bare, LegacyChannelHeader::BareFib);
        assert!(!bare.uses_rle_pre_decompress());
        assert_eq!(bare.rle_escape_len(), None);
        assert_eq!(bare.freq_table_offset(), 2);
        assert_eq!(bare.to_byte(), 0x00);

        // `spec/07` §2.1 second bullet — outer header 0x01..=0x03
        // is the RLE-then-Fibonacci wire (5-byte prefix; RLE bytes
        // at offset 5). The escape_len equals the outer-header byte.
        for h in 0x01u8..=0x03 {
            let lc = LegacyChannelHeader::from_byte(h).unwrap();
            assert_eq!(lc, LegacyChannelHeader::RleThenFib { escape_len: h });
            assert!(lc.uses_rle_pre_decompress());
            assert_eq!(lc.rle_escape_len(), Some(h));
            assert_eq!(lc.freq_table_offset(), 5);
            assert_eq!(lc.to_byte(), h);
        }
    }

    #[test]
    fn legacy_channel_header_rejects_out_of_range_bytes() {
        // `spec/07` §2.1: the legal outer-header set is strictly
        // `{0x00, 0x01, 0x02, 0x03}`. Every other byte (including
        // the modern-only headers 0x04..=0x07 and 0xff) must surface
        // BadChannelHeader carrying the offending byte.
        for b in [0x04u8, 0x05, 0x06, 0x07, 0x08, 0x10, 0x7f, 0x80, 0xfe, 0xff] {
            assert!(
                matches!(
                    LegacyChannelHeader::from_byte(b),
                    Err(Error::BadChannelHeader(x)) if x == b,
                ),
                "legacy byte 0x{b:02x} should be rejected as BadChannelHeader"
            );
        }
    }

    #[test]
    fn legacy_channel_header_roundtrip_to_byte() {
        // Every accepted outer-header byte round-trips losslessly,
        // and re-classification of `to_byte()` reproduces the same
        // variant (i.e. `from_byte ∘ to_byte = id` on the accepted
        // set `{0x00, 0x01, 0x02, 0x03}`).
        for b in 0x00u8..=0x03 {
            let lc = LegacyChannelHeader::from_byte(b).unwrap();
            assert_eq!(lc.to_byte(), b);
            assert_eq!(LegacyChannelHeader::from_byte(lc.to_byte()).unwrap(), lc);
        }
    }

    /// `LegacyChannelHeader::prefix_size` matches `spec/07` §1.3 +
    /// §2.1: 2 bytes for `BareFib` (outer header byte + inner
    /// codec-mode flag byte) and 5 bytes for `RleThenFib` (outer
    /// header byte + 4-byte u32 LE post-RLE length field). Pinned
    /// per-byte across the full four-element accepted set
    /// `{0x00, 0x01, 0x02, 0x03}`.
    #[test]
    fn legacy_channel_header_prefix_size() {
        assert_eq!(
            LegacyChannelHeader::from_byte(0x00).unwrap().prefix_size(),
            2,
            "BareFib (0x00) prefix is outer header + inner codec-mode flag = 2 bytes",
        );
        for h in 0x01u8..=0x03 {
            assert_eq!(
                LegacyChannelHeader::from_byte(h).unwrap().prefix_size(),
                5,
                "RleThenFib (escape_len={h}) prefix is outer header + u32 LE length = 5 bytes",
            );
        }
    }

    /// On the legacy (type-7) path the wire body the dispatcher
    /// reads next is always the Fibonacci-coded frequency-table
    /// input bytes — directly on the `BareFib` path; via the
    /// post-RLE intermediate buffer on the `RleThenFib` path.
    /// `prefix_size` must therefore equal `freq_table_offset` on
    /// every accepted variant.
    #[test]
    fn legacy_channel_header_prefix_size_equals_freq_table_offset() {
        for b in 0x00u8..=0x03 {
            let lc = LegacyChannelHeader::from_byte(b).unwrap();
            assert_eq!(
                lc.prefix_size(),
                lc.freq_table_offset(),
                "byte 0x{b:02x}: prefix_size must equal freq_table_offset on every legacy variant",
            );
        }
    }

    /// `LegacyChannelHeader::prefix_size` reports the same value on
    /// the `RleThenFib` variant as `ChannelHeader::prefix_size` on
    /// the modern `ArithRle` variant — both are the
    /// header + 4-byte u32 length field, 5 bytes total. The two
    /// wire forms parallel each other at the channel-prefix level
    /// even though the legacy set lacks the modern raw / raw-RLE /
    /// constant-fill sub-paths (`spec/03` §2.1 + `spec/07` §2.1).
    #[test]
    fn legacy_and_modern_prefix_size_agree_on_rle_subpath() {
        for h in 0x01u8..=0x03 {
            let modern_arith_rle = ChannelHeader::from_byte(h).unwrap();
            let legacy_rle_then_fib = LegacyChannelHeader::from_byte(h).unwrap();
            assert_eq!(
                modern_arith_rle.prefix_size(),
                legacy_rle_then_fib.prefix_size(),
                "modern ArithRle(0x{h:02x}) and legacy RleThenFib(0x{h:02x}) must share prefix_size",
            );
            assert_eq!(modern_arith_rle.prefix_size(), 5);
        }
    }

    /// `freq_table_offset` matches `spec/06` §1.2 + `spec/03` §2.1:
    /// `Some(1)` for `BareArithmetic` (call site B — Fibonacci
    /// prefix begins at channel-data byte 1), `Some(5)` for
    /// `ArithRle` (call site A — prefix begins at byte 5 after the
    /// 4-byte u32 length field at bytes 1..4), and `None` for the
    /// three non-arithmetic forms `Raw` / `RawRle` / `ConstantFill`
    /// (which carry no Fibonacci-coded freq table).
    #[test]
    fn channel_header_freq_table_offset() {
        assert_eq!(
            ChannelHeader::from_byte(0x00).unwrap().freq_table_offset(),
            Some(1),
        );
        for h in 0x01u8..=0x03 {
            assert_eq!(
                ChannelHeader::from_byte(h).unwrap().freq_table_offset(),
                Some(5),
                "ArithRle escape_len={h} should expose Fib prefix at byte 5",
            );
        }
        assert_eq!(
            ChannelHeader::from_byte(0x04).unwrap().freq_table_offset(),
            None,
        );
        for h in 0x05u8..=0x07 {
            assert_eq!(
                ChannelHeader::from_byte(h).unwrap().freq_table_offset(),
                None,
                "RawRle escape_len={} should have no Fib prefix",
                h - 4,
            );
        }
        assert_eq!(
            ChannelHeader::from_byte(0xff).unwrap().freq_table_offset(),
            None,
        );
    }

    /// `prefix_size` matches `spec/03` §2.1 + `spec/06` §1.2:
    /// 1 byte for every variant that consumes only the header byte
    /// before the wire body proper begins (`BareArithmetic`,
    /// `Raw`, `RawRle`, `ConstantFill`), and 5 bytes for `ArithRle`
    /// (header + 4-byte u32 pre-RLE length field).
    #[test]
    fn channel_header_prefix_size() {
        assert_eq!(ChannelHeader::from_byte(0x00).unwrap().prefix_size(), 1);
        for h in 0x01u8..=0x03 {
            assert_eq!(
                ChannelHeader::from_byte(h).unwrap().prefix_size(),
                5,
                "ArithRle escape_len={h} should have 5-byte prefix",
            );
        }
        assert_eq!(ChannelHeader::from_byte(0x04).unwrap().prefix_size(), 1);
        for h in 0x05u8..=0x07 {
            assert_eq!(
                ChannelHeader::from_byte(h).unwrap().prefix_size(),
                1,
                "RawRle escape_len={} should have 1-byte prefix",
                h - 4,
            );
        }
        assert_eq!(ChannelHeader::from_byte(0xff).unwrap().prefix_size(), 1);
    }

    /// The new `freq_table_offset` and `prefix_size` accessors are
    /// consistent: when a header carries a Fibonacci freq table,
    /// the prefix size equals the freq-table offset (the Fibonacci
    /// bit stream begins exactly where the header machinery ends);
    /// when there is no freq table, the prefix size is 1 (just the
    /// header byte itself, with the payload — raw bytes, RLE
    /// bytes, or fill byte — beginning at byte 1).
    #[test]
    fn channel_header_freq_table_offset_consistent_with_prefix_size() {
        for b in (0x00u8..=0x07).chain(std::iter::once(0xffu8)) {
            let ch = ChannelHeader::from_byte(b).unwrap();
            match ch.freq_table_offset() {
                Some(off) => assert_eq!(
                    off,
                    ch.prefix_size(),
                    "byte 0x{b:02x}: freq_table_offset must equal prefix_size on arithmetic forms",
                ),
                None => assert_eq!(
                    ch.prefix_size(),
                    1,
                    "byte 0x{b:02x}: non-arithmetic forms must have a 1-byte prefix",
                ),
            }
        }
    }

    /// `freq_table_offset` and `uses_arithmetic_body` are
    /// equivalent classifiers — a channel-header byte uses the
    /// arithmetic body iff it carries a Fibonacci-coded freq
    /// table. Verifies the two accessors agree on every byte in
    /// the accepted nine-element set.
    #[test]
    fn channel_header_freq_table_offset_implies_uses_arithmetic_body() {
        for b in (0x00u8..=0x07).chain(std::iter::once(0xffu8)) {
            let ch = ChannelHeader::from_byte(b).unwrap();
            assert_eq!(
                ch.freq_table_offset().is_some(),
                ch.uses_arithmetic_body(),
                "byte 0x{b:02x}: freq_table_offset.is_some() must match uses_arithmetic_body",
            );
        }
    }
}
