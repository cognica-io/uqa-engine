//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Execute actual C and C++ clients against the Cargo-built shared artifact.

#[cfg(unix)]
macro_rules! native_clients {
    ($module:ident, $fixture:literal) => {
        mod $module {
            #[test]
            fn memory() {
                super::native_client($fixture, "memory");
            }
            #[test]
            fn sqlite_native() {
                super::native_client($fixture, "sqlite_native");
            }
            #[test]
            fn sqlite_kv() {
                super::native_client($fixture, "sqlite_kv");
            }
            #[test]
            fn redb() {
                super::native_client($fixture, "redb");
            }
        }
    };
}

#[cfg(unix)]
native_clients!(messages, "messages.c");
#[cfg(unix)]
native_clients!(cursors, "cursors.c");
#[cfg(unix)]
native_clients!(notices, "notices.c");
#[cfg(unix)]
native_clients!(copy, "copy.c");
#[cfg(unix)]
native_clients!(notifications, "notifications.c");
#[cfg(unix)]
native_clients!(statements, "statements.c");
#[cfg(unix)]
native_clients!(storage, "storage.c");
#[cfg(unix)]
native_clients!(values, "values.c");
#[cfg(unix)]
native_clients!(typed_values, "typed_values.c");
#[cfg(unix)]
native_clients!(packed_values, "packed_values.c");
#[cfg(unix)]
native_clients!(udf_options, "udf_options.c");

#[cfg(unix)]
fn native_client(fixture: &str, backend: &str) {
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
            "{fixture}, {backend}, {compiler}: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let temporary = output.path().join(format!("temporary-{compiler}"));
        std::fs::create_dir(&temporary).unwrap();
        let databases = output.path().join(format!("databases-{compiler}"));
        std::fs::create_dir(&databases).unwrap();
        let run = Command::new(&executable)
            .env("UQA_TEST_STORAGE", backend)
            .env("UQA_TEST_DATABASE_DIR", &databases)
            .env("TMPDIR", &temporary)
            .env("TMP", &temporary)
            .env("TEMP", &temporary)
            .env("SQLITE_TMPDIR", &temporary)
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{fixture}, {backend}, {compiler}: {}\n{}",
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
