//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::env;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=MLX_C_LIB_DIR");
    println!("cargo:rerun-if-env-changed=MLX_LIB_DIR");
    println!("cargo:rerun-if-env-changed=HOMEBREW_PREFIX");

    if env::var_os("CARGO_FEATURE_MLX").is_none() {
        return;
    }

    let mut lib_dirs = Vec::new();
    if let Some(dir) = env::var_os("MLX_C_LIB_DIR") {
        lib_dirs.push(PathBuf::from(dir));
    }
    if let Some(dir) = env::var_os("MLX_LIB_DIR") {
        lib_dirs.push(PathBuf::from(dir));
    }
    if let Some(prefix) = env::var_os("HOMEBREW_PREFIX") {
        lib_dirs.push(PathBuf::from(prefix).join("lib"));
    }
    lib_dirs.push(PathBuf::from("/opt/homebrew/lib"));
    lib_dirs.push(PathBuf::from("/usr/local/lib"));

    lib_dirs.sort();
    lib_dirs.dedup();
    for dir in lib_dirs {
        if dir.exists() {
            println!("cargo:rustc-link-search=native={}", dir.display());
            if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
                println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir.display());
            }
        }
    }

    println!("cargo:rustc-link-lib=dylib=mlxc");
    println!("cargo:rustc-link-lib=dylib=mlx");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=dylib=c++");
        println!("cargo:rustc-link-lib=framework=Accelerate");
        println!("cargo:rustc-link-lib=framework=Metal");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=framework=QuartzCore");
    }
}
