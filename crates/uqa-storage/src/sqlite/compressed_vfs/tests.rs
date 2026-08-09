//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

#[test]
fn vfs_delete_with_dir_sync_removes_real_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("delete-with-dir-sync.sqlite3");
    std::fs::write(&path, b"content").unwrap();
    let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();

    // SAFETY: `name` is a valid NUL-terminated path for the duration of the call.
    let result = unsafe { vfs_delete(ptr::null_mut(), name.as_ptr(), 1) };

    assert_eq!(result, ffi::SQLITE_OK);
    assert!(!path.exists());
}

#[test]
fn vfs_delete_nonexistent_file_with_dir_sync_returns_ok() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing-with-dir-sync.sqlite3");
    let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();

    // SAFETY: `name` is a valid NUL-terminated path for the duration of the call.
    let result = unsafe { vfs_delete(ptr::null_mut(), name.as_ptr(), 1) };

    assert_eq!(result, ffi::SQLITE_OK);
}

#[test]
fn parent_dir_sync_succeeds_for_real_temp_directory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("directory-entry");

    sync_parent_directory(&path).unwrap();
}

#[cfg(unix)]
#[test]
fn parent_dir_sync_surfaces_nonexistent_parent_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("missing-parent").join("directory-entry");

    assert_eq!(
        sync_parent_directory(&path).unwrap_err().kind(),
        std::io::ErrorKind::NotFound
    );
}

#[test]
fn options_validate_rejects_bad_chunk_size() {
    assert!(SQLiteCompressionOptions {
        codec: SQLiteCompressionCodec::Zstd,
        page_size: 1000,
        chunk_pages: 1,
        level: 3,
    }
    .validate()
    .is_err());
    assert!(SQLiteCompressionOptions {
        codec: SQLiteCompressionCodec::Zstd,
        page_size: 4096,
        chunk_pages: 512,
        level: 3,
    }
    .validate()
    .is_err());
    assert!(SQLiteCompressionOptions {
        codec: SQLiteCompressionCodec::Zstd,
        page_size: u32::MAX,
        chunk_pages: u32::MAX,
        level: 3,
    }
    .chunk_size()
    .is_err());
}

#[test]
fn legacy_v1_container_is_rejected_by_the_open_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("legacy-v1.uqac.sqlite3");
    let mut header = [0_u8; HEADER_SIZE];
    header[..LEGACY_MAGIC.len()].copy_from_slice(LEGACY_MAGIC);
    std::fs::write(&path, header).unwrap();

    let error = ContainerFile::open(
        path,
        OpenOptionsEntry {
            compression: SQLiteCompressionOptions::default(),
            key: Some("legacy-key".to_string()),
            trusted_anchor: None,
        },
    )
    .unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error
        .to_string()
        .contains("version 1 has no authenticated metadata"));
}

#[test]
fn persisted_oversized_stored_length_is_rejected_before_allocation() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("oversized-stored-length.uqac.sqlite3");
    let compression = SQLiteCompressionOptions {
        codec: SQLiteCompressionCodec::Zstd,
        page_size: 512,
        chunk_pages: 1,
        level: 1,
    };
    let options = OpenOptionsEntry {
        compression,
        key: None,
        trusted_anchor: None,
    };
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    file.seek(SeekFrom::Start((HEADER_SIZE + 16) as u64))
        .unwrap();
    file.write_all(&u64::MAX.to_le_bytes()).unwrap();
    file.flush().unwrap();
    drop(file);

    let Err(error) = ContainerFile::open(path, options) else {
        panic!("corrupt stored length unexpectedly opened");
    };
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    assert!(error.to_string().contains("compressed chunk payload"));
}

#[test]
fn decompression_honors_the_persisted_raw_length_bound() {
    let oversized_lz4 = [u32::MAX.to_le_bytes().as_slice(), &[0_u8; 8]].concat();
    let lz4_error = decompress_chunk(SQLiteCompressionCodec::LZ4, &oversized_lz4, 512).unwrap_err();
    assert!(lz4_error.to_string().contains("decoded length mismatch"));

    let oversized_raw = vec![b'z'; 8 * 1024];
    let oversized_zstd = zstd::stream::encode_all(oversized_raw.as_slice(), 1).unwrap();
    let zstd_error =
        decompress_chunk(SQLiteCompressionCodec::Zstd, &oversized_zstd, 512).unwrap_err();
    assert!(zstd_error.to_string().contains("decoded length mismatch"));
}

#[test]
fn flush_appends_only_dirty_chunk_records() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("incremental.uqac.sqlite3");
    let compression = SQLiteCompressionOptions {
        codec: SQLiteCompressionCodec::Zstd,
        page_size: 512,
        chunk_pages: 1,
        level: 1,
    };
    let options = OpenOptionsEntry {
        compression,
        key: None,
        trusted_anchor: None,
    };
    let chunk_size = compression.chunk_size().unwrap();

    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &vec![b'a'; chunk_size * 16]).unwrap();
    container.flush().unwrap();
    let first_len = std::fs::metadata(&path).unwrap().len();
    let first_records = scan_chunk_record_generations(&path);
    assert_eq!(first_records.len(), 16);
    assert!(first_records.iter().all(|generation| *generation == 1));

    let update_offset = chunk_size * 3 + 7;
    container.write_at(update_offset, b"xyz").unwrap();
    container.flush().unwrap();
    let second_len = std::fs::metadata(&path).unwrap().len();
    let second_records = scan_chunk_record_generations(&path);
    assert_eq!(second_records.len(), 17);
    assert_eq!(
        second_records
            .iter()
            .filter(|generation| **generation == 2)
            .count(),
        1
    );
    assert!(second_len - first_len < first_len / 4);

    let mut reopened = ContainerFile::open(path, options).unwrap();
    let mut out = [0_u8; 3];
    assert_eq!(reopened.read_at(update_offset, &mut out).unwrap(), 3);
    assert_eq!(&out, b"xyz");
}

#[test]
fn repeated_autocommit_updates_trigger_stale_record_compaction() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("autocommit.uqac.sqlite3");
    let compression = SQLiteCompressionOptions {
        codec: SQLiteCompressionCodec::Zstd,
        page_size: 512,
        chunk_pages: 1,
        level: 1,
    };
    let options = OpenOptionsEntry {
        compression,
        key: None,
        trusted_anchor: None,
    };

    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container
        .write_at(0, &vec![b'a'; compression.chunk_size().unwrap()])
        .unwrap();
    container.flush().unwrap();
    for i in 0..300_u16 {
        container.write_at(17, &i.to_le_bytes()).unwrap();
        container.flush().unwrap();
    }

    let file_len = std::fs::metadata(&path).unwrap().len();
    assert!(file_len < 16 * 1024);
    assert!(scan_chunk_record_generations(&path).len() < 80);

    let mut reopened = ContainerFile::open(path, options).unwrap();
    let mut out = [0_u8; 2];
    assert_eq!(reopened.read_at(17, &mut out).unwrap(), 2);
    assert_eq!(out, 299_u16.to_le_bytes());
}

#[test]
fn encrypted_compaction_reauthenticates_relocated_chunks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("encrypted-compaction.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    for value in 0..100_u16 {
        container.write_at(17, &value.to_le_bytes()).unwrap();
        container.flush().unwrap();
    }
    drop(container);

    assert!(scan_records(&path).len() < 80);
    let mut reopened = ContainerFile::open(path, options).unwrap();
    let mut out = [0_u8; 2];
    reopened.read_at(17, &mut out).unwrap();
    assert_eq!(out, 99_u16.to_le_bytes());
}

#[test]
fn sqlite_journal_files_are_not_compressed_containers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.uqac.sqlite3-journal");
    let options = OpenOptionsEntry {
        compression: SQLiteCompressionOptions::default(),
        key: None,
        trusted_anchor: None,
    };
    let flags =
        ffi::SQLITE_OPEN_MAIN_JOURNAL | ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE;
    let file = VfsFile::open(path, options, flags, false).unwrap();
    assert!(matches!(file, VfsFile::Plain(_)));
}

#[test]
fn encrypted_sqlite_journal_files_stay_encrypted_containers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main-encrypted.uqac.sqlite3-journal");
    let options = OpenOptionsEntry {
        compression: SQLiteCompressionOptions::default(),
        key: Some("correct horse battery staple".to_string()),
        trusted_anchor: None,
    };
    let flags =
        ffi::SQLITE_OPEN_MAIN_JOURNAL | ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE;
    let file = VfsFile::open(path, options, flags, false).unwrap();
    assert!(matches!(file, VfsFile::Compressed(_)));
}

#[test]
fn encrypted_header_metadata_tamper_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("header-tamper.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    flip_file_byte(&path, 52);
    let error = ContainerFile::open(path, options).unwrap_err();
    assert!(error.to_string().contains("header authentication failed"));
}

#[test]
fn encrypted_chunk_metadata_tamper_is_rejected_during_commit_scan() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chunk-metadata-tamper.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    flip_file_byte(&path, (HEADER_SIZE + 36) as u64);
    let error = ContainerFile::open(path, options).unwrap_err();
    assert!(error.to_string().contains("commit authentication failed"));
}

#[test]
fn encrypted_commit_metadata_tamper_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("commit-tamper.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    let commit = scan_records(&path)
        .into_iter()
        .find(|record| record.entry.flags & CHUNK_COMMIT != 0)
        .unwrap();
    flip_file_byte(&path, commit.start + 8);
    let error = ContainerFile::open(path, options).unwrap_err();
    assert!(error.to_string().contains("commit authentication failed"));
}

#[test]
fn appended_old_generation_chunk_replay_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uncommitted-replay.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    let first_chunk = scan_records(&path)
        .into_iter()
        .find(|record| record.entry.flags & CHUNK_COMMIT == 0)
        .unwrap();
    let mut replay = read_file_range(&path, first_chunk.start, first_chunk.end);

    container.write_at(0, &[b'b'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    let append_at = std::fs::metadata(&path).unwrap().len();
    write_u64(&mut replay, 8, append_at + ENTRY_SIZE as u64);
    append_file_bytes(&path, &replay);

    let error = ContainerFile::open(path, options).unwrap_err();
    assert!(error.to_string().contains("already committed"));
}

#[test]
fn uncommitted_next_generation_tail_is_not_applied() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("uncommitted-tail.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    let chunk = scan_records(&path)
        .into_iter()
        .find(|record| record.entry.flags & CHUNK_COMMIT == 0)
        .unwrap();
    let mut tail = read_file_range(&path, chunk.start, chunk.end);
    let append_at = std::fs::metadata(&path).unwrap().len();
    write_u64(&mut tail, 8, append_at + ENTRY_SIZE as u64);
    write_u64(&mut tail, 64, 2);
    append_file_bytes(&path, &tail);

    let mut reopened = ContainerFile::open(path, options).unwrap();
    let mut value = [0_u8; 1];
    reopened.read_at(0, &mut value).unwrap();
    assert_eq!(value, [b'a']);
}

#[test]
fn replayed_chunk_and_forged_commit_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("committed-replay.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    let first_records = scan_records(&path);
    let first_chunk = first_records
        .iter()
        .find(|record| record.entry.flags & CHUNK_COMMIT == 0)
        .unwrap();
    let first_commit = first_records
        .iter()
        .find(|record| record.entry.flags & CHUNK_COMMIT != 0)
        .unwrap();
    let mut replayed_chunk = read_file_range(&path, first_chunk.start, first_chunk.end);
    let mut forged_commit = read_file_range(&path, first_commit.start, first_commit.end);

    container.write_at(0, &[b'b'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    let append_at = std::fs::metadata(&path).unwrap().len();
    write_u64(&mut replayed_chunk, 8, append_at + ENTRY_SIZE as u64);
    write_u64(&mut replayed_chunk, 64, 3);
    write_u64(&mut forged_commit, 64, 3);
    append_file_bytes(&path, &replayed_chunk);
    append_file_bytes(&path, &forged_commit);

    let error = ContainerFile::open(path, options).unwrap_err();
    assert!(error.to_string().contains("commit authentication failed"));
}

#[test]
fn commit_chain_rejects_same_file_fork_splicing() {
    let dir = tempfile::tempdir().unwrap();
    let original_path = dir.path().join("chain-original.uqac.sqlite3");
    let fork_path = dir.path().join("chain-fork.uqac.sqlite3");
    let options = encrypted_options();

    let mut base = ContainerFile::open(original_path.clone(), options.clone()).unwrap();
    base.write_at(0, &[b'a'; 512]).unwrap();
    base.flush().unwrap();
    drop(base);
    std::fs::copy(&original_path, &fork_path).unwrap();

    let mut original = ContainerFile::open(original_path.clone(), options.clone()).unwrap();
    original.write_at(0, &[b'b'; 512]).unwrap();
    original.flush().unwrap();
    let mut fork = ContainerFile::open(fork_path.clone(), options.clone()).unwrap();
    fork.write_at(0, &[b'c'; 512]).unwrap();
    fork.flush().unwrap();
    drop(fork);

    original.write_at(0, &[b'd'; 512]).unwrap();
    original.flush().unwrap();
    drop(original);

    let original_generation = generation_span(&original_path, 2);
    let fork_generation = generation_span(&fork_path, 2);
    assert_eq!(
        original_generation.end - original_generation.start,
        fork_generation.end - fork_generation.start
    );
    let fork_bytes = read_file_range(&fork_path, fork_generation.start, fork_generation.end);
    overwrite_file_range(&original_path, original_generation.start, &fork_bytes);

    let error = ContainerFile::open(original_path, options).unwrap_err();
    assert!(error.to_string().contains("commit authentication failed"));
}

#[test]
fn trusted_anchor_rejects_a_divergent_same_generation_fork() {
    let dir = tempfile::tempdir().unwrap();
    let original_path = dir.path().join("anchor-original.uqac.sqlite3");
    let fork_path = dir.path().join("anchor-fork.uqac.sqlite3");
    let options = encrypted_options();

    let mut base = ContainerFile::open(original_path.clone(), options.clone()).unwrap();
    base.write_at(0, &[b'a'; 512]).unwrap();
    base.flush().unwrap();
    drop(base);
    std::fs::copy(&original_path, &fork_path).unwrap();

    let mut original = ContainerFile::open(original_path, options.clone()).unwrap();
    original.write_at(0, &[b'b'; 512]).unwrap();
    original.flush().unwrap();
    let trusted = original.authenticated_anchor();

    let mut fork = ContainerFile::open(fork_path, options).unwrap();
    fork.write_at(0, &[b'c'; 512]).unwrap();
    fork.flush().unwrap();
    let divergent = fork.authenticated_anchor();
    assert_eq!(trusted.database_id, divergent.database_id);
    assert_eq!(trusted.generation, divergent.generation);
    assert_ne!(trusted.state_tag, divergent.state_tag);
    let error = fork.require_trusted_anchor(trusted).unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match the trusted anchor"));
}

#[test]
fn encrypted_chunk_cannot_be_replayed_across_files() {
    let dir = tempfile::tempdir().unwrap();
    let first_path = dir.path().join("first.uqac.sqlite3");
    let second_path = dir.path().join("second.uqac.sqlite3");
    let options = encrypted_options();
    for (path, byte) in [(&first_path, b'a'), (&second_path, b'b')] {
        let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
        container.write_at(0, &vec![byte; 512]).unwrap();
        container.flush().unwrap();
    }
    let first_chunk = scan_records(&first_path)
        .into_iter()
        .find(|record| record.entry.flags & CHUNK_COMMIT == 0)
        .unwrap();
    let second_chunk = scan_records(&second_path)
        .into_iter()
        .find(|record| record.entry.flags & CHUNK_COMMIT == 0)
        .unwrap();
    assert_eq!(
        first_chunk.end - first_chunk.start,
        second_chunk.end - second_chunk.start
    );
    let replay = read_file_range(&first_path, first_chunk.start, first_chunk.end);
    overwrite_file_range(&second_path, second_chunk.start, &replay);

    let error = ContainerFile::open(second_path, options).unwrap_err();
    assert!(error.to_string().contains("commit authentication failed"));
}

#[test]
fn truncation_before_authenticated_header_generation_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("truncated-generation.uqac.sqlite3");
    let options = encrypted_options();
    let mut container = ContainerFile::open(path.clone(), options.clone()).unwrap();
    container.write_at(0, &[b'a'; 512]).unwrap();
    container.flush().unwrap();
    let first_generation_len = std::fs::metadata(&path).unwrap().len();
    container.write_at(0, &[b'b'; 512]).unwrap();
    container.flush().unwrap();
    drop(container);

    OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(first_generation_len)
        .unwrap();
    let error = ContainerFile::open(path, options).unwrap_err();
    assert!(error.to_string().contains("truncated or rolled back"));
}

#[derive(Clone)]
struct RecordSpan {
    start: u64,
    end: u64,
    entry: ChunkEntry,
}

fn encrypted_options() -> OpenOptionsEntry {
    OpenOptionsEntry {
        compression: SQLiteCompressionOptions {
            codec: SQLiteCompressionCodec::Zstd,
            page_size: 512,
            chunk_pages: 1,
            level: 1,
        },
        key: Some("correct horse battery staple".to_string()),
        trusted_anchor: None,
    }
}

fn scan_records(path: &Path) -> Vec<RecordSpan> {
    let mut file = File::open(path).unwrap();
    let mut header_bytes = [0_u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes).unwrap();
    parse_header(&header_bytes).unwrap();
    let file_len = file.metadata().unwrap().len();
    let mut offset = HEADER_SIZE as u64;
    let mut records = Vec::new();
    while offset + ENTRY_SIZE as u64 <= file_len {
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut entry_bytes = [0_u8; ENTRY_SIZE];
        file.read_exact(&mut entry_bytes).unwrap();
        let entry = parse_entry(&entry_bytes).unwrap();
        let end = offset + ENTRY_SIZE as u64 + entry.allocated_len as u64;
        if end > file_len {
            break;
        }
        records.push(RecordSpan {
            start: offset,
            end,
            entry,
        });
        offset = end;
    }
    records
}

fn generation_span(path: &Path, generation: u64) -> RecordSpan {
    let records = scan_records(path)
        .into_iter()
        .filter(|record| record.entry.generation == generation)
        .collect::<Vec<_>>();
    RecordSpan {
        start: records.first().unwrap().start,
        end: records.last().unwrap().end,
        entry: records.last().unwrap().entry.clone(),
    }
}

fn read_file_range(path: &Path, start: u64, end: u64) -> Vec<u8> {
    let mut file = File::open(path).unwrap();
    file.seek(SeekFrom::Start(start)).unwrap();
    let mut bytes = vec![0_u8; usize::try_from(end - start).unwrap()];
    file.read_exact(&mut bytes).unwrap();
    bytes
}

fn overwrite_file_range(path: &Path, offset: u64, bytes: &[u8]) {
    let mut file = OpenOptions::new().write(true).open(path).unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn append_file_bytes(path: &Path, bytes: &[u8]) {
    let mut file = OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn flip_file_byte(path: &Path, offset: u64) {
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .unwrap();
    file.seek(SeekFrom::Start(offset)).unwrap();
    let mut byte = [0_u8; 1];
    file.read_exact(&mut byte).unwrap();
    byte[0] ^= 1;
    file.seek(SeekFrom::Start(offset)).unwrap();
    file.write_all(&byte).unwrap();
    file.sync_all().unwrap();
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn scan_chunk_record_generations(path: &Path) -> Vec<u64> {
    scan_records(path)
        .into_iter()
        .filter(|record| record.entry.flags & CHUNK_COMMIT == 0)
        .map(|record| record.entry.generation)
        .collect()
}
