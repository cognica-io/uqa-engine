//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A container handle bounds the decompressed chunks it retains and the changes it holds.

use super::*;

const SMALL_CHUNK: usize = 512;
const ROWS: i64 = 700;

fn small_chunks(key: Option<&str>) -> OpenOptionsEntry {
    OpenOptionsEntry {
        compression: SQLiteCompressionOptions {
            codec: SQLiteCompressionCodec::Zstd,
            page_size: 512,
            chunk_pages: 1,
            level: 1,
        },
        key: key.map(str::to_owned),
        trusted_anchor: None,
        live_anchor: Arc::default(),
    }
}

fn default_chunks() -> OpenOptionsEntry {
    OpenOptionsEntry {
        compression: SQLiteCompressionOptions::default(),
        key: None,
        trusted_anchor: None,
        live_anchor: Arc::default(),
    }
}

fn pattern(chunk: usize, round: u8) -> Vec<u8> {
    let mut bytes = vec![round; SMALL_CHUNK];
    bytes[..8].copy_from_slice(&(chunk as u64).to_le_bytes());
    bytes
}

fn random_bytes(len: usize) -> Vec<u8> {
    let mut bytes = vec![0_u8; len];
    fill_random(&mut bytes).unwrap();
    bytes
}

fn row_count(connection: &rusqlite::Connection) -> i64 {
    connection
        .query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
        .unwrap()
}

fn integrity(connection: &rusqlite::Connection) -> String {
    connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap()
}

fn open_rows(path: &Path, key: &str) -> rusqlite::Connection {
    register_database(path, SQLiteCompressionOptions::default(), Some(key)).unwrap();
    rusqlite::Connection::open_with_flags_and_vfs(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE,
        VFS_NAME,
    )
    .unwrap()
}

fn insert_rows(connection: &rusqlite::Connection) {
    // Each row carries an incompressible 32 KiB value, so the transaction exceeds both SQLite's page cache and the dirty budget.
    for id in 2..=ROWS {
        connection
            .execute("INSERT INTO rows VALUES (?1, randomblob(32768))", [id])
            .unwrap();
    }
}

fn journal(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push("-journal");
    PathBuf::from(name)
}

#[test]
fn reading_more_than_the_clean_budget_retains_only_the_budget() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bounded-reads.uqac.sqlite3");
    let mut writer = ContainerFile::open(path.clone(), small_chunks(None)).unwrap();
    for chunk in 0..256 {
        writer
            .write_at(chunk * SMALL_CHUNK, &pattern(chunk, 1))
            .unwrap();
    }
    writer.flush().unwrap();
    drop(writer);

    let mut reader = ContainerFile::open(path, small_chunks(None)).unwrap();
    reader.set_memory_limits(8 * SMALL_CHUNK, DIRTY_CHUNK_PUBLISH_BYTES);
    let mut out = vec![0_u8; SMALL_CHUNK];
    for pass in 0..2 {
        for chunk in 0..256 {
            assert_eq!(
                reader.read_at(chunk * SMALL_CHUNK, &mut out).unwrap(),
                SMALL_CHUNK
            );
            assert_eq!(out, pattern(chunk, 1), "pass {pass}, chunk {chunk}");
            assert!(reader.cache.clean_bytes() <= 8 * SMALL_CHUNK);
        }
    }
    assert_eq!(reader.cache.len(), 8);
}

#[test]
fn changes_beyond_the_dirty_budget_are_published_before_the_final_flush() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bounded-writes.uqac.sqlite3");
    let key = Some("bounded writes credential");
    let mut container = ContainerFile::open(path.clone(), small_chunks(key)).unwrap();
    container.set_memory_limits(8 * SMALL_CHUNK, 16 * SMALL_CHUNK);
    for chunk in 0..256 {
        container
            .write_at(chunk * SMALL_CHUNK, &pattern(chunk, 2))
            .unwrap();
        assert!(container.cache.dirty_bytes() <= 16 * SMALL_CHUNK);
        assert!(container.cache.clean_bytes() <= 8 * SMALL_CHUNK);
    }
    assert!(container.generation > 1);
    container.flush().unwrap();
    let published = container.generation;
    drop(container);

    let mut reopened = ContainerFile::open(path, small_chunks(key)).unwrap();
    assert_eq!(reopened.generation, published);
    let mut out = vec![0_u8; SMALL_CHUNK];
    for chunk in 0..256 {
        reopened.read_at(chunk * SMALL_CHUNK, &mut out).unwrap();
        assert_eq!(out, pattern(chunk, 2), "chunk {chunk}");
    }
}

#[test]
fn compaction_keeps_the_clean_budget_and_the_live_contents() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("bounded-compaction.uqac.sqlite3");
    let mut container = ContainerFile::open(path.clone(), small_chunks(None)).unwrap();
    container.set_memory_limits(8 * SMALL_CHUNK, 64 * SMALL_CHUNK);
    for round in 1..=3_u8 {
        for chunk in 0..256 {
            container
                .write_at(chunk * SMALL_CHUNK, &pattern(chunk, round))
                .unwrap();
        }
        container.flush().unwrap();
        assert!(container.cache.clean_bytes() <= 8 * SMALL_CHUNK);
    }
    assert!(scan_chunk_record_generations(&path).len() < 3 * 256);
    assert!(!container.needs_compaction().unwrap());
    drop(container);

    let mut reopened = ContainerFile::open(path, small_chunks(None)).unwrap();
    let mut out = vec![0_u8; SMALL_CHUNK];
    for chunk in 0..256 {
        reopened.read_at(chunk * SMALL_CHUNK, &mut out).unwrap();
        assert_eq!(out, pattern(chunk, 3), "chunk {chunk}");
    }
}

#[test]
fn compaction_waits_until_stale_records_exceed_the_live_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("proportional-compaction.uqac.sqlite3");
    let chunk_size = SQLiteCompressionOptions::default().chunk_size().unwrap();
    let chunks = 320;
    let mut container = ContainerFile::open(path.clone(), default_chunks()).unwrap();
    container
        .write_at(0, &random_bytes(chunk_size * chunks))
        .unwrap();
    container.flush().unwrap();
    let live_len = std::fs::metadata(&path).unwrap().len();

    // Rewriting 280 of 320 incompressible chunks leaves more than 8 MiB of stale records, which is still less than the live records.
    for chunk in 0..280 {
        container
            .write_at(chunk * chunk_size, &random_bytes(chunk_size))
            .unwrap();
    }
    container.flush().unwrap();
    assert!(std::fs::metadata(&path).unwrap().len() - live_len > 8 * 1024 * 1024);
    assert_eq!(scan_chunk_record_generations(&path).len(), chunks + 280);

    for chunk in 280..chunks {
        container
            .write_at(chunk * chunk_size, &random_bytes(chunk_size))
            .unwrap();
    }
    container.flush().unwrap();
    assert_eq!(scan_chunk_record_generations(&path).len(), chunks);
    assert!(std::fs::metadata(&path).unwrap().len() <= live_len + 4096);
}

#[test]
fn a_transaction_larger_than_the_dirty_budget_commits_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("large-commit.sqlite3");
    let key = "large commit credential";
    let mut connection = open_rows(&path, key);
    connection
        .execute_batch("CREATE TABLE rows (id INTEGER PRIMARY KEY, body BLOB NOT NULL); INSERT INTO rows VALUES (1, zeroblob(16))")
        .unwrap();
    let transaction = connection.transaction().unwrap();
    insert_rows(&transaction);
    transaction.commit().unwrap();
    assert_eq!(row_count(&connection), ROWS);
    drop(connection);

    let reopened = open_rows(&path, key);
    assert_eq!(row_count(&reopened), ROWS);
    assert_eq!(integrity(&reopened), "ok");
}

#[test]
fn an_interrupted_transaction_after_early_publication_rolls_back_from_its_journal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("interrupted.sqlite3");
    let key = "interrupted transaction credential";
    let mut connection = open_rows(&path, key);
    connection
        .execute_batch("CREATE TABLE rows (id INTEGER PRIMARY KEY, body BLOB NOT NULL); INSERT INTO rows VALUES (1, zeroblob(16))")
        .unwrap();
    let committed = ContainerFile::load(path.clone(), Some(key))
        .unwrap()
        .generation;
    let transaction = connection.transaction().unwrap();
    insert_rows(&transaction);
    let published = ContainerFile::load(path.clone(), Some(key))
        .unwrap()
        .generation;
    assert!(published > committed);

    // The copies are what an interrupted process leaves: the main container with early-published pages and the hot journal that restores them.
    let copies = tempfile::tempdir().unwrap();
    let copy = copies.path().join("interrupted.sqlite3");
    std::fs::copy(&path, &copy).unwrap();
    std::fs::copy(journal(&path), journal(&copy)).unwrap();
    transaction.rollback().unwrap();
    assert_eq!(row_count(&connection), 1);
    assert_eq!(integrity(&connection), "ok");

    let recovered = open_rows(&copy, key);
    assert_eq!(row_count(&recovered), 1);
    assert_eq!(integrity(&recovered), "ok");
    assert!(!journal(&copy).exists());
}
