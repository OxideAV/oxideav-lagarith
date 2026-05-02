//! Bit-exact integration test against a real Lagarith AVI fixture.
//!
//! `tests/data/solid5_720x480_rgba.avi` is the first 5 frames of the
//! `samples.ffmpeg.org/V-codecs/lagarith/2889-assassin_OL.avi` clip,
//! extracted with `ffmpeg -c:v copy -frames:v 5`. Each of those frames is
//! a SOLID_RGBA opcode (per the trace doc, the assassin clip opens with
//! pure-black frames) so the expected pixel buffer is all zeros.
//!
//! The test decodes every packet via [`oxideav_lagarith::decode_packet`]
//! and asserts the output is byte-identical to what `ffmpeg -i ... -pix_fmt
//! rgba` produces for the same inputs (a `720*480*4`-byte buffer of zeros
//! per frame).

use oxideav_lagarith::decode_packet;

const WIDTH: usize = 720;
const HEIGHT: usize = 480;
const FRAME_BYTES: usize = WIDTH * HEIGHT * 4;
const EXPECTED_FRAMES: usize = 5;

#[test]
fn decodes_real_lagarith_avi_bit_exact() {
    let avi_path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/data/solid5_720x480_rgba.avi"
    );
    let avi = std::fs::read(avi_path).expect("fixture present");

    let packets = extract_movi_packets(&avi);
    assert_eq!(
        packets.len(),
        EXPECTED_FRAMES,
        "expected {EXPECTED_FRAMES} `00dc` chunks in fixture"
    );

    for (i, payload) in packets.iter().enumerate() {
        // All five frames in this fixture are SOLID_RGBA (frametype 0x09)
        // with R=G=B=A=0 — the trace doc confirms the clip's opening run
        // of pure-black frames.
        assert_eq!(payload[0], 0x09, "frame {i} should be SOLID_RGBA");

        let vf = decode_packet(payload, WIDTH, HEIGHT, None)
            .unwrap_or_else(|e| panic!("frame {i}: decode failed: {e}"));
        assert_eq!(vf.planes.len(), 1, "packed RGBA = 1 plane");
        let p = &vf.planes[0];
        assert_eq!(p.stride, WIDTH * 4, "stride matches packed RGBA");
        assert_eq!(p.data.len(), FRAME_BYTES, "frame size matches WxH*4");
        // Bit-exact compare against the ffmpeg-decoded reference (all zeros).
        let expected = vec![0u8; FRAME_BYTES];
        assert_eq!(
            p.data, expected,
            "frame {i}: byte-by-byte mismatch vs ffmpeg reference"
        );
    }
}

#[test]
fn registry_round_trip() {
    use oxideav_core::CodecRegistry;
    let mut reg = CodecRegistry::new();
    oxideav_lagarith::register(&mut reg);
    // The codec is now registered; reg.all_implementations() must include it.
    let mut found = false;
    for (id, _impl) in reg.all_implementations() {
        if id.as_str() == oxideav_lagarith::CODEC_ID_STR {
            found = true;
            break;
        }
    }
    assert!(
        found,
        "lagarith should be discoverable in the registry after register()"
    );
}

// ────────────────────────── tiny AVI helper ──────────────────────────

/// Pull every `00dc` chunk payload out of the `movi` LIST. Sufficient for
/// the fixture; not a real AVI demuxer.
fn extract_movi_packets(avi: &[u8]) -> Vec<&[u8]> {
    let movi = avi
        .windows(4)
        .position(|w| w == b"movi")
        .expect("fixture has movi tag");
    // The 4 bytes at `movi` are part of the LIST sub-id; chunks start
    // immediately after.
    let mut pos = movi + 4;
    let mut out = Vec::new();
    while pos + 8 <= avi.len() {
        let id = &avi[pos..pos + 4];
        let sz =
            u32::from_le_bytes([avi[pos + 4], avi[pos + 5], avi[pos + 6], avi[pos + 7]]) as usize;
        pos += 8;
        if id == b"idx1" {
            break;
        }
        if id == b"JUNK" {
            // skip with word-pad
            pos += sz + (sz & 1);
            continue;
        }
        // 00dc / 01wb / etc. — collect 00dc only.
        if pos + sz > avi.len() {
            break;
        }
        if id == b"00dc" {
            out.push(&avi[pos..pos + sz]);
        }
        pos += sz + (sz & 1);
    }
    out
}
