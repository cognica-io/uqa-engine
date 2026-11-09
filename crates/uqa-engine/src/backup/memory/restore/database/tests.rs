//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;
use crate::{backup::BackupOptions, Engine};
use uqa_core::{Edge, Value, Vertex};
use uqa_storage::backup::{
    BackupCaptureStep, BackupImageSource, BackupLimits, BackupRoot, LocalRepository, Protection,
};

fn source() -> Engine {
    let engine = Engine::new();
    engine.sql("CREATE TYPE mood AS ENUM('low','high'); COMMENT ON TYPE mood IS 'saved'; CREATE SEQUENCE ticket START 41; CREATE TABLE items(id integer PRIMARY KEY, body text, embedding vector(2), ticket bigint DEFAULT nextval('ticket')); CREATE INDEX text_index ON items USING gin(body); CREATE INDEX vector_index ON items USING hnsw(embedding); CREATE INDEX value_index ON items(lower(body)); INSERT INTO items(id,body,embedding) VALUES(1,'Cats DOGS',ARRAY[1.0,0.0]),(2,'birds',ARRAY[0.0,1.0]); CREATE TABLE empty(id integer); CREATE UNLOGGED TABLE unlogged(value integer); INSERT INTO unlogged VALUES(17); CREATE TEMP TABLE transient(value integer); INSERT INTO transient VALUES(99); CREATE VIEW visible AS SELECT id,body FROM items; CREATE FUNCTION increment(n integer) RETURNS integer LANGUAGE SQL AS 'SELECT n+1'", &[]).unwrap();
    engine.sql("CREATE TABLE typed(value mood); INSERT INTO typed VALUES('low'); CREATE ROLE archive_reader; GRANT USAGE ON SCHEMA public TO archive_reader; GRANT USAGE ON TYPE mood TO archive_reader; GRANT SELECT(id,body) ON items TO archive_reader; GRANT SELECT ON visible TO archive_reader; GRANT USAGE ON SEQUENCE ticket TO archive_reader; GRANT EXECUTE ON FUNCTION increment(integer) TO archive_reader; GRANT SELECT(relname) ON pg_class TO archive_reader", &[]).unwrap();
    engine.sql("CREATE DOMAIN positive AS integer CHECK(VALUE > 0); CREATE TYPE pair AS (mood mood, score positive); CREATE TABLE shaped(value pair); INSERT INTO shaped VALUES(ROW('low',1)::pair); CREATE DOMAIN bundle AS pair[]; CREATE TABLE bundled(value bundle)", &[]).unwrap();
    for id in [1, 2] {
        engine
            .add_graph_vertex(Vertex::new(id, "node"), "saved")
            .unwrap();
    }
    engine
        .add_graph_edge(Edge::new(1, 1, 2, "edge"), "saved")
        .unwrap();
    engine.sql("CREATE VIEW nested_visible AS WITH items AS (SELECT id FROM public.items) SELECT * FROM items; SELECT create_graph('query_graph'); SELECT create_vlabel('query_graph','node'); CREATE VIEW saved_nodes AS SELECT * FROM query_graph.node; CREATE FUNCTION first_item() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT min(id) FROM public.items; END", &[]).unwrap();
    engine
        .build_path_index("saved_path", "saved", &[vec!["edge".into()]])
        .unwrap();
    engine
        .save_model(
            "saved_model",
            &uqa_ml::DeepModel {
                layers: vec![uqa_ml::DeepLayerSpec::Embed {
                    embedding: vec![0.25, -0.0, 0.75],
                }],
                alpha: 0.25,
                gating: uqa_ml::GatingSpec::None,
            },
        )
        .unwrap();
    engine
        .register_named_analyzer("saved_analyzer", r#"{"tokenizer":{"type":"whitespace"}}"#)
        .unwrap();
    engine
}

type EncodedUnit = (u16, Vec<u8>, Vec<u8>);

fn image(engine: &Engine) -> Vec<EncodedUnit> {
    let control = StorageReadControl::with_limit(32 << 20);
    let mut image = crate::backup::memory::capture(engine, &control).unwrap();
    let mut units = Vec::new();
    loop {
        match image.advance(1).unwrap() {
            BackupSourceStep::Pending => continue,
            BackupSourceStep::Complete => break,
            BackupSourceStep::Unit => (),
        }
        let unit = image.current_unit().unwrap();
        let family = unit.address.family;
        let key = unit.address.key.to_vec();
        let mut bytes = Vec::new();
        let mut buffer = [0; 127];
        loop {
            let count = image.read_current(&mut buffer).unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        // A reconstructed database receives a new source identity; every other native observation is preserved.
        if family != 1 {
            units.push((family, key, bytes));
        }
    }
    drop(image);
    assert_eq!(control.memory().used(), 0);
    units
}

fn publish(engine: &Engine, repository: &LocalRepository) -> BackupRoot {
    let control = StorageReadControl::with_limit(32 << 20);
    let mut job = engine
        .begin_backup(repository, BackupOptions::new(32 << 20, 16 << 20), &control)
        .unwrap();
    while job.advance(7, 1024).unwrap() == BackupCaptureStep::Pending {}
    let root = job.publish().unwrap();
    drop(job);
    assert_eq!(control.memory().used(), 0);
    root
}

fn input<'a>(
    repository: &'a LocalRepository,
    root: BackupRoot,
    control: &StorageReadControl,
) -> Input<'a> {
    let lease = repository.open_backup(root.backup(), control).unwrap();
    Input::new(
        MemoryImageImportReader::new(lease, 16 << 20, control).unwrap(),
        AnalyzerResources::default(),
        DiskANNTemporaryBudget::new(16 << 20),
        8 << 20,
        control,
    )
    .unwrap()
}

fn repository(directory: &std::path::Path) -> LocalRepository {
    LocalRepository::create(
        directory,
        Protection::Plaintext,
        BackupLimits::new(4096, 4096).unwrap(),
        &StorageReadControl::with_limit(1 << 20),
    )
    .unwrap()
}

#[test]
fn complete_repository_input_preserves_native_state_after_source_retirement_and_runtime_writes() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = source();
    let expected = image(&source);
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
    };
    assert!(matches!(input.advance(1024), Err(BackupError::Finished)));
    assert_eq!(state.header.source, root.cut().source);
    assert!(state.header.catalog_oid_position.is_some());
    // This fixture starts ordinary execution from independently known valid source state. Public activation still requires complete cross-catalog validation.
    let restored = state
        .into_private_engine(MemoryBudget::new(32 << 20), &control)
        .unwrap();
    drop(input);
    control.cancellation().cancel();
    let full = control
        .memory()
        .reserve(control.memory().available())
        .unwrap();
    assert_eq!(image(&restored), expected);
    restored.sql("SET ROLE archive_reader", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT body FROM items", &[])
            .unwrap()
            .rows
            .len(),
        2
    );
    assert_eq!(
        restored
            .sql("UPDATE items SET body='denied' WHERE id=1", &[])
            .unwrap_err()
            .sqlstate(),
        Some("42501")
    );
    restored.sql("RESET ROLE", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT (value).score = 1 AS valid FROM shaped", &[])
            .unwrap()
            .rows[0]["valid"],
        Value::Bool(true)
    );
    assert_eq!(
        restored
            .sql("SELECT nextval('ticket') AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(43)
    );
    assert_eq!(
        restored
            .sql("SELECT increment(7) AS value", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(8)
    );
    assert_eq!(
        restored
            .sql("SELECT count(*) AS value FROM visible", &[])
            .unwrap()
            .rows[0]["value"],
        Value::Int(2)
    );
    assert!(restored.sql("SELECT * FROM transient", &[]).is_err());
    for (sql, expected) in [
        ("SELECT count(*) AS value FROM nested_visible", 2),
        ("SELECT count(*) AS value FROM saved_nodes", 0),
        ("SELECT first_item() AS value", 1),
    ] {
        assert_eq!(
            restored.sql(sql, &[]).unwrap().rows[0]["value"],
            Value::Int(expected)
        );
    }
    restored.sql("BEGIN; UPDATE items SET body='rollback' WHERE id=1; ROLLBACK; INSERT INTO items(id,body,embedding) VALUES(3,'new',ARRAY[0.5,0.5])", &[]).unwrap();
    assert_eq!(
        restored
            .sql("SELECT body FROM items WHERE id=1", &[])
            .unwrap()
            .rows[0]["body"],
        Value::Str("Cats DOGS".into())
    );
    restored
        .add_graph_vertex(Vertex::new(3, "after"), "saved")
        .unwrap();
    let capture_control = StorageReadControl::with_limit(32 << 20);
    let retained = crate::backup::memory::capture(&restored, &capture_control).unwrap();
    drop((restored, full));
    assert!(control.memory().used() > 0);
    drop(retained);
    assert_eq!(control.memory().used(), 0);
    assert_eq!(capture_control.memory().used(), 0);
}

#[test]
fn complete_repository_input_rejects_missing_and_replaced_authority_before_private_activation() {
    for replace in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source.sql("CREATE ROLE owner_role; CREATE TABLE secured(id integer); ALTER TABLE secured OWNER TO owner_role", &[]).unwrap();
        let mut owner = source
            .durable
            .roles
            .read()
            .get("owner_role")
            .unwrap()
            .clone();
        source.durable.roles.write().remove("owner_role").unwrap();
        if replace {
            owner.object_id = [99; 16];
            source
                .durable
                .roles
                .write()
                .insert(owner.name.clone(), owner)
                .unwrap();
        }
        let root = publish(&source, &repository);
        drop(source);
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        let state = loop {
            if let Some(state) = input.advance(1024).unwrap() {
                break state;
            }
        };
        drop(input);
        let result = state.into_private_engine(MemoryBudget::new(32 << 20), &control);
        let Err(error) = result else {
            panic!("invalid owner reached private activation");
        };
        assert!(
            error
                .to_string()
                .contains("missing or replaced role incarnation"),
            "{error}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_repository_input_rejects_missing_user_type_before_private_activation() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = Engine::new();
    source.sql("CREATE TYPE mood AS ENUM('low','high'); CREATE TABLE typed(value mood); INSERT INTO typed VALUES('low')", &[]).unwrap();
    source.durable.enums.write().remove("public.mood").unwrap();
    let root = publish(&source, &repository);
    drop(source);
    let control = StorageReadControl::with_limit(32 << 20);
    let mut input = input(&repository, root, &control);
    let state = loop {
        if let Some(state) = input.advance(1024).unwrap() {
            break state;
        }
    };
    drop(input);
    let Err(error) = state.into_private_engine(MemoryBudget::new(32 << 20), &control) else {
        panic!("missing type reached private activation");
    };
    assert!(
        error.to_string().contains("invalid native type reference"),
        "{error}"
    );
    assert_eq!(control.memory().used(), 0);
}

#[test]
fn complete_repository_input_rejects_missing_stored_plan_relations_before_private_activation() {
    for definition in [
        "CREATE VIEW visible AS SELECT id FROM public.items",
        "CREATE FUNCTION first_item() RETURNS integer LANGUAGE SQL BEGIN ATOMIC SELECT min(id) FROM public.items; END",
    ] {
        let directory = tempfile::tempdir().unwrap();
        let repository = repository(&directory.path().join("repository"));
        let source = Engine::new();
        source
            .sql(
                "CREATE TABLE items(id integer); INSERT INTO items VALUES(1)",
                &[],
            )
            .unwrap();
        source.sql(definition, &[]).unwrap();
        source
            .storage
            .tables
            .write()
            .remove(&uqa_core::RelationIdentity::new("public", "items"))
            .unwrap()
            .unwrap();
        let root = publish(&source, &repository);
        drop(source);
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        let state = loop {
            if let Some(state) = input.advance(1024).unwrap() {
                break state;
            }
        };
        drop(input);
        let Err(error) = state.into_private_engine(MemoryBudget::new(32 << 20), &control) else {
            panic!("missing relation reached private activation");
        };
        assert!(
            error.to_string().contains("missing native plan relation"),
            "{error}"
        );
        assert_eq!(control.memory().used(), 0);
    }
}

#[test]
fn complete_input_failure_retires_all_prior_native_owners_and_repository_lease() {
    let directory = tempfile::tempdir().unwrap();
    let repository = repository(&directory.path().join("repository"));
    let source = source();
    let root = publish(&source, &repository);
    drop(source);
    for cancel in [false, true] {
        let control = StorageReadControl::with_limit(32 << 20);
        let mut input = input(&repository, root, &control);
        // Reach and decode graph membership after all catalog and field definitions.
        loop {
            input.advance(1024).unwrap();
            if matches!(
                input
                    .image
                    .as_ref()
                    .and_then(MemoryImageImportReader::current_unit)
                    .map(|unit| unit.address),
                Some(DecodedMemoryImageAddress::Resource { kind: 1, .. })
            ) {
                break;
            }
        }
        let occupied = if cancel {
            control.cancellation().cancel();
            None
        } else {
            Some(
                control
                    .memory()
                    .reserve(control.memory().available())
                    .unwrap(),
            )
        };
        while let Ok(state) = input.advance(1024) {
            assert!(state.is_none(), "denied input cannot complete");
        }
        assert!(matches!(input.advance(1024), Err(BackupError::Finished)));
        assert!(input.candidate.is_none());
        assert!(input.image.is_none());
        drop(occupied);
        assert_eq!(control.memory().used(), 0);
    }
}
