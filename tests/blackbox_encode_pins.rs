//! Round-451 black-box cross-validation pins for the encoder.
//!
//! `examples/blackbox_capture.rs` encodes a deterministic case matrix
//! covering every frame type the crate can emit, wraps each stream in
//! a minimal `LAGS` AVI, and hands it to an **independent third-party
//! decoder used purely as a black-box binary oracle** (out of CI).
//! The capture run recorded one FNV-1a-64 hash per encoded stream and
//! one verdict per case; this test re-derives the identical inputs,
//! re-encodes, and asserts the hashes — so the exact bytes the oracle
//! validated are the bytes the encoder keeps producing, with the
//! oracle out of the CI loop.
//!
//! Verdict classes captured 2026-08-24 (see the example's doc header):
//!
//! * `Exact` — the oracle reconstructed the stream sample-exactly
//!   (after its bottom-up DIB flip for the RGB families / its
//!   `Y‖U‖V` plane order for 4:2:0). Every RGB24 / RGBA / YV12
//!   arithmetic, solid and downscale-election case is in this class —
//!   including the round-127 "structured pattern" class whose
//!   re-capture was the standing open item, and the non-power-of-two
//!   totals the `0x180001050` normalizer handles.
//! * `OracleUnsupported` — the oracle build rejects the frame type
//!   itself ("Unsupported Lagarith frame type"): types 1 / 7 / 11.
//!   The NULL ("JUMP") payload is undecidable through it too (its
//!   demuxer drops zero-byte packets). Pinned for stability only.
//! * The even-width YUY2 cases joined the `Exact` class in the
//!   round-451 second pass (complete recovery of the YUY2 predictor:
//!   raw second row-0 luma sample, plain-L row-1 first chunk,
//!   8-bit-wrapping median); the odd-width case stays stability-only
//!   because the oracle rejects odd-width YUY2 frames outright.
//!
//! Two coder-semantics recoveries landed with this capture (both
//! confirmed by the oracle reproducing our streams byte-exactly, and
//! flagged as spec erratum candidates):
//!
//! 1. The modern range coder's top-symbol (0xff) interval absorbs the
//!    quotient slack (`src/range_coder.rs` Step B; `spec/02` §5
//!    erratum candidate).
//! 2. The YV12 / YUY2 / reduced-res first-column rule is
//!    `FirstColRule::Yuv` — `pred = L` at row 1, Rule-B median for
//!    rows ≥ 2 (`src/predict.rs`; closes `spec/06` §6.4 for YV12,
//!    `spec/06` §3.8 erratum candidate).

use oxideav_lagarith::{decode_frame, encode_frame, wire_forms, PixelKind};

/// 64-bit LCG — must stay bit-identical to the capture example's.
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

fn fnv64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[derive(Clone, Copy)]
enum Content {
    GradientNoise,
    Random,
    ZeroHeavy,
    Structured,
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
                let run = 8 + (lcg.next_u8() as usize % 48);
                out.resize((i + run).min(len), 0);
                i = out.len();
                if i < len {
                    out.push(lcg.next_u8() | 1);
                    i += 1;
                }
            }
        }
        Content::Structured => {
            for i in 0..len {
                out.push((((i as u64).wrapping_mul(73).wrapping_add(11)) >> 3) as u8);
            }
        }
        Content::GradientNoise => {
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
    out
}

type ForceFn = fn(&[u8], u32, u32) -> Vec<u8>;

struct Pin {
    name: &'static str,
    kind: PixelKind,
    w: u32,
    h: u32,
    content: Content,
    seed: u64,
    force: Option<ForceFn>,
    /// FNV-1a-64 of the encoded stream, frozen from the 2026-08-24
    /// oracle capture run.
    fnv: u64,
    /// `true` — the oracle reconstructed this exact stream
    /// sample-exactly; `false` — pinned for stability only (oracle
    /// lacks the type, or the documented YUY2 gap).
    oracle_exact: bool,
    /// Lossless self-roundtrip expected (`false` only for the
    /// reduced-resolution type 11, whose downsample is lossy).
    lossless: bool,
}

#[allow(clippy::too_many_arguments)]
const fn pin(
    name: &'static str,
    kind: PixelKind,
    w: u32,
    h: u32,
    content: Content,
    seed: u64,
    force: Option<ForceFn>,
    fnv: u64,
    oracle_exact: bool,
    lossless: bool,
) -> Pin {
    Pin {
        name,
        kind,
        w,
        h,
        content,
        seed,
        force,
        fnv,
        oracle_exact,
        lossless,
    }
}

fn pins() -> Vec<Pin> {
    use Content::{GradientNoise, Random, Solid, Structured, ZeroHeavy};
    use PixelKind::{Bgr24, Bgra32, Yuy2, Yv12};
    vec![
        // ── oracle-EXACT class ──
        pin(
            "rgb24_64x48_gradient_t4",
            Bgr24,
            64,
            48,
            GradientNoise,
            11,
            None,
            0x7fbe6e25630bc83b,
            true,
            true,
        ),
        pin(
            "rgb24_63x47_gradient_t2",
            Bgr24,
            63,
            47,
            GradientNoise,
            12,
            None,
            0x7f157070b15a3c92,
            true,
            true,
        ),
        pin(
            "rgb24_16x16_structured",
            Bgr24,
            16,
            16,
            Structured,
            0,
            None,
            0x6f9db3ddf638733f,
            true,
            true,
        ),
        pin(
            "rgb24_32x32_structured",
            Bgr24,
            32,
            32,
            Structured,
            0,
            None,
            0x6b731709cbea2987,
            true,
            true,
        ),
        pin(
            "rgb24_64x48_zeroheavy",
            Bgr24,
            64,
            48,
            ZeroHeavy,
            13,
            None,
            0x3dec52a5506749e4,
            true,
            true,
        ),
        pin(
            "rgb24_63x47_zeroheavy_t2",
            Bgr24,
            63,
            47,
            ZeroHeavy,
            14,
            None,
            0xd8877c5818468a0b,
            true,
            true,
        ),
        pin(
            "rgb24_640x480_gradient_dse",
            Bgr24,
            640,
            480,
            GradientNoise,
            16,
            None,
            0x883839891d91d5da,
            true,
            true,
        ),
        pin(
            "rgb24_solid_grey_t5",
            Bgr24,
            64,
            48,
            Solid(0x55, 0x55, 0x55, 0),
            0,
            None,
            0x08215f07b4dcb70f,
            true,
            true,
        ),
        pin(
            "rgb24_solid_rgb_t6",
            Bgr24,
            64,
            48,
            Solid(0x20, 0x40, 0x80, 0),
            0,
            None,
            0xf8dabe6e551c6933,
            true,
            true,
        ),
        pin(
            "rgba_64x48_gradient_t8",
            Bgra32,
            64,
            48,
            GradientNoise,
            21,
            None,
            0x4edb0858e64c5b74,
            true,
            true,
        ),
        pin(
            "rgba_16x16_structured",
            Bgra32,
            16,
            16,
            Structured,
            0,
            None,
            0xb30cbc76cac28e87,
            true,
            true,
        ),
        pin(
            "rgba_64x48_zeroheavy",
            Bgra32,
            64,
            48,
            ZeroHeavy,
            22,
            None,
            0x7f84b9a3b3533a75,
            true,
            true,
        ),
        pin(
            "rgba_solid_t9",
            Bgra32,
            64,
            48,
            Solid(0x10, 0x20, 0x30, 0x40),
            0,
            None,
            0x2836868758e26504,
            true,
            true,
        ),
        pin(
            "rgba_63x47_gradient",
            Bgra32,
            63,
            47,
            GradientNoise,
            23,
            None,
            0xbd66890c0ce7af8b,
            true,
            true,
        ),
        pin(
            "yv12_64x48_gradient_t10",
            Yv12,
            64,
            48,
            GradientNoise,
            31,
            None,
            0xe67b966307443268,
            true,
            true,
        ),
        pin(
            "yv12_64x48_zeroheavy",
            Yv12,
            64,
            48,
            ZeroHeavy,
            32,
            None,
            0x701ab7083160c8ea,
            true,
            true,
        ),
        pin(
            "yv12_320x240_gradient",
            Yv12,
            320,
            240,
            GradientNoise,
            33,
            None,
            0xb2c9571680511ff1,
            true,
            true,
        ),
        // ── stability-only pins (oracle-unsupported type) ──
        pin(
            "rgb24_64x48_random_t1",
            Bgr24,
            64,
            48,
            Random,
            15,
            None,
            0x3be64b63e82dc259,
            false,
            true,
        ),
        pin(
            "rgb24_64x48_forced_t1",
            Bgr24,
            64,
            48,
            GradientNoise,
            71,
            Some((|p: &[u8], _w, _h| wire_forms::encode_uncompressed(p)) as ForceFn),
            0x53ef0c96b76988bc,
            false,
            true,
        ),
        pin(
            "legacy_rgb_64x48_gradient_t7",
            Bgr24,
            64,
            48,
            GradientNoise,
            51,
            Some(wire_forms::encode_legacy_rgb as ForceFn),
            0x1e40aee0d34d6ff8,
            false,
            true,
        ),
        pin(
            "legacy_rgb_32x32_zeroheavy_t7",
            Bgr24,
            32,
            32,
            ZeroHeavy,
            52,
            Some(wire_forms::encode_legacy_rgb as ForceFn),
            0x5d6645768a7c65e4,
            false,
            true,
        ),
        pin(
            "legacy_rgb_rle1_16x16_t7",
            Bgr24,
            16,
            16,
            GradientNoise,
            53,
            Some((|p: &[u8], w, h| wire_forms::encode_legacy_rgb_rle(p, w, h, 1)) as ForceFn),
            0x2597ed580a69622a,
            false,
            true,
        ),
        pin(
            "reduced_res_64x48_gradient_t11",
            Yv12,
            64,
            48,
            GradientNoise,
            61,
            Some(wire_forms::encode_arith_reduced_res as ForceFn),
            0x003d1ef0bcbedaa6,
            false,
            false,
        ),
        // ── YUY2 (oracle-EXACT since the round-451 second pass) ──
        pin(
            "yuy2_64x48_gradient_t3",
            Yuy2,
            64,
            48,
            GradientNoise,
            41,
            None,
            0xfeeec7e2f6e408b3,
            true,
            true,
        ),
        pin(
            "yuy2_64x48_zeroheavy",
            Yuy2,
            64,
            48,
            ZeroHeavy,
            42,
            None,
            0x2233a38373d4218d,
            true,
            true,
        ),
        // Odd width: the tail chroma slot is decoder-synthesised
        // (`0x80` neutral fill), so a raw input buffer round-trips to
        // its normalised form — pinned via idempotence.
        pin(
            "yuy2_63x48_gradient_odd",
            Yuy2,
            63,
            48,
            GradientNoise,
            43,
            None,
            0xfb60be7f07a66a76,
            false,
            false,
        ),
    ]
}

/// Every pinned case re-encodes to the exact bytes the oracle capture
/// validated (hash-pinned), decodes under our own decoder, and — for
/// the lossless cases — reproduces the input byte-exactly.
#[test]
fn blackbox_capture_pins_hold() {
    let mut oracle_exact = 0usize;
    for p in pins() {
        let pixels = fill(p.kind, p.w, p.h, p.content, p.seed);
        let encoded = match p.force {
            Some(f) => f(&pixels, p.w, p.h),
            None => encode_frame(&pixels, p.w, p.h, p.kind).expect("encode_frame"),
        };
        assert_eq!(
            fnv64(&encoded),
            p.fnv,
            "{}: encoded bytes drifted from the oracle-captured stream \
             (len {}; if the wire semantics changed deliberately, re-run \
             examples/blackbox_capture.rs against the oracle and re-freeze)",
            p.name,
            encoded.len(),
        );
        let dec = decode_frame(&encoded, p.w, p.h, p.kind)
            .unwrap_or_else(|e| panic!("{}: self-decode failed: {e}", p.name));
        if p.lossless {
            assert_eq!(dec.pixels, pixels, "{}: lossless self-roundtrip", p.name);
        } else {
            // Reduced-res (lossy downsample) and odd-width YUY2 (the
            // decoder synthesises the tail chroma slot): fixed-point
            // idempotence instead of input equality.
            let re = match p.force {
                Some(f) => f(&dec.pixels, p.w, p.h),
                None => encode_frame(&dec.pixels, p.w, p.h, p.kind).expect("re-encode"),
            };
            let dec2 = decode_frame(&re, p.w, p.h, p.kind).expect("re-decode");
            assert_eq!(dec2.pixels, dec.pixels, "{}: idempotent", p.name);
        }
        if p.oracle_exact {
            oracle_exact += 1;
        }
    }
    // The capture matrix's third-party-validated surface: every RGB24
    // / RGBA / YV12 case. Guards against silently shrinking the
    // validated class when editing the table.
    assert_eq!(oracle_exact, 19, "oracle-exact pin count changed");
}
