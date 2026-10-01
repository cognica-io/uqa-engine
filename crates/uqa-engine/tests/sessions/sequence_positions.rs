//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Independently opened processes draw from one sequence position, and only a database left without an orderly close skips the values its record covers.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use uqa_engine::Engine;

const WORKER: &str = "storage::sessions::sequence_positions::sequence_worker";

fn wait_for_file(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !path.exists() {
        assert!(
            Instant::now() < deadline,
            "missing handshake: {}",
            path.display()
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Draws `UQA_SEQUENCE_WORKER_COUNT` values, publishes them, and then either closes its engine or keeps it open until the parent kills the process.
#[test]
fn sequence_worker() {
    let Some(path) = std::env::var_os("UQA_SEQUENCE_WORKER_DATABASE") else {
        return;
    };
    let path = Path::new(&path);
    let name = std::env::var("UQA_SEQUENCE_WORKER_NAME").unwrap();
    let count = std::env::var("UQA_SEQUENCE_WORKER_COUNT")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let engine = Engine::open(path).unwrap();
    let values = (0..count)
        .map(|_| engine.nextval("ids").unwrap().to_string())
        .collect::<Vec<_>>();
    let directory = path.parent().unwrap();
    let published = directory.join(format!("values-{name}"));
    std::fs::write(
        directory.join(format!("values-{name}.tmp")),
        values.join(","),
    )
    .unwrap();
    if std::env::var("UQA_SEQUENCE_WORKER_END").unwrap() == "close" {
        drop(engine);
        std::fs::rename(directory.join(format!("values-{name}.tmp")), published).unwrap();
        return;
    }
    std::fs::rename(directory.join(format!("values-{name}.tmp")), published).unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn worker(path: &Path, name: &str, count: usize, end: &str) -> Child {
    Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg(WORKER)
        .arg("--nocapture")
        .env("UQA_SEQUENCE_WORKER_DATABASE", path)
        .env("UQA_SEQUENCE_WORKER_NAME", name)
        .env("UQA_SEQUENCE_WORKER_COUNT", count.to_string())
        .env("UQA_SEQUENCE_WORKER_END", end)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap()
}

/// The values a worker drew, once it has drawn all of them and, for a closing worker, closed its engine.
fn values(path: &Path, name: &str) -> Vec<i64> {
    let published = path.parent().unwrap().join(format!("values-{name}"));
    wait_for_file(&published);
    std::fs::read_to_string(published)
        .unwrap()
        .split(',')
        .map(|value| value.parse().unwrap())
        .collect()
}

fn closed(path: &Path, name: &str, count: usize) -> Vec<i64> {
    let mut child = worker(path, name, count, "close");
    let drawn = values(path, name);
    assert!(child.wait().unwrap().success());
    drawn
}

fn killed(path: &Path, name: &str, count: usize) -> Vec<i64> {
    let mut child = worker(path, name, count, "hold");
    let drawn = values(path, name);
    child.kill().unwrap();
    child.wait().unwrap();
    drawn
}

#[test]
fn processes_draw_consecutive_values_while_any_of_them_keeps_the_database_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sequence-processes.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE SEQUENCE ids", &[]).unwrap();
    for expected in 1..=3 {
        assert_eq!(root.nextval("ids").unwrap(), expected);
    }
    // The other process passes the values the first record covers and writes the next record.
    assert_eq!(closed(&path, "first", 40), (4..=43).collect::<Vec<_>>());
    assert_eq!(root.nextval("ids").unwrap(), 44);
    // A killed process leaves the position in the file the surviving process reads.
    assert_eq!(killed(&path, "second", 5), (45..=49).collect::<Vec<_>>());
    assert_eq!(root.nextval("ids").unwrap(), 50);
    let shown = root
        .sql("SELECT last_value, log_cnt, is_called FROM ids", &[])
        .unwrap();
    assert_eq!(shown.rows[0]["last_value"], uqa_core::Value::Int(50));
    assert_eq!(shown.rows[0]["log_cnt"], uqa_core::Value::Int(16));
}

#[test]
fn only_a_database_left_without_an_orderly_close_skips_the_values_its_record_covers() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("sequence-restarts.db");
    let root = Engine::open(&path).unwrap();
    root.sql("CREATE SEQUENCE ids", &[]).unwrap();
    for expected in 1..=3 {
        assert_eq!(root.nextval("ids").unwrap(), expected);
    }
    drop(root);
    // Each process closes before the next one opens the database, and each continues exactly.
    assert_eq!(closed(&path, "first", 2), [4, 5]);
    assert_eq!(closed(&path, "second", 2), [6, 7]);
    // The only process with the database open is killed. Its position may be older than the values it drew, as after a machine failure, so the next process continues past the record, which holds 33.
    assert_eq!(killed(&path, "third", 2), [8, 9]);
    let root = Engine::open(&path).unwrap();
    assert_eq!(root.nextval("ids").unwrap(), 34);
    assert_eq!(root.nextval("ids").unwrap(), 35);
    drop(root);
    assert_eq!(closed(&path, "fourth", 2), [36, 37]);
}
