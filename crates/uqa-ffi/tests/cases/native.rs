//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute actual C and C++ clients against the Cargo-built shared artifact.

#[cfg(unix)]
#[test]
fn c_and_cpp_messages_use_the_shared_abi() {
    native_client("messages.c");
}

#[cfg(unix)]
#[test]
fn c_and_cpp_cursors_use_the_shared_abi() {
    native_client("cursors.c");
}

#[cfg(unix)]
#[test]
fn c_and_cpp_notices_use_the_shared_abi() {
    native_client("notices.c");
}

#[cfg(unix)]
#[test]
fn c_and_cpp_copy_use_the_shared_abi() {
    native_client("copy.c");
}

#[cfg(unix)]
fn native_client(fixture: &str) {
    use std::{path::PathBuf, process::Command};

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dependencies = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .to_owned();
    let library = dependencies.join(if cfg!(target_os = "macos") {
        "libuqa_ffi.dylib"
    } else {
        "libuqa_ffi.so"
    });
    assert!(
        library.exists(),
        "missing shared artifact: {}",
        library.display()
    );
    let output = tempfile::tempdir().unwrap();
    for (compiler, language, standard) in [("cc", "c", "c11"), ("c++", "c++", "c++17")] {
        let executable = output.path().join(compiler);
        let build = Command::new(compiler)
            .args([
                "-x",
                language,
                &format!("-std={standard}"),
                "-Wall",
                "-Wextra",
                "-Werror",
                "-DUQA_USE_SHARED",
            ])
            .arg("-I")
            .arg(root.join("include"))
            .arg(root.join("tests/native").join(fixture))
            .args(["-x", "none"])
            .arg(&library)
            .arg(format!("-Wl,-rpath,{}", dependencies.display()))
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{compiler}: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let temporary = output.path().join(format!("temporary-{compiler}"));
        std::fs::create_dir(&temporary).unwrap();
        let run = Command::new(&executable)
            .env("TMPDIR", &temporary)
            .env("TMP", &temporary)
            .env("TEMP", &temporary)
            .env("SQLITE_TMPDIR", &temporary)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{compiler}: {}\n{}",
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

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
