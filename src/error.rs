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
    /// build (e.g. type 7 legacy RGB, or YUY2 / YV12 — round 1 ships
    /// arithmetic RGB24/RGB32/RGBA + literal Solid + Uncompressed
    /// only).
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
        }
    }
}

impl std::error::Error for Error {}

/// Crate-local Result alias.
pub type Result<T> = core::result::Result<T, Error>;
