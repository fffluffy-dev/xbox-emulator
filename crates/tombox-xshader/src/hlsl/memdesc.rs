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
//! size from the V# value the shader loaded. Images (T# -> bindless) are not covered yet.
//!
//! The mode is only a fallback: `translate` uses it when the normal translation fails on an
//! unresolvable descriptor and the shader has no root signature, so shaders that translate today
//! are unchanged.

use super::{r, sreg_opnd, Emitter};
use crate::global_abi::{TB_GLOBAL_SPACE, TB_USER_CBV};
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
}
