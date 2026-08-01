//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

use super::*;

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
fn sqlite_journal_files_are_not_compressed_containers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("main.uqac.sqlite3-journal");
    let options = OpenOptionsEntry {
        compression: SQLiteCompressionOptions::default(),
        key: None,
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
    };
    let flags =
        ffi::SQLITE_OPEN_MAIN_JOURNAL | ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_CREATE;
    let file = VfsFile::open(path, options, flags, false).unwrap();
    assert!(matches!(file, VfsFile::Compressed(_)));
}

fn scan_chunk_record_generations(path: &Path) -> Vec<u64> {
    let mut file = File::open(path).unwrap();
    let mut header_bytes = [0_u8; HEADER_SIZE];
    file.read_exact(&mut header_bytes).unwrap();
    parse_header(&header_bytes).unwrap();
    let file_len = file.metadata().unwrap().len();
    let mut offset = HEADER_SIZE as u64;
    let mut generations = Vec::new();
    while offset + ENTRY_SIZE as u64 <= file_len {
        file.seek(SeekFrom::Start(offset)).unwrap();
        let mut entry_bytes = [0_u8; ENTRY_SIZE];
        file.read_exact(&mut entry_bytes).unwrap();
        let entry = parse_entry(&entry_bytes).unwrap();
        if entry.flags & CHUNK_COMMIT != 0 {
            offset += ENTRY_SIZE as u64;
            continue;
        }
        let payload_offset = offset + ENTRY_SIZE as u64;
        let payload_end = payload_offset + entry.allocated_len as u64;
        if payload_end > file_len {
            break;
        }
        generations.push(entry.generation);
        offset = payload_end;
    }
    generations
}
