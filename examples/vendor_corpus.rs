//! Vendor-corpus scorecard driver (out of CI).
//!
//! Walks a directory of vendor-encoded Lagarith fixtures — one
//! sub-directory per stream holding `frame.lags` (or `frame0.lags`,
//! `frame1.lags`, … for a null-frame sequence) and a `notes.md`
//! recording the SHA-256 of the vendor decoder's output — decodes
//! every stream through the public [`decode_frame`] / [`Decoder`]
//! entry points and prints a per-stream verdict plus the aggregate
//! scorecard (byte-exact vs. vendor-lossy families).
//!
//! The fixture files are opaque data: the vendor codec binary itself
//! is never run here, and the only oracle is the recorded output hash
//! (plus the uncompressed input when it is present, for diffing).
//!
//! Run with:
//!     cargo run --release --example vendor_corpus -- <fixtures-dir> [filter]

use std::fs;
use std::path::Path;

use oxideav_lagarith::{
    decode_frame, decode_frame_vendor_layout, encode_frame, Decoder, FrameType, PixelKind,
};

#[path = "../tests/common/sha256.rs"]
mod sha256;

struct Notes {
    input_sha: Option<String>,
    vendor_sha: Option<String>,
    roundtrip_exact: bool,
    frame_type: Option<String>,
}

fn parse_notes(text: &str) -> Notes {
    let mut n = Notes {
        input_sha: None,
        vendor_sha: None,
        roundtrip_exact: false,
        frame_type: None,
    };
    for line in text.lines() {
        let t = line.trim_start_matches("- ").trim();
        if t.starts_with("Input:") {
            n.input_sha = extract_sha(t);
        } else if t.starts_with("Decoded with the same DLL") {
            n.vendor_sha = extract_sha(t);
        } else if t.starts_with("Round trip:") {
            n.roundtrip_exact = t.contains("**byte-exact**");
        } else if t.starts_with("Frame type byte:") {
            n.frame_type = t.split('`').nth(1).map(str::to_owned);
        }
    }
    n
}

fn extract_sha(line: &str) -> Option<String> {
    let idx = line.find("SHA-256 `")?;
    let rest = &line[idx + "SHA-256 `".len()..];
    let end = rest.find('`')?;
    Some(rest[..end].to_owned())
}

fn geometry(name: &str) -> Option<(PixelKind, &'static str, u32, u32)> {
    let mut parts = name.split('-');
    let fmt = parts.next()?;
    let dims = parts.next()?;
    let (w, h) = dims.split_once('x')?;
    let (kind, ext) = match fmt {
        "rgb24" => (PixelKind::Bgr24, "bgr24"),
        "rgb32" => (PixelKind::Bgra32, "bgr32"),
        "rgba" => (PixelKind::Bgra32, "bgra"),
        "yuy2" => (PixelKind::Yuy2, "yuy2"),
        "yv12" => (PixelKind::Yv12, "yv12"),
        _ => return None,
    };
    Some((kind, ext, w.parse().ok()?, h.parse().ok()?))
}

fn hex(d: &[u8; 32]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn first_diff(a: &[u8], b: &[u8]) -> String {
    if a.len() != b.len() {
        return format!("len {} vs {}", a.len(), b.len());
    }
    let n = a.iter().zip(b).filter(|(x, y)| x != y).count();
    let first = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(0);
    format!("{n} bytes differ, first at {first}")
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args
        .next()
        .expect("usage: vendor_corpus <fixtures-dir> [filter]");
    let filter = args.next();
    let dump_dir = std::env::var("LAGS_DUMP").ok();
    let mut names: Vec<String> = fs::read_dir(&dir)
        .expect("read fixtures dir")
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| filter.as_ref().map_or(true, |f| n.contains(f.as_str())))
        .collect();
    names.sort();

    let (mut exact_ok, mut exact_total, mut lossy_ok, mut lossy_total) = (0, 0, 0, 0);
    let mut failures = Vec::new();
    for name in &names {
        let base = Path::new(&dir).join(name);
        let notes = parse_notes(&fs::read_to_string(base.join("notes.md")).unwrap_or_default());
        let Some((kind, ext, w, h)) = geometry(name) else {
            continue;
        };
        let input = fs::read(base.join(format!("input.{ext}"))).ok();
        // A multi-frame sequence records no round-trip verdict of its
        // own: its expected output is the (shared) uncompressed input.
        let sequence = !base.join("frame.lags").exists();
        let roundtrip_exact = notes.roundtrip_exact || (sequence && input.is_some());
        let expected_sha = if notes.roundtrip_exact {
            notes.input_sha.clone()
        } else if sequence {
            input.as_ref().map(|inp| hex(&sha256::sha256(inp)))
        } else {
            notes.vendor_sha.clone()
        };
        let frames: Vec<Vec<u8>> = if base.join("frame.lags").exists() {
            vec![fs::read(base.join("frame.lags")).unwrap()]
        } else {
            let mut v = Vec::new();
            for i in 0.. {
                match fs::read(base.join(format!("frame{i}.lags"))) {
                    Ok(b) => v.push(b),
                    Err(_) => break,
                }
            }
            v
        };
        if frames.is_empty() {
            continue;
        }
        let mut dec = Decoder::new();
        let mut verdict = String::new();
        let mut ok = true;
        for (i, f) in frames.iter().enumerate() {
            // Vendor-lossy streams are compared against the vendor
            // decoder's own host-buffer output, so they go through the
            // vendor-layout entry point; byte-exact streams through
            // the wire-format decode.
            let plain = std::env::var("LAGS_PLAIN").is_ok();
            let res = if frames.len() == 1 && !roundtrip_exact && !plain {
                decode_frame_vendor_layout(f, w, h, kind)
            } else if frames.len() == 1 {
                decode_frame(f, w, h, kind)
            } else {
                dec.decode(f, w, h, kind)
            };
            match res {
                Ok(d) => {
                    if let Some(dd) = &dump_dir {
                        let _ = fs::write(format!("{dd}/{name}.frame{i}.out"), &d.pixels);
                    }
                    let got = hex(&sha256::sha256(&d.pixels));
                    let want = expected_sha.clone().unwrap_or_default();
                    if got != want {
                        ok = false;
                        let diff = input
                            .as_ref()
                            .filter(|_| roundtrip_exact)
                            .map(|inp| first_diff(&d.pixels, inp))
                            .unwrap_or_else(|| "no input to diff".into());
                        verdict = format!("MISMATCH frame{i} ({diff})");
                    }
                }
                Err(e) => {
                    ok = false;
                    verdict = format!("ERR frame{i}: {e}");
                }
            }
        }
        let class = if roundtrip_exact {
            exact_total += 1;
            if ok {
                exact_ok += 1;
            }
            "exact"
        } else {
            lossy_total += 1;
            if ok {
                lossy_ok += 1;
            }
            "lossy"
        };
        let ty = notes.frame_type.as_deref().unwrap_or("?");
        if ok {
            println!("PASS  {class:5} type {ty} {name}");
        } else {
            println!("FAIL  {class:5} type {ty} {name}: {verdict}");
            failures.push(name.clone());
        }
    }
    if std::env::var("LAGS_ENCODE").is_ok() {
        encoder_report(&dir, &names);
    }
    println!();
    println!("vendor-byte-exact streams: {exact_ok}/{exact_total}");
    println!("vendor-lossy streams (vendor-decoder parity): {lossy_ok}/{lossy_total}");
    if !failures.is_empty() {
        println!("failures: {}", failures.join(" "));
    }
}

/// Split a modern arithmetic frame into (type, channel slices) using
/// the public channel-offset layout (`spec/01` §2.3).
fn channels(frame: &[u8]) -> Option<(u8, Vec<&[u8]>)> {
    let ty = FrameType::from_byte(frame[0]).ok()?;
    let n = ty.n_channels();
    if n == 0 {
        return None;
    }
    let prefix = 1 + 4 * (n - 1);
    let mut offs = vec![prefix];
    for i in 0..n - 1 {
        let o = u32::from_le_bytes([
            frame[1 + 4 * i],
            frame[2 + 4 * i],
            frame[3 + 4 * i],
            frame[4 + 4 * i],
        ]) as usize;
        offs.push(o);
    }
    offs.push(frame.len());
    let mut v = Vec::new();
    for i in 0..n {
        v.push(frame.get(offs[i]..offs[i + 1])?);
    }
    Some((frame[0], v))
}

/// Encoder-side comparison against the vendor bytes: per stream, is
/// our `encode_frame` output byte-identical, and if not, does it
/// pick the same frame type and per-channel headers, and which
/// channels are byte-identical anyway.
fn encoder_report(dir: &str, names: &[String]) {
    let (mut identical, mut same_type, mut same_headers, mut total) = (0, 0, 0, 0);
    let mut ch_same = 0;
    let mut ch_total = 0;
    println!();
    for name in names {
        let base = Path::new(dir).join(name);
        let Some((kind, ext, w, h)) = geometry(name) else {
            continue;
        };
        let (Ok(input), Ok(vendor)) = (
            fs::read(base.join(format!("input.{ext}"))),
            fs::read(base.join("frame.lags")),
        ) else {
            continue;
        };
        let notes = parse_notes(&fs::read_to_string(base.join("notes.md")).unwrap_or_default());
        if !notes.roundtrip_exact {
            continue;
        }
        total += 1;
        let ours = encode_frame(&input, w, h, kind).expect("encode");
        if ours == vendor {
            identical += 1;
            println!("IDENTICAL {name} ({} bytes)", ours.len());
            continue;
        }
        let mut detail = format!("type {:#04x} vs vendor {:#04x}", ours[0], vendor[0]);
        if ours[0] == vendor[0] {
            same_type += 1;
            if let (Some((_, a)), Some((_, b))) = (channels(&ours), channels(&vendor)) {
                let ha: Vec<u8> = a.iter().map(|c| c[0]).collect();
                let hb: Vec<u8> = b.iter().map(|c| c[0]).collect();
                if ha == hb {
                    same_headers += 1;
                }
                let mut per = Vec::new();
                for (x, y) in a.iter().zip(&b) {
                    ch_total += 1;
                    if x == y {
                        ch_same += 1;
                        per.push(format!("{:02x}=", x[0]));
                    } else {
                        per.push(format!(
                            "{:02x}/{:02x}({}/{})",
                            x[0],
                            y[0],
                            x.len(),
                            y.len()
                        ));
                    }
                }
                detail = format!("channels [{}]", per.join(" "));
            }
        }
        println!(
            "DIFF      {name}: ours {} B, vendor {} B — {detail}",
            ours.len(),
            vendor.len()
        );
    }
    println!();
    println!(
        "encoder: identical {identical}/{total}; same frame type {same_type}; same headers {same_headers}; identical channels {ch_same}/{ch_total}"
    );
}
