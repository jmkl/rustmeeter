// Links the Cactus engine when the `cactus` feature is on (it is by default).
//
// Expected layout:   <project>/cactus/libcactus.a   (override the folder with CACTUS_LIB_DIR)
// Target:            x86_64-pc-windows-gnullvm      (set in .cargo/config.toml)
// C++ runtime:       libc++ + libunwind are linked statically from llvm-mingw so the .exe needs no DLLs
//                    (llvm-mingw folder defaults to C:/tools/llvm-mingw, override with LLVM_MINGW_DIR).
//                    For some other toolchain, set CACTUS_CXX_LIB=stdc++ (or c++) instead.
use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-env-changed=CACTUS_LIB_DIR");
    println!("cargo::rerun-if-env-changed=LLVM_MINGW_DIR");
    println!("cargo::rerun-if-env-changed=CACTUS_CXX_LIB");

    // cactus-core always links the Cactus engine.

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib_dir = env::var_os("CACTUS_LIB_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("cactus"));
    if !lib_dir.join("libcactus.a").exists() {
        panic!(
            "libcactus.a not found in {}.\n\
             Put the Cactus release files there (libcactus.a), or set CACTUS_LIB_DIR,\n\
             or build without captions: cargo build --release --no-default-features",
            lib_dir.display()
        );
    }
    println!("cargo::rustc-link-search=native={}", lib_dir.display());
    println!("cargo::rustc-link-lib=static=cactus");

    let target = env::var("TARGET").unwrap_or_default();
    if target.ends_with("windows-gnullvm") {
        let llvm = env::var("LLVM_MINGW_DIR").unwrap_or_else(|_| "C:/tools/llvm-mingw".into());
        println!("cargo::rustc-link-search=native={llvm}/x86_64-w64-mingw32/lib");
        println!("cargo::rustc-link-lib=static=c++");
        println!("cargo::rustc-link-lib=static=unwind");
    }
    if let Ok(l) = env::var("CACTUS_CXX_LIB") {
        println!("cargo::rustc-link-lib={l}");
    }
}
