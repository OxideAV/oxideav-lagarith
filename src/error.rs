//! Crate-local error type. Concrete variants land here as the
//! decoder and (test-only) encoder grow.

/// Errors produced by the Lagarith decoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// Frame payload was empty (a NULL frame; presumed identical to
    /// the previous frame per `spec/01` §1.1). Decoders are expected
    /// to handle this at the container layer; the codec surfaces it
    /// as a distinct error so callers can disambiguate.
    NullFrame,
    /// Frame-type byte fell outside the accepted `1..=11` range
    /// (`spec/01` §1.2).
    BadFrameType(u8),
    /// Pixel format / frame-type combination is unsupported by this
    /// build. Reserved variant — every wire-recognised frame-type
    /// byte (1..=11 except 0) is implemented as of round 4; this
    /// variant remains in the enum for forward-compat use by future
    /// decoder paths that opt to bail out at dispatch.
    #[allow(dead_code)]
    UnsupportedFrameType(u8),
    /// Channel-header byte was outside the legal set
    /// `{0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0xff}`.
    BadChannelHeader(u8),
    /// Compressed frame body was truncated relative to a length
    /// field, channel-offset table, or required tail.
    Truncated {
        /// Where we ran out of bytes (informative).
        context: &'static str,
    },
    /// A channel-offset table entry pointed past the frame buffer.
    OffsetOutOfRange,
    /// Fibonacci probability prefix encoded a value larger than the
    /// 7-entry x86-64 table can represent (would index out of bounds
    /// per `spec/04` §2.4).
    FibonacciOverflow,
    /// Decoded probability table summed to zero — every symbol has
    /// frequency zero. The range coder cannot decode against an
    /// empty CDF.
    EmptyProbabilityTable,
    /// Caller asked for a frame whose dimensions do not match a
    /// supported (width, height, pixel-format) tuple.
    BadDimensions {
        /// Width as understood by the caller.
        width: u32,
        /// Height as understood by the caller.
        height: u32,
    },
    /// A NULL frame (zero-byte payload) was supplied to the stateful
    /// decoder before any predecessor frame existed. Per `spec/01`
    /// §1.1 a NULL frame "is presumed unchanged from the previous
    /// frame"; with no previous frame to copy, the decoder cannot
    /// produce output.
    NullFrameWithoutPredecessor,
    /// Caller-requested pixel format does not match the frame on the
    /// wire (e.g. asking for `Bgr24` while decoding a YV12 frame).
    PixelFormatMismatch {
        /// Frame-type byte from the wire (informative).
        frame_type: u8,
    },
    /// **Legacy type-7 only.** A type-7 channel's transmitted 256-
    /// entry frequency table matches the *rare-symbol-cluster*
    /// signature of `audit/12 §3.6` / §7.1 — `freq[0] >= 0.95 * Σfreq`
    /// **and** ≥ 3 distinct nonzero bins each with `freq ∈ {1, 2}`.
    /// Audit/12 §5..§6 retracts spec/07 §3.4's "may equivalently use
    /// a flat 257-entry CDF" allowance for this fixture class: the
    /// proprietary's pair-packed 513-entry CDF and the cleanroom's
    /// flat 257-entry CDF *are not* bit-equivalent here, so the
    /// cleanroom range coder running against this freq table would
    /// produce a different decoded byte stream from the proprietary's.
    ///
    /// Our own encoder applies *Strategy E* (`audit/12 §7.1`) and
    /// re-routes such fixtures to type 1, so a stream that hits this
    /// error was produced by some *other* encoder — most plausibly
    /// the proprietary's hypothetical type-7 writer (the shipped
    /// proprietary build is decode-only per `spec/07 §6` / §9.2 item
    /// 8) or a third-party encoder. Surfacing the error explicitly
    /// avoids the silent miscoding pre-Strategy E-on-decoder rounds
    /// would have produced.
    ///
    /// To accept these streams, a future round should land
    /// `Strategy F` (the full pair-packed 513-entry CDF + 3-refill
    /// init refactor of `audit/12 §7.1` Strategy F). That work is
    /// blocked on a proprietary-encoded type-7 fixture appearing at
    /// `samples.oxideav.org/lagarith/` — without one, Strategy F
    /// has no validation oracle (`audit/04 §5`).
    LegacyRareSymbolClusterUnsupported,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NullFrame => f.write_str("Lagarith: zero-length payload (NULL frame)"),
            Error::BadFrameType(b) => {
                write!(f, "Lagarith: frame-type byte {b} out of range 1..=11")
            }
            Error::UnsupportedFrameType(b) => {
                write!(
                    f,
                    "Lagarith: frame-type byte {b} not yet supported in this build"
                )
            }
            Error::BadChannelHeader(h) => {
                write!(f, "Lagarith: channel-header byte 0x{h:02x} unrecognised")
            }
            Error::Truncated { context } => write!(f, "Lagarith: input truncated at {context}"),
            Error::OffsetOutOfRange => {
                f.write_str("Lagarith: channel offset points past frame end")
            }
            Error::FibonacciOverflow => {
                f.write_str("Lagarith: Fibonacci prefix encodes value > 33 (decoder cap)")
            }
            Error::EmptyProbabilityTable => {
                f.write_str("Lagarith: probability table summed to zero")
            }
            Error::BadDimensions { width, height } => {
                write!(f, "Lagarith: width={width} height={height} not supported")
            }
            Error::NullFrameWithoutPredecessor => f.write_str(
                "Lagarith: NULL frame supplied to stateful decoder before any predecessor frame",
            ),
            Error::PixelFormatMismatch { frame_type } => write!(
                f,
                "Lagarith: caller-requested pixel format does not match frame-type byte {frame_type}"
            ),
            Error::LegacyRareSymbolClusterUnsupported => f.write_str(
                "Lagarith: type-7 channel's freq table matches the rare-symbol-cluster signature \
                 of audit/12 §7.1 (the cleanroom's flat 257-entry CDF and the proprietary's \
                 pair-packed 513-entry CDF are NOT bit-equivalent on this fixture class — \
                 decoding would silently miscode); this stream needs Strategy F (audit/12 §7.1) \
                 which is blocked on a proprietary-encoded type-7 fixture",
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Crate-local Result alias.
pub type Result<T> = core::result::Result<T, Error>;
