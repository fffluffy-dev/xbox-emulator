//! Float-mode probe: which DXIL compile options make a D3D12 implementation flush f64
//! denormals or round f64/f32 sums differently from IEEE round-to-nearest-even.
//!
//! One compute shader, compiled four ways (with/without 16-bit types, with/without
//! `-denorm ftz`), each computing the same values from buffer inputs:
//!   f64 denormal   min normal * 0.5            want 0x00080000_00000000
//!   f64 RTE sum    1.0 + (1+2^-52) * 2^-53      want 0x3ff00000_00000001
//!   f32 RTE sum    1.0f + (1+2^-23) * 2^-24     want 0x3f800001
//!   f32 denormal   min normal * 0.5             want 0x00400000 (0 under -denorm ftz)
//!   f16 denormal   2^-15 * 0.5 in float16_t     want 0x0100 (16-bit variants only)
//!   f64 fma denormal  fma(min normal, 0.5, 0)  want 0x00080000_00000000
//!   f64->f32 tie   (float)(1 + 2^-24)           want 0x3f800000 (RTE)
//!   f64->f32 above (float)(1 + 2^-24 + 2^-52)   want 0x3f800001
//!   f64 TwoSum err (1 + 2^-60) error term        want 2^-60 (0x3c300000_00000000)
//! D3D12 requires f64 denormals to be preserved and IEEE RTE for these operations, so any
//! difference is the implementation's (driver or translation layer), not the shader's.

use crate::{d3d, vcall};
use core::ffi::c_void;
use d3d::Gpu;
use tombox_xshader::dxc;

const SRC: &str = r#"RWByteAddressBuffer io : register(u0);
[RootSignature("UAV(u0)")]
[numthreads(1, 1, 1)]
void main() {
    uint4 a = io.Load4(0);
    uint4 b = io.Load4(16);
    uint4 c = io.Load4(32);
    double mn = asdouble(a.x, a.y);
    precise double dn = mn * 0.5;
    precise double s64 = asdouble(a.z, a.w) + asdouble(b.x, b.y);
    precise float s32 = asfloat(b.z) + asfloat(b.w);
    precise float d32 = asfloat(c.y) * 0.5;
    uint lo0, hi0, lo1, hi1;
    asuint(dn, lo0, hi0);
    asuint(s64, lo1, hi1);
    io.Store4(64, uint4(lo0, hi0, lo1, hi1));
    io.Store2(80, uint2(asuint(s32), asuint(d32)));
    uint4 e = io.Load4(48);
    // f64 fma with a denormal result: fma(min normal, 0.5, 0) = 2^-1023.
    precise double fd = fma(mn, 0.5, asdouble(e.x, e.y));
    // f64 -> f32 conversion at a tie (1 + 2^-24 -> 1.0 under RTE) and just above it (-> 1 + 2^-23).
    // 2^-24 relative to 1.0 is fraction bit 28, in the low word. Inputs keep DXC from folding.
    precise float ct = (float)asdouble(e.z | 0x10000000u, 0x3ff00000u);
    precise float cu = (float)asdouble(e.w | 0x10000000u, 0x3ff00000u);
    // TwoSum error term in f64: a = 1, b = 2^-60 -> s = 1, err = 2^-60.
    double ta = asdouble(a.z, a.w), tb = asdouble(e.z, 0x3c300000u);
    precise double ts = ta + tb;
    precise double tbb = ts - ta;
    precise double terr = (ta - (ts - tbb)) + (tb - tbb);
    uint fl, fh, el, eh;
    asuint(fd, fl, fh);
    asuint(terr, el, eh);
    io.Store4(96, uint4(fl, fh, asuint(ct), asuint(cu)));
    io.Store2(112, uint2(el, eh));
#ifdef F16
    float16_t h = (float16_t)asfloat(c.x);
    precise float16_t h2 = h * (float16_t)0.5;
    io.Store(88, (uint)asuint16(h2));
#else
    io.Store(88, 0xffffffffu);
#endif
}
"#;

const INPUT: [u32; 16] = [
    0x0000_0000, 0x0010_0000, // f64 min normal
    0x0000_0000, 0x3ff0_0000, // f64 1.0
    0x0000_0001, 0x3ca0_0000, // f64 (1 + 2^-52) * 2^-53
    0x3f80_0000, 0x3380_0001, // f32 1.0, (1 + 2^-23) * 2^-24
    0x3800_0000, 0x0080_0000, // f32 2^-15 (f16 denormal source), f32 min normal
    0, 0,
    0x0000_0000, 0x0000_0000, // f64 +0.0 (fma addend)
    0x0000_0000, 0x0000_0001, // e.z = 0 (tie / TwoSum), e.w = 1 (1 + 2^-24 + 2^-52, just above the tie)
];

struct Variant {
    name: &'static str,
    args: &'static [&'static str],
    f32_ftz: bool,
    f16: bool,
}

const VARIANTS: &[Variant] = &[
    Variant { name: "A plain", args: &[], f32_ftz: false, f16: false },
    Variant { name: "B 16-bit types", args: &["-enable-16bit-types", "-D", "F16"], f32_ftz: false, f16: true },
    Variant { name: "C -denorm ftz", args: &["-denorm", "ftz"], f32_ftz: true, f16: false },
    Variant { name: "D 16-bit + ftz", args: &["-enable-16bit-types", "-D", "F16", "-denorm", "ftz"], f32_ftz: true, f16: true },
];

unsafe fn run(g: &mut Gpu, code: &[u8]) -> Result<[u32; 13], String> {
    let total = 128u64; // inputs 0..64, outputs 64..120
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 16, fn(u32, *const u8, usize, *const d3d::Guid, *mut *mut c_void) -> i32, 0, code.as_ptr(), code.len(), &d3d::IID_ROOTSIG, &mut rs);
    if hr < 0 {
        return Err(format!("CreateRootSignature {hr:#x}"));
    }
    let mut d = [0u8; 56];
    d[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
    d[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    d[16..24].copy_from_slice(&(code.len() as u64).to_le_bytes());
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 11, fn(*const u8, *const d3d::Guid, *mut *mut c_void) -> i32, d.as_ptr(), &d3d::IID_PSO, &mut pso);
    if hr < 0 {
        return Err(format!("CreateComputePipelineState {hr:#x}"));
    }
    let buf = g.buffer(total, 1, d3d::COPY_DEST, true)?;
    let up = g.buffer(total, 2, d3d::GENERIC_READ, false)?;
    let rb = g.buffer(total, 3, d3d::COPY_DEST, false)?;
    let mut bytes: Vec<u8> = INPUT.iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.resize(total as usize, 0);
    Gpu::map_write(up, &bytes);
    g.begin();
    let l = g.list;
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), buf, 0, up, 0, total);
    g.barrier(buf, d3d::COPY_DEST, 8);
    vcall!(l, 29, fn(*mut c_void) -> (), rs);
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    vcall!(l, 41, fn(u32, u64) -> (), 0, Gpu::gpu_va(buf));
    vcall!(l, 14, fn(u32, u32, u32) -> (), 1, 1, 1);
    g.barrier(buf, 8, d3d::COPY_SOURCE);
    vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, buf, 0, total);
    g.submit()?;
    let out = Gpu::map_read(rb, total as usize).ok_or("map")?;
    for k in [pso, rs, buf, up, rb] {
        d3d::release(k);
    }
    let w = |o: usize| u32::from_le_bytes(out[o..o + 4].try_into().unwrap());
    Ok([w(64), w(68), w(72), w(76), w(80), w(84), w(88), w(96), w(100), w(104), w(108), w(112), w(116)])
}

pub fn main() {
    let mut g = unsafe { Gpu::new(false) }.expect("D3D12 device");
    g.open = false;
    println!("fp_mode_probe: f64/f32/f16 denormal and rounding behaviour per DXIL compile option");
    let mut bad = 0;
    for v in VARIANTS {
        let code = match dxc::compile_with(SRC, "cs_6_2", v.args) {
            Ok(c) => c,
            Err(e) => {
                println!("{:16} compile failed: {}", v.name, e.lines().next().unwrap_or(""));
                bad += 1;
                continue;
            }
        };
        let r = match unsafe { run(&mut g, &code) } {
            Ok(r) => r,
            Err(e) => {
                println!("{:16} run failed: {e}", v.name);
                bad += 1;
                continue;
            }
        };
        let checks: Vec<(&str, u64, u64)> = {
            let mut c = vec![
                ("f64 denormal", (r[1] as u64) << 32 | r[0] as u64, 0x0008_0000_0000_0000),
                ("f64 RTE sum", (r[3] as u64) << 32 | r[2] as u64, 0x3ff0_0000_0000_0001),
                ("f32 RTE sum", r[4] as u64, 0x3f80_0001),
                ("f32 denormal", r[5] as u64, if v.f32_ftz { 0 } else { 0x0040_0000 }),
            ];
            c.push(("f64 fma denormal", (r[8] as u64) << 32 | r[7] as u64, 0x0008_0000_0000_0000));
            c.push(("f64->f32 tie", r[9] as u64, 0x3f80_0000));
            c.push(("f64->f32 above tie", r[10] as u64, 0x3f80_0001));
            c.push(("f64 TwoSum err", (r[12] as u64) << 32 | r[11] as u64, 0x3c30_0000_0000_0000));
            if v.f16 {
                c.push(("f16 denormal", r[6] as u64, 0x0100));
            }
            c
        };
        let mut line = format!("{:16}", v.name);
        for (what, got, want) in checks {
            if got == want {
                line.push_str(&format!("  {what} ok"));
            } else {
                bad += 1;
                line.push_str(&format!("  {what} BAD got {got:#x} want {want:#x}"));
            }
        }
        println!("{line}");
    }
    println!("{}", if bad == 0 { "fp_mode_probe: all IEEE-conformant" } else { "fp_mode_probe: non-conformant results above" });
    std::process::exit(if bad == 0 { 0 } else { 1 });
}
