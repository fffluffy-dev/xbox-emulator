use serde_json::{json, Value};
use std::collections::{BTreeSet, HashMap};
#[cfg(windows)]
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

struct Args {
    flags: HashMap<String, Vec<String>>,
}

impl Args {
    fn parse(v: &[String]) -> Args {
        let mut flags: HashMap<String, Vec<String>> = HashMap::new();
        let mut i = 0;
        while i < v.len() {
            if let Some(k) = v[i].strip_prefix("--") {
                if i + 1 < v.len() && !v[i + 1].starts_with("--") {
                    flags.entry(k.to_string()).or_default().push(v[i + 1].clone());
                    i += 2;
                    continue;
                }
                let _ = k;
            }
            i += 1;
        }
        Args { flags }
    }
    fn one(&self, k: &str) -> Option<String> {
        self.flags.get(k).and_then(|v| v.last().cloned())
    }
    fn need(&self, k: &str) -> Result<String, String> {
        self.one(k).ok_or_else(|| format!("missing --{k}"))
    }
    fn many(&self, k: &str) -> Vec<String> {
        self.flags.get(k).cloned().unwrap_or_default()
    }
}

fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
    }
    std::fs::write(path, serde_json::to_string_pretty(v).unwrap()).map_err(|e| format!("write {}: {e}", path.display()))
}

#[cfg(windows)]
fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    let want = name.to_ascii_lowercase();
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.is_file() && p.file_name().map(|f| f.to_string_lossy().to_ascii_lowercase() == want).unwrap_or(false))
}

fn identifiers_with_prefix(bytes: &[u8], prefix: &str) -> Vec<String> {
    let mut out = BTreeSet::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if i > start {
            let s = String::from_utf8_lossy(&bytes[start..i]);
            let before_ok = start == 0 || !(0x20..0x7f).contains(&bytes[start - 1]);
            let after_ok = i == bytes.len() || bytes[i] == 0;
            if before_ok && after_ok && s.starts_with(prefix) && s.len() > prefix.len() {
                out.insert(s.into_owned());
            }
        }
        i += 1;
    }
    out.into_iter().collect()
}

#[cfg(windows)]
fn cmd_scan(a: &Args) -> Result<Value, String> {
    let title = a.need("title")?;
    let bin = PathBuf::from(a.need("bin-dir")?);
    let xvd = a.one("xvd").map(PathBuf::from);
    let stages: Vec<PathBuf> = a.many("stage-dir").into_iter().map(PathBuf::from).collect();
    Ok(tombox_probe::runtime::scan(&title, &bin, xvd.as_deref(), &stages))
}

fn sanitize(s: &str) -> String {
    s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' { c } else { '_' }).collect()
}

#[cfg(windows)]
fn cmd_load(a: &Args) -> Result<Value, String> {
    use tombox_probe::image::{file_version, Image};
    use tombox_probe::names::{is_api_set, is_vc_redist, normalize_module};
    let title = a.need("title")?;
    let bin = PathBuf::from(a.need("bin-dir")?);
    let rel = a.need("dll")?;
    let src = bin.join(&rel);
    let xvd = a.one("xvd").map(PathBuf::from);
    let scratch_root = PathBuf::from(a.need("scratch")?);
    let timeout: u64 = a.one("timeout").and_then(|t| t.parse().ok()).unwrap_or(120);
    let file_name = src.file_name().ok_or("bad --dll")?.to_string_lossy().into_owned();
    let scratch = scratch_root.join(sanitize(&title)).join(sanitize(&rel.replace(['/', '\\'], "_")));
    if scratch.exists() {
        std::fs::remove_dir_all(&scratch).map_err(|e| format!("clear {}: {e}", scratch.display()))?;
    }
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let mut search_dirs: Vec<PathBuf> = vec![src.parent().unwrap_or(&bin).to_path_buf(), bin.clone()];
    for s in a.many("search") {
        search_dirs.push(bin.join(s));
    }
    let sys32 = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join("System32");
    let mut staged: Vec<Value> = Vec::new();
    let mut have: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<(PathBuf, String, String)> = VecDeque::new();
    queue.push_back((src.clone(), "title".into(), "target".into()));
    while let Some((from, source, why)) = queue.pop_front() {
        let name = from.file_name().unwrap().to_string_lossy().into_owned();
        let key = normalize_module(&name);
        if !have.insert(key) {
            continue;
        }
        let to = scratch.join(&name);
        std::fs::copy(&from, &to).map_err(|e| format!("copy {} -> {}: {e}", from.display(), to.display()))?;
        let bytes = std::fs::read(&to).map_err(|e| e.to_string())?;
        staged.push(json!({"file": name, "source": source, "from": from.to_string_lossy(), "reason": why, "file_version": file_version(&bytes), "bytes": bytes.len()}));
        let Ok(img) = Image::open(&to) else { continue };
        let mods: BTreeSet<String> = img.import_rows().into_iter().map(|r| r.module).collect();
        for m in mods {
            if is_api_set(&m) || have.contains(&normalize_module(&m)) {
                continue;
            }
            if let Some(p) = search_dirs.iter().find_map(|d| find_ci(d, &m)) {
                queue.push_back((p, "title".into(), format!("imported by {name}")));
            } else if is_vc_redist(&m) {
                if let Some(p) = find_ci(&sys32, &m) {
                    queue.push_back((p, "host VC++ 14.x redist (title does not ship it)".into(), format!("imported by {name}")));
                }
            }
        }
    }
    for s in a.many("stage") {
        let p = PathBuf::from(&s);
        let name = p.file_name().ok_or("bad --stage")?.to_string_lossy().into_owned();
        if !have.insert(normalize_module(&name)) {
            continue;
        }
        std::fs::copy(&p, scratch.join(&name)).map_err(|e| format!("stage {s}: {e}"))?;
        let bytes = std::fs::read(&p).map_err(|e| e.to_string())?;
        staged.push(json!({"file": name, "source": "emulator", "from": s, "file_version": file_version(&bytes), "bytes": bytes.len()}));
    }
    let mut expect = Vec::new();
    for e in a.many("expect-from") {
        let importer = bin.join(&e);
        for r in tombox_probe::runtime::expected_rows(&importer, &file_name)? {
            expect.push((e.replace('\\', "/"), r));
        }
    }
    let mut pinvoke = Vec::new();
    if let Some(p) = a.one("pinvoke-prefix") {
        for f in a.many("pinvoke-from") {
            let bytes = std::fs::read(&f).map_err(|e| format!("read {f}: {e}"))?;
            pinvoke.push((Path::new(&f).file_name().unwrap().to_string_lossy().into_owned(), identifiers_with_prefix(&bytes, &p)));
        }
    }
    let plan = tombox_probe::runtime::ChildPlan { title: title.clone(), binary: rel.replace('\\', "/"), target: scratch.join(&file_name), scratch: scratch.clone(), xvd: xvd.clone(), expect, pinvoke };
    let plan_path = scratch.join("plan.json");
    let result_path = scratch.join("result.json");
    write_json(&plan_path, &plan.to_json())?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut child = std::process::Command::new(exe)
        .arg("load-child")
        .arg("--plan")
        .arg(&plan_path)
        .arg("--result")
        .arg(&result_path)
        .current_dir(&scratch)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("spawn child: {e}"))?;
    let t0 = std::time::Instant::now();
    let mut timed_out = false;
    let status = loop {
        if let Some(s) = child.try_wait().map_err(|e| e.to_string())? {
            break Some(s);
        }
        if t0.elapsed().as_secs() >= timeout {
            let _ = child.kill();
            timed_out = true;
            break child.wait().ok();
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    };
    let mut stderr = String::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_string(&mut stderr);
    }
    let result: Value = std::fs::read_to_string(&result_path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or(Value::Null);
    let code = status.and_then(|s| s.code());
    Ok(json!({
        "probe": "tombox-probe load",
        "title": title,
        "binary": rel.replace('\\', "/"),
        "source": src.to_string_lossy(),
        "scratch": scratch.to_string_lossy(),
        "staged": staged,
        "child": {"exit_code": code, "exit_code_hex": code.map(|c| format!("{:#x}", c as u32)), "timed_out": timed_out, "seconds": t0.elapsed().as_secs_f64(), "stderr_tail": stderr.chars().rev().take(2000).collect::<String>().chars().rev().collect::<String>()},
        "outcome": if timed_out { "timeout" } else if result.is_null() { "load_crashed" } else if result["loaded"] == true { "loaded" } else { "load_failed" },
        "result": result,
    }))
}

#[cfg(windows)]
fn cmd_child(a: &Args) -> Result<Value, String> {
    let plan_path = a.need("plan")?;
    let result = PathBuf::from(a.need("result")?);
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&plan_path).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
    let plan = tombox_probe::runtime::ChildPlan::from_json(&v).ok_or("bad plan")?;
    let out = tombox_probe::runtime::child(&plan);
    write_json(&result, &out)?;
    Ok(Value::Null)
}

fn cmd_token(a: &Args) -> Result<Value, String> {
    let token = a.need("token")?;
    let mut exact = Vec::new();
    let mut near = BTreeSet::new();
    let stem = token.trim_end_matches(".dll").trim_end_matches(".DLL").to_string();
    for r in a.many("root") {
        let root = PathBuf::from(&r);
        for h in tombox_probe::token::search(&root, &token, true) {
            exact.push(json!({"file": h.file.to_string_lossy(), "offset": format!("{:#x}", h.offset), "wide": h.wide, "context": h.context}));
        }
        for h in tombox_probe::token::search(&root, &stem, false) {
            if h.context.to_ascii_lowercase().contains(".dll") || h.context.to_ascii_lowercase().contains("_gamecore") {
                near.insert(format!("{} [{}{}]: {}", h.file.strip_prefix(&root).unwrap_or(&h.file).display(), if h.wide { "utf16 " } else { "" }, format!("{:#x}", h.offset), h.context));
            }
        }
    }
    Ok(json!({"probe": "tombox-probe token", "token": token, "roots": a.many("root"), "exact_hits": exact, "exact_hit_count": exact.len(), "module_like_near_matches": near.into_iter().collect::<Vec<_>>()}))
}

fn cmd_sites(a: &Args) -> Result<Value, String> {
    let img = tombox_probe::image::Image::open(Path::new(&a.need("dll")?))?;
    let v: Vec<Value> = tombox_probe::sites::analyze(&img)
        .into_iter()
        .map(|s| {
            json!({"dll": s.dll, "lea_rva": format!("{:#x}", s.lea_rva), "loader": s.loader, "load_failure_checked": s.null_checked,
                "check_branch_target_rva": s.failure_branch_rva.map(|r| format!("{r:#x}")),
                "procs": s.procs.iter().map(|p| json!({"name": p.name, "call_rva": format!("{:#x}", p.call_rva), "null_checked": p.null_checked})).collect::<Vec<_>>()})
        })
        .collect();
    Ok(json!({"probe": "tombox-probe sites", "dll": a.need("dll")?, "sites": v}))
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = argv.first().cloned() else {
        eprintln!("usage: tombox-probe <scan|load|token|sites> [--flags]");
        return ExitCode::from(2);
    };
    let a = Args::parse(&argv[1..]);
    let r = match cmd.as_str() {
        #[cfg(windows)]
        "scan" => cmd_scan(&a),
        #[cfg(windows)]
        "load" => cmd_load(&a),
        #[cfg(windows)]
        "load-child" => cmd_child(&a),
        #[cfg(not(windows))]
        "scan" | "load" | "load-child" => Err(format!("{cmd} needs Windows (it loads the modules with the PC loader)")),
        "token" => cmd_token(&a),
        "sites" => cmd_sites(&a),
        _ => Err(format!("unknown command {cmd}")),
    };
    match r {
        Ok(Value::Null) => ExitCode::SUCCESS,
        Ok(v) => {
            if let Some(o) = a.one("out") {
                if let Err(e) = write_json(Path::new(&o), &v) {
                    eprintln!("{e}");
                    return ExitCode::from(1);
                }
                let s = if v["summary"].is_null() { &v["result"]["summary"] } else { &v["summary"] };
                println!("wrote {o} rows={} resolved={} unresolved={} iat_mismatch={} outcome={}", s["rows"], s["resolved"], s["unresolved"], s["iat_mismatch"], v["outcome"]);
            } else {
                println!("{}", serde_json::to_string_pretty(&v).unwrap());
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_prefix_identifiers() {
        let v: Vec<String> = ["--title", "T", "--search", "a", "--search", "b", "--flag"].iter().map(|s| s.to_string()).collect();
        let a = Args::parse(&v);
        assert_eq!(a.one("title").as_deref(), Some("T"));
        assert_eq!(a.many("search"), vec!["a", "b"]);
        assert!(a.one("flag").is_none());
        let ids = identifiers_with_prefix(b"\0PartyXblInitialize\0PartyXblCleanup\0xPartyXblNo\0PartyXbl\0", "PartyXbl");
        assert_eq!(ids, vec!["PartyXblCleanup", "PartyXblInitialize"]);
        assert_eq!(sanitize("Data/Plugins/x.dll"), "Data_Plugins_x.dll");
    }
}
