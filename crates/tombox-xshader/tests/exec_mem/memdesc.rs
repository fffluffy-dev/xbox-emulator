//! GPU check of the memory-descriptor mode (hlsl/memdesc.rs): a compute shader with no root
//! signature takes a descriptor-table pointer from the user data (cbuffer TbUser), loads a V#
//! from that table with s_load, reads constants through the V# with s_buffer_load (one inside
//! and one past num_records) and stores through the same V# with buffer_store. The table, the
//! constant and the store target all live in a global window.
use super::*;
use tombox_xshader::global_abi::{fault_of, window_cb, Window, TB_GLOBAL_SPACE};

const W0: Window = Window { base: 0x10_0000_0000, size: 4096 };
const W1: Window = Window { base: 0x7f10_0000_0000, size: 1024 };
const MAGIC: u32 = 0xc0ff_ee01;

fn prog() -> Vec<u32> {
    vec![
        0xf408_0108, // s_load_dwordx4 s[4:7], s[16:17], 0x20       V# from the table
        0xfa00_0020,
        0xf420_0202, // s_buffer_load_dword s8, s[4:7], 0x10       = MAGIC
        0xfa00_0010,
        0xf420_0242, // s_buffer_load_dword s9, s[4:7], 0x500      past num_records: 0
        0xfa00_0500,
        vop2(0x1a, 2, ic(2), 0), // v_lshlrev_b32 v2, 2, v0
        0x890a_0908,             // s_xor_b32 s10, s8, s9
        vop1(1, 1, 10),          // v_mov_b32 v1, s10
        0xe070_1200,             // buffer_store_dword v1, v2, s[4:7], 0 offen offset:0x200
        0x8001_0102,
        S_ENDPGM,
    ]
}

fn window0() -> Vec<u32> {
    let mut w = vec![0u32; W0.size as usize / 4];
    // V# at table offset 0x20: base = W0.base + 0x400, stride 0, num_records 0x400 bytes.
    w[0x20 / 4] = (W0.base as u32).wrapping_add(0x400);
    w[0x24 / 4] = (W0.base >> 32) as u32;
    w[0x28 / 4] = 0x400;
    w[0x2c / 4] = 0;
    w[0x410 / 4] = MAGIC;
    w
}

unsafe fn run_mem(words: &[u32], user: &[u32; 32]) -> Result<(Vec<u32>, [u32; 4], String), String> {
    let mut ex = cs_extract(words, 64);
    ex.root_sig = None;
    ex.has_root_sig = false;
    let t = translate(&ex)?;
    if !t.mem_user_data || !t.needs_global {
        return Err(format!("memory-descriptor mode not used (mem_user_data {}, needs_global {})\n{}", t.mem_user_data, t.needs_global, t.hlsl));
    }
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(ctx_new()));
    let c = m.lock().unwrap_or_else(|e| e.into_inner());
    let g = &c.gpu;
    // b0 = global windows, b1 = TbUser; u0/u1 = windows, u2 = fault record.
    let blob = ser_ranges(&[vec![[2, 2, 0, TB_GLOBAL_SPACE, 0xffff_ffff], [1, 3, 0, TB_GLOBAL_SPACE, 0xffff_ffff]]])?;
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, blob.as_ptr(), blob.len(), &d3d::IID_ROOTSIG, &mut rs);
    if hr < 0 {
        return Err(format!("CreateRootSignature {hr:#x}"));
    }
    let mut d = [0u8; 56];
    d[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
    d[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    d[16..24].copy_from_slice(&(code.len() as u64).to_le_bytes());
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 11, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &d3d::IID_PSO, &mut pso);
    if hr < 0 {
        d3d::release(rs);
        return Err(format!("CreateComputePipelineState hr={:#010x}{}\n{}", hr as u32, g.messages(), t.hlsl));
    }
    let (vh, vcpu, vgpu, vinc) = g.heap(0, 5, true)?;
    let sizes = [W0.size as u64, W1.size as u64, 256u64];
    let init: [Vec<u32>; 3] = [window0(), vec![0; W1.size as usize / 4], vec![0; 64]];
    let mut kill = vec![pso, rs, vh];
    let mut bufs = Vec::new();
    for k in 0..3 {
        let b = g.buffer(sizes[k], 1, d3d::COPY_DEST, true)?;
        let up = g.buffer(sizes[k], 2, d3d::GENERIC_READ, false)?;
        let rb = g.buffer(sizes[k], 3, d3d::COPY_DEST, false)?;
        let mut bytes: Vec<u8> = init[k].iter().flat_map(|w| w.to_le_bytes()).collect();
        bytes.resize(sizes[k] as usize, 0);
        Gpu::map_write(up, &bytes);
        let mut ud = [0u8; 40];
        ud[0..4].copy_from_slice(&39u32.to_le_bytes());
        ud[4..8].copy_from_slice(&1u32.to_le_bytes());
        ud[16..20].copy_from_slice(&((sizes[k] / 4) as u32).to_le_bytes());
        ud[32..36].copy_from_slice(&1u32.to_le_bytes());
        vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), b, core::ptr::null_mut(), ud.as_ptr(), vcpu + (2 + k) * vinc);
        kill.extend([b, up, rb]);
        bufs.push((b, up, rb));
    }
    for (slot, words) in [(0usize, window_cb(&[W0, W1], [0, 0]).to_vec()), (1, user.to_vec())] {
        let cb = g.buffer(256, 2, d3d::GENERIC_READ, false)?;
        kill.push(cb);
        let mut cbd: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        cbd.resize(256, 0);
        Gpu::map_write(cb, &cbd);
        let cv = [Gpu::gpu_va(cb), 256u64];
        vcall!(g.device, 17, fn(*const u64, usize) -> (), cv.as_ptr(), vcpu + slot * vinc);
    }
    drop(c);
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    c.gpu.begin();
    let l = c.gpu.list;
    for (k, &(b, up, _)) in bufs.iter().enumerate() {
        vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), b, 0, up, 0, sizes[k]);
        c.gpu.barrier(b, d3d::COPY_DEST, 8);
    }
    let heaps = [vh];
    vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 1, heaps.as_ptr());
    vcall!(l, 29, fn(*mut c_void) -> (), rs);
    vcall!(l, 31, fn(u32, u64) -> (), 0, vgpu);
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    vcall!(l, 14, fn(u32, u32, u32) -> (), 1, 1, 1);
    for (k, &(b, _, rb)) in bufs.iter().enumerate() {
        c.gpu.barrier(b, 8, d3d::COPY_SOURCE);
        vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, b, 0, sizes[k]);
    }
    let r = c.gpu.submit();
    let read = |k: usize| -> Vec<u32> { Gpu::map_read(bufs[k].2, sizes[k] as usize).unwrap_or_default().chunks(4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).collect() };
    let out = r.map(|_| {
        let f = read(2);
        (read(0), [f[0], f[1], f[2], f[3]], t.hlsl.clone())
    });
    for k in kill {
        d3d::release(k);
    }
    out
}

#[test]
fn memory_descriptors_round_trip_through_a_window() {
    let mut user = [0u32; 32];
    user[16] = W0.base as u32;
    user[17] = (W0.base >> 32) as u32;
    let (w0, fault, hlsl) = unsafe { run_mem(&prog(), &user) }.unwrap_or_else(|e| panic!("{e}"));
    let mut bad = Vec::new();
    for t in 0..64usize {
        let got = w0[(0x400 + 0x200) / 4 + t];
        if got != MAGIC {
            bad.push(format!("lane {t}: stored {got:08x} want {MAGIC:08x}"));
        }
    }
    let f = fault_of(&fault);
    if f.count != 0 {
        bad.push(format!("{} global-window misses, first at {:#x}", f.count, f.first_va));
    }
    assert!(bad.is_empty(), "{}\n{hlsl}", bad.join("\n"));
}

// ---- Step 2: images and samplers from memory (bindless T# / S# lookup) ----

const TSHARP: [u32; 8] = [0x1111_0000, 0x2222_0001, 0x3333_0002, 0x4444_0003, 0, 0, 0, 0];
const SSHARP: [u32; 4] = [0x5555_0000, 1, 2, 3];
// Heap layout: b0 windows, b1 TbUser, u0/u1 windows, u2 fault, t0 lookup table, then the
// texture and the null SRV the table returns for unknown descriptors.
const H_TEX: u32 = 6;
const H_NULL: u32 = 7;

fn img_prog() -> Vec<u32> {
    vec![
        0xf408_0108, // s_load_dwordx4 s[4:7], s[16:17], 0x20       V# (store target)
        0xfa00_0020,
        0xf40c_0508, // s_load_dwordx8 s[20:27], s[16:17], 0x40     T#
        0xfa00_0040,
        0xf408_0708, // s_load_dwordx4 s[28:31], s[16:17], 0x60     S#
        0xfa00_0060,
        vop1(1, 1, 0x80),                // v_mov_b32 v1, 0
        0xf000_0f08,                     // image_load v[4:7], v[0:1], s[20:27] dmask:0xf dim:2D
        0x0005_0400,
        vop1(6, 2, 256),                 // v_cvt_f32_u32 v2, v0
        vop2(3, 2, 0xf0, 2),             // v_add_f32 v2, 0.5, v2
        vop2(8, 2, 0xff, 2),             // v_mul_f32 v2, 1/64, v2
        0x3c80_0000,
        vop1(1, 3, 0xf0),                // v_mov_b32 v3, 0.5
        0xf09c_0108,                     // image_sample_lz v8, v[2:3], s[20:27], s[28:31] dmask:0x1 dim:2D
        0x00e5_0802,
        vop2(0x1a, 12, ic(2), 0),        // v_lshlrev_b32 v12, 2, v0
        0xe070_1200,                     // buffer_store_dword v4, v12, s[4:7], 0 offen offset:0x200
        0x8001_040c,
        0xe070_1300,                     // buffer_store_dword v8, v12, s[4:7], 0 offen offset:0x300
        0x8001_080c,
        S_ENDPGM,
    ]
}

fn img_window0() -> Vec<u32> {
    let mut w = window0();
    w[0x40 / 4..0x60 / 4].copy_from_slice(&TSHARP);
    w[0x60 / 4..0x70 / 4].copy_from_slice(&SSHARP);
    w
}

fn texel(_: u32, _: u32, x: u32, _: u32) -> [u32; 4] {
    [(1.0 + x as f32).to_bits(), 0, 0, 0]
}

/// Root signature 1.1 with one descriptor table and the heap-directly-indexed flags.
unsafe fn ser_v11(ranges: &[[u32; 6]], flags: u32) -> Result<Vec<u8>, String> {
    let d3d = LoadLibraryA(b"d3d12.dll\0".as_ptr());
    let p = GetProcAddress(d3d, b"D3D12SerializeVersionedRootSignature\0".as_ptr());
    if p.is_null() {
        return Err("no D3D12SerializeVersionedRootSignature".into());
    }
    let ser: unsafe extern "system" fn(*const u8, *mut *mut c_void, *mut *mut c_void) -> i32 = core::mem::transmute(p);
    let mut param = [0u8; 32];
    param[8..12].copy_from_slice(&(ranges.len() as u32).to_le_bytes());
    param[16..24].copy_from_slice(&(ranges.as_ptr() as u64).to_le_bytes());
    let mut desc = [0u8; 48];
    desc[0..4].copy_from_slice(&2u32.to_le_bytes());
    desc[8..12].copy_from_slice(&1u32.to_le_bytes());
    desc[16..24].copy_from_slice(&(param.as_ptr() as u64).to_le_bytes());
    desc[40..44].copy_from_slice(&flags.to_le_bytes());
    let mut blob: *mut c_void = core::ptr::null_mut();
    let mut err: *mut c_void = core::ptr::null_mut();
    if ser(desc.as_ptr(), &mut blob, &mut err) < 0 {
        let msg = if err.is_null() { String::new() } else { std::ffi::CStr::from_ptr(vcall!(err, 3, fn() -> *const i8)).to_string_lossy().into_owned() };
        return Err(format!("serialize root signature 1.1: {msg}"));
    }
    let p = vcall!(blob, 3, fn() -> *const u8);
    let n = vcall!(blob, 4, fn() -> usize);
    Ok(core::slice::from_raw_parts(p, n).to_vec())
}

/// Runs `img_prog` with the T# / S# registered in the lookup table (or not): returns window 0,
/// the fault record ([misses, va lo, va hi, descriptor misses]) and the HLSL.
unsafe fn run_img(register_tex: bool) -> Result<(Vec<u32>, [u32; 4], String), String> {
    use tombox_xshader::global_abi::{desc_key, desc_table, DESC_SAMPLER, DESC_SRV};
    let mut ex = cs_extract(&img_prog(), 64);
    ex.root_sig = None;
    ex.has_root_sig = false;
    let t = translate(&ex)?;
    if !t.mem_user_data || !t.mem_desc_table {
        return Err(format!("descriptor lookup not used (mem_user_data {}, mem_desc_table {})\n{}", t.mem_user_data, t.mem_desc_table, t.hlsl));
    }
    let code = dxc::compile(&t.hlsl, &dxc::profile_for(t.profile)).map_err(|e| format!("dxc: {e}\n{}", t.hlsl))?;
    let m = CTX.get_or_init(|| Mutex::new(ctx_new()));
    let c = m.lock().unwrap_or_else(|e| e.into_inner());
    let g = &c.gpu;
    const VOL: u32 = 3; // DESCRIPTORS_VOLATILE | DATA_VOLATILE (the 1.0 behaviour)
    let ranges = [[2, 2, 0, TB_GLOBAL_SPACE, VOL, 0xffff_ffff], [1, 3, 0, TB_GLOBAL_SPACE, VOL, 0xffff_ffff], [0, 1, 0, TB_GLOBAL_SPACE, VOL, 0xffff_ffff]];
    let blob = ser_v11(&ranges, 0x400 | 0x800)?;
    let mut rs: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 16, fn(u32, *const u8, usize, *const Guid, *mut *mut c_void) -> i32, 0, blob.as_ptr(), blob.len(), &d3d::IID_ROOTSIG, &mut rs);
    if hr < 0 {
        return Err(format!("CreateRootSignature {hr:#x}{}", g.messages()));
    }
    let mut d = [0u8; 56];
    d[0..8].copy_from_slice(&(rs as u64).to_le_bytes());
    d[8..16].copy_from_slice(&(code.as_ptr() as u64).to_le_bytes());
    d[16..24].copy_from_slice(&(code.len() as u64).to_le_bytes());
    let mut pso: *mut c_void = core::ptr::null_mut();
    let hr = vcall!(g.device, 11, fn(*const u8, *const Guid, *mut *mut c_void) -> i32, d.as_ptr(), &d3d::IID_PSO, &mut pso);
    if hr < 0 {
        d3d::release(rs);
        return Err(format!("CreateComputePipelineState hr={:#010x}{}\n{}", hr as u32, g.messages(), t.hlsl));
    }
    let (vh, vcpu, vgpu, vinc) = g.heap(0, 8, true)?;
    let (sh, scpu, _sgpu, sinc) = g.heap(1, 2, true)?;
    let mut kill = vec![pso, rs, vh, sh];
    // Windows and the fault record (u0..u2).
    let sizes = [W0.size as u64, W1.size as u64, 256u64];
    let init: [Vec<u32>; 3] = [img_window0(), vec![0; W1.size as usize / 4], vec![0; 64]];
    let mut bufs = Vec::new();
    for k in 0..3 {
        let b = g.buffer(sizes[k], 1, d3d::COPY_DEST, true)?;
        let up = g.buffer(sizes[k], 2, d3d::GENERIC_READ, false)?;
        let rb = g.buffer(sizes[k], 3, d3d::COPY_DEST, false)?;
        let mut bytes: Vec<u8> = init[k].iter().flat_map(|w| w.to_le_bytes()).collect();
        bytes.resize(sizes[k] as usize, 0);
        Gpu::map_write(up, &bytes);
        let mut ud = [0u8; 40];
        ud[0..4].copy_from_slice(&39u32.to_le_bytes());
        ud[4..8].copy_from_slice(&1u32.to_le_bytes());
        ud[16..20].copy_from_slice(&((sizes[k] / 4) as u32).to_le_bytes());
        ud[32..36].copy_from_slice(&1u32.to_le_bytes());
        vcall!(g.device, 19, fn(*mut c_void, *mut c_void, *const u8, usize) -> (), b, core::ptr::null_mut(), ud.as_ptr(), vcpu + (2 + k) * vinc);
        kill.extend([b, up, rb]);
        bufs.push((b, up, rb));
    }
    let mut user = [0u32; 32];
    user[16] = W0.base as u32;
    user[17] = (W0.base >> 32) as u32;
    for (slot, words) in [(0usize, window_cb(&[W0, W1], [0, 0]).to_vec()), (1, user.to_vec())] {
        let cb = g.buffer(256, 2, d3d::GENERIC_READ, false)?;
        kill.push(cb);
        let mut cbd: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        cbd.resize(256, 0);
        Gpu::map_write(cb, &cbd);
        let cv = [Gpu::gpu_va(cb), 256u64];
        vcall!(g.device, 17, fn(*const u64, usize) -> (), cv.as_ptr(), vcpu + slot * vinc);
    }
    // t0: the T# / S# lookup table.
    let tkey = desc_key(DESC_SRV, [TSHARP[0], TSHARP[1], TSHARP[2], TSHARP[3]]);
    let skey = desc_key(DESC_SAMPLER, SSHARP);
    let mut entries = vec![(skey, 0u32)];
    if register_tex {
        entries.push((tkey, H_TEX));
    }
    let table = desc_table(&entries, [H_NULL, H_NULL, 1]);
    let tb = g.buffer((table.len() * 4).max(256) as u64, 2, d3d::GENERIC_READ, false)?;
    kill.push(tb);
    Gpu::map_write(tb, &table.iter().flat_map(|w| w.to_le_bytes()).collect::<Vec<u8>>());
    let mut sd = [0u8; 40];
    sd[0..4].copy_from_slice(&39u32.to_le_bytes());
    sd[4..8].copy_from_slice(&1u32.to_le_bytes());
    sd[8..12].copy_from_slice(&0x1688u32.to_le_bytes());
    sd[24..28].copy_from_slice(&(table.len() as u32).to_le_bytes());
    sd[32..36].copy_from_slice(&1u32.to_le_bytes());
    vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), tb, sd.as_ptr(), vcpu + 5 * vinc);
    // The texture (64 x 1 R32_FLOAT, texel x = 1 + x) and the null SRV.
    let img = Img { w: 64, h: 1, layers: 1, array: false, mips: 1, fmt: R32_FLOAT, texel };
    let (tex, tex_up, fps, _) = upload_img(g, &img, false, &mut kill)?;
    let sd = srv_desc(&img, 0, 1);
    vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), tex, sd.as_ptr(), vcpu + H_TEX as usize * vinc);
    vcall!(g.device, 18, fn(*mut c_void, *const u8, usize) -> (), core::ptr::null_mut(), sd.as_ptr(), vcpu + H_NULL as usize * vinc);
    let samp = sampler(0, 3, 0);
    for k in 0..2 {
        vcall!(g.device, 22, fn(*const u8, usize) -> (), samp.as_ptr(), scpu + k * sinc);
    }
    drop(c);
    let mut c = m.lock().unwrap_or_else(|e| e.into_inner());
    c.gpu.begin();
    let l = c.gpu.list;
    for (k, &(b, up, _)) in bufs.iter().enumerate() {
        vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), b, 0, up, 0, sizes[k]);
        c.gpu.barrier(b, d3d::COPY_DEST, 8);
    }
    copy_subs(l, tex, tex_up, &fps, true);
    c.gpu.barrier(tex, d3d::COPY_DEST, 0x40 | 0x80);
    let heaps = [vh, sh];
    vcall!(l, 28, fn(u32, *const *mut c_void) -> (), 2, heaps.as_ptr());
    vcall!(l, 29, fn(*mut c_void) -> (), rs);
    vcall!(l, 31, fn(u32, u64) -> (), 0, vgpu);
    vcall!(l, 25, fn(*mut c_void) -> (), pso);
    vcall!(l, 14, fn(u32, u32, u32) -> (), 1, 1, 1);
    for (k, &(b, _, rb)) in bufs.iter().enumerate() {
        c.gpu.barrier(b, 8, d3d::COPY_SOURCE);
        vcall!(l, 15, fn(*mut c_void, u64, *mut c_void, u64, u64) -> (), rb, 0, b, 0, sizes[k]);
    }
    let r = c.gpu.submit().map_err(|e| format!("{e}{}", c.gpu.messages()));
    let read = |k: usize| -> Vec<u32> { Gpu::map_read(bufs[k].2, sizes[k] as usize).unwrap_or_default().chunks(4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).collect() };
    let out = r.map(|_| {
        let f = read(2);
        (read(0), [f[0], f[1], f[2], f[3]], t.hlsl.clone())
    });
    for k in kill {
        d3d::release(k);
    }
    out
}

#[test]
fn memory_images_bind_through_the_descriptor_table() {
    let (w0, fault, hlsl) = unsafe { run_img(true) }.unwrap_or_else(|e| panic!("{e}"));
    let mut bad = Vec::new();
    for t in 0..64usize {
        let want = (1.0 + t as f32).to_bits();
        let (ld, sm) = (w0[(0x400 + 0x200) / 4 + t], w0[(0x400 + 0x300) / 4 + t]);
        if ld != want || sm != want {
            bad.push(format!("lane {t}: image_load {ld:08x} image_sample_lz {sm:08x} want {want:08x}"));
        }
    }
    let f = fault_of(&fault);
    if f.count != 0 || fault[3] != 0 {
        bad.push(format!("{} global-window misses (first at {:#x}), {} descriptor misses", f.count, f.first_va, fault[3]));
    }
    assert!(bad.is_empty(), "{}\n{hlsl}", bad.join("\n"));
}

#[test]
fn memory_image_misses_take_the_null_descriptor() {
    let (w0, fault, hlsl) = unsafe { run_img(false) }.unwrap_or_else(|e| panic!("{e}"));
    let mut bad = Vec::new();
    for t in 0..64usize {
        let (ld, sm) = (w0[(0x400 + 0x200) / 4 + t], w0[(0x400 + 0x300) / 4 + t]);
        if ld != 0 || sm != 0 {
            bad.push(format!("lane {t}: image_load {ld:08x} image_sample_lz {sm:08x} want 0 (null SRV)"));
        }
    }
    if fault[3] == 0 {
        bad.push("descriptor miss not counted".into());
    }
    assert!(bad.is_empty(), "{}\n{hlsl}", bad.join("\n"));
}
