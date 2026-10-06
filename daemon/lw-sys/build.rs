//! Compile la couche C (macOS uniquement).
fn main() {
    println!("cargo:rerun-if-changed=csrc/lw_sys.c");
    println!("cargo:rerun-if-changed=csrc/lw_sys.h");
    println!("cargo:rerun-if-changed=csrc/lw_shm.c");
    println!("cargo:rerun-if-changed=csrc/lw_shm.h");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return; // Linux (nodes) : implémentations de repli en Rust, sans C.
    }
    cc::Build::new()
        .file("csrc/lw_sys.c")
        .file("csrc/lw_shm.c")
        .flag("-std=gnu11")
        .flag("-fblocks")
        .flag("-Wall")
        .flag("-Wextra")
        .flag("-Werror")
        .compile("lw_sys");
}
