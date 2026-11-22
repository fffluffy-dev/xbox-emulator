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
