use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Source {
    Title,
    Runtime,
    ApiSet,
    System,
    ConsoleMissing,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Title => "title",
            Source::Runtime => "runtime (staged)",
            Source::ApiSet => "api set (host resolves; runtime routes console hosts)",
            Source::System => "system (PC provides it)",
            Source::ConsoleMissing => "MISSING: console library the runtime does not provide",
        }
    }
}

#[derive(Debug, Default)]
pub struct Inputs {
    pub imports: BTreeMap<String, Vec<String>>,
    pub dynamic: BTreeMap<String, Vec<String>>,
    pub console: BTreeSet<String>,
    pub runtime: BTreeSet<String>,
    pub shipped: BTreeSet<String>,
    pub host: Option<BTreeSet<String>>,
    pub prefer_runtime: BTreeSet<String>,
    pub prefer_title: BTreeSet<String>,
    pub extra: BTreeSet<String>,
}

#[derive(Debug, Default)]
pub struct Plan {
    pub rows: BTreeMap<String, (Source, BTreeSet<String>)>,
    pub stage: BTreeSet<String>,
}

const PC_CORE: &[&str] = &[
    "kernel32.dll", "kernelbase.dll", "ntdll.dll", "user32.dll", "gdi32.dll", "advapi32.dll", "ole32.dll", "oleaut32.dll", "combase.dll",
    "shell32.dll", "shlwapi.dll", "ws2_32.dll", "winhttp.dll", "crypt32.dll", "bcrypt.dll", "secur32.dll", "sechost.dll", "rpcrt4.dll",
    "msvcrt.dll", "ucrtbase.dll", "dbghelp.dll", "iphlpapi.dll", "version.dll", "winmm.dll", "dxgi.dll", "d3d12.dll", "d3d11.dll",
];

pub fn lower(s: &str) -> String {
    s.to_ascii_lowercase()
}

pub fn is_api_set(n: &str) -> bool {
    n.starts_with("api-ms-") || n.starts_with("ext-ms-")
}

fn classify(n: &str, i: &Inputs) -> Source {
    if i.shipped.contains(n) {
        return if i.runtime.contains(n) && !i.prefer_title.contains(n) { Source::Runtime } else { Source::Title };
    }
    if is_api_set(n) {
        return Source::ApiSet;
    }
    let on_host = match &i.host {
        Some(h) => h.contains(n),
        None => PC_CORE.contains(&n),
    };
    let console_only = !on_host && (i.console.contains(n) || i.console.is_empty());
    if i.runtime.contains(n) && (console_only || i.prefer_runtime.contains(n) || i.extra.contains(n)) {
        return Source::Runtime;
    }
    if console_only && !i.console.is_empty() {
        return Source::ConsoleMissing;
    }
    Source::System
}

pub fn build(i: &Inputs) -> Plan {
    let mut p = Plan::default();
    let add = |dll: &str, by: &str, p: &mut Plan| {
        let n = lower(dll);
        let src = classify(&n, i);
        let e = p.rows.entry(n.clone()).or_insert((src, BTreeSet::new()));
        e.1.insert(by.to_string());
        if src == Source::Runtime {
            p.stage.insert(n);
        }
    };
    for (bin, dlls) in &i.imports {
        for d in dlls {
            add(d, bin, &mut p);
        }
    }
    for (bin, dlls) in &i.dynamic {
        for d in dlls {
            add(d, &format!("{bin} (loaded by name)"), &mut p);
        }
    }
    for x in &i.extra {
        if i.runtime.contains(x) {
            p.stage.insert(x.clone());
            p.rows.entry(x.clone()).or_insert((Source::Runtime, BTreeSet::new())).1.insert("--stage".into());
        }
    }
    p
}

fn name_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.'
}

pub fn dll_strings(bytes: &[u8]) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let n = bytes.len();
    let mut i = 0;
    while i + 4 <= n {
        if bytes[i] == b'.' && bytes[i + 1..i + 4].eq_ignore_ascii_case(b"dll") && (i + 4 == n || !(bytes[i + 4].is_ascii_alphanumeric() || bytes[i + 4] == b'_')) {
            let mut s = i;
            while s > 0 && name_char(bytes[s - 1]) && i - s < 96 {
                s -= 1;
            }
            if s < i {
                let t = lower(&String::from_utf8_lossy(&bytes[s..i + 4]));
                let t = t.trim_start_matches(['.', '-']);
                if t.len() > 4 {
                    out.insert(t.to_string());
                }
            }
        }
        i += 1;
    }
    let mut i = 0;
    while i + 8 <= n {
        if bytes[i] == b'.' && bytes[i + 1] == 0 && bytes[i + 2..i + 7].iter().step_by(2).zip(b"dll").all(|(a, b)| a.to_ascii_lowercase() == *b) && bytes[i + 3] == 0 && bytes[i + 5] == 0 && bytes[i + 7] == 0 {
            let mut s = i;
            let mut name = Vec::new();
            while s >= 2 && bytes[s - 1] == 0 && name_char(bytes[s - 2]) && name.len() < 96 {
                name.push(bytes[s - 2]);
                s -= 2;
            }
            if !name.is_empty() {
                name.reverse();
                let t = lower(&format!("{}.dll", String::from_utf8_lossy(&name)));
                let t = t.trim_start_matches(['.', '-']);
                if t.len() > 4 {
                    out.insert(t.to_string());
                }
            }
        }
        i += 1;
    }
    out
}

pub fn dynamic_names(bytes: &[u8], wanted: &BTreeSet<String>) -> Vec<String> {
    dll_strings(bytes).intersection(wanted).cloned().collect()
}


#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn inputs() -> Inputs {
        let mut i = Inputs::default();
        i.imports.insert("GbxGame.exe".into(), ["d3d12_x.dll", "XMEM.dll", "GameInput.dll", "XCurl.dll", "api-ms-win-core-kernel32-legacy-l1-1-5.dll", "WINHTTP.dll", "kernel32.dll"].iter().map(|s| s.to_string()).collect());
        i.console = set(&["d3d12_x.dll", "xmem.dll", "gameinput.dll", "kernel32.dll", "winhttp.dll"]);
        i.runtime = set(&["d3d12_x.dll", "xmem.dll", "kernel32.dll", "xcurl.dll"]);
        i.shipped = set(&["xcurl.dll"]);
        i
    }

    #[test]
    fn classifies_every_import() {
        let mut i = inputs();
        i.host = Some(set(&["kernel32.dll", "winhttp.dll"]));
        let p = build(&i);
        let src = |n: &str| p.rows[n].0;
        assert_eq!(src("d3d12_x.dll"), Source::Runtime);
        assert_eq!(src("xmem.dll"), Source::Runtime);
        assert_eq!(src("gameinput.dll"), Source::ConsoleMissing);
        assert_eq!(src("xcurl.dll"), Source::Runtime);
        assert_eq!(src("api-ms-win-core-kernel32-legacy-l1-1-5.dll"), Source::ApiSet);
        assert_eq!(src("winhttp.dll"), Source::System);
        assert_eq!(src("kernel32.dll"), Source::System);
        assert_eq!(p.stage, set(&["d3d12_x.dll", "xcurl.dll", "xmem.dll"]));
    }

    #[test]
    fn extra_stages_on_request_and_prefer_title_keeps_the_shipped_copy() {
        let mut i = inputs();
        i.host = Some(set(&["kernel32.dll"]));
        i.extra = set(&["kernel32.dll", "notbuilt.dll"]);
        let p = build(&i);
        assert_eq!(p.stage, set(&["d3d12_x.dll", "kernel32.dll", "xcurl.dll", "xmem.dll"]));
        i.prefer_title = set(&["xcurl.dll"]);
        let p = build(&i);
        assert_eq!(p.rows["xcurl.dll"].0, Source::Title);
        assert!(!p.stage.contains("xcurl.dll"));
    }

    #[test]
    fn a_shipped_dll_the_runtime_lacks_stays_the_titles_even_when_preferred() {
        let mut i = inputs();
        i.runtime.remove("xcurl.dll");
        i.prefer_runtime = set(&["xcurl.dll"]);
        assert_eq!(build(&i).rows["xcurl.dll"].0, Source::Title);
    }

    #[test]
    fn without_a_host_listing_pc_core_dlls_stay_system() {
        let mut i = inputs();
        let p = build(&i);
        assert_eq!(p.rows["kernel32.dll"].0, Source::System);
        assert_eq!(p.rows["winhttp.dll"].0, Source::System);
        assert_eq!(p.rows["gameinput.dll"].0, Source::ConsoleMissing);
        i.console.clear();
        let p = build(&i);
        assert_eq!(p.rows["d3d12_x.dll"].0, Source::Runtime);
        assert_eq!(p.rows["gameinput.dll"].0, Source::System);
    }

    #[test]
    fn finds_names_loaded_by_string_in_ascii_and_utf16() {
        let mut b = b"xx LoadLibrary AudioHALWrapper.DLL yy".to_vec();
        b.extend("Xmem.dll".encode_utf16().flat_map(|c| c.to_le_bytes()));
        let got = dynamic_names(&b, &set(&["audiohalwrapper.dll", "xmem.dll", "xg_x.dll"]));
        assert_eq!(got, vec!["audiohalwrapper.dll".to_string(), "xmem.dll".to_string()]);
        assert!(dll_strings(b"..mfplat_sys.dll.. advapi32_sys.RegOpenKeyW advapi32_sys.dll").contains("mfplat_sys.dll"));
        assert!(dll_strings(b"forward advapi32_sys.dll").contains("advapi32_sys.dll"));
    }
}
