//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{env, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-changed=vendor/libxml2");
    println!("cargo:rerun-if-changed=src/xml/native.c");
    println!("cargo:rerun-if-changed=src/xml/native.h");
    let target = env::var("TARGET").expect("Cargo target");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let mut build = cmake::Config::new("vendor/libxml2");
    build
        .profile("Release")
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("CMAKE_INSTALL_LIBDIR", "lib")
        .define("CMAKE_C_VISIBILITY_PRESET", "hidden")
        .define(
            "LIBXML2_WITH_THREADS",
            if target.contains("emscripten") {
                "OFF"
            } else {
                "ON"
            },
        )
        .define("LIBXML2_WITH_VALID", "ON");
    for feature in [
        "CATALOG",
        "DEBUG",
        "DOCS",
        "HTML",
        "HTTP",
        "ICONV",
        "ICU",
        "ISO8859X",
        "LEGACY",
        "MODULES",
        "OUTPUT",
        "PATTERN",
        "PROGRAMS",
        "PUSH",
        "PYTHON",
        "READLINE",
        "REGEXPS",
        "SAX1",
        "TESTS",
        "TLS",
        "XINCLUDE",
        "XPATH",
        "ZLIB",
        "C14N",
        "HISTORY",
        "READER",
        "SCHEMAS",
        "SCHEMATRON",
        "THREAD_ALLOC",
        "WRITER",
        "XPTR",
        "RELAXNG",
    ] {
        build.define(format!("LIBXML2_WITH_{feature}"), "OFF");
    }
    let library = build.build();
    let archive = if target.contains("msvc") {
        ("libxml2s.lib", "uqa_sql_xml.lib")
    } else {
        ("libxml2.a", "libuqa_sql_xml.a")
    };
    std::fs::copy(library.join("lib").join(archive.0), output.join(archive.1))
        .expect("copy private XML parser archive");
    cc::Build::new()
        .file("src/xml/native.c")
        .include(library.join("include/libxml2"))
        .define("LIBXML_STATIC", None)
        .flag_if_supported("-fvisibility=hidden")
        .compile("uqa_sql_xml_bridge");
    println!("cargo:rustc-link-search=native={}", output.display());
    println!("cargo:rustc-link-lib=static=uqa_sql_xml");
    if target.contains("windows") {
        println!("cargo:rustc-link-lib=bcrypt");
    } else {
        println!("cargo:rustc-link-lib=m");
    }
}
