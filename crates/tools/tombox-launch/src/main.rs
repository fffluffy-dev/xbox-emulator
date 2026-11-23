mod config;
mod plan;

use plan::{lower, Inputs, Plan, Source};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const USAGE: &str = "tombox-launch <dump or Mount dir> --runtime <dir with the runtime DLLs> [options]

  --runtime DIR        runtime build output (target\\release, or the CI tombox-runtime bundle)
  --work DIR           where the overlay, saves and logs go (default: tombox-run/<identity>)
  --exe NAME           executable to start (default: the Game entry of MicrosoftGame.Config)
  --stage NAME         also stage this runtime DLL beside the executable (repeatable)
  --prefer-title N     keep the title's own N where the runtime has a replacement (repeatable;
                       by default the runtime's copy wins, e.g. its WinHTTP-based XCurl.dll)
  --prefer-runtime N   stage the runtime's N even where it would not be (repeatable)
  --env K=V            extra environment for the title (repeatable)
  --dxc DIR            directory with Microsoft's dxcompiler.dll + dxil.dll for dxcompiler_x
                       (sets TOMBOX_DXC_DIR; default: $TOMBOX_DXC_DIR)
  --prefix DIR         Wine prefix (default: $WINEPREFIX); its system32 is the host DLL set
  --wine CMD           Wine / Proton wine64 used to start the title off Windows (default: $WINE or wine)
  --mdbg PATH          start the title under mdbg.exe (OutputDebugString and exception trace)
  --into-prefix NAME   also copy the runtime's NAME into the Wine prefix's system32, for DLLs a title
                       loads from System32 only (repeatable; never overwrites a different file)
  --copy               copy the package instead of linking it (needed across volumes on Windows)
  --plan               only print the plan; touch nothing
  --run                start the title after staging
  -- ARGS              arguments passed to the title";

#[derive(Default)]
struct Args {
    title: Option<PathBuf>,
    runtime: Option<PathBuf>,
    work: Option<PathBuf>,
    exe: Option<String>,
    stage: BTreeSet<String>,
    prefer: BTreeSet<String>,
    prefer_title: BTreeSet<String>,
    env: Vec<(String, String)>,
    prefix: Option<PathBuf>,
    dxc: Option<PathBuf>,
    into_prefix: BTreeSet<String>,
    wine: Option<String>,
    mdbg: Option<PathBuf>,
    copy: bool,
    plan_only: bool,
    run: bool,
    game_args: Vec<String>,
}

fn parse_args(v: &[String]) -> Result<Args, String> {
    let mut a = Args::default();
    let mut i = 0;
    let next = |i: &mut usize| -> Result<String, String> {
        *i += 1;
        v.get(*i).cloned().ok_or_else(|| format!("{} needs a value", v[*i - 1]))
    };
    while i < v.len() {
        match v[i].as_str() {
            "--runtime" => a.runtime = Some(PathBuf::from(next(&mut i)?)),
            "--work" => a.work = Some(PathBuf::from(next(&mut i)?)),
            "--exe" => a.exe = Some(next(&mut i)?),
            "--stage" => {
                a.stage.insert(lower(&next(&mut i)?));
            }
            "--prefer-runtime" => {
                a.prefer.insert(lower(&next(&mut i)?));
            }
            "--prefer-title" => {
                a.prefer_title.insert(lower(&next(&mut i)?));
            }
            "--env" => {
                let kv = next(&mut i)?;
                let (k, val) = kv.split_once('=').ok_or_else(|| format!("--env {kv}: expected K=V"))?;
                a.env.push((k.to_string(), val.to_string()));
            }
            "--prefix" => a.prefix = Some(PathBuf::from(next(&mut i)?)),
            "--dxc" => a.dxc = Some(PathBuf::from(next(&mut i)?)),
            "--into-prefix" => {
                a.into_prefix.insert(lower(&next(&mut i)?));
            }
            "--wine" => a.wine = Some(next(&mut i)?),
            "--mdbg" => a.mdbg = Some(PathBuf::from(next(&mut i)?)),
            "--copy" => a.copy = true,
            "--plan" => a.plan_only = true,
            "--run" => a.run = true,
            "--" => {
                a.game_args = v[i + 1..].to_vec();
                break;
            }
            "-h" | "--help" => return Err(String::new()),
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            s => {
                if a.title.is_some() {
                    return Err(format!("unexpected argument {s}"));
                }
                a.title = Some(PathBuf::from(s));
            }
        }
        i += 1;
    }
    Ok(a)
}

fn find_ci(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.file_name().is_some_and(|f| f.to_string_lossy().eq_ignore_ascii_case(name)))
}

struct Layout {
    mount: PathBuf,
    console_dirs: Vec<PathBuf>,
}

fn layout(title: &Path) -> Result<Layout, String> {
    let (mount, root) = if find_ci(title, "MicrosoftGame.Config").is_some() {
        (title.to_path_buf(), title.parent().map(Path::to_path_buf))
    } else if let Some(m) = find_ci(title, "Mount").filter(|m| find_ci(m, "MicrosoftGame.Config").is_some()) {
        (m, Some(title.to_path_buf()))
    } else {
        return Err(format!("{}: no MicrosoftGame.Config here or in Mount/", title.display()));
    };
    let mut console_dirs = Vec::new();
    if let Some(r) = root {
        if let Some(sys) = find_ci(&r, "EmbeddedXvd").and_then(|x| find_ci(&x, "Windows")).and_then(|w| find_ci(&w, "System32")) {
            if let Some(f) = find_ci(&sys, "forwarders") {
                console_dirs.push(f);
            }
            console_dirs.push(sys);
        }
    }
    Ok(Layout { mount, console_dirs })
}

fn dll_names(dir: &Path) -> BTreeSet<String> {
    std::fs::read_dir(dir).map(|rd| rd.flatten().filter_map(|e| e.file_name().to_str().map(lower)).filter(|n| n.ends_with(".dll")).collect()).unwrap_or_default()
}

fn host_dlls(prefix: Option<&Path>) -> Option<BTreeSet<String>> {
    if cfg!(windows) {
        let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows"));
        return Some(dll_names(&root.join("System32")));
    }
    let p = prefix.map(Path::to_path_buf).or_else(|| std::env::var_os("WINEPREFIX").map(PathBuf::from))?;
    let s = p.join("drive_c").join("windows").join("system32");
    s.is_dir().then(|| dll_names(&s))
}

fn host_system32(prefix: Option<&Path>) -> Option<PathBuf> {
    if cfg!(windows) {
        return Some(std::env::var_os("SystemRoot").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("C:\\Windows")).join("System32"));
    }
    let p = prefix.map(Path::to_path_buf).or_else(|| std::env::var_os("WINEPREFIX").map(PathBuf::from))?;
    Some(p.join("drive_c").join("windows").join("system32"))
}

fn binaries(mount: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![mount.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if p.extension().is_some_and(|x| x.eq_ignore_ascii_case("exe") || x.eq_ignore_ascii_case("dll")) {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn imports_of(bytes: &[u8]) -> Vec<String> {
    let Ok(pe) = tombox_pe::PeFile::parse(bytes) else { return Vec::new() };
    let mut notes = Vec::new();
    let mut v: Vec<String> = tombox_pe::directories::parse_imports(&pe, &mut notes).map(|i| i.dlls.into_iter().map(|d| d.name).collect()).unwrap_or_default();
    if let Some(d) = tombox_pe::directories::parse_delay_imports(&pe, &mut notes) {
        v.extend(d.dlls.into_iter().map(|d| d.name));
    }
    v
}

fn rel(base: &Path, p: &Path) -> String {
    p.strip_prefix(base).unwrap_or(p).to_string_lossy().replace('\\', "/")
}

fn win_path(p: &Path) -> String {
    if cfg!(windows) {
        return p.display().to_string();
    }
    let abs = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    format!("Z:{}", abs.display()).replace('/', "\\")
}

fn link_tree(src: &Path, dst: &Path, skip: &BTreeSet<String>, copy: bool) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for e in std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))?.flatten() {
        let name = e.file_name();
        if skip.contains(&lower(&name.to_string_lossy())) {
            continue;
        }
        let (s, d) = (e.path(), dst.join(&name));
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        if copy {
            if is_dir {
                link_tree(&s, &d, &BTreeSet::new(), true)?;
            } else {
                std::fs::copy(&s, &d).map_err(|e| format!("copy {}: {e}", s.display()))?;
            }
            continue;
        }
        #[cfg(unix)]
        {
            let abs = std::fs::canonicalize(&s).unwrap_or(s.clone());
            std::os::unix::fs::symlink(&abs, &d).map_err(|e| format!("link {}: {e}", d.display()))?;
        }
        #[cfg(not(unix))]
        {
            if is_dir {
                link_tree(&s, &d, &BTreeSet::new(), false)?;
            } else {
                std::fs::hard_link(&s, &d).map_err(|e| format!("hard link {} (use --copy or a --work on the same volume): {e}", d.display()))?;
            }
        }
    }
    Ok(())
}

fn default_work() -> PathBuf {
    let base = if cfg!(windows) { std::env::var_os("LOCALAPPDATA") } else { std::env::var_os("HOME") };
    base.map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")).join("tombox-run")
}

type Snapshot = BTreeMap<PathBuf, (u64, Option<std::time::SystemTime>)>;

fn snapshot(root: &Path) -> Snapshot {
    let mut out = Snapshot::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(m) = e.metadata() else { continue };
            if m.is_dir() {
                stack.push(e.path());
            } else {
                out.insert(e.path(), (m.len(), m.modified().ok()));
            }
        }
    }
    out
}

fn changed(before: &Snapshot, after: &Snapshot) -> Vec<String> {
    let mut v = Vec::new();
    for (p, m) in after {
        match before.get(p) {
            None => v.push(format!("new: {}", p.display())),
            Some(b) if b != m => v.push(format!("modified: {}", p.display())),
            _ => {}
        }
    }
    for p in before.keys().filter(|p| !after.contains_key(*p)) {
        v.push(format!("removed: {}", p.display()));
    }
    v
}

fn report(cfg: &config::GameConfig, exe: &str, plan: &Plan, notes: &[String]) -> String {
    let mut r = String::new();
    let _ = writeln!(r, "# tombox-launch plan\n\n{} {} - {exe}\n", cfg.identity, cfg.version);
    let mut by: BTreeMap<Source, Vec<(&String, &BTreeSet<String>)>> = BTreeMap::new();
    for (n, (s, who)) in &plan.rows {
        by.entry(*s).or_default().push((n, who));
    }
    for (s, rows) in by.iter().rev() {
        let _ = writeln!(r, "## {} ({})\n", s.label(), rows.len());
        for (n, who) in rows {
            let _ = writeln!(r, "- {n}  <- {}", who.iter().cloned().collect::<Vec<_>>().join(", "));
        }
        r.push('\n');
    }
    if !notes.is_empty() {
        let _ = writeln!(r, "## Notes\n");
        for n in notes {
            let _ = writeln!(r, "- {n}");
        }
    }
    r
}

fn run(a: Args) -> Result<(), String> {
    let title = a.title.clone().ok_or("no title directory given")?;
    let lay = layout(&title)?;
    let cfg_path = find_ci(&lay.mount, "MicrosoftGame.Config").ok_or("MicrosoftGame.Config vanished")?;
    let cfg = config::parse(&std::fs::read_to_string(&cfg_path).map_err(|e| format!("{}: {e}", cfg_path.display()))?);
    let exe = match &a.exe {
        Some(e) => e.clone(),
        None => cfg.executables.iter().find(|e| e.id.eq_ignore_ascii_case("Game")).or(cfg.executables.first()).map(|e| e.name.clone()).ok_or("MicrosoftGame.Config lists no executable; pass --exe")?,
    };
    let exe_path = find_ci(&lay.mount, &exe).ok_or_else(|| format!("{exe} is not in {}", lay.mount.display()))?;
    let runtime = a.runtime.clone().ok_or("--runtime is required")?;
    let mut inp = Inputs { prefer_runtime: a.prefer.clone(), prefer_title: a.prefer_title.clone(), extra: a.stage.clone(), ..Default::default() };
    inp.runtime = dll_names(&runtime);
    if inp.runtime.is_empty() && !a.plan_only {
        return Err(format!("{}: no DLLs; build the runtime or unpack the CI bundle there (--plan works without them)", runtime.display()));
    }
    for d in &lay.console_dirs {
        inp.console.extend(dll_names(d));
    }
    inp.host = host_dlls(a.prefix.as_deref());
    inp.shipped = dll_names(&lay.mount);
    let mut notes = Vec::new();
    if inp.runtime.is_empty() {
        notes.push(format!("{} has no DLLs: every console library shows as missing", runtime.display()));
    }
    if inp.console.is_empty() {
        notes.push("no EmbeddedXvd/Windows/System32 next to Mount: console libraries are guessed from the runtime".into());
    }
    if inp.host.is_none() {
        notes.push("no host DLL listing (no Wine prefix): PC libraries are guessed from a built-in list".into());
    }
    let wanted: BTreeSet<String> = inp.runtime.union(&inp.console).cloned().collect();
    for b in binaries(&lay.mount) {
        let Ok(bytes) = std::fs::read(&b) else { continue };
        let name = rel(&lay.mount, &b);
        let imp = imports_of(&bytes);
        let imp_l: BTreeSet<String> = imp.iter().map(|s| lower(s)).collect();
        let dynm: Vec<String> = plan::dynamic_names(&bytes, &wanted).into_iter().filter(|d| !imp_l.contains(d) && !d.eq_ignore_ascii_case(b.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default().as_str())).collect();
        if !imp.is_empty() {
            inp.imports.insert(name.clone(), imp);
        }
        if !dynm.is_empty() {
            inp.dynamic.insert(name, dynm);
        }
    }
    let plan = plan::build(&inp);
    let mut companions: Vec<(String, String)> = Vec::new();
    for e in std::fs::read_dir(&runtime).map(|rd| rd.flatten().collect::<Vec<_>>()).unwrap_or_default() {
        let n = lower(&e.file_name().to_string_lossy());
        if !n.ends_with(".dll") || n.ends_with("_sys.dll") {
            continue;
        }
        let Ok(bytes) = std::fs::read(e.path()) else { continue };
        for c in plan::dll_strings(&bytes).into_iter().filter(|c| c.ends_with("_sys.dll")) {
            let src = format!("{}.dll", c.trim_end_matches("_sys.dll"));
            if !companions.iter().any(|(_, have)| *have == c) && !inp.runtime.contains(&c) {
                companions.push((src, c));
            }
        }
    }
    companions.sort();
    let dxc = a.dxc.clone().or_else(|| std::env::var_os("TOMBOX_DXC_DIR").map(PathBuf::from));
    match &dxc {
        Some(d) if find_ci(d, "dxcompiler.dll").is_none() && !cfg!(windows) => notes.push(format!("{}: no dxcompiler.dll there; dxcompiler_x will answer E_NOTIMPL", d.display())),
        Some(_) => {}
        None if plan.stage.contains("dxcompiler_x.dll") && find_ci(&runtime, "dxcompiler.dll").is_none() => {
            notes.push("dxcompiler_x is staged but no DXC was given: pass --dxc <dir with dxcompiler.dll and dxil.dll>, or the title's shader compiler calls fail".into())
        }
        None => {}
    }
    for (src, c) in &companions {
        notes.push(format!("{c}: copy of the host's {src} (runtime DLLs forward to it)"));
    }
    let rep = report(&cfg, &exe, &plan, &notes);
    print!("{rep}");
    if a.plan_only {
        return Ok(());
    }
    let work = a.work.clone().unwrap_or_else(|| default_work().join(if cfg.identity.is_empty() { "title" } else { &cfg.identity }));
    let overlay = work.join("Mount");
    if overlay.exists() {
        std::fs::remove_dir_all(&overlay).map_err(|e| format!("{}: {e}", overlay.display()))?;
    }
    let mut skip: BTreeSet<String> = plan.stage.clone();
    skip.extend(companions.iter().map(|(_, c)| c.clone()));
    link_tree(&lay.mount, &overlay, &skip, a.copy)?;
    for s in &plan.stage {
        let src = find_ci(&runtime, s).ok_or_else(|| format!("{s} vanished from the runtime"))?;
        std::fs::copy(&src, overlay.join(src.file_name().unwrap())).map_err(|e| format!("stage {}: {e}", src.display()))?;
    }
    let sys = host_system32(a.prefix.as_deref());
    for (src, c) in &companions {
        match sys.as_ref().and_then(|d| find_ci(d, src)) {
            Some(p) => {
                std::fs::copy(&p, overlay.join(c)).map_err(|e| format!("companion {c}: {e}"))?;
            }
            None => println!("note: no host {src} to stage as {c}"),
        }
    }
    for n in &a.into_prefix {
        if cfg!(windows) {
            return Err("--into-prefix is for Wine prefixes only".into());
        }
        let dir = host_system32(a.prefix.as_deref()).ok_or("--into-prefix needs --prefix or $WINEPREFIX")?;
        let src = find_ci(&runtime, n).ok_or_else(|| format!("--into-prefix {n}: not in the runtime"))?;
        let dst = dir.join(src.file_name().unwrap());
        let bytes = std::fs::read(&src).map_err(|e| format!("{}: {e}", src.display()))?;
        match find_ci(&dir, n) {
            Some(existing) if std::fs::read(&existing).ok().as_deref() != Some(&bytes[..]) => {
                return Err(format!("--into-prefix {n}: {} exists and differs; remove it yourself if it is not needed", existing.display()));
            }
            Some(_) => {}
            None => std::fs::write(&dst, &bytes).map_err(|e| format!("{}: {e}", dst.display()))?,
        }
        println!("{n} is in the prefix: {}", dst.display());
    }
    let gdk = work.join("gdkroot");
    let logs = work.join("logs");
    for d in [&gdk, &logs] {
        std::fs::create_dir_all(d).map_err(|e| format!("{}: {e}", d.display()))?;
    }
    let mut env: Vec<(String, String)> = vec![
        ("TOMBOX_PACKAGE_ROOT".into(), win_path(&overlay)),
        ("TOMBOX_XBTHUNK_TITLE_ROOT".into(), win_path(&overlay)),
        ("TOMBOX_GDK_ROOT".into(), win_path(&gdk)),
        ("TOMBOX_XCRD_ROOT".into(), win_path(&work)),
        ("TOMBOX_COMPAT_DIR".into(), win_path(&runtime)),
        ("TOMBOX_LOG".into(), win_path(&logs.join("tombox_compat.log"))),
    ];
    if let Some(d) = &dxc {
        env.push(("TOMBOX_DXC_DIR".into(), win_path(d)));
    }
    if !cfg!(windows) {
        env.push(("WINEPATH".into(), win_path(&runtime)));
        let mut ov: Vec<String> = plan.stage.iter().map(|s| format!("{}=n,b", s.trim_end_matches(".dll"))).collect();
        if let Ok(old) = std::env::var("WINEDLLOVERRIDES") {
            ov.push(old);
        }
        env.push(("WINEDLLOVERRIDES".into(), ov.join(";")));
    }
    env.extend(a.env.iter().cloned());
    let mut envtxt = String::new();
    for (k, v) in &env {
        let _ = writeln!(envtxt, "{k}={v}");
    }
    std::fs::write(work.join("launch.env"), &envtxt).map_err(|e| e.to_string())?;
    std::fs::write(work.join("launch_report.md"), &rep).map_err(|e| e.to_string())?;
    let staged_exe = overlay.join(exe_path.file_name().unwrap());
    println!("staged {} runtime DLLs into {}; environment in {}", plan.stage.len(), overlay.display(), work.join("launch.env").display());
    if !a.run {
        println!("not started (pass --run)");
        return Ok(());
    }
    let mut argv: Vec<String> = Vec::new();
    if let Some(m) = &a.mdbg {
        argv.push(win_path(m));
    }
    argv.push(win_path(&staged_exe));
    argv.extend(a.game_args.iter().cloned());
    let (prog, args): (String, Vec<String>) = if cfg!(windows) {
        (argv[0].clone(), argv[1..].to_vec())
    } else {
        let wine = a.wine.clone().or_else(|| std::env::var("WINE").ok()).unwrap_or_else(|| "wine".into());
        let mut v = argv.clone();
        if a.mdbg.is_none() {
            v[0] = staged_exe.display().to_string();
        }
        (wine, v)
    };
    let mut cmd = std::process::Command::new(&prog);
    cmd.args(&args).current_dir(&overlay).envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    if let Some(p) = &a.prefix {
        cmd.env("WINEPREFIX", p);
    }
    println!("starting: {prog} {}", args.join(" "));
    let before = if a.copy { Snapshot::new() } else { snapshot(&lay.mount) };
    let status = cmd.status().map_err(|e| format!("{prog}: {e}"))?;
    println!("exit: {status}");
    if !a.copy {
        let ch = changed(&before, &snapshot(&lay.mount));
        if ch.is_empty() {
            println!("the dump at {} is unchanged", lay.mount.display());
        } else {
            let txt = ch.join("\n");
            eprintln!("warning: the title wrote into the dump through the overlay links ({} entries); use --copy to isolate it:\n{txt}", ch.len());
            let _ = std::fs::write(work.join("dump_changes.txt"), txt);
        }
    }
    Ok(())
}

fn main() -> ExitCode {
    let v: Vec<String> = std::env::args().skip(1).collect();
    match parse_args(&v).and_then(run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) if e.is_empty() => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("tombox-launch: {e}\n\n{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tombox-launch-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn finds_the_mount_and_the_console_system32() {
        let d = tmp("layout");
        std::fs::create_dir_all(d.join("Mount/Packs")).unwrap();
        std::fs::create_dir_all(d.join("EmbeddedXvd/Windows/System32/forwarders")).unwrap();
        std::fs::write(d.join("Mount/MicrosoftGame.Config"), "<Game/>").unwrap();
        let l = layout(&d).unwrap();
        assert_eq!(l.mount, d.join("Mount"));
        assert_eq!(l.console_dirs.len(), 2);
        assert_eq!(layout(&d.join("Mount")).unwrap().mount, d.join("Mount"));
        assert!(layout(&d.join("Mount/Packs")).is_err());
    }

    #[test]
    fn plan_mode_parses_arguments() {
        let a = parse_args(&["dump".into(), "--runtime".into(), "rt".into(), "--stage".into(), "XAudio2_9.dll".into(), "--env".into(), "A=b=c".into(), "--plan".into(), "--".into(), "-x".into()]).unwrap();
        assert_eq!(a.title, Some(PathBuf::from("dump")));
        assert!(a.stage.contains("xaudio2_9.dll") && a.plan_only);
        assert_eq!(a.env, vec![("A".to_string(), "b=c".to_string())]);
        assert_eq!(a.game_args, vec!["-x".to_string()]);
        assert!(parse_args(&["--bogus".into()]).is_err());
    }

    #[test]
    fn snapshots_report_new_modified_and_removed_files() {
        let d = tmp("snap");
        std::fs::write(d.join("a"), "1").unwrap();
        std::fs::write(d.join("b"), "2").unwrap();
        let before = snapshot(&d);
        std::fs::write(d.join("a"), "longer").unwrap();
        std::fs::remove_file(d.join("b")).unwrap();
        std::fs::write(d.join("c"), "3").unwrap();
        let mut ch = changed(&before, &snapshot(&d));
        ch.sort();
        assert_eq!(ch.len(), 3, "{ch:?}");
        assert!(ch[0].starts_with("modified: ") && ch[1].starts_with("new: ") && ch[2].starts_with("removed: "), "{ch:?}");
        assert!(changed(&before, &before).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn overlay_links_the_package_and_skips_staged_names() {
        let d = tmp("overlay");
        std::fs::create_dir_all(d.join("src/Packs")).unwrap();
        std::fs::write(d.join("src/Game.exe"), "x").unwrap();
        std::fs::write(d.join("src/XMEM.dll"), "title copy").unwrap();
        std::fs::write(d.join("src/Packs/a.pak"), "p").unwrap();
        let skip: BTreeSet<String> = ["xmem.dll".to_string()].into_iter().collect();
        link_tree(&d.join("src"), &d.join("ov"), &skip, false).unwrap();
        assert!(std::fs::symlink_metadata(d.join("ov/Game.exe")).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(d.join("ov/Packs/a.pak")).unwrap(), "p");
        assert!(!d.join("ov/XMEM.dll").exists());
        assert!(win_path(&d.join("ov")).starts_with("Z:\\"));
    }
}
