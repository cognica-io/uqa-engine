//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Real Engine generations pass through the common provider interface and repository owners.

use std::{path::Path, sync::Arc};
use uqa_core::{Value, Vertex};
use uqa_engine::{Engine, ScoringMode};
use uqa_graph::GraphStore;
use uqa_sql::result::ResultRow;
use uqa_storage::{
    backup::{
        BackupCaptureReport, BackupCut, BackupImageSource, BackupKey, BackupLimits, BackupResult,
        BackupRoot, BackupSourceStatistics, BackupSourceStep, BackupSourceUnit, ImageProvider,
        LocalRepository, OwnedBackupImage, Protection, ProtectionId, UnitKind,
    },
    read_control::StorageReadControl,
    PersistentStorageProvider,
};
use uqa_storage_redb::RedbStorage;
use uqa_storage_sqlite::{
    ManagedConnection, SQLiteCompressionOptions, SQLiteKeyValueStorage, SQLiteStorageProvider,
};

#[path = "backup_provider/restore.rs"]
mod restore;

const KEY: &str = "engine-image-fixture";

#[derive(Clone, Copy, Debug)]
enum Provider {
    Native,
    KV,
    Redb,
}

impl Provider {
    fn open(self, path: &Path, mode: usize) -> Arc<dyn PersistentStorageProvider> {
        if matches!(self, Self::Redb) {
            return Arc::new(RedbStorage::open(path).unwrap());
        }
        let connection = match mode {
            0 => ManagedConnection::open(path),
            1 => ManagedConnection::open_encrypted(path, KEY),
            2 => ManagedConnection::open_compressed(path, SQLiteCompressionOptions::default()),
            3 => ManagedConnection::open_compressed_encrypted(
                path,
                KEY,
                SQLiteCompressionOptions::default(),
            ),
            _ => unreachable!(),
        }
        .unwrap();
        match self {
            Self::Native => Arc::new(SQLiteStorageProvider::new(connection)),
            Self::KV => Arc::new(SQLiteKeyValueStorage::from_connection(connection).unwrap()),
            Self::Redb => unreachable!(),
        }
    }

    fn image(self) -> ImageProvider {
        match self {
            Self::Native => ImageProvider::SQLiteNative,
            Self::KV => ImageProvider::SQLiteKV,
            Self::Redb => ImageProvider::Redb,
        }
    }
}

fn sql(engine: &Engine, statement: &str) -> Vec<ResultRow> {
    engine
        .sql(statement, &[])
        .unwrap_or_else(|error| panic!("{statement}: {error}"))
        .rows
}

fn setup(engine: &Engine) {
    for statement in [
        "CREATE SCHEMA archive",
        "CREATE ROLE backup_reader",
        "CREATE TYPE archive.status AS ENUM ('ready', 'done')",
        "CREATE DOMAIN archive.positive AS INTEGER CHECK (VALUE > 0)",
        "CREATE TABLE archive.docs (id INTEGER PRIMARY KEY, title TEXT, embedding VECTOR(2), state archive.status, quantity archive.positive)",
        "CREATE INDEX docs_text ON archive.docs USING gin (title)",
        "INSERT INTO archive.docs VALUES (1, 'original alpha', ARRAY[1.0, 0.0], 'ready', 3), (2, 'original beta', ARRAY[0.0, 1.0], 'done', 5)",
        "CREATE VIEW archive.titles AS SELECT id, title FROM archive.docs",
        "CREATE MATERIALIZED VIEW archive.saved AS SELECT id, title FROM archive.docs",
        "CREATE FUNCTION archive.double(n INTEGER) RETURNS INTEGER LANGUAGE SQL AS 'SELECT n * 2'",
        "CREATE SEQUENCE archive.ticket START WITH 40 CACHE 1",
        "SELECT nextval('archive.ticket')",
        "GRANT USAGE ON SCHEMA archive TO backup_reader",
        "GRANT SELECT ON archive.docs TO backup_reader",
        "COMMENT ON TYPE archive.status IS 'retained type description'",
        "CREATE UNLOGGED TABLE archive.unlogged (value INTEGER)",
        "INSERT INTO archive.unlogged VALUES (17)",
        "CREATE TEMP TABLE private_temporary (value INTEGER)",
        "INSERT INTO private_temporary VALUES (99)",
    ] {
        sql(engine, statement);
    }
    engine.create_graph("retained").unwrap();
    let mut vertex = Vertex::new(7, "record");
    vertex
        .properties
        .insert("value".into(), Value::Str("original graph".into()));
    engine.add_graph_vertex(vertex, "retained").unwrap();
    assert_eq!(
        sql(engine, "SELECT sum(quantity) AS value FROM archive.docs")[0]["value"],
        Value::Int(8)
    );
}

#[derive(Debug, PartialEq)]
struct Observed {
    rows: Vec<Vec<ResultRow>>,
    sequences: Vec<uqa_storage::SequenceRow>,
    text: Vec<(u64, u64)>,
    vectors: Vec<(u64, u64)>,
}

fn observe(engine: &Engine, provider: &dyn PersistentStorageProvider) -> Observed {
    let rows = [
        "SELECT id, title, state::text AS state, quantity FROM archive.docs ORDER BY id",
        "SELECT id, title FROM archive.titles ORDER BY id",
        "SELECT id, title FROM archive.saved ORDER BY id",
        "SELECT 'archive.docs'::regclass::oid AS relation, 'archive.status'::regtype::oid AS type",
        "SELECT archive.double(7) AS value, has_table_privilege('backup_reader', 'archive.docs', 'SELECT') AS permitted",
        "SELECT value FROM archive.unlogged",
        "SELECT obj_description('archive.status'::regtype, 'pg_type') AS value",
    ]
    .map(|statement| sql(engine, statement))
    .into();
    // A physical backup preserves the durable reservation endpoint, not this process's position within its already logged values. Compare every owned sequence field directly; SQL observations are checked separately after fresh import.
    let session = provider.open_session().unwrap();
    let mut sequences = session.catalog.load_sequence_rows().unwrap();
    sequences.sort_by(|left, right| left.relation.cmp(&right.relation));
    drop(session);
    let text = engine
        .search(
            "archive.docs",
            "title",
            "original",
            &ScoringMode::default(),
            10,
        )
        .unwrap()
        .into_iter()
        .map(|hit| (hit.doc_id, hit.score.to_bits()))
        .collect();
    let vectors = engine
        .knn_search("archive.docs", "embedding", vec![1.0, 0.0], 10)
        .unwrap()
        .into_iter()
        .map(|hit| (hit.doc_id, hit.score.to_bits()))
        .collect();
    let vertex = engine
        .graph_with("retained", |store| store.get_vertex(7).unwrap())
        .unwrap()
        .flatten()
        .unwrap();
    assert_eq!(
        vertex.properties["value"],
        Value::Str("original graph".into())
    );
    Observed {
        rows,
        sequences,
        text,
        vectors,
    }
}

struct ProgressImage<'a> {
    source: OwnedBackupImage,
    writer: Option<&'a Engine>,
}

impl BackupImageSource for ProgressImage<'_> {
    fn cut(&self) -> BackupCut {
        self.source.cut()
    }
    fn advance(&mut self, maximum: usize) -> BackupResult<BackupSourceStep> {
        self.source.advance(maximum)
    }
    fn current_unit(&self) -> Option<BackupSourceUnit<'_>> {
        self.source.current_unit()
    }
    fn read_current(&mut self, output: &mut [u8]) -> BackupResult<usize> {
        let record = self
            .source
            .current_unit()
            .is_some_and(|unit| unit.kind == UnitKind::Record);
        let before = self.source.statistics().copied_payload_bytes;
        let count = self.source.read_current(output)?;
        if record && self.source.statistics().copied_payload_bytes > before {
            if let Some(writer) = self.writer.take() {
                // This independent session commits after actual record bytes are read and before the next read. A retained physical read would block compressed rollback-journal writers here.
                sql(
                    writer,
                    "UPDATE archive.docs SET title = 'after cut' WHERE id = 1",
                );
            }
        }
        Ok(count)
    }
    fn statistics(&self) -> BackupSourceStatistics {
        self.source.statistics()
    }
}

fn publish(
    repository: &LocalRepository,
    source: impl BackupImageSource,
    parent: Option<BackupRoot>,
    control: &StorageReadControl,
) -> (BackupRoot, BackupCaptureReport) {
    let mut operation = repository
        .begin_backup(
            source.cut(),
            parent.map(BackupRoot::backup),
            32 << 20,
            control,
        )
        .unwrap();
    let report = operation.prepare_image(source, 16 << 20, control).unwrap();
    let root = operation.publish(control).unwrap();
    (root, report)
}

#[rstest::rstest]
#[case::native_plain(Provider::Native, 0)]
#[case::native_encrypted(Provider::Native, 1)]
#[case::native_compressed(Provider::Native, 2)]
#[case::native_compressed_encrypted(Provider::Native, 3)]
#[case::kv_plain(Provider::KV, 0)]
#[case::kv_encrypted(Provider::KV, 1)]
#[case::kv_compressed(Provider::KV, 2)]
#[case::kv_compressed_encrypted(Provider::KV, 3)]
#[case::redb(Provider::Redb, 0)]
fn common_provider_images_restore_engine_state_and_independent_incremental_roots(
    #[case] kind: Provider,
    #[case] mode: usize,
) {
    let directory = tempfile::tempdir().unwrap();
    let source_path = directory.path().join("source");
    std::fs::create_dir(&source_path).unwrap();
    let provider = kind.open(&source_path.join("database"), mode);
    let engine = Engine::from_persistent_provider(provider.clone()).unwrap();
    setup(&engine);
    let original = observe(&engine, provider.as_ref());
    assert_eq!(
        sql(&engine, "SELECT last_value FROM archive.ticket")[0]["last_value"],
        Value::Int(40)
    );
    let control = StorageReadControl::with_limit(16 << 20);
    let repository = LocalRepository::create(
        directory.path().join("repository"),
        Protection::Encrypted {
            domain: ProtectionId::from_bytes([3; 16]).unwrap(),
            key: BackupKey::new([5; 32]),
        },
        BackupLimits::new(4096, 4096).unwrap(),
        &control,
    )
    .unwrap();

    sql(&engine, "BEGIN");
    sql(
        &engine,
        "UPDATE archive.docs SET title = 'private' WHERE id = 2",
    );
    sql(
        &engine,
        "CREATE TABLE archive.private_definition (value INTEGER)",
    );
    let image = provider.capture_backup_image(&control).unwrap();
    assert_eq!(image.cut().format.provider(), kind.image());
    sql(&engine, "ROLLBACK");
    let writer = Engine::from_persistent_provider(provider.clone()).unwrap();
    let (full, report) = publish(
        &repository,
        ProgressImage {
            source: image,
            writer: Some(&writer),
        },
        None,
        &control,
    );
    assert!(report.source.visited_positions > 0);
    assert!(report.source.copied_payload_bytes > 0);
    assert_eq!(report.image.reused_units, 0);
    assert_eq!(
        sql(&writer, "SELECT title FROM archive.docs WHERE id = 1")[0]["title"],
        Value::Str("after cut".into())
    );
    drop(writer);

    sql(&engine, "DELETE FROM archive.docs WHERE id = 2");
    sql(
        &engine,
        "INSERT INTO archive.docs VALUES (3, 'original gamma', ARRAY[0.5, 0.5], 'ready', 7)",
    );
    assert_eq!(
        sql(&engine, "SELECT nextval('archive.ticket') AS value")[0]["value"],
        Value::Int(41)
    );
    sql(&engine, "SELECT setval('archive.ticket', 9, false)");
    let changed = observe(&engine, provider.as_ref());
    assert_ne!(original, changed);
    let image = provider.capture_backup_image(&control).unwrap();
    // The stream keeps its actual open database owner and committed cut, without requiring either Engine or the provider factory to remain alive.
    drop((engine, provider));
    let (child, report) = publish(&repository, image, Some(full), &control);
    assert!(report.image.reused_units > 0);
    assert!(report.image.written_units > 0);
    assert!(
        report.source.copied_payload_bytes > 0,
        "uncertified reuse reads its payload"
    );
    assert_eq!(control.memory().used(), 0);
    std::fs::remove_dir_all(source_path).unwrap();

    restore::verify(
        &repository,
        full,
        &directory.path().join("full"),
        kind,
        mode,
        &original,
        &control,
    );
    repository.remove_backup(full.backup(), &control).unwrap();
    assert!(repository.open_backup(full.backup(), &control).is_err());
    restore::verify(
        &repository,
        child,
        &directory.path().join("child"),
        kind,
        (mode + 1) % 4,
        &changed,
        &control,
    );
    assert_eq!(control.memory().used(), 0);
}
