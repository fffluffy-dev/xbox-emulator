#[cfg(windows)]
fn main() {
    std::process::exit(tombox_xshader::dxc::host_main());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("tombox_dxc_host runs only on Windows (it hosts dxcompiler.dll)");
    std::process::exit(2);
}
