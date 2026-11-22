//! The tool needs DXC, the D3D12 shader cache and a Win32 window, which exist only on Windows.
//! On other hosts the binary still builds (so `cargo test` covers the workspace) and exits with an error.

#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    win::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("xshader_prelaunch needs Windows (DXC, the D3D12 shader cache and a Win32 window)");
    std::process::exit(1);
}
