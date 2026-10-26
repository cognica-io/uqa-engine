//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use std::{
    collections::BTreeSet,
    env,
    fmt::Write as _,
    path::{Path, PathBuf},
};

// Isolate the statically linked parser, including its allocator globals, from a host's libxml2.
// The pinned upstream sources remain unchanged; every C translation unit receives the same map.
fn private_symbols(root: &Path, names: &mut BTreeSet<String>, macros: &mut BTreeSet<String>) {
    for entry in std::fs::read_dir(root).expect("XML source directory") {
        let path = entry.expect("XML source entry").path();
        if path.is_dir() {
            private_symbols(&path, names, macros);
        } else if matches!(
            path.extension().and_then(|name| name.to_str()),
            Some("c" | "h")
        ) {
            let source = std::fs::read_to_string(path).expect("XML C source");
            for line in source.lines() {
                if let Some(directive) = line.trim_start().strip_prefix('#') {
                    let mut words = directive.split_whitespace();
                    if words.next() == Some("define") {
                        if let Some(name) = words.next().and_then(|word| word.split('(').next()) {
                            macros.insert(name.into());
                        }
                    }
                }
            }
            for word in source
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
            {
                // The pinned parser exposes two internal stack helpers without the library prefix.
                if word.starts_with("xml")
                    || word.starts_with("__xml")
                    || matches!(word, "nodePush" | "nodePop")
                {
                    names.insert(word.into());
                }
            }
        }
    }
}

fn private_symbol_header(output: &Path) -> PathBuf {
    let mut names = BTreeSet::new();
    let mut macros = BTreeSet::new();
    private_symbols(Path::new("vendor/libxml2"), &mut names, &mut macros);
    // These names are variables because THREAD_ALLOC is disabled below; the
    // remaining macros expand into private functions, variables or inline code.
    for name in [
        "xmlMalloc",
        "xmlMallocAtomic",
        "xmlRealloc",
        "xmlFree",
        "xmlMemStrdup",
    ] {
        macros.remove(name);
    }
    names.retain(|name| !macros.contains(name));
    let prefix_header = output.join("uqa_xml_symbols.h");
    let mut definitions = String::from(
        "/*\n * Unified Query Algebra\n *\n * Copyright (c) 2023-2026 Cognica, Inc.\n */\n\n",
    );
    for name in names {
        writeln!(definitions, "#define {name} uqa_sql_private_{name}").expect("symbol definition");
    }
    std::fs::write(&prefix_header, definitions).expect("private XML symbol map");
    prefix_header
}

fn configure_runtime(build: &mut cmake::Config, target: &str) {
    if target.contains("msvc") {
        let static_crt = env::var("CARGO_CFG_TARGET_FEATURE")
            .unwrap_or_default()
            .split(',')
            .any(|feature| feature == "crt-static");
        // CMake's runtime property takes precedence over the compiler's /MT or /MD flags.
        build.define(
            "CMAKE_MSVC_RUNTIME_LIBRARY",
            if static_crt {
                "MultiThreaded"
            } else {
                "MultiThreadedDLL"
            },
        );
    }
}

fn main() {
    println!("cargo:rerun-if-changed=vendor/libxml2");
    println!("cargo:rerun-if-changed=src/xml/native.c");
    println!("cargo:rerun-if-changed=src/xml/native.h");
    println!("cargo:rerun-if-changed=src/xml/allocation.c");
    println!("cargo:rerun-if-changed=src/xml/allocation.h");
    let target = env::var("TARGET").expect("Cargo target");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo output directory"));
    let prefix_header = private_symbol_header(&output);
    let prefix_flag = if target.contains("msvc") {
        "/FI"
    } else {
        "-include"
    };
    let mut build = cmake::Config::new("vendor/libxml2");
    configure_runtime(&mut build, &target);
    let quoted_header = if cfg!(windows) {
        prefix_header.to_string_lossy().replace('\\', "/")
    } else {
        prefix_header.to_string_lossy().replace('\\', "\\\\")
    }
    .replace('"', "\\\"");
    build
        .cflag(prefix_flag)
        .cflag(format!("\"{quoted_header}\""));
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
        .file("src/xml/allocation.c")
        .include(library.join("include/libxml2"))
        .define("LIBXML_STATIC", None)
        .flag(prefix_flag)
        .flag(prefix_header.to_str().expect("XML output path"))
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
