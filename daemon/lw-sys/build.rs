//! Compile la couche C : région partagée (commune) et couche système du système cible.
fn main() {
    for f in [
        "csrc/lw_shm.c",
        "csrc/lw_shm.h",
        "csrc/lw_sys.h",
        "csrc/macos/lw_sys_macos.c",
        "csrc/linux/lw_sys_linux.c",
        "csrc/posix/lw_posix.c",
        "csrc/windows/lw_sys_win.c",
    ] {
        println!("cargo:rerun-if-changed={f}");
    }
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let mut b = cc::Build::new();
    b.file("csrc/lw_shm.c");
    match os.as_str() {
        "macos" => {
            b.file("csrc/macos/lw_sys_macos.c")
                .file("csrc/posix/lw_posix.c")
                .flag("-std=gnu11")
                .flag("-fblocks");
        }
        "linux" => {
            b.file("csrc/linux/lw_sys_linux.c")
                .file("csrc/posix/lw_posix.c")
                .flag("-std=c11");
        }
        "windows" => {
            b.file("csrc/windows/lw_sys_win.c");
        }
        other => panic!("système non pris en charge : {other}"),
    }
    if env == "msvc" {
        b.flag("/std:c11")
            .flag("/W3")
            .flag("/WX")
            .define("_CRT_SECURE_NO_WARNINGS", None);
    } else {
        if os == "windows" {
            b.flag("-std=c11");
        }
        b.flag("-Wall").flag("-Wextra").flag("-Werror");
    }
    b.compile("lw_sys");
    // Après la bibliothèque statique : l'éditeur de liens GNU (mingw) résout dans l'ordre.
    if os == "windows" {
        for lib in ["avrt", "advapi32", "qwave", "ws2_32"] {
            println!("cargo:rustc-link-lib={lib}");
        }
    }
}
