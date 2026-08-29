//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

#![allow(unsafe_code)]

//! Schema-neutral compressed `SQLite` VFS.
//!
//! The facade defines the shared container model. Codec policy, on-disk
//! format, container mutation, file callbacks, and VFS registration are
//! implemented in focused modules.
//!
//! Encrypted containers use authenticated format v2. Header fields, complete
//! chunk entries, physical record locations, and commit records are bound to a
//! per-file identity. A valid whole-file snapshot rollback remains outside the
//! format's threat boundary because it requires an external freshness anchor.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Component, Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::{Argon2, Block};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
#[cfg(not(target_os = "emscripten"))]
use fs2::FileExt;
use hmac::{Hmac, Mac};
use rusqlite::ffi;
use sha2::Sha256;

#[cfg(target_os = "emscripten")]
struct FileExt;

#[cfg(target_os = "emscripten")]
impl FileExt {
    fn try_lock_exclusive(_file: &File) -> std::io::Result<()> {
        Ok(())
    }

    fn try_lock_shared(_file: &File) -> std::io::Result<()> {
        Ok(())
    }

    fn unlock(_file: &File) -> std::io::Result<()> {
        Ok(())
    }
}

mod codec;
mod compaction;
mod container;
mod directory_sync;
mod file;
mod format;
mod io_callbacks;
mod options;
mod record_scan;
mod registration;
mod vfs_callbacks;

pub use options::{SQLiteCompressionCodec, SQLiteCompressionOptions};
pub use registration::{register_database, register_database_with_anchor};

use codec::{compress_chunk, decompress_chunk, keys_from_key};
use container::{build_commit_entry, chunk_count_for};
use directory_sync::sync_parent_directory;
use format::{
    allocate_payload, build_entry, build_header, chunk_aad, chunk_payload_tag,
    commit_authentication_tag, fill_random, invalid_data, parse_entry, parse_header, usize_to_u64,
    validate_chunk_entry, validate_commit_entry, verify_commit_authentication,
    verify_header_authentication,
};
use io_callbacks::IO_METHODS;
use record_scan::scan_committed_records;
use registration::{normalize_path, options_for_path};
use vfs_callbacks::{
    vfs_access, vfs_current_time, vfs_delete, vfs_full_pathname, vfs_get_last_error, vfs_open,
    vfs_randomness, vfs_sleep,
};

pub const VFS_NAME: &str = "uqa_compressed";

/// Authenticated identity, generation, and exact committed-state tag for one
/// encrypted compressed container.
///
/// Persist this value outside the database and supply it when reopening to
/// reject a different container or any divergent whole-file snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SQLiteCompressedContainerAnchor {
    pub database_id: [u8; FILE_ID_LEN],
    pub generation: u64,
    /// Authentication tag of the exact committed state at `generation`.
    pub state_tag: [u8; AUTH_TAG_LEN],
}

/// Read and authenticate the current anchor of an encrypted compressed
/// container.
pub fn read_authenticated_anchor(
    path: &Path,
    key: &str,
) -> std::io::Result<SQLiteCompressedContainerAnchor> {
    if key.is_empty() {
        return Err(invalid_data("encryption key must not be empty"));
    }
    let container = ContainerFile::load(path.to_path_buf(), Some(key))?;
    if container.keys.is_none() {
        return Err(invalid_data(
            "compressed container is not encrypted and has no authenticated anchor",
        ));
    }
    Ok(container.authenticated_anchor())
}

const VFS_NAME_C: &[u8] = b"uqa_compressed\0";
pub(crate) const MAGIC: &[u8; 8] = b"UQACDB2\0";
pub(crate) const LEGACY_MAGIC: &[u8; 8] = b"UQACDB1\0";
const VERSION: u32 = 2;
const HEADER_SIZE: usize = 128;
const ENTRY_SIZE: usize = 80;
pub(crate) const FLAG_ENCRYPTED: u32 = 1;
/// Byte offset of the little-endian flags word in the container header.
pub(crate) const HEADER_FLAGS_OFFSET: usize = 12;
const CHUNK_COMPRESSED: u32 = 1;
const CHUNK_ENCRYPTED: u32 = 2;
const CHUNK_COMMIT: u32 = 4;
const CHUNK_AUTHENTICATED: u32 = 8;
const COMMIT_CHUNK_ID: u64 = u64::MAX;
const MIN_COMPACT_STALE_BYTES: u64 = 4 * 1024;
const MAX_COMPACT_STALE_BYTES: u64 = 8 * 1024 * 1024;
const SALT_LEN: usize = 16;
const FILE_ID_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const AEAD_TAG_LEN: usize = 16;
const AUTH_TAG_LEN: usize = 32;
const HEADER_FILE_ID_OFFSET: usize = 80;
const HEADER_AUTH_OFFSET: usize = 96;
const DEFAULT_PAGE_SIZE: u32 = 4096;
const DEFAULT_CHUNK_PAGES: u32 = 8;
const DEFAULT_LEVEL: i32 = 3;
const SQLITE_LOCK_NONE: c_int = 0;
const SQLITE_LOCK_SHARED: c_int = 1;
const SQLITE_LOCK_RESERVED: c_int = 2;

#[derive(Debug, Clone)]
struct OpenOptionsEntry {
    compression: SQLiteCompressionOptions,
    key: Option<String>,
    trusted_anchor: Option<SQLiteCompressedContainerAnchor>,
}

#[derive(Debug)]
struct Header {
    flags: u32,
    compression: SQLiteCompressionOptions,
    chunk_count: usize,
    logical_len: usize,
    generation: u64,
    salt: [u8; SALT_LEN],
    file_id: [u8; FILE_ID_LEN],
    auth_tag: [u8; AUTH_TAG_LEN],
}

#[derive(Debug, Clone, Copy)]
struct HeaderMetadata {
    flags: u32,
    compression: SQLiteCompressionOptions,
    chunk_count: usize,
    logical_len: usize,
    generation: u64,
    salt: [u8; SALT_LEN],
    file_id: [u8; FILE_ID_LEN],
}

type HmacSha256 = Hmac<Sha256>;

struct ContainerKeys {
    cipher: XChaCha20Poly1305,
    mac_key: [u8; 32],
}

impl std::fmt::Debug for ContainerKeys {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ContainerKeys(<redacted>)")
    }
}

#[derive(Debug, Clone)]
struct ChunkEntry {
    chunk_id: u64,
    offset: u64,
    stored_len: usize,
    raw_len: usize,
    flags: u32,
    crc32: u32,
    nonce: [u8; NONCE_LEN],
    generation: u64,
    allocated_len: usize,
}

#[derive(Debug, Clone)]
struct AuthenticatedChunkRecord {
    entry: ChunkEntry,
    payload_tag: [u8; AEAD_TAG_LEN],
}

#[derive(Debug)]
struct ContainerFile {
    path: PathBuf,
    logical_len: usize,
    append_offset: u64,
    chunks: BTreeMap<u64, ChunkEntry>,
    cache: BTreeMap<u64, Vec<u8>>,
    dirty_chunks: BTreeSet<u64>,
    compression: SQLiteCompressionOptions,
    keys: Option<ContainerKeys>,
    salt: [u8; SALT_LEN],
    file_id: [u8; FILE_ID_LEN],
    generation: u64,
    state_tag: [u8; AUTH_TAG_LEN],
    committed_file_len: u64,
    dirty_header: bool,
}

#[repr(C)]
struct CompressedSQLiteFile {
    base: ffi::sqlite3_file,
    handle: *mut FileHandle,
}

struct FileHandle {
    file: VfsFile,
    lock_file: File,
    read_only: bool,
    delete_on_close: bool,
    lock_state: c_int,
}

#[derive(Debug)]
enum VfsFile {
    Compressed(Box<ContainerFile>),
    Plain(PlainFile),
}

#[derive(Debug)]
struct PlainFile {
    path: PathBuf,
    file: File,
}

static REGISTRY: OnceLock<Mutex<BTreeMap<String, OpenOptionsEntry>>> = OnceLock::new();
static VFS_REGISTERED: OnceLock<std::result::Result<(), c_int>> = OnceLock::new();

#[cfg(test)]
mod tests;
