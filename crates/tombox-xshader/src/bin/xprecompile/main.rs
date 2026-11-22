//! The tool needs DXC and the D3D12 shader cache, which exist only on Windows.
//! On other hosts the binary still builds (so `cargo test` covers the workspace) and exits with an error.

#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    win::main()
}

#[cfg(not(windows))]
fn main() -> std::process::ExitCode {
    eprintln!("xprecompile needs Windows (DXC and the D3D12 shader cache)");
    std::process::ExitCode::FAILURE
}
