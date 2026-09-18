//! Vendor-corpus conformance pins (CI).
//!
//! `tests/vendor_corpus/` vendors the 240-stream corpus that the docs
//! workspace staged under `docs/video/lagarith/fixtures/` on
//! 2026-09-12: every `.lags` file was produced by the **vendor's own
//! encoder** from a deterministic synthetic input (`<fmt>-<W>x<H>-
//! <pattern>`), and `manifest.tsv` carries, per stream, the SHA-256 of
//! the output a byte-exact decoder must reproduce — the uncompressed
//! input for the 187 streams the vendor codec round-trips byte-exactly
//! (plus the 3-frame null sequence), or the vendor *decoder's* own
//! output for the 52 degenerate geometries at which the vendor codec is
//! itself lossy. The vendor binary is never run here; the streams and
//! digests are opaque data and the only oracle.
//!
//! Two pins per stream class:
//!
//! * [`vendor_corpus_decodes_byte_exactly`] — every stream outside the
//!   `KNOWN_GAPS` table must decode to the recorded digest, and every
//!   stream *inside* it must still miss (so the table can only shrink
//!   by fixing the decoder, never by forgetting a stream).
//! * [`vendor_corpus_inputs_roundtrip_through_encoder`] — every vendored
//!   input encodes through the public `encode_frame` and decodes back
//!   byte-exactly, and the number of streams the encoder reproduces
//!   **byte-identically to the vendor's bytes** is pinned as a floor.
//!
//! The out-of-CI driver `examples/vendor_corpus.rs` runs the same
//! comparison directly against the docs staging (with per-byte diffs
//! against the inputs).

use std::fs;
use std::path::{Path, PathBuf};

use oxideav_lagarith::{decode_frame, encode_frame, Decoder, PixelKind};

#[path = "common/sha256.rs"]
mod sha256;
use sha256::sha256_hex;

/// One manifest row.
#[derive(Debug, Clone)]
struct Stream {
    name: String,
    kind: PixelKind,
    ext: &'static str,
    width: u32,
    height: u32,
    /// `exact` (vendor round trip byte-exact; expected = input),
    /// `lossy` (expected = vendor decoder output), or `sequence`
    /// (null-frame run; expected = the shared input on every frame).
    class: String,
    frame_type: String,
    headers: String,
    expected_sha256: String,
}

fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/vendor_corpus")
}

fn load_manifest() -> Vec<Stream> {
    let text = fs::read_to_string(corpus_dir().join("manifest.tsv")).expect("manifest.tsv");
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            assert_eq!(f.len(), 9, "manifest row: {l}");
            let (kind, ext) = match f[1] {
                "rgb24" => (PixelKind::Bgr24, "bgr24"),
                "rgb32" => (PixelKind::Bgra32, "bgr32"),
                "rgba" => (PixelKind::Bgra32, "bgra"),
                "yuy2" => (PixelKind::Yuy2, "yuy2"),
                "yv12" => (PixelKind::Yv12, "yv12"),
                other => panic!("manifest format {other}"),
            };
            Stream {
                name: f[0].to_owned(),
                kind,
                ext,
                width: f[2].parse().unwrap(),
                height: f[3].parse().unwrap(),
                class: f[4].to_owned(),
                frame_type: f[5].to_owned(),
                headers: f[6].to_owned(),
                expected_sha256: f[7].to_owned(),
            }
        })
        .collect()
}

fn frames_of(s: &Stream) -> Vec<Vec<u8>> {
    let dir = corpus_dir().join(&s.name);
    let single = dir.join("frame.lags");
    if single.exists() {
        return vec![fs::read(single).unwrap()];
    }
    let mut v = Vec::new();
    for i in 0.. {
        match fs::read(dir.join(format!("frame{i}.lags"))) {
            Ok(b) => v.push(b),
            Err(_) => break,
        }
    }
    assert!(!v.is_empty(), "{}: no frames", s.name);
    v
}

fn input_of(s: &Stream) -> Option<Vec<u8>> {
    fs::read(corpus_dir().join(&s.name).join(format!("input.{}", s.ext))).ok()
}

/// Decode every frame of the stream and report whether *all* of them
/// hash to the expected digest.
fn decodes_to_expected(s: &Stream) -> Result<bool, String> {
    let frames = frames_of(s);
    let mut dec = Decoder::new();
    for (i, f) in frames.iter().enumerate() {
        let out = if frames.len() == 1 {
            decode_frame(f, s.width, s.height, s.kind)
        } else {
            dec.decode(f, s.width, s.height, s.kind)
        }
        .map_err(|e| format!("{}: frame{i}: {e}", s.name))?;
        if sha256_hex(&out.pixels) != s.expected_sha256 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Streams the decoder does not yet reproduce, each with the reason.
/// The conformance test asserts these still miss, so a fix must remove
/// its rows here in the same commit.
const KNOWN_GAPS: &[(&str, &str)] = &[
    ("rgb24-1x2-edges", "host DIB-stride re-layout of 24-bpp rows (spec/06 §3.2 step 1 validated note); the vendor round trip passes only because the pad byte equals the input"),
    ("rgb24-1x2-flat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-1x2-grey", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-1x2-nearflat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-1x2-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-1x2-ramp", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-2x2-edges", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-2x2-flat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-2x2-gradient", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-2x2-grey", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-2x2-nearflat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-2x2-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-2x2-ramp", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-33x27-edges", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-33x27-flat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-33x27-gradient", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-33x27-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-3x3-edges", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-3x3-flat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-3x3-gradient", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-3x3-grey", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-3x3-nearflat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-3x3-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-3x3-ramp", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-5x7-edges", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-5x7-flat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-5x7-grey", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-5x7-gradient", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-5x7-nearflat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-5x7-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb24-5x7-ramp", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-1x1-edges", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-1x1-flat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-1x1-grey", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-1x1-nearflat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-1x1-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-1x1-ramp", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-2x1-edges", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-2x1-flat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-2x1-grey", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-2x1-nearflat", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-2x1-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("rgb32-2x1-ramp", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
    ("yv12-4x2-noise", "vendor-lossy geometry (host DIB-stride / degenerate-size quirks, spec/06 §3.2 / §3.7 / §3.8)"),
];

/// Scorecard floors: (vendor-byte-exact streams incl. the null
/// sequence, vendor-lossy streams reproducing the vendor decoder).
/// Raised in the same commit as each decoder fix.
const EXPECTED_SCORECARD: (usize, usize) = (187, 9);

#[test]
fn vendor_corpus_decodes_byte_exactly() {
    let streams = load_manifest();
    assert_eq!(streams.len(), 240, "corpus size");
    let gap = |name: &str| KNOWN_GAPS.iter().find(|(n, _)| *n == name);
    for (n, _) in KNOWN_GAPS {
        assert!(
            streams.iter().any(|s| s.name == *n),
            "KNOWN_GAPS names an unknown stream {n}"
        );
    }
    let (mut exact_ok, mut exact_total, mut lossy_ok, mut lossy_total) = (0, 0, 0, 0);
    let mut unexpected = Vec::new();
    let mut stale_gaps = Vec::new();
    for s in &streams {
        let ok = decodes_to_expected(s).unwrap_or_else(|e| panic!("{e}"));
        let is_lossy = s.class == "lossy";
        if is_lossy {
            lossy_total += 1;
            lossy_ok += usize::from(ok);
        } else {
            exact_total += 1;
            exact_ok += usize::from(ok);
        }
        match (ok, gap(&s.name)) {
            (true, None) | (false, Some(_)) => {}
            (false, None) => unexpected.push(format!(
                "{} (type {}, headers {})",
                s.name, s.frame_type, s.headers
            )),
            (true, Some((_, reason))) => {
                stale_gaps.push(format!("{} — listed as: {reason}", s.name))
            }
        }
    }
    assert!(
        unexpected.is_empty(),
        "vendor streams no longer byte-exact: {unexpected:#?}"
    );
    assert!(
        stale_gaps.is_empty(),
        "streams now byte-exact but still listed in KNOWN_GAPS (remove them): {stale_gaps:#?}"
    );
    assert_eq!(
        (exact_ok, lossy_ok),
        EXPECTED_SCORECARD,
        "scorecard drifted: byte-exact {exact_ok}/{exact_total}, vendor-lossy parity {lossy_ok}/{lossy_total}"
    );
}

/// Floor on the number of vendored inputs whose `encode_frame` output
/// is byte-identical to the vendor's own stream (encoder parity).
const ENCODER_VENDOR_IDENTICAL_FLOOR: usize = 23;

#[test]
fn vendor_corpus_inputs_roundtrip_through_encoder() {
    let streams = load_manifest();
    let mut identical = 0;
    let mut encoded = 0;
    for s in &streams {
        let Some(input) = input_of(s) else {
            continue;
        };
        if s.class != "exact" {
            // The vendor codec itself does not round-trip these
            // inputs; the encoder-side claim is only about geometries
            // the wire format defines losslessly.
            continue;
        }
        let frame = encode_frame(&input, s.width, s.height, s.kind)
            .unwrap_or_else(|e| panic!("{}: encode: {e}", s.name));
        let back = decode_frame(&frame, s.width, s.height, s.kind)
            .unwrap_or_else(|e| panic!("{}: decode of own encode: {e}", s.name));
        assert_eq!(back.pixels, input, "{}: encoder self-roundtrip", s.name);
        encoded += 1;
        let vendor = frames_of(s);
        if vendor.len() == 1 && vendor[0] == frame {
            identical += 1;
        }
    }
    eprintln!("encoder vendor-identical streams: {identical}/{encoded}");
    assert!(
        encoded > 150,
        "expected the vendored inputs to be present ({encoded})"
    );
    assert!(
        identical >= ENCODER_VENDOR_IDENTICAL_FLOOR,
        "encoder vendor-identical streams dropped to {identical} (floor {ENCODER_VENDOR_IDENTICAL_FLOOR})"
    );
}
