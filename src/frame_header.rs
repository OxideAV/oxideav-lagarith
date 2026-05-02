//! Per-packet frame-header parsing for Lagarith.
//!
//! Lagarith packets begin with a single byte that selects both the
//! compression mode and the target pixel format. Compressed (range-coded)
//! frames then carry a 4 / 8 / 12-byte plane offset table; solid frames
//! carry the constant value(s) directly.
//!
//! See `docs/video/lagarith/lagarith-trace-reverse-engineering.md` §3.

use oxideav_core::{Error, Result};

/// One-byte frame-type opcode at offset 0 of every Lagarith packet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameType {
    Raw = 0x01,
    URgb24 = 0x02,
    ArithYuy2 = 0x03,
    ArithRgb24 = 0x04,
    SolidGray = 0x05,
    SolidColor = 0x06,
    OldArithRgb = 0x07,
    ArithRgba = 0x08,
    SolidRgba = 0x09,
    ArithYv12 = 0x0a,
    ReducedRes = 0x0b,
}

impl FrameType {
    pub fn from_u8(b: u8) -> Result<Self> {
        Ok(match b {
            0x01 => Self::Raw,
            0x02 => Self::URgb24,
            0x03 => Self::ArithYuy2,
            0x04 => Self::ArithRgb24,
            0x05 => Self::SolidGray,
            0x06 => Self::SolidColor,
            0x07 => Self::OldArithRgb,
            0x08 => Self::ArithRgba,
            0x09 => Self::SolidRgba,
            0x0a => Self::ArithYv12,
            0x0b => Self::ReducedRes,
            other => {
                return Err(Error::invalid(format!(
                    "Lagarith: unsupported frame type byte 0x{other:02x}"
                )))
            }
        })
    }

    /// `true` for the constant-value frame types whose payload is a
    /// handful of bytes (no offset table, no per-plane data).
    pub fn is_solid(self) -> bool {
        matches!(self, Self::SolidGray | Self::SolidColor | Self::SolidRgba)
    }

    /// Number of plane offsets carried in the header (excluding the
    /// implicit plane-0 offset that always sits right after the table).
    pub fn header_offset_count(self) -> usize {
        match self {
            Self::ArithRgba => 3,
            Self::ArithRgb24
            | Self::URgb24
            | Self::ArithYuy2
            | Self::ArithYv12
            | Self::OldArithRgb
            | Self::ReducedRes => 2,
            // Solid + Raw don't carry an offset table proper, but the
            // decoder still reads bytes 1..=8 unconditionally — they are
            // simply ignored on those paths. Returning 2 keeps the header
            // length math (`1 + 4 * count`) consistent.
            Self::Raw | Self::SolidGray | Self::SolidColor | Self::SolidRgba => 2,
        }
    }
}

/// Parsed frame header — frame type and the absolute plane offsets within
/// the packet. Plane 0 always begins immediately after the header.
#[derive(Clone, Debug)]
pub struct FrameHeader {
    pub frametype: FrameType,
    pub offset_plane0: usize,
    pub offset_plane1: Option<usize>,
    pub offset_plane2: Option<usize>,
    pub offset_plane3: Option<usize>,
}

impl FrameHeader {
    /// Parse a Lagarith packet header from `packet`. Returns the parsed
    /// header **and** the byte index where plane 0 begins.
    pub fn parse(packet: &[u8]) -> Result<Self> {
        if packet.is_empty() {
            return Err(Error::invalid("Lagarith: empty packet"));
        }
        let frametype = FrameType::from_u8(packet[0])?;

        // Compute header length and read offsets if applicable.
        let (off1, off2, off3, plane0_off) = match frametype {
            FrameType::ArithRgba => {
                if packet.len() < 13 {
                    return Err(Error::invalid(
                        "Lagarith: 4-plane frame header truncated (need 13 bytes)",
                    ));
                }
                (
                    Some(read_u32_le(packet, 1)? as usize),
                    Some(read_u32_le(packet, 5)? as usize),
                    Some(read_u32_le(packet, 9)? as usize),
                    13usize,
                )
            }
            FrameType::Raw
            | FrameType::SolidGray
            | FrameType::SolidColor
            | FrameType::SolidRgba => {
                // Solid + Raw don't have a real offset table. The trace
                // doc warns the decoder unconditionally reads bytes 1..=8
                // anyway; we don't actually use those bytes on the solid
                // path so we don't enforce a 9-byte header here.
                (None, None, None, 1usize)
            }
            _ => {
                if packet.len() < 9 {
                    return Err(Error::invalid(
                        "Lagarith: 3-plane frame header truncated (need 9 bytes)",
                    ));
                }
                (
                    Some(read_u32_le(packet, 1)? as usize),
                    Some(read_u32_le(packet, 5)? as usize),
                    None,
                    9usize,
                )
            }
        };

        Ok(Self {
            frametype,
            offset_plane0: plane0_off,
            offset_plane1: off1,
            offset_plane2: off2,
            offset_plane3: off3,
        })
    }
}

fn read_u32_le(buf: &[u8], off: usize) -> Result<u32> {
    if off + 4 > buf.len() {
        return Err(Error::invalid("Lagarith: u32 read past packet end"));
    }
    Ok(u32::from_le_bytes([
        buf[off],
        buf[off + 1],
        buf[off + 2],
        buf[off + 3],
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solid_rgba_header() {
        // The first frame of `2889-assassin_OL.avi` — solid black RGBA.
        let pkt = [0x09u8, 0x00, 0x00, 0x00, 0x00];
        let hdr = FrameHeader::parse(&pkt).unwrap();
        assert_eq!(hdr.frametype, FrameType::SolidRgba);
        assert_eq!(hdr.offset_plane0, 1);
        assert!(hdr.offset_plane1.is_none());
    }

    #[test]
    fn arith_rgba_header() {
        // Synthetic 4-plane header pattern.
        let mut pkt = vec![0x08u8];
        pkt.extend_from_slice(&[0x3D, 0x04, 0x00, 0x00]); // gu = 1085
        pkt.extend_from_slice(&[0x72, 0x16, 0x00, 0x00]); // bv = 5746
        pkt.extend_from_slice(&[0x6E, 0x1E, 0x00, 0x00]); // a  = 7790
        let hdr = FrameHeader::parse(&pkt).unwrap();
        assert_eq!(hdr.frametype, FrameType::ArithRgba);
        assert_eq!(hdr.offset_plane0, 13);
        assert_eq!(hdr.offset_plane1, Some(1085));
        assert_eq!(hdr.offset_plane2, Some(5746));
        assert_eq!(hdr.offset_plane3, Some(7790));
    }

    #[test]
    fn arith_rgb24_header() {
        let mut pkt = vec![0x04u8];
        pkt.extend_from_slice(&[0x17, 0x4B, 0x00, 0x00]); // gu = 19223
        pkt.extend_from_slice(&[0x63, 0xB3, 0x00, 0x00]); // bv = 45923
        let hdr = FrameHeader::parse(&pkt).unwrap();
        assert_eq!(hdr.frametype, FrameType::ArithRgb24);
        assert_eq!(hdr.offset_plane0, 9);
        assert_eq!(hdr.offset_plane1, Some(19223));
        assert_eq!(hdr.offset_plane2, Some(45923));
        assert!(hdr.offset_plane3.is_none());
    }

    #[test]
    fn unknown_frametype_rejected() {
        let pkt = [0xFFu8, 0, 0, 0, 0, 0, 0, 0, 0];
        assert!(FrameHeader::parse(&pkt).is_err());
    }
}
