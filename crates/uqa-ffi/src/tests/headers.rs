//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

#[cfg(unix)]
#[test]
fn installed_headers_compile_independently_in_each_linkage_mode() {
    use std::{path::PathBuf, process::Command};
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let output = tempfile::tempdir().unwrap();
    let source = output.path().join("headers.c");
    let copyright =
        "/*\n * Unified Query Algebra\n *\n * Copyright (c) 2023-2026 Cognica, Inc.\n */\n";
    for header in [
        "uqa_export.h",
        "uqa.h",
        "uqa_udf.h",
        "uqa_arrow.h",
        "uqa_log.h",
    ] {
        std::fs::write(&source, format!("{copyright}\n#include <uqa/{header}>\n")).unwrap();
        for (compiler, language, standard) in [("cc", "c", "c11"), ("c++", "c++", "c++17")] {
            for mode in [None, Some("UQA_BUILD_SHARED"), Some("UQA_USE_SHARED")] {
                let mut command = Command::new(compiler);
                command
                    .args([
                        "-x",
                        language,
                        &format!("-std={standard}"),
                        "-fsyntax-only",
                        "-Wall",
                        "-Wextra",
                        "-Werror",
                    ])
                    .arg("-I")
                    .arg(root.join("include"))
                    .arg(&source);
                if let Some(mode) = mode {
                    command.arg(format!("-D{mode}"));
                }
                let build = command.output().unwrap();
                assert!(
                    build.status.success(),
                    "{header}, {compiler}, {mode:?}: {}",
                    String::from_utf8_lossy(&build.stderr)
                );
            }
            let conflict = Command::new(compiler)
                .args([
                    "-x",
                    language,
                    &format!("-std={standard}"),
                    "-fsyntax-only",
                    "-DUQA_BUILD_SHARED",
                    "-DUQA_USE_SHARED",
                ])
                .arg("-I")
                .arg(root.join("include"))
                .arg(&source)
                .output()
                .unwrap();
            assert!(
                !conflict.status.success(),
                "{header} accepted conflicting shared modes"
            );
        }
    }
}
