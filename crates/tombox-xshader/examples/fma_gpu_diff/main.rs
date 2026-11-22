//! Needs DXC (and, for GPU runs, D3D12), which exist only on Windows.
//! On other hosts the example still builds (so `cargo test` covers the crate) and exits with an error.

#[cfg(windows)]
#[path = "../../../tools/xreplay/src/d3d.rs"]
#[allow(dead_code)]
mod d3d;

#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() {
    win::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("fma_gpu_diff needs Windows (DXC / D3D12)");
    std::process::exit(1);
}
