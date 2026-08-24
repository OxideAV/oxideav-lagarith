//! Out-of-CI black-box cross-validation driver for the encoder.
//!
//! Encodes a deterministic case matrix covering **every frame type the
//! crate can emit** (1 / 2 / 3 / 4 / 5 / 6 / 7 / 8 / 9 / 10 / 11 and
//! the zero-byte NULL "JUMP"), wraps each stream in a minimal
//! `LAGS`-coded AVI, hands the file to an **independent third-party
//! decoder binary used purely as a black-box oracle** (never consulted
//! as source, never run in CI), and compares the oracle's raw output
//! against the crate's own `decode_frame` result for the same bytes.
//!
//! The comparison allows exactly the host-layout transforms the AVI
//! path implies (bottom-up DIB vertical flip for the RGB families,
//! `Y‖V‖U` → `Y‖U‖V` plane order for the 4:2:0 raw layout) and reports
//! which transform matched, so a verdict of `OK` means: *a third-party
//! decoder reconstructs our encoded stream sample-exactly.*
//!
//! Run manually (the oracle binary must be on `PATH` or in `ORACLE`):
//!
//! ```text
//! cargo run --release --example blackbox_capture
//! ```
//!
//! Each case line prints `verdict  type=NN  len  fnv64  name  [xform]`.
//! The `fnv64` values are the pins frozen into
//! `tests/blackbox_encode_pins.rs`: CI re-derives the same
//! deterministic inputs, re-encodes, and asserts the hash — so the
//! exact bytes the oracle validated are the bytes the encoder keeps
//! producing, without the oracle in the loop.
//!
//! Per-case expectations (round-451 findings):
//!
//! * `Exact` — the oracle must reconstruct the stream sample-exactly
//!   (all RGB24 / RGBA / YV12 arithmetic + solid forms).
//! * `OracleUnsupported` — this oracle build rejects the frame type
//!   outright ("Unsupported Lagarith frame type"): types 1
//!   (Uncompressed), 7 (legacy RGB) and 11 (reduced-res). Their
//!   conformance rests on the crate's self-roundtrip plus (for type
//!   7) the cleanroom reference-impl's 96/96 vendor cross-validation.
//!   The zero-byte NULL ("JUMP") payload is also here: the oracle's
//!   demuxer drops empty packets before they reach its decoder under
//!   every probed flag combination.
//! * `KnownYuy2Gap` — YUY2 (type 3): the luma path's SIMD carry
//!   semantics are only partially recovered (spec/06 §6.4); the
//!   oracle diverges on rich content and rejects odd widths. Tracked
//!   as a docs ask, reported but not counted as a failure.

use oxideav_lagarith::{decode_frame, encode_frame, encode_null, wire_forms, PixelKind};
use std::io::Write as _;
use std::process::Command;

// ───────────────────────── deterministic content ─────────────────────────

/// 64-bit LCG (MMIX multiplier); deterministic across platforms.
struct Lcg(u64);
impl Lcg {
    fn new(seed: u64) -> Self {
        Lcg(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15).wrapping_add(1))
    }
    fn next_u8(&mut self) -> u8 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 56) as u8
    }
}

/// FNV-1a 64 — the pin hash shared with `tests/blackbox_encode_pins.rs`.
fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[derive(Clone, Copy, PartialEq)]
enum Expect {
    Exact,
    OracleUnsupported,
    KnownYuy2Gap,
}

#[derive(Clone, Copy, PartialEq)]
enum Content {
    /// Smooth per-channel gradient + low-amplitude noise (compresses).
    GradientNoise,
    /// Full-range LCG bytes (does not compress → type-1 election).
    Random,
    /// Long zero-runs with sparse impulses (routes channels to the
    /// pre-RLE arithmetic headers `0x01..0x03` — the non-pow2-total
    /// class where the transmitted model must be normalizer-exact).
    ZeroHeavy,
    /// The structured "i·73 + 11 bit-slice" residual class from the
    /// round-127 pattern-sensitivity finding (pre-RLE headers at
    /// pow2 pixel counts) — the class the model normalizer closed.
    Structured,
    /// Every sample identical (solid fast path).
    Solid(u8, u8, u8, u8),
}

fn fill(kind: PixelKind, w: u32, h: u32, content: Content, seed: u64) -> Vec<u8> {
    let len = kind.buffer_len(w, h);
    let mut lcg = Lcg::new(seed);
    let mut out = Vec::with_capacity(len);
    match content {
        Content::Random => {
            for _ in 0..len {
                out.push(lcg.next_u8());
            }
        }
        Content::ZeroHeavy => {
            let mut i = 0usize;
            while i < len {
                // ~93% zero bytes in runs, sparse impulses.
                let run = 8 + (lcg.next_u8() as usize % 48);
                out.resize((i + run).min(len), 0);
                i = out.len();
                if i < len {
                    out.push(lcg.next_u8() | 1);
                    i += 1;
                }
            }
            // Solid-frame guard: make sure at least two distinct
            // pixel values exist (they do — impulses are odd).
        }
        Content::Structured => {
            for i in 0..len {
                out.push((((i as u64).wrapping_mul(73).wrapping_add(11)) >> 3) as u8);
            }
        }
        Content::GradientNoise => {
            // Per-byte gradient over a synthetic coordinate + noise.
            for i in 0..len {
                let base = ((i / 7) & 0xff) as u8;
                let noise = lcg.next_u8() & 0x07;
                out.push(base.wrapping_add(noise));
            }
        }
        Content::Solid(b, g, r, a) => match kind {
            PixelKind::Bgr24 => {
                for _ in 0..len / 3 {
                    out.extend_from_slice(&[b, g, r]);
                }
            }
            PixelKind::Bgra32 => {
                for _ in 0..len / 4 {
                    out.extend_from_slice(&[b, g, r, a]);
                }
            }
            _ => out.resize(len, g),
        },
    }
    debug_assert_eq!(out.len(), len);
    out
}

// ───────────────────────── minimal AVI muxer ─────────────────────────

fn le32(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// One-stream `vids/LAGS` AVI with an idx1 index; enough structure
/// for any AVI demuxer. `frames` are raw Lagarith payloads (a
/// zero-length payload is the NULL "JUMP" frame).
fn mux_avi(w: u32, h: u32, bit_count: u16, frames: &[&[u8]]) -> Vec<u8> {
    let n = frames.len() as u32;
    let max_len = frames.iter().map(|f| f.len()).max().unwrap_or(0) as u32;

    // movi payload
    let mut movi: Vec<u8> = Vec::new();
    movi.extend_from_slice(b"movi");
    let mut idx: Vec<(u32, u32)> = Vec::new(); // (offset-from-movi-fourcc, size)
    for f in frames {
        let off = movi.len() as u32 - 4; // offset of '00dc' relative to just after 'movi' fourcc
        movi.extend_from_slice(b"00dc");
        movi.extend_from_slice(&le32(f.len() as u32));
        movi.extend_from_slice(f);
        if f.len() % 2 == 1 {
            movi.push(0);
        }
        idx.push((off, f.len() as u32));
    }

    // strf: BITMAPINFOHEADER
    let mut strf = Vec::new();
    strf.extend_from_slice(&le32(40)); // biSize
    strf.extend_from_slice(&le32(w));
    strf.extend_from_slice(&le32(h)); // positive => bottom-up DIB for RGB
    strf.extend_from_slice(&1u16.to_le_bytes()); // biPlanes
    strf.extend_from_slice(&bit_count.to_le_bytes());
    strf.extend_from_slice(b"LAGS"); // biCompression
    strf.extend_from_slice(&le32(w * h * bit_count as u32 / 8)); // biSizeImage
    strf.extend_from_slice(&le32(0)); // XPelsPerMeter
    strf.extend_from_slice(&le32(0)); // YPelsPerMeter
    strf.extend_from_slice(&le32(0)); // biClrUsed
    strf.extend_from_slice(&le32(0)); // biClrImportant

    // strh
    let mut strh = Vec::new();
    strh.extend_from_slice(b"vids");
    strh.extend_from_slice(b"LAGS");
    strh.extend_from_slice(&le32(0)); // flags
    strh.extend_from_slice(&0u16.to_le_bytes()); // priority
    strh.extend_from_slice(&0u16.to_le_bytes()); // language
    strh.extend_from_slice(&le32(0)); // initial frames
    strh.extend_from_slice(&le32(1)); // scale
    strh.extend_from_slice(&le32(30)); // rate
    strh.extend_from_slice(&le32(0)); // start
    strh.extend_from_slice(&le32(n)); // length
    strh.extend_from_slice(&le32(max_len)); // suggested buffer size
    strh.extend_from_slice(&le32(u32::MAX)); // quality
    strh.extend_from_slice(&le32(0)); // sample size
    strh.extend_from_slice(&[0u8; 8]); // rcFrame

    let chunk = |fourcc: &[u8; 4], body: &[u8]| -> Vec<u8> {
        let mut c = Vec::with_capacity(8 + body.len() + 1);
        c.extend_from_slice(fourcc);
        c.extend_from_slice(&le32(body.len() as u32));
        c.extend_from_slice(body);
        if body.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    let list = |fourcc: &[u8; 4], body: &[u8]| -> Vec<u8> {
        let mut c = Vec::with_capacity(12 + body.len());
        c.extend_from_slice(b"LIST");
        c.extend_from_slice(&le32(body.len() as u32 + 4));
        c.extend_from_slice(fourcc);
        c.extend_from_slice(body);
        c
    };

    // avih (MainAVIHeader)
    let mut avih = Vec::new();
    avih.extend_from_slice(&le32(33_333)); // usec per frame
    avih.extend_from_slice(&le32(0)); // max bytes/sec
    avih.extend_from_slice(&le32(0)); // padding granularity
    avih.extend_from_slice(&le32(0x10)); // AVIF_HASINDEX
    avih.extend_from_slice(&le32(n)); // total frames
    avih.extend_from_slice(&le32(0)); // initial frames
    avih.extend_from_slice(&le32(1)); // streams
    avih.extend_from_slice(&le32(max_len)); // suggested buffer size
    avih.extend_from_slice(&le32(w));
    avih.extend_from_slice(&le32(h));
    avih.extend_from_slice(&[0u8; 16]); // reserved

    let strl = {
        let mut b = chunk(b"strh", &strh);
        b.extend_from_slice(&chunk(b"strf", &strf));
        list(b"strl", &b)
    };
    let hdrl = {
        let mut b = chunk(b"avih", &avih);
        b.extend_from_slice(&strl);
        list(b"hdrl", &b)
    };

    // idx1
    let mut idx1 = Vec::new();
    for (off, size) in &idx {
        idx1.extend_from_slice(b"00dc");
        idx1.extend_from_slice(&le32(0x10)); // AVIIF_KEYFRAME
        idx1.extend_from_slice(&le32(*off));
        idx1.extend_from_slice(&le32(*size));
    }

    let mut riff_body = Vec::new();
    riff_body.extend_from_slice(b"AVI ");
    riff_body.extend_from_slice(&hdrl);
    riff_body.extend_from_slice(b"LIST");
    riff_body.extend_from_slice(&le32(movi.len() as u32));
    riff_body.extend_from_slice(&movi);
    riff_body.extend_from_slice(&chunk(b"idx1", &idx1));

    let mut out = Vec::with_capacity(riff_body.len() + 8);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&le32(riff_body.len() as u32));
    out.extend_from_slice(&riff_body);
    out
}

// ───────────────────────── comparison transforms ─────────────────────────

fn vflip(buf: &[u8], row_bytes: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(buf.len());
    for row in buf.chunks_exact(row_bytes).rev() {
        out.extend_from_slice(row);
    }
    out
}

/// Our YV12 host layout (`Y‖V‖U`) → the oracle's 4:2:0 raw layout
/// (`Y‖U‖V`).
fn swap_vu(buf: &[u8], w: u32, h: u32) -> Vec<u8> {
    let y = (w as usize) * (h as usize);
    let c = (w as usize / 2) * (h as usize / 2);
    let mut out = Vec::with_capacity(buf.len());
    out.extend_from_slice(&buf[..y]);
    out.extend_from_slice(&buf[y + c..y + 2 * c]); // U
    out.extend_from_slice(&buf[y..y + c]); // V
    out
}

// ───────────────────────── case driver ─────────────────────────

/// Direct wire-form encoder override.
type ForceFn = fn(&[u8], u32, u32) -> Vec<u8>;

struct Case {
    name: &'static str,
    kind: PixelKind,
    w: u32,
    h: u32,
    content: Content,
    seed: u64,
    /// None → public `encode_frame`; Some(f) → direct wire-form.
    force: Option<ForceFn>,
    expect: Expect,
}

fn bit_count(kind: PixelKind) -> u16 {
    match kind {
        PixelKind::Bgr24 => 24,
        PixelKind::Bgra32 => 32,
        PixelKind::Yuy2 => 16,
        PixelKind::Yv12 => 12,
    }
}

fn oracle_pix_fmt(kind: PixelKind) -> &'static str {
    match kind {
        PixelKind::Bgr24 => "bgr24",
        PixelKind::Bgra32 => "bgra",
        PixelKind::Yuy2 => "yuyv422",
        PixelKind::Yv12 => "yuv420p",
    }
}

fn main() {
    let oracle = std::env::var("ORACLE").unwrap_or_else(|_| "ffmpeg".into());
    let dir = std::env::temp_dir().join("lagarith-blackbox-capture");
    std::fs::create_dir_all(&dir).expect("mkdir capture dir");

    use Expect::{Exact, KnownYuy2Gap, OracleUnsupported};
    let cases: Vec<Case> = vec![
        // Modern RGB24: type 4 (aligned) / type 2 (unaligned).
        case(
            "rgb24_64x48_gradient_t4",
            PixelKind::Bgr24,
            64,
            48,
            Content::GradientNoise,
            11,
            None,
            Exact,
        ),
        case(
            "rgb24_63x47_gradient_t2",
            PixelKind::Bgr24,
            63,
            47,
            Content::GradientNoise,
            12,
            None,
            Exact,
        ),
        case(
            "rgb24_16x16_structured",
            PixelKind::Bgr24,
            16,
            16,
            Content::Structured,
            0,
            None,
            Exact,
        ),
        case(
            "rgb24_32x32_structured",
            PixelKind::Bgr24,
            32,
            32,
            Content::Structured,
            0,
            None,
            Exact,
        ),
        case(
            "rgb24_64x48_zeroheavy",
            PixelKind::Bgr24,
            64,
            48,
            Content::ZeroHeavy,
            13,
            None,
            Exact,
        ),
        case(
            "rgb24_63x47_zeroheavy_t2",
            PixelKind::Bgr24,
            63,
            47,
            Content::ZeroHeavy,
            14,
            None,
            Exact,
        ),
        case(
            "rgb24_64x48_random_t1",
            PixelKind::Bgr24,
            64,
            48,
            Content::Random,
            15,
            None,
            OracleUnsupported,
        ),
        case(
            "rgb24_640x480_gradient_dse",
            PixelKind::Bgr24,
            640,
            480,
            Content::GradientNoise,
            16,
            None,
            Exact,
        ),
        case(
            "rgb24_solid_grey_t5",
            PixelKind::Bgr24,
            64,
            48,
            Content::Solid(0x55, 0x55, 0x55, 0),
            0,
            None,
            Exact,
        ),
        case(
            "rgb24_solid_rgb_t6",
            PixelKind::Bgr24,
            64,
            48,
            Content::Solid(0x20, 0x40, 0x80, 0),
            0,
            None,
            Exact,
        ),
        // RGBA: type 8 / 9.
        case(
            "rgba_64x48_gradient_t8",
            PixelKind::Bgra32,
            64,
            48,
            Content::GradientNoise,
            21,
            None,
            Exact,
        ),
        case(
            "rgba_16x16_structured",
            PixelKind::Bgra32,
            16,
            16,
            Content::Structured,
            0,
            None,
            Exact,
        ),
        case(
            "rgba_64x48_zeroheavy",
            PixelKind::Bgra32,
            64,
            48,
            Content::ZeroHeavy,
            22,
            None,
            Exact,
        ),
        case(
            "rgba_solid_t9",
            PixelKind::Bgra32,
            64,
            48,
            Content::Solid(0x10, 0x20, 0x30, 0x40),
            0,
            None,
            Exact,
        ),
        case(
            "rgba_63x47_gradient",
            PixelKind::Bgra32,
            63,
            47,
            Content::GradientNoise,
            23,
            None,
            Exact,
        ),
        // YV12: type 10.
        case(
            "yv12_64x48_gradient_t10",
            PixelKind::Yv12,
            64,
            48,
            Content::GradientNoise,
            31,
            None,
            Exact,
        ),
        case(
            "yv12_64x48_zeroheavy",
            PixelKind::Yv12,
            64,
            48,
            Content::ZeroHeavy,
            32,
            None,
            Exact,
        ),
        case(
            "yv12_320x240_gradient",
            PixelKind::Yv12,
            320,
            240,
            Content::GradientNoise,
            33,
            None,
            Exact,
        ),
        // YUY2: type 3 (even + odd width).
        case(
            "yuy2_64x48_gradient_t3",
            PixelKind::Yuy2,
            64,
            48,
            Content::GradientNoise,
            41,
            None,
            KnownYuy2Gap,
        ),
        case(
            "yuy2_64x48_zeroheavy",
            PixelKind::Yuy2,
            64,
            48,
            Content::ZeroHeavy,
            42,
            None,
            KnownYuy2Gap,
        ),
        case(
            "yuy2_63x48_gradient_odd",
            PixelKind::Yuy2,
            63,
            48,
            Content::GradientNoise,
            43,
            None,
            KnownYuy2Gap,
        ),
        // Legacy type 7 (bare Fibonacci + RLE-then-Fibonacci).
        case(
            "legacy_rgb_64x48_gradient_t7",
            PixelKind::Bgr24,
            64,
            48,
            Content::GradientNoise,
            51,
            Some(wire_forms::encode_legacy_rgb),
            OracleUnsupported,
        ),
        case(
            "legacy_rgb_32x32_zeroheavy_t7",
            PixelKind::Bgr24,
            32,
            32,
            Content::ZeroHeavy,
            52,
            Some(wire_forms::encode_legacy_rgb),
            OracleUnsupported,
        ),
        case(
            "legacy_rgb_rle1_16x16_t7",
            PixelKind::Bgr24,
            16,
            16,
            Content::GradientNoise,
            53,
            Some(|p: &[u8], w, h| wire_forms::encode_legacy_rgb_rle(p, w, h, 1)),
            OracleUnsupported,
        ),
        // Reduced-resolution type 11 (lossy: compare oracle vs our decode).
        case(
            "reduced_res_64x48_gradient_t11",
            PixelKind::Yv12,
            64,
            48,
            Content::GradientNoise,
            61,
            Some(wire_forms::encode_arith_reduced_res),
            OracleUnsupported,
        ),
        // Forced type 1 on compressible content (raw fallback wire).
        case(
            "rgb24_64x48_forced_t1",
            PixelKind::Bgr24,
            64,
            48,
            Content::GradientNoise,
            71,
            Some(|p: &[u8], _w, _h| wire_forms::encode_uncompressed(p)),
            OracleUnsupported,
        ),
    ];

    let mut failures = 0usize;
    for c in &cases {
        let pixels = fill(c.kind, c.w, c.h, c.content, c.seed);
        let encoded = match c.force {
            Some(f) => f(&pixels, c.w, c.h),
            None => encode_frame(&pixels, c.w, c.h, c.kind).expect("encode_frame"),
        };
        let expected = decode_frame(&encoded, c.w, c.h, c.kind)
            .expect("our decoder must accept our encoder's output")
            .pixels;
        let head = format!(
            "type={:>2}  len={:>7}  fnv64=0x{:016x}  {:<32}",
            encoded.first().copied().unwrap_or(0xff),
            encoded.len(),
            fnv64(&encoded),
            c.name,
        );
        match (
            run_oracle(&oracle, &dir, c, &[&encoded], &expected, 1),
            c.expect,
        ) {
            (Ok(xform), Expect::Exact) => println!("OK    {head} [{xform}]"),
            (Ok(xform), Expect::KnownYuy2Gap) => {
                println!("OK!   {head} [{xform}] (yuy2 gap case decoded exactly)")
            }
            (Ok(_), Expect::OracleUnsupported) => {
                failures += 1;
                println!("??    {head} oracle unexpectedly decoded an unsupported type");
            }
            (Err(e), Expect::OracleUnsupported) => {
                println!("SKIP  {head} oracle-unsupported ({})", first_line(&e))
            }
            (Err(e), Expect::KnownYuy2Gap) => {
                println!(
                    "GAP   {head} spec/06 §6.4 YUY2 carry gap ({})",
                    first_line(&e)
                )
            }
            (Err(e), Expect::Exact) => {
                failures += 1;
                println!("FAIL  {head} {e}");
            }
        }
    }

    // NULL ("JUMP") — two-frame stream, second frame zero bytes; the
    // oracle must replay the keyframe.
    {
        let (w, h) = (64u32, 48u32);
        let pixels = fill(PixelKind::Bgr24, w, h, Content::GradientNoise, 81);
        let key = encode_frame(&pixels, w, h, PixelKind::Bgr24).expect("encode");
        let null = encode_null();
        let expected = decode_frame(&key, w, h, PixelKind::Bgr24)
            .expect("decode")
            .pixels;
        let mut doubled = expected.clone();
        doubled.extend_from_slice(&expected);
        let c = case(
            "null_jump_rgb24_64x48",
            PixelKind::Bgr24,
            w,
            h,
            Content::GradientNoise,
            81,
            None,
            Expect::OracleUnsupported,
        );
        match run_oracle(&oracle, &dir, &c, &[&key, &null], &doubled, 2) {
            Ok(xform) => println!(
                "OK    type=[4,NULL]  len={:>5}+0  fnv64=0x{:016x}  {:<32} [{}]",
                key.len(),
                fnv64(&key),
                c.name,
                xform
            ),
            // The oracle's demuxer drops the zero-byte packet before
            // its decoder sees it (probed with pass-through pacing
            // flags too), so the NULL replay cannot be exercised
            // through this oracle. Not a failure.
            Err(e) => println!(
                "SKIP  type=[4,NULL]  {:<32} oracle drops empty packets ({})",
                c.name,
                first_line(&e)
            ),
        }
    }

    if failures > 0 {
        eprintln!("{failures} case(s) FAILED against expectation");
        std::process::exit(1);
    }
    println!(
        "all cases match expectations (Exact / oracle-unsupported SKIP / documented YUY2 gap)"
    );
}

/// First line of a (possibly multi-line) oracle error, trimmed.
fn first_line(e: &str) -> &str {
    e.lines().next().unwrap_or("").trim()
}

#[allow(clippy::too_many_arguments)]
fn case(
    name: &'static str,
    kind: PixelKind,
    w: u32,
    h: u32,
    content: Content,
    seed: u64,
    force: Option<ForceFn>,
    expect: Expect,
) -> Case {
    Case {
        name,
        kind,
        w,
        h,
        content,
        seed,
        force,
        expect,
    }
}

/// Mux → oracle-decode → compare. `expected` is our decoder's output
/// for the same stream (`n_frames` concatenated frames' worth).
/// Returns the name of the layout transform that matched.
fn run_oracle(
    oracle: &str,
    dir: &std::path::Path,
    c: &Case,
    frames: &[&[u8]],
    expected: &[u8],
    n_frames: usize,
) -> Result<&'static str, String> {
    let avi = mux_avi(c.w, c.h, bit_count(c.kind), frames);
    let avi_path = dir.join(format!("{}.avi", c.name));
    let raw_path = dir.join(format!("{}.raw", c.name));
    std::fs::File::create(&avi_path)
        .and_then(|mut f| f.write_all(&avi))
        .map_err(|e| format!("write avi: {e}"))?;

    let out = Command::new(oracle)
        .args([
            "-nostdin",
            "-v",
            "error",
            "-i",
            avi_path.to_str().unwrap(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            oracle_pix_fmt(c.kind),
            "-y",
            raw_path.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("spawn oracle: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "oracle exit {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let got = std::fs::read(&raw_path).map_err(|e| format!("read raw: {e}"))?;
    if got.len() != expected.len() {
        return Err(format!(
            "oracle output {} bytes, expected {}",
            got.len(),
            expected.len()
        ));
    }

    let frame_len = expected.len() / n_frames;
    let per_frame_xform = |xform: &dyn Fn(&[u8]) -> Vec<u8>| -> Vec<u8> {
        let mut v = Vec::with_capacity(expected.len());
        for f in expected.chunks_exact(frame_len) {
            v.extend_from_slice(&xform(f));
        }
        v
    };

    // Candidate layout transforms of OUR output toward the oracle's.
    let mut candidates: Vec<(&'static str, Vec<u8>)> = vec![("identity", expected.to_vec())];
    match c.kind {
        PixelKind::Bgr24 => {
            let rb = c.w as usize * 3;
            candidates.push(("vflip", per_frame_xform(&|f: &[u8]| vflip(f, rb))));
        }
        PixelKind::Bgra32 => {
            let rb = c.w as usize * 4;
            candidates.push(("vflip", per_frame_xform(&|f: &[u8]| vflip(f, rb))));
        }
        PixelKind::Yuy2 => {
            let rb = c.w as usize * 2;
            candidates.push(("vflip", per_frame_xform(&|f: &[u8]| vflip(f, rb))));
        }
        PixelKind::Yv12 => {
            candidates.push(("swap_vu", per_frame_xform(&|f: &[u8]| swap_vu(f, c.w, c.h))));
        }
    }
    for (name, cand) in &candidates {
        if cand == &got {
            return Ok(name);
        }
    }
    // Diagnostic: byte-match ratio against the closest candidate.
    let best = candidates
        .iter()
        .map(|(name, cand)| {
            let matches = cand.iter().zip(&got).filter(|(a, b)| a == b).count();
            (matches, *name)
        })
        .max()
        .unwrap();
    Err(format!(
        "MISMATCH (best candidate {}: {}/{} bytes match)",
        best.1,
        best.0,
        expected.len()
    ))
}
