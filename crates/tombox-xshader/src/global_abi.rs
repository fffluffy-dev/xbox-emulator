pub const TB_GLOBAL_SPACE: u32 = 4_000_003;
pub const TB_GLOBAL_WINDOWS: usize = 2;
pub const TB_GLOBAL_CBV: u32 = 0;
/// Memory-descriptor mode: initial s0..s31 (cbuffer TbUser, 8 x uint4) in the global space.
pub const TB_USER_CBV: u32 = 1;
/// Memory-descriptor mode, images and samplers: the T# / S# -> heap index lookup table
/// (ByteAddressBuffer, see `desc_table`) in the global space.
pub const TB_DESC_TABLE_SRV: u32 = 0;
/// Lookup kinds: the key of a T# read as an SRV, as a UAV, of an S#, of a comparison S#.
pub const DESC_SRV: u32 = 0;
pub const DESC_UAV: u32 = 1;
pub const DESC_SAMPLER: u32 = 2;
pub const DESC_SAMPLER_CMP: u32 = 3;
/// Longest probe sequence a shader walks before it takes the miss index.
pub const DESC_MAX_PROBE: u32 = 16;
pub const TB_GLOBAL_FAULT_UAV: u32 = TB_GLOBAL_WINDOWS as u32;
pub const TB_GLOBAL_CB_DWORDS: usize = 4 * TB_GLOBAL_WINDOWS + 4;
pub const TB_GLOBAL_FAULT_BYTES: usize = 16;
pub const TB_GLOBAL_ROOT_DWORDS: u32 = 2 * (TB_GLOBAL_WINDOWS as u32 + 2);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Window {
    pub base: u64,
    pub size: u32,
}

pub fn window_cb(win: &[Window], ms_in: [u32; 2]) -> [u32; TB_GLOBAL_CB_DWORDS] {
    let mut c = [0u32; TB_GLOBAL_CB_DWORDS];
    for (i, w) in win.iter().take(TB_GLOBAL_WINDOWS).enumerate() {
        c[4 * i] = w.base as u32;
        c[4 * i + 1] = (w.base >> 32) as u32;
        c[4 * i + 2] = w.size;
    }
    c[4 * TB_GLOBAL_WINDOWS] = ms_in[0];
    c[4 * TB_GLOBAL_WINDOWS + 1] = ms_in[1];
    c
}

pub fn resolve(win: &[Window], va: u64) -> Option<(usize, u32)> {
    win.iter().take(TB_GLOBAL_WINDOWS).enumerate().find_map(|(i, w)| {
        let off = va.wrapping_sub(w.base);
        (w.size != 0 && va >= w.base && off < w.size as u64).then_some((i, off as u32))
    })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Fault {
    pub count: u32,
    pub first_va: u64,
}

pub fn fault_of(rec: &[u32; 4]) -> Fault {
    Fault { count: rec[0], first_va: rec[1] as u64 | (rec[2] as u64) << 32 }
}

/// Key of a descriptor in the lookup table: FNV-1a over the kind and the first four dwords of
/// the T# / S# (the part that names the resource and its format), never 0 (0 marks a free
/// slot). Matches `tb_dkey` in the shader.
pub fn desc_key(kind: u32, d: [u32; 4]) -> u32 {
    let mut h = 2_166_136_261u32;
    for w in [kind, d[0], d[1], d[2], d[3]] {
        h = (h ^ w).wrapping_mul(16_777_619);
    }
    h.max(1)
}

/// The lookup table the shader reads (`tb_didx`): header [mask, miss SRV index, miss UAV index,
/// miss sampler index], then `mask + 1` entries of [key, heap index], open addressing with
/// linear probing from `key & mask`. Grows until every entry is within `DESC_MAX_PROBE` probes.
/// `miss` holds the indices returned for an unknown descriptor (null descriptors, normally).
pub fn desc_table(entries: &[(u32, u32)], miss: [u32; 3]) -> Vec<u32> {
    let mut cap = (2 * entries.len()).next_power_of_two().max(16);
    'grow: loop {
        let mask = cap as u32 - 1;
        let mut slots = vec![[0u32; 2]; cap];
        for &(key, index) in entries {
            let key = key.max(1);
            let mut i = key & mask;
            let mut placed = false;
            for _ in 0..DESC_MAX_PROBE {
                let s = &mut slots[i as usize];
                if s[0] == 0 || s[0] == key {
                    *s = [key, index];
                    placed = true;
                    break;
                }
                i = (i + 1) & mask;
            }
            if !placed {
                cap *= 2;
                continue 'grow;
            }
        }
        let mut t = vec![mask, miss[0], miss[1], miss[2]];
        t.extend(slots.iter().flatten());
        return t;
    }
}

/// Reference lookup (what `tb_didx` does on the GPU).
pub fn desc_lookup(table: &[u32], kind: u32, d: [u32; 4]) -> u32 {
    let key = desc_key(kind, d);
    let mask = table[0];
    let mut i = key & mask;
    for _ in 0..DESC_MAX_PROBE {
        let e = 4 + 2 * i as usize;
        if table[e] == key {
            return table[e + 1];
        }
        if table[e] == 0 {
            break;
        }
        i = (i + 1) & mask;
    }
    table[1 + kind.min(2) as usize]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_table_and_resolution() {
        let w = [Window { base: 0x10_0000_0000, size: 0x100 }, Window { base: 0x7f10_0000_0000, size: 64 }];
        let c = window_cb(&w, [3, 0x1234]);
        assert_eq!(&c[..8], &[0, 0x10, 0x100, 0, 0, 0x7f10, 64, 0]);
        assert_eq!(&c[8..10], &[3, 0x1234]);
        assert_eq!(resolve(&w, 0x10_0000_00fc), Some((0, 0xfc)));
        assert_eq!(resolve(&w, 0x10_0000_0100), None);
        assert_eq!(resolve(&w, 0x7f10_0000_0010), Some((1, 0x10)));
        assert_eq!(resolve(&w, 0x0f_ffff_fffc), None);
        assert_eq!(fault_of(&[2, 0x40, 0x7f, 0]), Fault { count: 2, first_va: 0x7f_0000_0040 });
        assert_eq!(TB_GLOBAL_ROOT_DWORDS, 8);
    }

    #[test]
    fn descriptor_table_finds_every_entry_and_misses_by_kind() {
        let descs: Vec<[u32; 4]> = (0..300u32).map(|i| [i * 0x100, 0x2000_0000 | i, 0x0003_f03f, 0xfac8]).collect();
        let mut entries: Vec<(u32, u32)> = descs.iter().enumerate().map(|(i, d)| (desc_key(DESC_SRV, *d), 10 + i as u32)).collect();
        entries.push((desc_key(DESC_SAMPLER, [1, 2, 3, 4]), 7));
        let t = desc_table(&entries, [1, 2, 3]);
        assert!((t[0] + 1).is_power_of_two() && t.len() == 4 + 2 * (t[0] as usize + 1));
        for (i, d) in descs.iter().enumerate() {
            assert_eq!(desc_lookup(&t, DESC_SRV, *d), 10 + i as u32);
        }
        assert_eq!(desc_lookup(&t, DESC_SAMPLER, [1, 2, 3, 4]), 7);
        // The same dwords under another kind are a different descriptor.
        assert_eq!(desc_lookup(&t, DESC_UAV, descs[0]), 2);
        assert_eq!(desc_lookup(&t, DESC_SRV, [9, 9, 9, 9]), 1);
        assert_eq!(desc_lookup(&t, DESC_SAMPLER_CMP, [1, 2, 3, 4]), 3);
        assert_ne!(desc_key(DESC_SRV, [0; 4]), 0);
    }
}
