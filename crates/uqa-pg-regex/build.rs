//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Compiles the imported `PostgreSQL` regular expression engine with the UQA shims into one static library.

use std::path::{Path, PathBuf};

/// Translation units in which the engine recurses. `regcomp.c` includes the other `regc_*.c` files and the shim's `regc_pg_locale.c`; `regexec.c` includes `rege_dfa.c`.
const RECURSIVE_SOURCES: [&str; 2] = [
    "postgres/src/backend/regex/regcomp.c",
    "postgres/src/backend/regex/regexec.c",
];

/// The other imported translation units, which do not recurse.
const LEAF_SOURCES: [&str; 4] = [
    "postgres/src/backend/regex/regerror.c",
    "postgres/src/backend/regex/regfree.c",
    "postgres/src/common/unicode_case.c",
    "postgres/src/common/unicode_category.c",
];

const SHIM_SOURCES: [&str; 1] = ["shim/uqa_pg_regex.c"];

/// The shim directories precede the imported headers so that `postgres.h` and the `regc_pg_locale.c` port resolve to UQA's files.
const INCLUDE_DIRECTORIES: [&str; 3] = ["shim/include", "shim/regex", "postgres/src/include"];

fn engine_build(root: &Path) -> cc::Build {
    let mut build = cc::Build::new();
    for directory in INCLUDE_DIRECTORIES {
        build.include(root.join(directory));
    }
    // The imported files are compiled exactly as PostgreSQL ships them; their upstream warning profile is not UQA's to change.
    build.warnings(false);
    // PostgreSQL compiles the engine with -O2. The stack each recursion level uses, which MAX_STACK_DEPTH bounds, depends on the optimization level, and unoptimized frames would refuse patterns that PostgreSQL accepts, so every profile compiles the engine as PostgreSQL does.
    build.opt_level(2);
    build
}

fn main() {
    let root = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("Cargo sets CARGO_MANIFEST_DIR"),
    );
    for directory in ["postgres", "shim"] {
        println!("cargo:rerun-if-changed={directory}");
    }
    let mut recursive = engine_build(&root);
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("emscripten") {
        // The engine's recursion runs on the WebAssembly call stack, which C cannot measure. The shim counts the live engine frames instead: every engine function reports its entry, and every one gets a frame of its own on the stack in linear memory, which shows when a counted frame has returned. Native builds measure their stack directly and stay uninstrumented.
        recursive
            .flag("-finstrument-function-entry-bare")
            .flag("-fstack-protector-all");
    }
    for source in RECURSIVE_SOURCES {
        recursive.file(root.join(source));
    }
    let objects = recursive.compile_intermediates();
    let mut library = engine_build(&root);
    for source in LEAF_SOURCES.iter().chain(SHIM_SOURCES.iter()) {
        library.file(root.join(source));
    }
    library.objects(objects);
    library.compile("uqa_pg_regex");
}
