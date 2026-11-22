//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! A handle observes another handle's commit by reading only the records appended since its own.

use super::*;

const CHUNK: usize = 512;
const CHUNKS: usize = 64;

fn options(key: Option<&str>) -> OpenOptionsEntry {
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

fn pattern(chunk: usize, round: u8) -> Vec<u8> {
    let mut bytes = vec![round; CHUNK];
    bytes[..8].copy_from_slice(&(chunk as u64).to_le_bytes());
    bytes
}

fn filled(path: &Path, key: Option<&str>) -> ContainerFile {
    let mut writer = ContainerFile::open(path.to_path_buf(), options(key)).unwrap();
    for chunk in 0..CHUNKS {
        writer.write_at(chunk * CHUNK, &pattern(chunk, 1)).unwrap();
    }
    writer.flush().unwrap();
    writer
}

/// A reader that has decoded every chunk and keeps all of them.
fn warmed(path: &Path, key: Option<&str>) -> ContainerFile {
    let mut reader = ContainerFile::open(path.to_path_buf(), options(key)).unwrap();
    reader.set_memory_limits(CHUNKS * CHUNK, DIRTY_CHUNK_PUBLISH_BYTES);
    let mut out = vec![0_u8; CHUNK];
    for chunk in 0..CHUNKS {
        reader.read_at(chunk * CHUNK, &mut out).unwrap();
        assert_eq!(out, pattern(chunk, 1));
    }
    assert_eq!(reader.cache.len(), CHUNKS);
    reader
}

fn read(container: &mut ContainerFile, chunk: usize) -> Vec<u8> {
    let mut out = vec![0_u8; CHUNK];
    let read = container.read_at(chunk * CHUNK, &mut out).unwrap();
    out.truncate(read);
    out
}

#[rstest::rstest]
#[case(None)]
#[case(Some("incremental refresh credential"))]
fn an_appended_commit_replaces_only_the_chunks_it_published(#[case] key: Option<&str>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("appended.uqac.sqlite3");
    let mut writer = filled(&path, key);
    let mut reader = warmed(&path, key);

    for round in 2..=4_u8 {
        writer.write_at(3 * CHUNK, &pattern(3, round)).unwrap();
        writer.flush().unwrap();
        reader.refresh_committed_state().unwrap();

        // The other decoded chunks are still held; only the replaced one is read again.
        assert_eq!(reader.cache.len(), CHUNKS - 1);
        assert_eq!(reader.generation, writer.generation);
        assert_eq!(reader.state_tag, writer.state_tag);
        assert_eq!(read(&mut reader, 3), pattern(3, round));
        assert_eq!(read(&mut reader, 4), pattern(4, 1));
    }
    let fresh = ContainerFile::open(path, options(key)).unwrap();
    assert_eq!(reader.chunks.len(), fresh.chunks.len());
    for (chunk_id, entry) in &fresh.chunks {
        assert_eq!(reader.chunks[chunk_id].offset, entry.offset, "{chunk_id}");
    }
}

#[test]
fn a_resumed_scan_reads_only_the_records_after_its_resume_point() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("resumed.uqac.sqlite3");
    let key = Some("resumed scan credential");
    let mut writer = filled(&path, key);
    let reader = ContainerFile::open(path.clone(), options(key)).unwrap();
    writer.write_at(7 * CHUNK, &pattern(7, 2)).unwrap();
    writer.write_at(9 * CHUNK, &pattern(9, 2)).unwrap();
    writer.flush().unwrap();

    // Damage a record before the resume point. A scan of the whole file no longer
    // authenticates; the resumed scan never reads it.
    let first_record = scan_records(&path).remove(0);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[first_record.start as usize + 36] ^= 0xff;
    std::fs::write(&path, bytes).unwrap();
    assert!(ContainerFile::load(path.clone(), key).is_err());

    let mut file = File::open(&path).unwrap();
    let mut header_bytes = [0_u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes).unwrap();
    let header = parse_header(&header_bytes).unwrap();
    let committed = scan_committed_records_after(
        &mut file,
        &header,
        reader.keys.as_ref(),
        CHUNK,
        ResumePoint {
            offset: reader.append_offset,
            generation: reader.generation,
            state_tag: reader.state_tag,
            logical_len: reader.logical_len,
            chunk_count: reader.chunk_count().unwrap(),
        },
    )
    .unwrap();
    assert_eq!(committed.chunks.keys().copied().collect::<Vec<_>>(), [7, 9]);
    assert_eq!(committed.generation, writer.generation);
    assert_eq!(committed.state_tag, writer.state_tag);
    assert_eq!(committed.minimum_chunk_count, CHUNKS);
}

#[rstest::rstest]
#[case(None)]
#[case(Some("compacted refresh credential"))]
fn a_compacted_file_is_read_from_its_beginning(#[case] key: Option<&str>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("compacted.uqac.sqlite3");
    let mut writer = filled(&path, key);
    let mut reader = warmed(&path, key);

    // Rewriting every chunk twice leaves more stale records than live ones, so the writer
    // replaces the file.
    for round in 2..=3_u8 {
        for chunk in 0..CHUNKS {
            writer
                .write_at(chunk * CHUNK, &pattern(chunk, round))
                .unwrap();
        }
        writer.flush().unwrap();
    }
    assert_eq!(scan_chunk_record_generations(&path).len(), CHUNKS);

    reader.refresh_committed_state().unwrap();
    assert_eq!(reader.cache.len(), 0);
    assert_eq!(reader.generation, writer.generation);
    for chunk in 0..CHUNKS {
        assert_eq!(read(&mut reader, chunk), pattern(chunk, 3), "chunk {chunk}");
    }
    // The handle resumes from the replaced file's own commit afterwards.
    writer.write_at(5 * CHUNK, &pattern(5, 4)).unwrap();
    writer.flush().unwrap();
    reader.refresh_committed_state().unwrap();
    assert_eq!(reader.cache.len(), CHUNKS - 1);
    assert_eq!(read(&mut reader, 5), pattern(5, 4));
}

#[test]
fn a_shrunk_file_loses_the_chunks_beyond_its_end_and_reads_zeros_when_it_grows_again() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("shrunk.uqac.sqlite3");
    let mut writer = filled(&path, None);
    let mut reader = warmed(&path, None);
    let kept = CHUNKS - 4;

    // Four chunks fewer leave far fewer stale records than live ones, so the file is not
    // replaced and the reader resumes.
    writer.truncate_to(kept * CHUNK).unwrap();
    writer.flush().unwrap();
    // The writer grows the file again without writing the chunks in between.
    writer
        .write_at((CHUNKS - 1) * CHUNK, &pattern(CHUNKS - 1, 2))
        .unwrap();
    writer.flush().unwrap();
    assert_eq!(scan_chunk_record_generations(&path).len(), CHUNKS + 1);

    reader.refresh_committed_state().unwrap();
    assert_eq!(reader.logical_len, CHUNKS * CHUNK);
    assert_eq!(reader.cache.len(), kept);
    assert_eq!(reader.chunks.len(), kept + 1);
    assert_eq!(read(&mut reader, kept - 1), pattern(kept - 1, 1));
    assert_eq!(read(&mut reader, kept), vec![0_u8; CHUNK]);
    assert_eq!(read(&mut reader, CHUNKS - 2), vec![0_u8; CHUNK]);
    assert_eq!(read(&mut reader, CHUNKS - 1), pattern(CHUNKS - 1, 2));
}

#[test]
fn a_connection_reads_every_commit_of_another_connection() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("two-connections.sqlite3");
    let key = "two connections credential";
    register_database(&path, SQLiteCompressionOptions::default(), Some(key)).unwrap();
    let open = || {
        rusqlite::Connection::open_with_flags_and_vfs(
            &path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_CREATE,
            VFS_NAME,
        )
        .unwrap()
    };
    let writer = open();
    writer
        .execute_batch(
            "CREATE TABLE rows (id INTEGER PRIMARY KEY, body BLOB NOT NULL);
             CREATE TABLE counter (value INTEGER NOT NULL);
             INSERT INTO counter VALUES (0);",
        )
        .unwrap();
    // Four megabytes of incompressible rows give the container more than a hundred records.
    for id in 0..128 {
        writer
            .execute("INSERT INTO rows VALUES (?1, randomblob(32768))", [id])
            .unwrap();
    }
    let reader = open();
    for value in 1..=100_i64 {
        writer
            .execute("UPDATE counter SET value = ?1", [value])
            .unwrap();
        let observed: i64 = reader
            .query_row("SELECT value FROM counter", [], |row| row.get(0))
            .unwrap();
        assert_eq!(observed, value);
    }
    let rows: i64 = reader
        .query_row("SELECT count(*) FROM rows", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 128);
    let integrity: String = reader
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(integrity, "ok");
}
