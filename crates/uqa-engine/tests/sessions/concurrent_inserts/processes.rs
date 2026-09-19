//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Generated identities also coordinate between independently opened processes.

use super::*;
use std::time::Instant;

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "missing handshake: {}",
            path.display()
        );
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn insert_worker() {
    let Some(path) = std::env::var_os("UQA_IDENTITY_INSERT_DATABASE") else {
        return;
    };
    let key = std::env::var("UQA_IDENTITY_INSERT_KEY").unwrap();
    let path = Path::new(&path);
    let directory = path.parent().unwrap().to_path_buf();
    let ready = directory.join(format!("ready-{key}"));
    let engine = Engine::open(path).unwrap();
    engine
        .register_scalar_function_with_options(
            "insert_checkpoint",
            SQLFunctionOptions::read_only(SQLFunctionVolatility::Volatile),
            move |_args: &[Value]| {
                std::fs::write(&ready, b"ready").unwrap();
                wait_for_file(&directory.join("release"));
                Ok(Value::Int(1))
            },
        )
        .unwrap();
    let result = engine
        .sql(
            "INSERT INTO items VALUES ($1) RETURNING key, _doc_id AS doc_id, insert_checkpoint()",
            &[uqa_sql::SQLParam::Scalar(Value::Str(key.clone()))],
        )
        .unwrap();
    assert_eq!(result.affected_rows, 1);
    assert_eq!(result.rows[0]["key"], Value::Str(key.clone()));
    let Value::Int(id) = result.rows[0]["doc_id"] else {
        panic!("expected physical document identity");
    };
    std::fs::write(
        path.parent().unwrap().join(format!("identity-{key}")),
        id.to_string(),
    )
    .unwrap();
}

#[test]
fn separate_process_inserts_preserve_both_returned_rows_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("process-inserts.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE TABLE items (key TEXT PRIMARY KEY)", &[])
        .unwrap();
    let children = ["first", "second"]
        .into_iter()
        .map(|key| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .arg("--exact")
                .arg("storage::sessions::concurrent_inserts::processes::insert_worker")
                .arg("--nocapture")
                .env("UQA_IDENTITY_INSERT_DATABASE", &path)
                .env("UQA_IDENTITY_INSERT_KEY", key)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect::<Vec<_>>();
    for key in ["first", "second"] {
        wait_for_file(&directory.path().join(format!("ready-{key}")));
    }
    std::fs::write(directory.path().join("release"), b"release").unwrap();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "worker failed: {output:?}");
    }
    let expected = ["first", "second"]
        .into_iter()
        .map(|key| {
            let id = std::fs::read_to_string(directory.path().join(format!("identity-{key}")))
                .unwrap()
                .parse::<i64>()
                .unwrap();
            BTreeMap::from([
                ("key".into(), Value::Str(key.into())),
                ("doc_id".into(), Value::Int(id)),
            ])
        })
        .collect::<Vec<_>>();
    assert_ne!(expected[0]["doc_id"], expected[1]["doc_id"]);
    assert_eq!(read_rows(&root.new_session().unwrap()), expected);
    drop(root);
    assert_eq!(read_rows(&Engine::open(&path).unwrap()), expected);
}
