//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! On-disk database format detection.
//!
//! `usql` and embedding applications need to decide which `Engine`
//! open variant fits an existing file before they have a key in hand:
//! plaintext `SQLite` and UQA compressed containers carry cleartext
//! magic bytes, while `SQLCipher` encrypts the whole file (including the
//! `SQLite` header), so an encrypted catalog is indistinguishable from a
//! non-database file without attempting a keyed open.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::sqlite::compressed_vfs::{FLAG_ENCRYPTED, HEADER_FLAGS_OFFSET, MAGIC};

/// First 16 bytes of every plaintext `SQLite` database file.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Number of leading bytes required to classify a file: the `SQLite`
/// magic is 16 bytes and the compressed-container flags word ends at
/// byte 16 as well.
const DETECT_PREFIX_LEN: usize = 16;

/// On-disk format of a database file, detected from its header bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DatabaseFileFormat {
    /// The file does not exist or is empty. Opening it creates a new
    /// database.
    Missing,
    /// A plaintext `SQLite` database (`SQLite format 3\0` magic).
    PlainSQLite,
    /// A UQA compressed container (`UQACDB1\0` magic). `encrypted`
    /// reflects the header flag: when set, opening requires a key.
    CompressedContainer { encrypted: bool },
    /// No recognizable magic. Either a `SQLCipher`-encrypted database or
    /// not a database at all; the two cannot be told apart without
    /// attempting an open with a key.
    Unrecognized,
}

impl DatabaseFileFormat {
    /// Whether opening this file is known to require an encryption
    /// key. `Unrecognized` returns `true` because the dominant cause
    /// for an unrecognized header on a database path is `SQLCipher`
    /// encryption.
    #[must_use]
    pub fn requires_key(self) -> bool {
        matches!(
            self,
            Self::CompressedContainer { encrypted: true } | Self::Unrecognized
        )
    }
}

/// Classify the on-disk format of `path` by reading its first bytes.
///
/// Only returns `Err` for I/O failures other than "file not found";
/// a missing or empty file is reported as [`DatabaseFileFormat::Missing`].
pub fn detect_database_file_format(path: &Path) -> std::io::Result<DatabaseFileFormat> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(DatabaseFileFormat::Missing);
        }
        Err(err) => return Err(err),
    };
    let mut prefix = [0_u8; DETECT_PREFIX_LEN];
    let mut read = 0;
    while read < prefix.len() {
        let n = file.read(&mut prefix[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    if read == 0 {
        return Ok(DatabaseFileFormat::Missing);
    }
    if read < DETECT_PREFIX_LEN {
        // Too short for any valid database header.
        return Ok(DatabaseFileFormat::Unrecognized);
    }
    if &prefix == SQLITE_MAGIC {
        return Ok(DatabaseFileFormat::PlainSQLite);
    }
    if &prefix[..MAGIC.len()] == MAGIC {
        let flags = u32::from_le_bytes([
            prefix[HEADER_FLAGS_OFFSET],
            prefix[HEADER_FLAGS_OFFSET + 1],
            prefix[HEADER_FLAGS_OFFSET + 2],
            prefix[HEADER_FLAGS_OFFSET + 3],
        ]);
        return Ok(DatabaseFileFormat::CompressedContainer {
            encrypted: flags & FLAG_ENCRYPTED != 0,
        });
    }
    Ok(DatabaseFileFormat::Unrecognized)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::sqlite::compressed_vfs::SQLiteCompressionOptions;
    use crate::sqlite::connection::ManagedConnection;

    fn temp_dir() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    #[test]
    fn missing_file_detects_as_missing() {
        let dir = temp_dir();
        let path = dir.path().join("absent.db");
        assert_eq!(
            detect_database_file_format(&path).unwrap(),
            DatabaseFileFormat::Missing
        );
    }

    #[test]
    fn empty_file_detects_as_missing() {
        let dir = temp_dir();
        let path = dir.path().join("empty.db");
        std::fs::write(&path, b"").unwrap();
        assert_eq!(
            detect_database_file_format(&path).unwrap(),
            DatabaseFileFormat::Missing
        );
    }

    #[test]
    fn plaintext_sqlite_detects_as_plain() {
        let dir = temp_dir();
        let path = dir.path().join("plain.db");
        {
            let conn = ManagedConnection::open(&path).unwrap();
            conn.with(|c| Ok(c.execute_batch("CREATE TABLE t (id INTEGER)")?))
                .unwrap();
        }
        assert_eq!(
            detect_database_file_format(&path).unwrap(),
            DatabaseFileFormat::PlainSQLite
        );
    }

    #[test]
    fn sqlcipher_database_detects_as_unrecognized() {
        let dir = temp_dir();
        let path = dir.path().join("cipher.db");
        {
            let conn = ManagedConnection::open_encrypted(&path, "secret").unwrap();
            conn.with(|c| Ok(c.execute_batch("CREATE TABLE t (id INTEGER)")?))
                .unwrap();
        }
        assert_eq!(
            detect_database_file_format(&path).unwrap(),
            DatabaseFileFormat::Unrecognized
        );
    }

    #[test]
    fn compressed_container_detects_with_encryption_flag() {
        let dir = temp_dir();
        let plain = dir.path().join("container.db");
        {
            let conn =
                ManagedConnection::open_compressed(&plain, SQLiteCompressionOptions::default())
                    .unwrap();
            conn.with(|c| Ok(c.execute_batch("CREATE TABLE t (id INTEGER)")?))
                .unwrap();
        }
        assert_eq!(
            detect_database_file_format(&plain).unwrap(),
            DatabaseFileFormat::CompressedContainer { encrypted: false }
        );

        let encrypted = dir.path().join("container-enc.db");
        {
            let conn = ManagedConnection::open_compressed_encrypted(
                &encrypted,
                "secret",
                SQLiteCompressionOptions::default(),
            )
            .unwrap();
            conn.with(|c| Ok(c.execute_batch("CREATE TABLE t (id INTEGER)")?))
                .unwrap();
        }
        assert_eq!(
            detect_database_file_format(&encrypted).unwrap(),
            DatabaseFileFormat::CompressedContainer { encrypted: true }
        );
    }

    #[test]
    fn short_or_foreign_files_detect_as_unrecognized() {
        let dir = temp_dir();
        let short = dir.path().join("short.bin");
        std::fs::write(&short, b"abc").unwrap();
        assert_eq!(
            detect_database_file_format(&short).unwrap(),
            DatabaseFileFormat::Unrecognized
        );

        let foreign = dir.path().join("foreign.bin");
        std::fs::write(&foreign, vec![0xAB_u8; 64]).unwrap();
        assert_eq!(
            detect_database_file_format(&foreign).unwrap(),
            DatabaseFileFormat::Unrecognized
        );
    }

    #[test]
    fn requires_key_reflects_format() {
        assert!(!DatabaseFileFormat::Missing.requires_key());
        assert!(!DatabaseFileFormat::PlainSQLite.requires_key());
        assert!(!DatabaseFileFormat::CompressedContainer { encrypted: false }.requires_key());
        assert!(DatabaseFileFormat::CompressedContainer { encrypted: true }.requires_key());
        assert!(DatabaseFileFormat::Unrecognized.requires_key());
    }
}
