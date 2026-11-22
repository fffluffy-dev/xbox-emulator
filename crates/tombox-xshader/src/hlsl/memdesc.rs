//! Descriptors read from memory at run time ("memory descriptor" mode).
//!
//! A shader without a root signature (all 8,765 GTA VI mesh programs, for one) gets its
//! descriptor-table pointers and constants in user SGPRs that the console driver loads from the
//! title's root arguments, and it reads V# / T# descriptors out of memory with `s_load`. The
//! static resolver cannot map those to root parameters, so translation used to stop at the first
//! resource load. This mode follows the model of the Leonida project (GTA VI static
//! recompilation, GPLv3+, docs/design.md "Translator status and runtime contract"): the initial
//! s0..s31 come from a runtime constant buffer, every memory access goes to the console GPU
//! address through the global windows (`global.rs`), and buffer accesses take base, stride and
//! size from the V# value the shader loaded. Images and samplers are bindless: the shader keys
//! the T# / S# it loaded (`global_abi::desc_key`), finds the heap index in a lookup table the
//! runtime keeps (`global_abi::desc_table`, ByteAddressBuffer t0 in the global space) and takes
//! the object from `ResourceDescriptorHeap` / `SamplerDescriptorHeap` (SM 6.6). An unknown
//! descriptor takes the table's miss index (a null descriptor) and is counted in dword 3 of the
//! fault record. Image loads bind an SRV view and stores / atomics a UAV view of the T#.
//!
//! The mode is only a fallback: `translate` uses it when the normal translation fails on an
//! unresolvable descriptor and the shader has no root signature, so shaders that translate today
//! are unchanged.

use super::{r, sreg_opnd, Emitter, Res, MEMIMG};
use crate::global_abi::{DESC_MAX_PROBE, DESC_SAMPLER, DESC_SAMPLER_CMP, DESC_SRV, DESC_UAV, TB_DESC_TABLE_SRV, TB_GLOBAL_SPACE, TB_USER_CBV};
use crate::rdna2::{Insn, Opnd};

/// User data: initial values of s0..s31 for the wave, written by the runtime.
pub(super) fn user_cbuffer() -> String {
    format!("cbuffer TbUser : register(b{TB_USER_CBV}, space{TB_GLOBAL_SPACE}) {{ uint4 tb_user[8]; }};\n")
}

/// Initial value of a scalar register in memory-descriptor mode (s0..s31 from TbUser).
pub(super) fn user_init(reg: &str) -> Option<String> {
    let n: u32 = reg.strip_prefix('s')?.parse().ok()?;
    (n < 32).then(|| format!("tb_user[{}].{}", n / 4, ["x", "y", "z", "w"][(n % 4) as usize]))
}

/// The T# / S# lookup (`global_abi::desc_key` / `desc_lookup` on the GPU). Needs the global
/// helpers (tb_gfault).
pub(super) fn dtab_helpers() -> String {
    format!(
        r#"ByteAddressBuffer tb_dtab : register(t{TB_DESC_TABLE_SRV}, space{TB_GLOBAL_SPACE});
uint tb_dkey(uint kind, uint4 d) {{
    uint h = 2166136261u;
    h = (h ^ kind) * 16777619u;
    h = (h ^ d.x) * 16777619u;
    h = (h ^ d.y) * 16777619u;
    h = (h ^ d.z) * 16777619u;
    h = (h ^ d.w) * 16777619u;
    return max(h, 1u);
}}
uint tb_didx(uint kind, uint4 d) {{
    uint key = tb_dkey(kind, d);
    uint m = tb_dtab.Load(0);
    uint i = key & m;
    [loop] for (uint k = 0u; k < {DESC_MAX_PROBE}u; k++) {{
        uint2 e = tb_dtab.Load2(16u + 8u * i);
        if (e.x == key) return e.y;
        if (e.x == 0u) break;
        i = (i + 1u) & m;
    }}
    uint p;
    tb_gfault.InterlockedAdd(12, 1u, p);
    return tb_dtab.Load(4u + 4u * min(kind, 2u));
}}
"#
    )
}

fn sgpr(n: u8) -> Opnd {
    sreg_opnd(n)
}

impl Emitter<'_> {
    fn soffset_expr(&mut self, ins: &Insn) -> Result<String, String> {
        let soff = ins.raw.get(1).copied().unwrap_or(0) >> 25;
        Ok(match soff {
            0x7d | 0x80 => "0u".to_string(),
            0..=105 => {
                let o = sgpr(soff as u8);
                self.touch(&o);
                r(&o)?
            }
            124 => {
                self.touch(&Opnd::M0);
                r(&Opnd::M0)?
            }
            other => return Err(format!("{} at {:#x}: soffset code {other} unsupported for a memory descriptor", ins.mnemonic, ins.offset)),
        })
    }

    /// `s_load_dword{,x2,x4,x8,x16} sD, s[B:B+1], imm [+ soffset]` from a run-time pointer.
    pub(super) fn emit_mem_sload(&mut self, ins: &Insn) -> Result<(), String> {
        let n = 1u32 << ins.op;
        let b = match ins.src.first() {
            Some(Opnd::Sgpr(b)) => *b,
            _ => return Err("s_load: bad base".into()),
        };
        let d = match ins.dst.first() {
            Some(Opnd::Sgpr(d)) => *d,
            _ => return Err("s_load: bad dst".into()),
        };
        let (lo, hi) = (sgpr(b), sgpr(b + 1));
        self.touch(&lo);
        self.touch(&hi);
        let so = self.soffset_expr(ins)?;
        self.uses_global = true;
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint t{t}a = {} + {:#x}u + {so}; uint t{t}h = ({} & 0xffffu) + (t{t}a < {} ? 1u : 0u);", r(&lo)?, ins.aux.imm as u32 & 0x1f_ffff, r(&hi)?, r(&lo)?));
        for k in 0..n {
            self.line(&format!("uint t{t}_{k} = tb_gmem_load(tb_gva(t{t}a, t{t}h, {}u, 0));", 4 * k));
        }
        for k in 0..n {
            let o = sgpr(d + k as u8);
            self.touch(&o);
            self.ws(&o, &format!("t{t}_{k}"))?;
        }
        Ok(())
    }

    /// `s_buffer_load_dword{,x2,x4,x8,x16} sD, s[V:V+3], imm [+ soffset]` with a run-time V#:
    /// base = V#[47:0], size = V#.num_records (bytes); out-of-range dwords read 0.
    pub(super) fn emit_mem_sbuf(&mut self, ins: &Insn) -> Result<(), String> {
        let n = 1u32 << (ins.op - 8);
        let v = match ins.src.first() {
            Some(Opnd::Sgpr(v)) => *v,
            _ => return Err("s_buffer_load: bad base".into()),
        };
        let d = match ins.dst.first() {
            Some(Opnd::Sgpr(d)) => *d,
            _ => return Err("s_buffer_load: bad dst".into()),
        };
        let vs: Vec<Opnd> = (0..3).map(|k| sgpr(v + k)).collect();
        for o in &vs {
            self.touch(o);
        }
        let so = self.soffset_expr(ins)?;
        self.uses_global = true;
        let t = self.tmp;
        self.tmp += 1;
        self.line(&format!("uint t{t}o = {:#x}u + {so};", ins.aux.imm as u32 & 0x1f_ffff));
        for k in 0..n {
            self.line(&format!(
                "uint t{t}_{k} = (t{t}o + {e}u <= {nr}) ? tb_gmem_load(tb_gva({lo}, {hi} & 0xffffu, t{t}o + {b}u, 0)) : 0u;",
                e = 4 * k + 4,
                b = 4 * k,
                nr = r(&vs[2])?,
                lo = r(&vs[0])?,
                hi = r(&vs[1])?
            ));
        }
        for k in 0..n {
            let o = sgpr(d + k as u8);
            self.touch(&o);
            self.ws(&o, &format!("t{t}_{k}"))?;
        }
        Ok(())
    }

    /// MUBUF dword loads / stores with a run-time V#: address = base + index * stride (IDXEN) +
    /// offset VGPR (OFFEN) + soffset + imm; raw buffers (stride 0 or no IDXEN) are bounded by
    /// num_records bytes, structured ones by num_records elements.
    pub(super) fn emit_mem_mubuf(&mut self, ins: &Insn) -> Result<(), String> {
        let m = ins.mnemonic.as_str();
        let (store, n) = match m {
            "buffer_load_dword" => (false, 1u8),
            "buffer_load_dwordx2" => (false, 2),
            "buffer_load_dwordx3" => (false, 3),
            "buffer_load_dwordx4" => (false, 4),
            "buffer_store_dword" => (true, 1),
            "buffer_store_dwordx2" => (true, 2),
            "buffer_store_dwordx3" => (true, 3),
            "buffer_store_dwordx4" => (true, 4),
            _ => return Err(format!("{m} at {:#x}: only dword buffer loads and stores are supported for a memory descriptor", ins.offset)),
        };
        let (offen, idxen) = (ins.aux.mask & 1 != 0, ins.aux.mask & 2 != 0);
        let va = match ins.src.first() {
            Some(Opnd::Vgpr(a)) => *a,
            _ => return Err("mubuf: bad vaddr".into()),
        };
        let v = match ins.src.get(1) {
            Some(Opnd::Sgpr(v)) => *v,
            _ => return Err("mubuf: bad srsrc".into()),
        };
        let vd = match ins.dst.first() {
            Some(Opnd::Vgpr(d)) => *d,
            _ => return Err("mubuf: bad vdata".into()),
        };
        if vd as usize + n as usize > 256 || va as usize + 2 > 256 {
            return Err(format!("{m} at {:#x}: register range past v255", ins.offset));
        }
        let vs: Vec<Opnd> = (0..3).map(|k| sgpr(v + k)).collect();
        for o in &vs {
            self.touch(o);
        }
        let so = match ins.src.get(2) {
            Some(o @ Opnd::Sgpr(_)) => {
                self.touch(o);
                r(o)?
            }
            Some(Opnd::ConstInt(c)) => format!("{:#x}u", *c as u32),
            Some(Opnd::M0) => {
                self.touch(&Opnd::M0);
                r(&Opnd::M0)?
            }
            _ => "0u".to_string(),
        };
        let (idx_e, off_e) = match (idxen, offen) {
            (true, true) => (Some(Opnd::Vgpr(va)), Some(Opnd::Vgpr(va + 1))),
            (true, false) => (Some(Opnd::Vgpr(va)), None),
            (false, true) => (None, Some(Opnd::Vgpr(va))),
            (false, false) => (None, None),
        };
        let mut part = |o: Option<Opnd>| -> Result<String, String> {
            match o {
                Some(o) => {
                    self.touch(&o);
                    r(&o)
                }
                None => Ok("0u".to_string()),
            }
        };
        let ie = part(idx_e)?;
        let oe = part(off_e)?;
        self.uses_global = true;
        let t = self.tmp;
        self.tmp += 1;
        let (lo, hi, nr) = (r(&vs[0])?, r(&vs[1])?, r(&vs[2])?);
        self.line(&format!(
            "uint t{t}s = ({hi} >> 16) & 0x3fffu; uint t{t}i = {ie}; uint t{t}o = {oe} + {so} + {imm:#x}u; bool t{t}k = ({idxen} && t{t}s != 0u) ? (t{t}i < {nr}) : (t{t}o + {last}u <= {nr}); uint t{t}a = t{t}o + ({idxen} ? t{t}i * t{t}s : 0u);",
            imm = ins.aux.imm as u32 & 0xfff,
            idxen = idxen,
            last = 4 * n as u32,
        ));
        for k in 0..n {
            let addr = format!("tb_gva({lo}, {hi} & 0xffffu, t{t}a + {}u, 0)", 4 * k as u32);
            let data = Opnd::Vgpr(vd + k);
            self.touch(&data);
            if store {
                self.line(&format!("if ((exec_lo & 1u) != 0u && t{t}k) tb_gmem_store({addr}, {});", r(&data)?));
            } else {
                self.line(&format!("uint t{t}_{k} = t{t}k ? tb_gmem_load({addr}) : 0u;"));
            }
        }
        if !store {
            for k in 0..n {
                self.wv(&Opnd::Vgpr(vd + k), &format!("t{t}_{k}"))?;
            }
        }
        Ok(())
    }

    /// Resources of a MIMG instruction whose T# / S# came from memory: one local handle per
    /// instruction (slot MEMIMG | index), typed like a bound resource.
    pub(super) fn mem_img_res(&mut self, idx: usize, ins: &Insn) -> Result<Vec<Res>, String> {
        let m = ins.mnemonic.as_str();
        let n = ins.src.len();
        if n < 2 || !matches!(ins.src[n - 2], Opnd::Sgpr(_)) || !matches!(ins.src[n - 1], Opnd::Sgpr(_)) {
            return Err(format!("{m} at {:#x}: bad descriptor operands", ins.offset));
        }
        let dim = ins.aux.imm as u32;
        let slot = MEMIMG | idx as u32;
        self.uses_dtab = true;
        self.uses_global = true;
        self.sm6_minor = self.sm6_minor.max(6);
        let mut v = Vec::new();
        if m.contains("store") || m.contains("atomic") {
            self.uav.insert(slot, dim);
            if m.starts_with("image_atomic_") {
                self.uav_uint.insert(slot);
            }
            v.push(Res::Uav(slot, dim));
        } else {
            self.tex.insert(slot, dim);
            v.push(Res::Tex(slot, dim));
        }
        if m.starts_with("image_sample") || m.starts_with("image_gather") || m == "image_get_lod" {
            if ins.op & 8 != 0 {
                self.samp_cmp.insert(slot);
            } else {
                self.samp.insert(slot);
            }
            v.push(Res::Samp(slot));
        }
        Ok(v)
    }

    fn desc_words(&mut self, base: u8) -> Result<String, String> {
        let mut w = Vec::with_capacity(4);
        for k in 0..4 {
            let o = sgpr(base + k);
            self.touch(&o);
            w.push(r(&o)?);
        }
        Ok(format!("uint4({})", w.join(", ")))
    }

    /// Declares the local handles `emit_mimg` uses for a memory-descriptor instruction.
    pub(super) fn emit_mem_img_bind(&mut self, ins: &Insn, rs: &[Res]) -> Result<(), String> {
        let n = ins.src.len();
        let (Some(&Opnd::Sgpr(tb)), Some(&Opnd::Sgpr(sb))) = (ins.src.get(n.wrapping_sub(2)), ins.src.get(n.wrapping_sub(1))) else {
            return Ok(());
        };
        for res in rs {
            let line = match *res {
                Res::Tex(slot, dim) if slot & MEMIMG != 0 => {
                    let k = self.desc_words(tb)?;
                    format!("{} tex{slot} = ResourceDescriptorHeap[NonUniformResourceIndex(tb_didx({DESC_SRV}u, {k}))];", self.srv_ty(slot, dim)?)
                }
                Res::Uav(slot, dim) if slot & MEMIMG != 0 => {
                    let k = self.desc_words(tb)?;
                    format!("{} uav{slot} = ResourceDescriptorHeap[NonUniformResourceIndex(tb_didx({DESC_UAV}u, {k}))];", self.uav_ty(slot, dim)?)
                }
                Res::Samp(slot) if slot & MEMIMG != 0 => {
                    let k = self.desc_words(sb)?;
                    if self.samp_cmp.contains(&slot) {
                        format!("SamplerComparisonState scmp{slot} = SamplerDescriptorHeap[NonUniformResourceIndex(tb_didx({DESC_SAMPLER_CMP}u, {k}))];")
                    } else {
                        format!("SamplerState samp{slot} = SamplerDescriptorHeap[NonUniformResourceIndex(tb_didx({DESC_SAMPLER}u, {k}))];")
                    }
                }
                _ => continue,
            };
            self.line(&line);
        }
        Ok(())
    }
}
