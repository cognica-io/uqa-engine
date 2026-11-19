//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use uqa_storage::backup::{BackupImageSource, BackupSourceStep, MemoryImageFamily, UnitKind};

#[derive(Debug, PartialEq, Eq)]
struct Unit {
    family: u16,
    key: Vec<u8>,
    kind: UnitKind,
    bytes: Vec<u8>,
}

fn drain(mut source: OwnedBackupImage) -> Vec<Unit> {
    let mut units = Vec::new();
    loop {
        let before = source.statistics().visited_positions;
        let next = source.advance(1).unwrap();
        assert!(source.statistics().visited_positions - before <= 1);
        match next {
            BackupSourceStep::Pending => continue,
            BackupSourceStep::Complete => break,
            BackupSourceStep::Unit => (),
        }
        let unit = source.current_unit().unwrap();
        assert_eq!(
            unit.address.namespace,
            uqa_storage::backup::memory_image_namespace()
        );
        assert_eq!(unit.address.generation, 0);
        let mut value = Unit {
            family: unit.address.family,
            key: unit.address.key.to_vec(),
            kind: unit.kind,
            bytes: Vec::new(),
        };
        if let Some(previous) = units.last() {
            let previous: &Unit = previous;
            assert!((previous.family, &previous.key) < (value.family, &value.key));
        }
        let mut buffer = [0; 31];
        loop {
            let count = source.read_current(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            value.bytes.extend_from_slice(&buffer[..count]);
        }
        units.push(value);
    }
    assert!(source.current_unit().is_none());
    assert_eq!(source.advance(1).unwrap(), BackupSourceStep::Complete);
    units
}

fn engine() -> Engine {
    let engine = Engine::new();
    engine.sql("CREATE TABLE a(id integer PRIMARY KEY, body text, embedding vector(2)); CREATE INDEX a_text ON a USING gin(body); CREATE INDEX a_vector ON a USING hnsw(embedding); CREATE INDEX a_value ON a(lower(body)); INSERT INTO a VALUES(1,'Original',ARRAY[1.0,0.0]),(9,'',ARRAY[0.0,1.0]); CREATE TABLE z(id integer); INSERT INTO z VALUES(4); CREATE TEMP TABLE transient(value text); INSERT INTO transient VALUES('excluded')", &[]).unwrap();
    engine
}

#[test]
fn memory_image_stream_covers_native_units_and_survives_private_writes_and_source_loss() {
    let engine = engine();
    let control = StorageReadControl::with_limit(32 << 20);
    let source = capture(&engine, &control).unwrap();
    let identity = source.cut();
    let expected = drain(source);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(
        expected
            .iter()
            .filter(|unit| unit.family == MemoryImageFamily::Catalog as u16)
            .count(),
        catalogs::CATALOG_COUNT as usize
    );
    assert_eq!(
        expected
            .iter()
            .filter(|unit| unit.bytes.starts_with(b"UQAMTM01"))
            .count(),
        2
    );
    assert!(expected
        .iter()
        .any(|unit| unit.bytes.starts_with(b"UQATXT01")));
    assert!(expected
        .iter()
        .any(|unit| unit.bytes.starts_with(b"UQAVAL01")));
    assert!(expected.iter().any(|unit| unit.kind == UnitKind::Artifact));
    engine
        .sql("BEGIN; UPDATE a SET body='private'; DELETE FROM z", &[])
        .unwrap();
    let retained = capture(&engine, &control).unwrap();
    assert_eq!(identity.source, retained.cut().source);
    assert_ne!(identity.snapshot, retained.cut().snapshot);
    engine
        .sql(
            "ROLLBACK; DROP TABLE a; DROP TABLE z; CREATE TABLE a(replacement text)",
            &[],
        )
        .unwrap();
    drop(engine);
    let actual = std::thread::spawn(move || drain(retained)).join().unwrap();
    assert_eq!(actual, expected);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_image_source_identity_is_stable_per_engine_and_independent_between_engines() {
    let first = Engine::new();
    let second = Engine::new();
    let control = StorageReadControl::with_limit(8 << 20);
    let source = capture(&first, &control).unwrap();
    let identity = source.cut().source;
    let expected = drain(source);
    let next = capture(&first, &control).unwrap();
    assert_eq!(next.cut().source, identity);
    assert_eq!(drain(next), expected);
    let other = capture(&second, &control).unwrap();
    assert_ne!(other.cut().source, identity);
    drop(other);
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn memory_image_failure_retires_the_cut_and_preserves_the_engine() {
    let engine = engine();
    let denied = StorageReadControl::with_limit(0);
    assert!(capture(&engine, &denied).is_err());
    assert_eq!(denied.memory().used(), 0);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut source = capture(&engine, &control).unwrap();
    while source.advance(1).unwrap() == BackupSourceStep::Pending {}
    assert_eq!(source.read_current(&mut [0; 1]).unwrap(), 1);
    control.cancellation().cancel();
    assert!(source.read_current(&mut [0; 1]).is_err());
    assert!(source.advance(1).is_err());
    drop(source);
    assert_eq!(control.memory().used(), 0);
    engine.sql("INSERT INTO z VALUES(5)", &[]).unwrap();
    assert_eq!(engine.transaction_depth(), 0);
}
