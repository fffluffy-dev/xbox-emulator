//! Parsers for the standard (PC-format) metadata chunks that survive in XSX shader
//! containers: ISG1/OSG1 signatures, PSV0 resource bindings and the RTS0 root signature.
//! These give the HLSL emitter the D3D-side view (semantics, registers, root parameters).

fn u32_at(b: &[u8], off: usize) -> Result<u32, String> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| format!("meta: u32 out of bounds at {off:#x} (len {})", b.len()))
}

fn cstr_at(b: &[u8], off: usize) -> String {
    let tail = b.get(off..).unwrap_or(&[]);
    let end = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[..end]).into_owned()
}

/// One element of an ISG1/OSG1 signature.
#[derive(Debug, Clone)]
pub struct SigElement {
    pub name: String,
    pub index: u32,
    /// D3D_NAME system value (0 = none, 1 = SV_Position, 64 = SV_Target, ...).
    pub sysval: u32,
    /// D3D_REGISTER_COMPONENT_TYPE: 1 = uint, 2 = int, 3 = float.
    pub comp_type: u32,
    pub register: u32,
    pub mask: u8,
    pub rw_mask: u8,
}

pub fn parse_signature(tag: &[u8; 4], body: &[u8]) -> Result<Vec<SigElement>, String> {
    let count = u32_at(body, 0)? as usize;
    if count == 0 {
        return Ok(Vec::new());
    }
    if count > 64 {
        return Err(format!("implausible signature element count {count}"));
    }
    let is_24 = matches!(tag, b"ISGN" | b"OSGN" | b"PCSG");
    let stride = if is_24 { 24 } else { 32 };
    if 8 + count * stride > body.len() {
        return Err(format!("signature body truncated for {count} elements"));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        if is_24 {
            let e = 8 + i * 24;
            let name_off = u32_at(body, e)? as usize;
            out.push(SigElement {
                name: cstr_at(body, name_off),
                index: u32_at(body, e + 4)?,
                sysval: u32_at(body, e + 8)?,
                comp_type: u32_at(body, e + 12)?,
                register: u32_at(body, e + 16)?,
                mask: *body.get(e + 20).ok_or("sig mask oob")?,
                rw_mask: *body.get(e + 21).ok_or("sig rwmask oob")?,
            });
        } else {
            let e = 8 + i * 32;
            let name_off = u32_at(body, e + 4)? as usize;
            out.push(SigElement {
                name: cstr_at(body, name_off),
                index: u32_at(body, e + 8)?,
                sysval: u32_at(body, e + 12)?,
                comp_type: u32_at(body, e + 16)?,
                register: u32_at(body, e + 20)?,
                mask: *body.get(e + 24).ok_or("sig mask oob")?,
                rw_mask: *body.get(e + 25).ok_or("sig rwmask oob")?,
            });
        }
    }
    Ok(out)
}

/// A PSV0 resource binding.
#[derive(Debug, Clone)]
pub struct PsvResource {
    /// 1 Sampler, 2 CBV, 3 SRVTyped, 4 SRVRaw, 5 SRVStructured, 6 UAVTyped, 7 UAVRaw, 8 UAVStructured...
    pub res_type: u32,
    pub space: u32,
    pub lower: u32,
    pub upper: u32,
    pub kind: u32,
}

/// Parse the PSV0 chunk far enough to recover the resource-binding list.
/// Geometry-shader facts from the PSV0 runtime info.
#[derive(Debug, Clone, Copy)]
pub struct GsInfo {
    /// D3D input primitive: 1 point, 2 line, 3 triangle, 6 line-adj, 7 triangle-adj.
    pub input_primitive: u32,
    /// D3D output topology: 1 point list, 3 line strip, 5 triangle strip.
    pub output_topology: u32,
    pub max_vertex_count: u16,
    pub mesh: Option<MeshInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MeshInfo {
    pub stage: u8,
    pub groupshared_bytes: u32,
    pub payload_bytes: u32,
    pub max_vertices: u16,
    pub max_primitives: u16,
    pub prim_vectors: u8,
    pub topology: u8,
    pub num_threads: [u32; 3],
}

pub(crate) fn mesh_thread_count(dims: [u32; 3]) -> Result<u32, String> {
    let count = dims.into_iter().try_fold(1u32, u32::checked_mul);
    match count {
        Some(n @ 1..=128) => Ok(n),
        _ => Err(format!("mesh/amplification thread-group size {dims:?} outside 1..128")),
    }
}

pub fn parse_psv0_mesh(info: &[u8]) -> Option<MeshInfo> {
    let stage = *info.get(24)?;
    let num_threads = if info.len() >= 48 { [u32_at(info, 36).ok()?, u32_at(info, 40).ok()?, u32_at(info, 44).ok()?] } else { [0; 3] };
    match stage {
        13 => Some(MeshInfo {
            stage,
            groupshared_bytes: u32_at(info, 0).ok()?,
            payload_bytes: u32_at(info, 8).ok()?,
            max_vertices: u16::from_le_bytes([*info.get(12)?, *info.get(13)?]),
            max_primitives: u16::from_le_bytes([*info.get(14)?, *info.get(15)?]),
            prim_vectors: *info.get(26)?,
            topology: *info.get(27)?,
            num_threads,
        }),
        14 => Some(MeshInfo {
            stage,
            groupshared_bytes: 0,
            payload_bytes: u32_at(info, 0).ok()?,
            max_vertices: 0,
            max_primitives: 0,
            prim_vectors: 0,
            topology: 0,
            num_threads,
        }),
        _ => None,
    }
}

pub fn parse_psv0_cs_threads(body: &[u8]) -> Option<[u32; 3]> {
    let info_size = u32_at(body, 0).ok()? as usize;
    if info_size < 48 {
        return None;
    }
    let info = body.get(4..4 + info_size)?;
    let t = [u32_at(info, 36).ok()?, u32_at(info, 40).ok()?, u32_at(info, 44).ok()?];
    let prod = (t[0] as u64).checked_mul(t[1] as u64)?.checked_mul(t[2] as u64)?;
    if t.iter().any(|d| !(1..=1024).contains(d)) || prod > 1024 {
        return None;
    }
    Some(t)
}

/// Parse the GS part of PSV0's PSVRuntimeInfo: the stage union starts at +4
/// ({InputPrimitive u32, OutputTopology u32, OutputStreamMask u32, ...}); PSVRuntimeInfo1
/// adds ShaderStage u8 @+24, UsesViewID u8 @+25 and MaxVertexCount u16 @+26.
pub fn parse_psv0_gs(body: &[u8]) -> Option<GsInfo> {
    let info_size = u32_at(body, 0).ok()? as usize;
    if info_size < 28 {
        return None;
    }
    let info = body.get(4..4 + info_size)?;
    Some(GsInfo {
        input_primitive: u32_at(info, 0).ok()?,
        output_topology: u32_at(info, 4).ok()?,
        max_vertex_count: u16::from_le_bytes([*info.get(26)?, *info.get(27)?]),
        mesh: parse_psv0_mesh(info),
    })
}

pub fn parse_psv0(body: &[u8]) -> Result<Vec<PsvResource>, String> {
    let info_size = u32_at(body, 0)? as usize;
    let mut p = 4 + info_size;
    let count = u32_at(body, p)? as usize;
    p += 4;
    if count == 0 {
        return Ok(vec![]);
    }
    if count > 256 {
        return Err(format!("implausible PSV0 resource count {count}"));
    }
    let bind_size = u32_at(body, p)? as usize;
    p += 4;
    if bind_size < 16 {
        return Err(format!("PSV0 bind size {bind_size} too small"));
    }
    let mut out = Vec::with_capacity(count);
    for i in 0..count {
        let e = p + i * bind_size;
        out.push(PsvResource {
            res_type: u32_at(body, e)?,
            space: u32_at(body, e + 4)?,
            lower: u32_at(body, e + 8)?,
            upper: u32_at(body, e + 12)?,
            kind: if bind_size >= 24 { u32_at(body, e + 16)? } else { 0 },
        });
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct RootRange {
    /// 0 SRV, 1 UAV, 2 CBV, 3 Sampler.
    pub range_type: u32,
    pub num: u32,
    pub base_register: u32,
    pub space: u32,
    pub offset_in_table: u32,
}

#[derive(Debug, Clone)]
pub enum RootParamKind {
    Table(Vec<RootRange>),
    Constants { register: u32, space: u32, num32: u32 },
    /// 2 = CBV, 3 = SRV, 4 = UAV root descriptor.
    Descriptor { kind: u32, register: u32, space: u32 },
}

#[derive(Debug, Clone)]
pub struct RootParam {
    pub visibility: u32,
    pub kind: RootParamKind,
}

#[derive(Debug, Clone)]
pub struct RootSig {
    pub version: u32,
    pub flags: u32,
    pub params: Vec<RootParam>,
    pub num_static_samplers: u32,
    pub static_samplers: Vec<StaticSampler>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StaticSampler {
    pub filter: u32,
    pub address_v: u32,
    pub address_w: u32,
    pub max_anisotropy: u32,
    pub comparison_func: u32,
    pub border_color: u32,
    pub min_lod_bits: u32,
    pub max_lod_bits: u32,
    pub address_u: u32,
    pub register: u32,
    pub space: u32,
    pub visibility: u32,
}

/// Parse an RTS0 (serialized root signature) chunk body.
pub fn parse_rts0(body: &[u8]) -> Result<RootSig, String> {
    let version = u32_at(body, 0)?;
    let nparams = u32_at(body, 4)? as usize;
    let params_off = u32_at(body, 8)? as usize;
    let num_static_samplers = u32_at(body, 12)?;
    let flags = u32_at(body, 20)?;
    if nparams > 64 {
        return Err(format!("implausible root parameter count {nparams}"));
    }
    let v11 = version >= 2;
    let mut params = Vec::with_capacity(nparams);
    for i in 0..nparams {
        let e = params_off + i * 12;
        let ptype = u32_at(body, e)?;
        let visibility = u32_at(body, e + 4)?;
        let off = u32_at(body, e + 8)? as usize;
        let kind = match ptype {
            0 => {
                let nranges = u32_at(body, off)? as usize;
                let roff = u32_at(body, off + 4)? as usize;
                if nranges > 64 {
                    return Err(format!("implausible range count {nranges}"));
                }
                let stride = if v11 { 24 } else { 20 };
                let mut ranges = Vec::with_capacity(nranges);
                for r in 0..nranges {
                    let q = roff + r * stride;
                    ranges.push(RootRange {
                        range_type: u32_at(body, q)?,
                        num: u32_at(body, q + 4)?,
                        base_register: u32_at(body, q + 8)?,
                        space: u32_at(body, q + 12)?,
                        offset_in_table: u32_at(body, q + if v11 { 20 } else { 16 })?,
                    });
                }
                RootParamKind::Table(ranges)
            }
            1 => RootParamKind::Constants {
                register: u32_at(body, off)?,
                space: u32_at(body, off + 4)?,
                num32: u32_at(body, off + 8)?,
            },
            2..=4 => RootParamKind::Descriptor {
                kind: ptype,
                register: u32_at(body, off)?,
                space: u32_at(body, off + 4)?,
            },
            other => return Err(format!("unknown root parameter type {other}")),
        };
        params.push(RootParam { visibility, kind });
    }
    let mut static_samplers = Vec::new();
    if num_static_samplers <= 64 {
        if let Ok(so) = u32_at(body, 16) {
            let stride = if version >= 3 { 56 } else { 52 };
            for i in 0..num_static_samplers as usize {
                let q = so as usize + i * stride;
                if let (Ok(address_u), Ok(register), Ok(space), Ok(visibility)) = (u32_at(body, q + 4), u32_at(body, q + 40), u32_at(body, q + 44), u32_at(body, q + 48)) {
                    let at = |o: usize| u32_at(body, q + o).unwrap_or(0);
                    static_samplers.push(StaticSampler {
                        filter: at(0),
                        address_v: at(8),
                        address_w: at(12),
                        max_anisotropy: at(20),
                        comparison_func: at(24),
                        border_color: at(28),
                        min_lod_bits: at(32),
                        max_lod_bits: at(36),
                        address_u,
                        register,
                        space,
                        visibility,
                    });
                }
            }
        }
    }
    Ok(RootSig { version, flags, params, num_static_samplers, static_samplers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_24byte_isgn_and_32byte_isg1_signatures() {
        let mut isgn = Vec::new();
        isgn.extend_from_slice(&1u32.to_le_bytes());
        isgn.extend_from_slice(&8u32.to_le_bytes());
        let name_offset_24 = 8 + 24;
        isgn.extend_from_slice(&(name_offset_24 as u32).to_le_bytes());
        isgn.extend_from_slice(&0u32.to_le_bytes());
        isgn.extend_from_slice(&0u32.to_le_bytes());
        isgn.extend_from_slice(&3u32.to_le_bytes());
        isgn.extend_from_slice(&0u32.to_le_bytes());
        isgn.push(0x0f);
        isgn.push(0x0f);
        isgn.extend_from_slice(&[0, 0]);
        isgn.extend_from_slice(b"POSITION\0");

        let sig_24 = parse_signature(b"ISGN", &isgn).expect("parse ISGN");
        assert_eq!(sig_24.len(), 1);
        assert_eq!(sig_24[0].name, "POSITION");
        assert_eq!(sig_24[0].comp_type, 3);
        assert_eq!(sig_24[0].register, 0);
        assert_eq!(sig_24[0].mask, 0x0f);
        assert_eq!(sig_24[0].rw_mask, 0x0f);

        let mut isg1 = Vec::new();
        isg1.extend_from_slice(&1u32.to_le_bytes());
        isg1.extend_from_slice(&8u32.to_le_bytes());
        let name_offset_32 = 8 + 32;
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.extend_from_slice(&(name_offset_32 as u32).to_le_bytes());
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.extend_from_slice(&3u32.to_le_bytes());
        isg1.extend_from_slice(&0u32.to_le_bytes());
        isg1.push(0x0f);
        isg1.push(0x0f);
        isg1.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        isg1.extend_from_slice(b"POSITION\0");

        let sig_32 = parse_signature(b"ISG1", &isg1).expect("parse ISG1");
        assert_eq!(sig_32.len(), 1);
        assert_eq!(sig_32[0].name, "POSITION");
        assert_eq!(sig_32[0].comp_type, 3);
        assert_eq!(sig_32[0].register, 0);
        assert_eq!(sig_32[0].mask, 0x0f);
        assert_eq!(sig_32[0].rw_mask, 0x0f);
    }

    fn cs_blob(info_size: u32, dims: [u32; 3]) -> Vec<u8> {
        let mut b = vec![0u8; 4 + info_size as usize];
        b[..4].copy_from_slice(&info_size.to_le_bytes());
        if (info_size as usize) >= 48 {
            for (i, d) in dims.iter().enumerate() {
                let o = 4 + 36 + i * 4;
                b[o..o + 4].copy_from_slice(&d.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn cs_thread_counts_reject_a_bad_group_size() {
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [8, 8, 1])), Some([8, 8, 1]));
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [32, 1, 1])), Some([32, 1, 1]));
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [32, 32, 1])), Some([32, 32, 1]));
        assert_eq!(parse_psv0_cs_threads(&cs_blob(36, [8, 8, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(12, [8, 8, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [0, 8, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [1025, 1, 1])), None);
        assert_eq!(parse_psv0_cs_threads(&cs_blob(48, [32, 32, 2])), None);
        assert_eq!(parse_psv0_cs_threads(&48u32.to_le_bytes()), None);
        let mut union_only = cs_blob(48, [4, 2, 1]);
        union_only[4..16].copy_from_slice(&[8, 0, 0, 0, 8, 0, 0, 0, 1, 0, 0, 0]);
        assert_eq!(parse_psv0_cs_threads(&union_only), Some([4, 2, 1]));
    }
}

/// One entry of the vertex-shader input table in the FSSM header: the fetch subroutine the
/// runtime builds from the input layout fills `count` VGPRs from `first_vgpr` with components
/// 0..count of input register `register` (ISG1). `read_mask` is the components the code reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FssmVertexInput {
    pub register: u32,
    pub read_mask: u8,
    pub first_vgpr: u32,
    pub count: u32,
}

/// Vertex input table of an FSSM blob: 5-byte entries from offset 0x138 while byte 0 is 6
/// (byte 1 input register, byte 2 read mask, byte 3 first VGPR, byte 4 highest component read).
/// Layout as documented by the americas-recomp project (docs/xbox/XBOX_SHADERS.md, "Vertex fetch
/// subroutine"), checked there on 2,236 Xbox Series vertex shaders: first VGPRs 4, 8, 12, ... for
/// shaders opening with `s_swappc_b64 s[4:5]` and 9, 13, ... for the VS merged in front of a GS.
/// Empty when the blob has no such table (shaders without vertex inputs).
pub fn fssm_vertex_inputs(fssm: &[u8]) -> Vec<FssmVertexInput> {
    let mut out = Vec::new();
    if fssm.get(0..4) != Some(b"FSSM") {
        return out;
    }
    let end = fssm.len().min(0x1d0);
    let mut p = 0x138;
    while p + 5 <= end && fssm[p] == 6 {
        let e = &fssm[p..p + 5];
        if e[4] > 3 {
            return Vec::new();
        }
        out.push(FssmVertexInput { register: e[1] as u32, read_mask: e[2], first_vgpr: e[3] as u32, count: e[4] as u32 + 1 });
        p += 5;
    }
    out
}

#[cfg(test)]
mod fssm_vin_tests {
    use super::*;

    fn blob(entries: &[[u8; 5]]) -> Vec<u8> {
        let mut b = vec![0u8; 0x200];
        b[0..4].copy_from_slice(b"FSSM");
        for (k, e) in entries.iter().enumerate() {
            b[0x138 + 5 * k..0x138 + 5 * k + 5].copy_from_slice(e);
        }
        b
    }

    #[test]
    fn reads_entries_until_the_tag_stops() {
        let b = blob(&[[6, 0, 0x7, 9, 2], [6, 2, 0x3, 13, 1]]);
        assert_eq!(
            fssm_vertex_inputs(&b),
            vec![
                FssmVertexInput { register: 0, read_mask: 7, first_vgpr: 9, count: 3 },
                FssmVertexInput { register: 2, read_mask: 3, first_vgpr: 13, count: 2 },
            ]
        );
    }

    #[test]
    fn no_table_without_fssm_magic_or_tag() {
        assert!(fssm_vertex_inputs(&blob(&[])).is_empty());
        let mut b = blob(&[[6, 0, 0xf, 4, 3]]);
        b[0..4].copy_from_slice(b"XXXX");
        assert!(fssm_vertex_inputs(&b).is_empty());
        assert!(fssm_vertex_inputs(&blob(&[[6, 0, 0xf, 4, 7]])).is_empty(), "component index past w is not a table");
    }
}

/// Wave size the FSSM header declares: `Some(true)` for wave32, `Some(false)` for wave64 (flags
/// word u32[0xD4], bit 5), `None` without an FSSM blob. Durango (GCN) blobs are not FSSM-headed
/// this way and give `None`. Rule from the Leonida project (docs/design.md "Wave size").
pub fn fssm_wave32(fssm: &[u8]) -> Option<bool> {
    if fssm.get(0..4) != Some(b"FSSM") {
        return None;
    }
    let w = fssm.get(0xd4..0xd8)?;
    Some(u32::from_le_bytes([w[0], w[1], w[2], w[3]]) & 0x20 != 0)
}

#[cfg(test)]
mod fssm_wave_tests {
    use super::fssm_wave32;

    #[test]
    fn flags_word_bit5_is_wave32() {
        let mut b = vec![0u8; 0x100];
        b[0..4].copy_from_slice(b"FSSM");
        assert_eq!(fssm_wave32(&b), Some(false));
        b[0xd4] = 0x20;
        assert_eq!(fssm_wave32(&b), Some(true));
        assert_eq!(fssm_wave32(&b[..0xd6]), None);
        assert_eq!(fssm_wave32(&[]), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FssmRootKind {
    Constants,
    RootDescriptor,
    TableDescriptor,
    StaticSampler,
    ResourceHeap,
    SamplerHeap,
    Other(u32),
}

impl FssmRootKind {
    pub fn from_raw(k: u32) -> Self {
        match k {
            0 => Self::Constants,
            1 => Self::RootDescriptor,
            3 => Self::TableDescriptor,
            4 => Self::StaticSampler,
            5 => Self::ResourceHeap,
            6 => Self::SamplerHeap,
            x => Self::Other(x),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FssmRootEntry {
    pub kind: FssmRootKind,
    pub offset_dwords: u32,
    pub count: u32,
    pub register: u32,
    pub root_param: u32,
    pub user_sgpr: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FssmRoot {
    pub user_sgprs: u32,
    pub entries: Vec<FssmRootEntry>,
}

pub const FSSM_NO_ROOT_TABLE: u32 = 0xdead_beef;
pub const FSSM_ROOT_ENTRY_BYTES: usize = 20;
pub const FSSM_ROOT_MAX_ENTRIES: usize = 4096;

pub fn fssm_root_table(fssm: &[u8]) -> Option<FssmRoot> {
    if fssm.get(0..4) != Some(b"FSSM") {
        return None;
    }
    let rd = |o: usize| fssm.get(o..o + 4).map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]));
    let user_sgprs = *fssm.get(0xd6)? as u32;
    let n = rd(0xf8)? as usize;
    if rd(0xe8)? == FSSM_NO_ROOT_TABLE || n == 0 {
        return Some(FssmRoot { user_sgprs, entries: Vec::new() });
    }
    if n > FSSM_ROOT_MAX_ENTRIES {
        return None;
    }
    let first = (rd(0xf0)? as usize).checked_sub(4 + FSSM_ROOT_ENTRY_BYTES * (n - 1))?;
    if first < 0xfc || first + FSSM_ROOT_ENTRY_BYTES * n > fssm.len() {
        return None;
    }
    let mut entries = Vec::with_capacity(n);
    for k in 0..n {
        let e = first + FSSM_ROOT_ENTRY_BYTES * k;
        let loc = rd(e + 16)?;
        entries.push(FssmRootEntry {
            kind: FssmRootKind::from_raw(rd(e)?),
            offset_dwords: rd(e + 4)?,
            count: rd(e + 8)?,
            register: rd(e + 12)?,
            root_param: loc >> 8,
            user_sgpr: loc & 0xff,
        });
    }
    Some(FssmRoot { user_sgprs, entries })
}

pub fn fssm_user_data_base(merged_or_ngg_stage: bool) -> u32 {
    if merged_or_ngg_stage { 8 } else { 0 }
}

#[cfg(test)]
mod fssm_root_tests {
    use super::*;

    fn blob(user_sgprs: u8, entries: &[[u32; 5]], at: usize, no_table: bool) -> Vec<u8> {
        let mut b = vec![0u8; at + 20 * entries.len() + 16];
        b[0..4].copy_from_slice(b"FSSM");
        b[0xd6] = user_sgprs;
        b[0xe8..0xec].copy_from_slice(&(if no_table { FSSM_NO_ROOT_TABLE } else { 0 }).to_le_bytes());
        b[0xf8..0xfc].copy_from_slice(&(entries.len() as u32).to_le_bytes());
        if !entries.is_empty() {
            let last = at + 20 * (entries.len() - 1) + 4;
            b[0xf0..0xf4].copy_from_slice(&(last as u32).to_le_bytes());
        }
        for (k, e) in entries.iter().enumerate() {
            for (j, v) in e.iter().enumerate() {
                let o = at + 20 * k + 4 * j;
                b[o..o + 4].copy_from_slice(&v.to_le_bytes());
            }
        }
        b
    }

    #[test]
    fn reads_every_entry_from_the_last_entry_pointer() {
        let b = blob(17, &[[0, 0, 4, 0, (0 << 8) | 8], [1, 0, 2, 1, (1 << 8) | 12], [3, 6, 8, 2, (2 << 8) | 14], [5, 0, 1, 0, (3 << 8) | 16]], 0x200, false);
        let r = fssm_root_table(&b).expect("table");
        assert_eq!(r.user_sgprs, 17);
        assert_eq!(r.entries.len(), 4);
        assert_eq!(r.entries[0], FssmRootEntry { kind: FssmRootKind::Constants, offset_dwords: 0, count: 4, register: 0, root_param: 0, user_sgpr: 8 });
        assert_eq!(r.entries[2], FssmRootEntry { kind: FssmRootKind::TableDescriptor, offset_dwords: 6, count: 8, register: 2, root_param: 2, user_sgpr: 14 });
        assert_eq!(r.entries[3].kind, FssmRootKind::ResourceHeap);
        assert_eq!(r.entries[1].kind, FssmRootKind::RootDescriptor);
    }

    #[test]
    fn deadbeef_or_zero_count_means_no_table() {
        let b = blob(17, &[[0, 0, 4, 0, 8]], 0x200, true);
        assert_eq!(fssm_root_table(&b), Some(FssmRoot { user_sgprs: 17, entries: Vec::new() }));
        let b = blob(9, &[], 0x200, false);
        assert_eq!(fssm_root_table(&b), Some(FssmRoot { user_sgprs: 9, entries: Vec::new() }));
    }

    #[test]
    fn refuses_tables_outside_the_blob_and_foreign_blobs() {
        let mut b = blob(17, &[[0, 0, 4, 0, 8], [6, 0, 1, 0, 9]], 0x200, false);
        assert!(fssm_root_table(&b[..0x200 + 30]).is_none());
        b[0xf0..0xf4].copy_from_slice(&8u32.to_le_bytes());
        assert!(fssm_root_table(&b).is_none());
        b[0xf8..0xfc].copy_from_slice(&100_000u32.to_le_bytes());
        assert!(fssm_root_table(&b).is_none());
        b[0..4].copy_from_slice(b"XXXX");
        assert!(fssm_root_table(&b).is_none());
        assert!(fssm_root_table(&[]).is_none());
    }

    #[test]
    fn kinds_and_user_data_base() {
        assert_eq!(FssmRootKind::from_raw(4), FssmRootKind::StaticSampler);
        assert_eq!(FssmRootKind::from_raw(6), FssmRootKind::SamplerHeap);
        assert_eq!(FssmRootKind::from_raw(2), FssmRootKind::Other(2));
        assert_eq!(fssm_user_data_base(true), 8);
        assert_eq!(fssm_user_data_base(false), 0);
    }
}
