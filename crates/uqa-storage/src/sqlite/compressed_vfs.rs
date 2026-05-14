//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

#![allow(unsafe_code)]

//! Schema-neutral compressed `SQLite` VFS.
//!
//! The VFS exposes a normal byte-addressed `SQLite` database file to the
//! pager. The on-disk file is a UQA container made of independently compressed
//! chunk records. Dirty chunks are appended with commit records on sync; active
//! records are compacted before stale autocommit history can dominate reopen
//! time or file size.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::CStr;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Component, Path, PathBuf};
use std::ptr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::Argon2;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
use fs2::FileExt;
use rand_core::{OsRng, RngCore};
use rusqlite::ffi;

pub const VFS_NAME: &str = "uqa_compressed";

const VFS_NAME_C: &[u8] = b"uqa_compressed\0";
const MAGIC: &[u8; 8] = b"UQACDB1\0";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 128;
const ENTRY_SIZE: usize = 80;
const FLAG_ENCRYPTED: u32 = 1;
const CHUNK_COMPRESSED: u32 = 1;
const CHUNK_ENCRYPTED: u32 = 2;
const CHUNK_COMMIT: u32 = 4;
const COMMIT_CHUNK_ID: u64 = u64::MAX;
const MIN_COMPACT_STALE_BYTES: u64 = 4 * 1024;
const MAX_COMPACT_STALE_BYTES: u64 = 8 * 1024 * 1024;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const DEFAULT_PAGE_SIZE: u32 = 4096;
const DEFAULT_CHUNK_PAGES: u32 = 8;
const DEFAULT_LEVEL: i32 = 3;
const SQLITE_LOCK_NONE: c_int = 0;
const SQLITE_LOCK_SHARED: c_int = 1;
const SQLITE_LOCK_RESERVED: c_int = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SQLiteCompressionCodec {
    #[default]
    Zstd,
    LZ4,
}

impl SQLiteCompressionCodec {
    const fn id(self) -> u32 {
        match self {
            Self::Zstd => 1,
            Self::LZ4 => 2,
        }
    }

    fn from_id(id: u32) -> Result<Self, String> {
        match id {
            0 | 1 => Ok(Self::Zstd),
            2 => Ok(Self::LZ4),
            _ => Err(format!("unsupported compression codec id {id}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SQLiteCompressionOptions {
    pub codec: SQLiteCompressionCodec,
    pub page_size: u32,
    pub chunk_pages: u32,
    pub level: i32,
}

impl Default for SQLiteCompressionOptions {
    fn default() -> Self {
        Self {
            codec: SQLiteCompressionCodec::default(),
            page_size: DEFAULT_PAGE_SIZE,
            chunk_pages: DEFAULT_CHUNK_PAGES,
            level: DEFAULT_LEVEL,
        }
    }
}

impl SQLiteCompressionOptions {
    pub fn zstd() -> Self {
        Self {
            codec: SQLiteCompressionCodec::Zstd,
            ..Self::default()
        }
    }

    pub fn lz4() -> Self {
        Self {
            codec: SQLiteCompressionCodec::LZ4,
            level: 0,
            ..Self::default()
        }
    }

    pub fn validate(self) -> Result<Self, String> {
        if self.page_size == 0 || !self.page_size.is_power_of_two() {
            return Err("page_size must be a non-zero power of two".to_string());
        }
        if !(512..=65_536).contains(&self.page_size) {
            return Err("page_size must be between 512 and 65536 bytes".to_string());
        }
        if self.chunk_pages == 0 {
            return Err("chunk_pages must be non-zero".to_string());
        }
        let chunk_size = u64::from(self.page_size) * u64::from(self.chunk_pages);
        if !(u64::from(self.page_size)..=1_048_576).contains(&chunk_size) {
            return Err("chunk size must be at most 1 MiB".to_string());
        }
        if self.codec == SQLiteCompressionCodec::Zstd && !(-7..=22).contains(&self.level) {
            return Err("zstd level must be between -7 and 22".to_string());
        }
        Ok(self)
    }

    pub fn chunk_size(self) -> usize {
        (self.page_size as usize) * (self.chunk_pages as usize)
    }
}

#[derive(Debug, Clone)]
struct OpenOptionsEntry {
    compression: SQLiteCompressionOptions,
    key: Option<String>,
}

#[derive(Debug)]
struct Header {
    flags: u32,
    compression: SQLiteCompressionOptions,
    chunk_count: usize,
    logical_len: usize,
    generation: u64,
    salt: [u8; SALT_LEN],
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

#[derive(Debug)]
struct ContainerFile {
    path: PathBuf,
    logical_len: usize,
    append_offset: u64,
    chunks: BTreeMap<u64, ChunkEntry>,
    cache: BTreeMap<u64, Vec<u8>>,
    dirty_chunks: BTreeSet<u64>,
    compression: SQLiteCompressionOptions,
    key: Option<String>,
    salt: [u8; SALT_LEN],
    generation: u64,
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
    Compressed(ContainerFile),
    Plain(PlainFile),
}

#[derive(Debug)]
struct PlainFile {
    path: PathBuf,
    file: File,
}

static REGISTRY: OnceLock<Mutex<BTreeMap<String, OpenOptionsEntry>>> = OnceLock::new();
static VFS_REGISTERED: OnceLock<std::result::Result<(), c_int>> = OnceLock::new();

pub fn register_database(
    path: &Path,
    compression: SQLiteCompressionOptions,
    key: Option<&str>,
) -> Result<(), String> {
    let compression = compression.validate()?;
    ensure_registered().map_err(|code| format!("sqlite3_vfs_register failed with code {code}"))?;
    let entry = OpenOptionsEntry {
        compression,
        key: key.map(str::to_string),
    };
    let mut registry = registry().lock().map_err(|_| "vfs registry poisoned")?;
    registry.insert(normalize_path(path), entry);
    Ok(())
}

fn ensure_registered() -> std::result::Result<(), c_int> {
    VFS_REGISTERED
        .get_or_init(|| {
            let vfs = Box::new(ffi::sqlite3_vfs {
                iVersion: 1,
                szOsFile: std::mem::size_of::<CompressedSQLiteFile>() as c_int,
                mxPathname: 4096,
                pNext: ptr::null_mut(),
                zName: VFS_NAME_C.as_ptr().cast::<c_char>(),
                pAppData: ptr::null_mut(),
                xOpen: Some(vfs_open),
                xDelete: Some(vfs_delete),
                xAccess: Some(vfs_access),
                xFullPathname: Some(vfs_full_pathname),
                xDlOpen: None,
                xDlError: None,
                xDlSym: None,
                xDlClose: None,
                xRandomness: Some(vfs_randomness),
                xSleep: Some(vfs_sleep),
                xCurrentTime: Some(vfs_current_time),
                xGetLastError: Some(vfs_get_last_error),
                xCurrentTimeInt64: None,
                xSetSystemCall: None,
                xGetSystemCall: None,
                xNextSystemCall: None,
            });
            let leaked = Box::leak(vfs);
            // SAFETY: `leaked` points to a process-lifetime sqlite3_vfs
            // value whose callback function pointers and name also have
            // process lifetime.
            let rc = unsafe { ffi::sqlite3_vfs_register(leaked, 0) };
            if rc == ffi::SQLITE_OK {
                Ok(())
            } else {
                Err(rc)
            }
        })
        .to_owned()
}

fn registry() -> &'static Mutex<BTreeMap<String, OpenOptionsEntry>> {
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

fn normalize_path(path: &Path) -> String {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in full.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out.to_string_lossy().into_owned()
}

fn options_for_path(path: &Path) -> OpenOptionsEntry {
    let normalized = normalize_path(path);
    let registry = registry().lock().expect("vfs registry poisoned");
    if let Some(options) = registry.get(&normalized) {
        return options.clone();
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        if let Some(base) = normalized.strip_suffix(suffix) {
            if let Some(options) = registry.get(base) {
                return options.clone();
            }
        }
    }
    OpenOptionsEntry {
        compression: SQLiteCompressionOptions::default(),
        key: None,
    }
}

impl VfsFile {
    fn open(
        path: PathBuf,
        options: OpenOptionsEntry,
        flags: c_int,
        read_only: bool,
    ) -> std::io::Result<Self> {
        if options.key.is_none() && should_store_plain(flags, &path) {
            PlainFile::open(path, read_only, flags & ffi::SQLITE_OPEN_CREATE != 0).map(Self::Plain)
        } else {
            ContainerFile::open(path, options).map(Self::Compressed)
        }
    }

    fn path(&self) -> &Path {
        match self {
            Self::Compressed(file) => &file.path,
            Self::Plain(file) => &file.path,
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Compressed(file) => file.flush(),
            Self::Plain(file) => file.flush(),
        }
    }

    fn read_at(&mut self, offset: usize, dest: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Compressed(file) => file.read_at(offset, dest),
            Self::Plain(file) => file.read_at(offset, dest),
        }
    }

    fn write_at(&mut self, offset: usize, source: &[u8]) -> std::io::Result<()> {
        match self {
            Self::Compressed(file) => file.write_at(offset, source),
            Self::Plain(file) => file.write_at(offset, source),
        }
    }

    fn truncate_to(&mut self, size: usize) -> std::io::Result<()> {
        match self {
            Self::Compressed(file) => file.truncate_to(size),
            Self::Plain(file) => file.truncate_to(size),
        }
    }

    fn size(&self) -> std::io::Result<usize> {
        match self {
            Self::Compressed(file) => Ok(file.logical_len),
            Self::Plain(file) => file.size(),
        }
    }
}

impl PlainFile {
    fn open(path: PathBuf, read_only: bool, create: bool) -> std::io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.read(true);
        if !read_only {
            options.write(true);
            if create {
                options.create(true);
            }
        }
        let file = options.open(&path)?;
        Ok(Self { path, file })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()?;
        self.file.sync_all()
    }

    fn read_at(&mut self, offset: usize, dest: &mut [u8]) -> std::io::Result<usize> {
        self.file.seek(SeekFrom::Start(offset as u64))?;
        let mut read = 0;
        while read < dest.len() {
            let n = self.file.read(&mut dest[read..])?;
            if n == 0 {
                break;
            }
            read += n;
        }
        dest[read..].fill(0);
        Ok(read)
    }

    fn write_at(&mut self, offset: usize, source: &[u8]) -> std::io::Result<()> {
        self.file.seek(SeekFrom::Start(offset as u64))?;
        self.file.write_all(source)
    }

    fn truncate_to(&mut self, size: usize) -> std::io::Result<()> {
        self.file.set_len(size as u64)
    }

    fn size(&self) -> std::io::Result<usize> {
        usize::try_from(self.file.metadata()?.len())
            .map_err(|_| invalid_data("plain file too large"))
    }
}

impl ContainerFile {
    fn open(path: PathBuf, options: OpenOptionsEntry) -> std::io::Result<Self> {
        if path.exists() && path.metadata()?.len() > 0 {
            return Self::load(path, options.key);
        }
        let mut salt = [0_u8; SALT_LEN];
        if options.key.is_some() {
            OsRng.fill_bytes(&mut salt);
        }
        Ok(Self {
            path,
            logical_len: 0,
            append_offset: HEADER_SIZE as u64,
            chunks: BTreeMap::new(),
            cache: BTreeMap::new(),
            dirty_chunks: BTreeSet::new(),
            compression: options.compression,
            key: options.key,
            salt,
            generation: 0,
            dirty_header: false,
        })
    }

    fn load(path: PathBuf, key: Option<String>) -> std::io::Result<Self> {
        let mut file = File::open(&path)?;
        let mut header_bytes = [0_u8; HEADER_SIZE];
        file.read_exact(&mut header_bytes)?;
        let header = parse_header(&header_bytes)?;
        let encrypted = header.flags & FLAG_ENCRYPTED != 0;
        if encrypted && key.is_none() {
            return Err(invalid_data(
                "compressed container requires an encryption key",
            ));
        }
        let file_len = file.metadata()?.len();
        let mut committed_generation = header.generation;
        let mut committed_logical_len = header.logical_len;
        let mut committed_chunk_count = header.chunk_count;
        let mut record_offset = HEADER_SIZE as u64;
        let mut entries = Vec::new();
        while record_offset + ENTRY_SIZE as u64 <= file_len {
            file.seek(SeekFrom::Start(record_offset))?;
            let mut entry_bytes = [0_u8; ENTRY_SIZE];
            file.read_exact(&mut entry_bytes)?;
            let entry = parse_entry(&entry_bytes)?;
            if entry.flags & CHUNK_COMMIT != 0 {
                if entry.chunk_id != COMMIT_CHUNK_ID
                    || entry.stored_len != 0
                    || entry.allocated_len != 0
                {
                    return Err(invalid_data("invalid compressed container commit record"));
                }
                if entry.generation > committed_generation {
                    committed_generation = entry.generation;
                    committed_logical_len = usize::try_from(entry.offset)
                        .map_err(|_| invalid_data("commit logical length"))?;
                    committed_chunk_count = entry.raw_len;
                }
                record_offset += ENTRY_SIZE as u64;
                continue;
            }
            let payload_offset = record_offset + ENTRY_SIZE as u64;
            let payload_end = payload_offset
                .checked_add(entry.allocated_len as u64)
                .ok_or_else(|| invalid_data("chunk payload offset overflow"))?;
            if payload_end > file_len {
                break;
            }
            if entry.offset != payload_offset {
                return Err(invalid_data("chunk payload offset mismatch"));
            }
            if entry.allocated_len < entry.stored_len {
                return Err(invalid_data(
                    "chunk allocation is smaller than stored payload",
                ));
            }
            if entry.raw_len > header.compression.chunk_size() {
                return Err(invalid_data(
                    "chunk raw length exceeds configured chunk size",
                ));
            }
            entries.push(entry);
            record_offset = payload_end;
        }
        let expected_chunk_count =
            chunk_count_for(committed_logical_len, header.compression.chunk_size());
        if committed_chunk_count != expected_chunk_count {
            return Err(invalid_data(
                "compressed container commit chunk count mismatch",
            ));
        }
        let mut chunks = BTreeMap::new();
        for entry in entries {
            if entry.generation <= committed_generation
                && entry.chunk_id < committed_chunk_count as u64
            {
                chunks.insert(entry.chunk_id, entry);
            }
        }
        Ok(Self {
            path,
            logical_len: committed_logical_len,
            append_offset: record_offset,
            chunks,
            cache: BTreeMap::new(),
            dirty_chunks: BTreeSet::new(),
            compression: header.compression,
            key,
            salt: header.salt,
            generation: committed_generation,
            dirty_header: false,
        })
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if self.dirty_chunks.is_empty() && !self.dirty_header {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        if self.key.is_some() && self.salt == [0_u8; SALT_LEN] {
            OsRng.fill_bytes(&mut self.salt);
        }
        let next_generation = self.generation.saturating_add(1);
        let encrypted = self.key.is_some();
        let cipher = if encrypted {
            Some(cipher_from_key(
                self.key.as_deref().unwrap_or_default(),
                &self.salt,
            )?)
        } else {
            None
        };
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        self.ensure_header(&mut file, encrypted)?;
        file.set_len(self.append_offset)?;
        file.seek(SeekFrom::Start(self.append_offset))?;
        let active_chunk_count = self.chunk_count() as u64;
        let dirty_chunks: Vec<u64> = self
            .dirty_chunks
            .iter()
            .copied()
            .filter(|chunk_id| *chunk_id < active_chunk_count)
            .collect();
        let mut append_offset = self.append_offset;
        let mut pending_entries = Vec::with_capacity(dirty_chunks.len());
        for chunk_id in dirty_chunks {
            let (entry, stored) =
                self.encode_dirty_chunk(chunk_id, append_offset, next_generation, cipher.as_ref())?;
            file.write_all(&build_entry(&entry))?;
            file.write_all(&stored)?;
            append_offset += ENTRY_SIZE as u64 + entry.allocated_len as u64;
            pending_entries.push(entry);
        }
        let commit = build_commit_entry(next_generation, self.logical_len, self.chunk_count());
        file.write_all(&build_entry(&commit))?;
        append_offset += ENTRY_SIZE as u64;
        file.flush()?;
        file.sync_all()?;
        self.generation = next_generation;
        self.append_offset = append_offset;
        drop(file);
        for entry in pending_entries {
            self.chunks.insert(entry.chunk_id, entry);
        }
        self.chunks
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.dirty_chunks.clear();
        self.dirty_header = false;
        self.compact_if_needed()?;
        Ok(())
    }

    fn ensure_header(&self, file: &mut File, encrypted: bool) -> std::io::Result<()> {
        if file.metadata()?.len() >= HEADER_SIZE as u64 {
            return Ok(());
        }
        file.set_len(HEADER_SIZE as u64)?;
        file.seek(SeekFrom::Start(0))?;
        let header_chunk_count = if self.generation == 0 {
            0
        } else {
            self.chunk_count()
        };
        let header_logical_len = if self.generation == 0 {
            0
        } else {
            self.logical_len
        };
        file.write_all(&build_header(
            if encrypted { FLAG_ENCRYPTED } else { 0 },
            self.compression,
            header_chunk_count,
            header_logical_len,
            self.generation,
            self.salt,
        ))
    }

    fn encode_dirty_chunk(
        &mut self,
        chunk_id: u64,
        append_offset: u64,
        generation: u64,
        cipher: Option<&XChaCha20Poly1305>,
    ) -> std::io::Result<(ChunkEntry, Vec<u8>)> {
        let raw = self.load_chunk(chunk_id)?.clone();
        let compressed = compress_chunk(self.compression, &raw)?;
        let mut flags = 0_u32;
        let mut stored = if compressed.len() < raw.len() {
            flags |= CHUNK_COMPRESSED;
            compressed
        } else {
            raw.clone()
        };
        let mut nonce = [0_u8; NONCE_LEN];
        if let Some(cipher) = cipher {
            flags |= CHUNK_ENCRYPTED;
            OsRng.fill_bytes(&mut nonce);
            stored = cipher
                .encrypt(
                    XNonce::from_slice(&nonce),
                    Payload {
                        msg: &stored,
                        aad: &chunk_id.to_le_bytes(),
                    },
                )
                .map_err(|_| invalid_data("chunk encryption failed"))?;
        }
        let stored_len = stored.len();
        Ok((
            ChunkEntry {
                chunk_id,
                offset: append_offset + ENTRY_SIZE as u64,
                stored_len,
                raw_len: raw.len(),
                flags,
                crc32: crc32fast::hash(&raw),
                nonce,
                generation,
                allocated_len: stored_len,
            },
            stored,
        ))
    }

    fn read_at(&mut self, offset: usize, dest: &mut [u8]) -> std::io::Result<usize> {
        if offset >= self.logical_len {
            dest.fill(0);
            return Ok(0);
        }
        let available = (self.logical_len - offset).min(dest.len());
        let chunk_size = self.compression.chunk_size();
        let mut copied = 0;
        while copied < available {
            let logical_offset = offset + copied;
            let chunk_id = (logical_offset / chunk_size) as u64;
            let chunk_offset = logical_offset % chunk_size;
            let copy_len = (available - copied).min(chunk_size - chunk_offset);
            let chunk = self.load_chunk(chunk_id)?;
            dest[copied..copied + copy_len]
                .copy_from_slice(&chunk[chunk_offset..chunk_offset + copy_len]);
            copied += copy_len;
        }
        dest[available..].fill(0);
        Ok(available)
    }

    fn write_at(&mut self, offset: usize, source: &[u8]) -> std::io::Result<()> {
        if source.is_empty() {
            return Ok(());
        }
        let end = offset
            .checked_add(source.len())
            .ok_or_else(|| invalid_data("write offset overflow"))?;
        if end > self.logical_len {
            self.logical_len = end;
            self.dirty_header = true;
        }
        let chunk_size = self.compression.chunk_size();
        let mut copied = 0;
        while copied < source.len() {
            let logical_offset = offset + copied;
            let chunk_id = (logical_offset / chunk_size) as u64;
            let chunk_offset = logical_offset % chunk_size;
            let copy_len = (source.len() - copied).min(chunk_size - chunk_offset);
            let chunk = self.load_chunk(chunk_id)?;
            chunk[chunk_offset..chunk_offset + copy_len]
                .copy_from_slice(&source[copied..copied + copy_len]);
            self.dirty_chunks.insert(chunk_id);
            copied += copy_len;
        }
        Ok(())
    }

    fn truncate_to(&mut self, size: usize) -> std::io::Result<()> {
        if size == self.logical_len {
            return Ok(());
        }
        let old_len = self.logical_len;
        let old_chunk_count = self.chunk_count();
        self.logical_len = size;
        self.dirty_header = true;
        let new_chunk_count = self.chunk_count();
        let active_chunk_count = new_chunk_count as u64;
        self.chunks
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.cache
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.dirty_chunks
            .retain(|chunk_id| *chunk_id < active_chunk_count);
        if size > 0 {
            let chunk_size = self.compression.chunk_size();
            let changed_chunk = if size < old_len {
                (size % chunk_size != 0).then_some((new_chunk_count - 1) as u64)
            } else if old_len > 0 && old_len % chunk_size != 0 {
                Some((old_chunk_count - 1) as u64)
            } else if old_chunk_count == new_chunk_count {
                Some((new_chunk_count - 1) as u64)
            } else {
                None
            };
            if let Some(chunk_id) = changed_chunk {
                let expected_len = self.expected_chunk_len(chunk_id);
                let chunk = self.load_chunk(chunk_id)?;
                chunk.resize(expected_len, 0);
                self.dirty_chunks.insert(chunk_id);
            }
        }
        Ok(())
    }

    fn load_chunk(&mut self, chunk_id: u64) -> std::io::Result<&mut Vec<u8>> {
        if !self.cache.contains_key(&chunk_id) {
            let raw = self.read_chunk_from_disk(chunk_id)?;
            self.cache.insert(chunk_id, raw);
        }
        let expected_len = self.expected_chunk_len(chunk_id);
        let chunk = self
            .cache
            .get_mut(&chunk_id)
            .expect("chunk cache populated above");
        chunk.resize(expected_len, 0);
        Ok(chunk)
    }

    fn read_chunk_from_disk(&self, chunk_id: u64) -> std::io::Result<Vec<u8>> {
        let expected_len = self.expected_chunk_len(chunk_id);
        if expected_len == 0 {
            return Ok(Vec::new());
        }
        let Some(entry) = self.chunks.get(&chunk_id) else {
            return Ok(vec![0_u8; expected_len]);
        };
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut payload = vec![0_u8; entry.stored_len];
        file.read_exact(&mut payload)?;
        if entry.flags & CHUNK_ENCRYPTED != 0 {
            let cipher = cipher_from_key(
                self.key
                    .as_deref()
                    .ok_or_else(|| invalid_data("encrypted chunk without key"))?,
                &self.salt,
            )?;
            payload = cipher
                .decrypt(
                    XNonce::from_slice(&entry.nonce),
                    Payload {
                        msg: &payload,
                        aad: &entry.chunk_id.to_le_bytes(),
                    },
                )
                .map_err(|_| invalid_data("invalid compressed container encryption key"))?;
        }
        let mut raw = if entry.flags & CHUNK_COMPRESSED != 0 {
            decompress_chunk(self.compression.codec, &payload)?
        } else {
            payload
        };
        if raw.len() != entry.raw_len {
            return Err(invalid_data("chunk raw length mismatch"));
        }
        if crc32fast::hash(&raw) != entry.crc32 {
            return Err(invalid_data("chunk checksum mismatch"));
        }
        if raw.len() > expected_len {
            raw.truncate(expected_len);
        } else {
            raw.resize(expected_len, 0);
        }
        Ok(raw)
    }

    fn chunk_count(&self) -> usize {
        chunk_count_for(self.logical_len, self.compression.chunk_size())
    }

    fn expected_chunk_len(&self, chunk_id: u64) -> usize {
        expected_chunk_len_for(
            self.logical_len,
            self.compression.chunk_size(),
            chunk_id as usize,
        )
    }

    fn active_record_bytes(&self) -> u64 {
        let chunk_count = self.chunk_count() as u64;
        self.chunks
            .iter()
            .filter(|(chunk_id, _)| **chunk_id < chunk_count)
            .map(|(_, entry)| ENTRY_SIZE as u64 + entry.allocated_len as u64)
            .sum()
    }

    fn compact_if_needed(&mut self) -> std::io::Result<()> {
        let compact_len = HEADER_SIZE as u64 + self.active_record_bytes();
        let stale_bytes = self.append_offset.saturating_sub(compact_len);
        let compact_threshold = (self.active_record_bytes().saturating_mul(2))
            .clamp(MIN_COMPACT_STALE_BYTES, MAX_COMPACT_STALE_BYTES);
        if stale_bytes <= compact_threshold {
            return Ok(());
        }
        let tmp_path = self.path.with_extension(format!(
            "{}.compact.tmp",
            self.path
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("uqac")
        ));
        let mut source = File::open(&self.path)?;
        let mut tmp = File::create(&tmp_path)?;
        tmp.write_all(&build_header(
            if self.key.is_some() {
                FLAG_ENCRYPTED
            } else {
                0
            },
            self.compression,
            self.chunk_count(),
            self.logical_len,
            self.generation,
            self.salt,
        ))?;
        let chunk_count = self.chunk_count() as u64;
        let mut append_offset = HEADER_SIZE as u64;
        let mut compacted = BTreeMap::new();
        for (&chunk_id, entry) in self.chunks.iter().filter(|(id, _)| **id < chunk_count) {
            let mut payload = vec![0_u8; entry.stored_len];
            source.seek(SeekFrom::Start(entry.offset))?;
            source.read_exact(&mut payload)?;
            let mut compacted_entry = entry.clone();
            compacted_entry.offset = append_offset + ENTRY_SIZE as u64;
            compacted_entry.allocated_len = compacted_entry.stored_len;
            tmp.write_all(&build_entry(&compacted_entry))?;
            tmp.write_all(&payload)?;
            append_offset += ENTRY_SIZE as u64 + compacted_entry.allocated_len as u64;
            compacted.insert(chunk_id, compacted_entry);
        }
        tmp.set_len(append_offset)?;
        tmp.sync_all()?;
        drop(tmp);
        fs::rename(&tmp_path, &self.path)?;
        self.append_offset = append_offset;
        self.chunks = compacted;
        Ok(())
    }
}

fn chunk_count_for(logical_len: usize, chunk_size: usize) -> usize {
    if logical_len == 0 {
        0
    } else {
        logical_len.div_ceil(chunk_size)
    }
}

fn expected_chunk_len_for(logical_len: usize, chunk_size: usize, chunk_id: usize) -> usize {
    let chunk_count = chunk_count_for(logical_len, chunk_size);
    if chunk_id >= chunk_count {
        return 0;
    }
    if chunk_id + 1 < chunk_count {
        return chunk_size;
    }
    logical_len - chunk_id * chunk_size
}

fn build_commit_entry(generation: u64, logical_len: usize, chunk_count: usize) -> ChunkEntry {
    ChunkEntry {
        chunk_id: COMMIT_CHUNK_ID,
        offset: logical_len as u64,
        stored_len: 0,
        raw_len: chunk_count,
        flags: CHUNK_COMMIT,
        crc32: 0,
        nonce: [0_u8; NONCE_LEN],
        generation,
        allocated_len: 0,
    }
}

fn should_store_plain(flags: c_int, path: &Path) -> bool {
    let auxiliary_flags = ffi::SQLITE_OPEN_MAIN_JOURNAL
        | ffi::SQLITE_OPEN_TEMP_JOURNAL
        | ffi::SQLITE_OPEN_SUBJOURNAL
        | ffi::SQLITE_OPEN_SUPER_JOURNAL
        | ffi::SQLITE_OPEN_WAL;
    if flags & auxiliary_flags != 0 {
        return true;
    }
    let normalized = path.to_string_lossy();
    normalized.ends_with("-journal") || normalized.ends_with("-wal") || normalized.ends_with("-shm")
}

fn cipher_from_key(key: &str, salt: &[u8; SALT_LEN]) -> std::io::Result<XChaCha20Poly1305> {
    let mut derived = [0_u8; 32];
    Argon2::default()
        .hash_password_into(key.as_bytes(), salt, &mut derived)
        .map_err(|_| invalid_data("failed to derive compressed container key"))?;
    XChaCha20Poly1305::new_from_slice(&derived)
        .map_err(|_| invalid_data("failed to initialize compressed container cipher"))
}

fn compress_chunk(compression: SQLiteCompressionOptions, raw: &[u8]) -> std::io::Result<Vec<u8>> {
    match compression.codec {
        SQLiteCompressionCodec::Zstd => zstd::stream::encode_all(raw, compression.level),
        SQLiteCompressionCodec::LZ4 => Ok(lz4_flex::compress_prepend_size(raw)),
    }
}

fn decompress_chunk(codec: SQLiteCompressionCodec, payload: &[u8]) -> std::io::Result<Vec<u8>> {
    match codec {
        SQLiteCompressionCodec::Zstd => zstd::stream::decode_all(payload),
        SQLiteCompressionCodec::LZ4 => lz4_flex::decompress_size_prepended(payload)
            .map_err(|err| invalid_data(err.to_string())),
    }
}

fn parse_header(bytes: &[u8]) -> std::io::Result<Header> {
    if bytes.len() < HEADER_SIZE {
        return Err(invalid_data("compressed container header is truncated"));
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(invalid_data("not a UQA compressed SQLite container"));
    }
    let version = read_u32(bytes, 8)?;
    if version != VERSION {
        return Err(invalid_data("unsupported compressed container version"));
    }
    let flags = read_u32(bytes, 12)?;
    let page_size = read_u32(bytes, 16)?;
    let chunk_pages = read_u32(bytes, 20)?;
    let level = read_i32(bytes, 24)?;
    let header_size = read_u32(bytes, 28)?;
    let entry_size = read_u32(bytes, 32)?;
    if header_size as usize != HEADER_SIZE || entry_size as usize != ENTRY_SIZE {
        return Err(invalid_data("unsupported compressed container layout"));
    }
    let chunk_count =
        usize::try_from(read_u64(bytes, 36)?).map_err(|_| invalid_data("chunk count"))?;
    let logical_len =
        usize::try_from(read_u64(bytes, 44)?).map_err(|_| invalid_data("logical length"))?;
    let generation = read_u64(bytes, 52)?;
    let mut salt = [0_u8; SALT_LEN];
    salt.copy_from_slice(&bytes[60..60 + SALT_LEN]);
    let codec = SQLiteCompressionCodec::from_id(read_u32(bytes, 76)?).map_err(invalid_data)?;
    let compression = SQLiteCompressionOptions {
        codec,
        page_size,
        chunk_pages,
        level,
    }
    .validate()
    .map_err(invalid_data)?;
    let expected_chunk_count = chunk_count_for(logical_len, compression.chunk_size());
    if chunk_count != expected_chunk_count {
        return Err(invalid_data("compressed container chunk count mismatch"));
    }
    Ok(Header {
        flags,
        compression,
        chunk_count,
        logical_len,
        generation,
        salt,
    })
}

fn build_header(
    flags: u32,
    compression: SQLiteCompressionOptions,
    chunk_count: usize,
    logical_len: usize,
    generation: u64,
    salt: [u8; SALT_LEN],
) -> [u8; HEADER_SIZE] {
    let mut out = [0_u8; HEADER_SIZE];
    out[..MAGIC.len()].copy_from_slice(MAGIC);
    write_u32(&mut out, 8, VERSION);
    write_u32(&mut out, 12, flags);
    write_u32(&mut out, 16, compression.page_size);
    write_u32(&mut out, 20, compression.chunk_pages);
    write_i32(&mut out, 24, compression.level);
    write_u32(&mut out, 28, HEADER_SIZE as u32);
    write_u32(&mut out, 32, ENTRY_SIZE as u32);
    write_u64(&mut out, 36, chunk_count as u64);
    write_u64(&mut out, 44, logical_len as u64);
    write_u64(&mut out, 52, generation);
    out[60..60 + SALT_LEN].copy_from_slice(&salt);
    write_u32(&mut out, 76, compression.codec.id());
    out
}

fn parse_entry(bytes: &[u8]) -> std::io::Result<ChunkEntry> {
    let chunk_id = read_u64(bytes, 0)?;
    let offset = read_u64(bytes, 8)?;
    let stored_len =
        usize::try_from(read_u64(bytes, 16)?).map_err(|_| invalid_data("chunk stored length"))?;
    let raw_len =
        usize::try_from(read_u64(bytes, 24)?).map_err(|_| invalid_data("chunk raw length"))?;
    let flags = read_u32(bytes, 32)?;
    let crc32 = read_u32(bytes, 36)?;
    let mut nonce = [0_u8; NONCE_LEN];
    nonce.copy_from_slice(&bytes[40..40 + NONCE_LEN]);
    let generation = read_u64(bytes, 64)?;
    let allocated_len = usize::try_from(read_u64(bytes, 72)?)
        .map_err(|_| invalid_data("chunk allocated length"))?;
    Ok(ChunkEntry {
        chunk_id,
        offset,
        stored_len,
        raw_len,
        flags,
        crc32,
        nonce,
        generation,
        allocated_len,
    })
}

fn build_entry(entry: &ChunkEntry) -> [u8; ENTRY_SIZE] {
    let mut out = [0_u8; ENTRY_SIZE];
    write_u64(&mut out, 0, entry.chunk_id);
    write_u64(&mut out, 8, entry.offset);
    write_u64(&mut out, 16, entry.stored_len as u64);
    write_u64(&mut out, 24, entry.raw_len as u64);
    write_u32(&mut out, 32, entry.flags);
    write_u32(&mut out, 36, entry.crc32);
    out[40..40 + NONCE_LEN].copy_from_slice(&entry.nonce);
    write_u64(&mut out, 64, entry.generation);
    write_u64(&mut out, 72, entry.allocated_len as u64);
    out
}

fn read_u32(bytes: &[u8], offset: usize) -> std::io::Result<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_data("u32 field truncated"))?;
    Ok(u32::from_le_bytes(
        slice.try_into().expect("slice length checked"),
    ))
}

fn read_i32(bytes: &[u8], offset: usize) -> std::io::Result<i32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_data("i32 field truncated"))?;
    Ok(i32::from_le_bytes(
        slice.try_into().expect("slice length checked"),
    ))
}

fn read_u64(bytes: &[u8], offset: usize) -> std::io::Result<u64> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid_data("u64 field truncated"))?;
    Ok(u64::from_le_bytes(
        slice.try_into().expect("slice length checked"),
    ))
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_i32(bytes: &mut [u8], offset: usize, value: i32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

fn invalid_data(message: impl Into<String>) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message.into())
}

unsafe fn file_from_sqlite<'a>(file: *mut ffi::sqlite3_file) -> Option<&'a mut FileHandle> {
    let compressed = file.cast::<CompressedSQLiteFile>();
    let handle = unsafe { (*compressed).handle };
    if handle.is_null() {
        None
    } else {
        Some(unsafe { &mut *handle })
    }
}

static IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(file_close),
    xRead: Some(file_read),
    xWrite: Some(file_write),
    xTruncate: Some(file_truncate),
    xSync: Some(file_sync),
    xFileSize: Some(file_size),
    xLock: Some(file_lock),
    xUnlock: Some(file_unlock),
    xCheckReservedLock: Some(file_check_reserved_lock),
    xFileControl: Some(file_control),
    xSectorSize: Some(file_sector_size),
    xDeviceCharacteristics: Some(file_device_characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn file_close(file: *mut ffi::sqlite3_file) -> c_int {
    let compressed = file.cast::<CompressedSQLiteFile>();
    let handle = unsafe { (*compressed).handle };
    if handle.is_null() {
        return ffi::SQLITE_OK;
    }
    unsafe {
        (*compressed).handle = ptr::null_mut();
    }
    let mut handle = unsafe { Box::from_raw(handle) };
    let flush = handle.file.flush();
    let _ = FileExt::unlock(&handle.lock_file);
    if handle.delete_on_close {
        let _ = fs::remove_file(handle.file.path());
    }
    unsafe {
        (*compressed).base.pMethods = ptr::null();
    }
    if flush.is_ok() {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_CLOSE
    }
}

unsafe extern "C" fn file_read(
    file: *mut ffi::sqlite3_file,
    out: *mut c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_READ;
    };
    if amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_READ;
    }
    let amount = amount as usize;
    let offset = offset as usize;
    let dest = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), amount) };
    match handle.file.read_at(offset, dest) {
        Ok(copy_len) if copy_len == amount => ffi::SQLITE_OK,
        Ok(_) => ffi::SQLITE_IOERR_SHORT_READ,
        Err(_) => ffi::SQLITE_IOERR_READ,
    }
}

unsafe extern "C" fn file_write(
    file: *mut ffi::sqlite3_file,
    input: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
    if handle.read_only {
        return ffi::SQLITE_READONLY;
    }
    if amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_WRITE;
    }
    let amount = amount as usize;
    let offset = offset as usize;
    let source = unsafe { std::slice::from_raw_parts(input.cast::<u8>(), amount) };
    match handle.file.write_at(offset, source) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_WRITE,
    }
}

unsafe extern "C" fn file_truncate(
    file: *mut ffi::sqlite3_file,
    size: ffi::sqlite3_int64,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_TRUNCATE;
    };
    if handle.read_only || size < 0 {
        return ffi::SQLITE_IOERR_TRUNCATE;
    }
    match handle.file.truncate_to(size as usize) {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_TRUNCATE,
    }
}

unsafe extern "C" fn file_sync(file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_FSYNC;
    };
    match handle.file.flush() {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_FSYNC,
    }
}

unsafe extern "C" fn file_size(
    file: *mut ffi::sqlite3_file,
    size_out: *mut ffi::sqlite3_int64,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    let Ok(size) = handle.file.size() else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    unsafe {
        *size_out = size as ffi::sqlite3_int64;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn file_lock(file: *mut ffi::sqlite3_file, lock: c_int) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_LOCK;
    };
    if lock <= handle.lock_state {
        return ffi::SQLITE_OK;
    }
    let lock_ok = if lock >= SQLITE_LOCK_RESERVED {
        if handle.lock_state != SQLITE_LOCK_NONE {
            let _ = FileExt::unlock(&handle.lock_file);
        }
        FileExt::try_lock_exclusive(&handle.lock_file).is_ok()
    } else {
        FileExt::try_lock_shared(&handle.lock_file).is_ok()
    };
    if !lock_ok {
        return ffi::SQLITE_BUSY;
    }
    handle.lock_state = handle.lock_state.max(lock);
    ffi::SQLITE_OK
}

unsafe extern "C" fn file_unlock(file: *mut ffi::sqlite3_file, lock: c_int) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_UNLOCK;
    };
    if lock <= SQLITE_LOCK_NONE {
        if FileExt::unlock(&handle.lock_file).is_err() {
            return ffi::SQLITE_IOERR_UNLOCK;
        }
        handle.lock_state = SQLITE_LOCK_NONE;
        return ffi::SQLITE_OK;
    }
    if lock == SQLITE_LOCK_SHARED
        && handle.lock_state > SQLITE_LOCK_SHARED
        && (FileExt::unlock(&handle.lock_file).is_err()
            || FileExt::try_lock_shared(&handle.lock_file).is_err())
    {
        return ffi::SQLITE_IOERR_UNLOCK;
    }
    handle.lock_state = lock;
    ffi::SQLITE_OK
}

unsafe extern "C" fn file_check_reserved_lock(
    file: *mut ffi::sqlite3_file,
    out: *mut c_int,
) -> c_int {
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_CHECKRESERVEDLOCK;
    };
    unsafe {
        *out = i32::from(handle.lock_state >= SQLITE_LOCK_RESERVED);
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn file_control(
    _file: *mut ffi::sqlite3_file,
    _op: c_int,
    _arg: *mut c_void,
) -> c_int {
    ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn file_sector_size(_file: *mut ffi::sqlite3_file) -> c_int {
    DEFAULT_PAGE_SIZE as c_int
}

unsafe extern "C" fn file_device_characteristics(_file: *mut ffi::sqlite3_file) -> c_int {
    0
}

unsafe extern "C" fn vfs_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: ffi::sqlite3_filename,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let path = if name.is_null() {
        temp_path()
    } else {
        match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(path) => PathBuf::from(path),
            Err(_) => return ffi::SQLITE_CANTOPEN,
        }
    };
    let normalized = PathBuf::from(normalize_path(&path));
    let options = options_for_path(&normalized);
    let read_only = flags & ffi::SQLITE_OPEN_READONLY != 0;
    let delete_on_close = flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0 || name.is_null();
    if flags & ffi::SQLITE_OPEN_CREATE == 0 && !normalized.exists() {
        return ffi::SQLITE_CANTOPEN;
    }
    let lock_path = lock_path(&normalized);
    let open_result = VfsFile::open(normalized, options, flags, read_only)
        .and_then(|container| open_lock_file(&lock_path).map(|lock_file| (container, lock_file)));
    let compressed = file.cast::<CompressedSQLiteFile>();
    unsafe {
        ptr::write(
            compressed,
            CompressedSQLiteFile {
                base: ffi::sqlite3_file {
                    pMethods: ptr::null(),
                },
                handle: ptr::null_mut(),
            },
        );
    }
    match open_result {
        Ok((container, lock_file)) => {
            let handle = Box::new(FileHandle {
                file: container,
                lock_file,
                read_only,
                delete_on_close,
                lock_state: SQLITE_LOCK_NONE,
            });
            unsafe {
                (*compressed).base.pMethods = &raw const IO_METHODS;
                (*compressed).handle = Box::into_raw(handle);
                if !out_flags.is_null() {
                    *out_flags = flags;
                }
            }
            ffi::SQLITE_OK
        }
        Err(_) => ffi::SQLITE_CANTOPEN,
    }
}

unsafe extern "C" fn vfs_delete(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _sync_dir: c_int,
) -> c_int {
    if name.is_null() {
        return ffi::SQLITE_OK;
    }
    let Ok(path) = (unsafe { CStr::from_ptr(name) }).to_str() else {
        return ffi::SQLITE_IOERR_DELETE;
    };
    let normalized = PathBuf::from(normalize_path(Path::new(path)));
    let result = fs::remove_file(&normalized);
    let _ = fs::remove_file(lock_path(&normalized));
    match result {
        Ok(()) => ffi::SQLITE_OK,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_DELETE,
    }
}

unsafe extern "C" fn vfs_access(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _flags: c_int,
    out: *mut c_int,
) -> c_int {
    if name.is_null() || out.is_null() {
        return ffi::SQLITE_IOERR_ACCESS;
    }
    let Ok(path) = (unsafe { CStr::from_ptr(name) }).to_str() else {
        return ffi::SQLITE_IOERR_ACCESS;
    };
    unsafe {
        *out = i32::from(Path::new(&normalize_path(Path::new(path))).exists());
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn vfs_full_pathname(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    output_len: c_int,
    output: *mut c_char,
) -> c_int {
    if name.is_null() || output.is_null() || output_len <= 0 {
        return ffi::SQLITE_CANTOPEN;
    }
    let Ok(path) = (unsafe { CStr::from_ptr(name) }).to_str() else {
        return ffi::SQLITE_CANTOPEN;
    };
    let normalized = normalize_path(Path::new(path));
    let bytes = normalized.as_bytes();
    if bytes.len() + 1 > output_len as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe {
        ptr::copy_nonoverlapping(bytes.as_ptr(), output.cast::<u8>(), bytes.len());
        *output.add(bytes.len()) = 0;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn vfs_randomness(
    _vfs: *mut ffi::sqlite3_vfs,
    amount: c_int,
    out: *mut c_char,
) -> c_int {
    if amount <= 0 || out.is_null() {
        return 0;
    }
    let len = amount as usize;
    let dest = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), len) };
    OsRng.fill_bytes(dest);
    amount
}

unsafe extern "C" fn vfs_sleep(_vfs: *mut ffi::sqlite3_vfs, microseconds: c_int) -> c_int {
    if microseconds > 0 {
        std::thread::sleep(Duration::from_micros(microseconds as u64));
    }
    microseconds
}

unsafe extern "C" fn vfs_current_time(_vfs: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    if out.is_null() {
        return ffi::SQLITE_ERROR;
    }
    let unix_seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    unsafe {
        *out = unix_seconds / 86_400.0 + 2_440_587.5;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn vfs_get_last_error(
    _vfs: *mut ffi::sqlite3_vfs,
    _amount: c_int,
    _out: *mut c_char,
) -> c_int {
    0
}

fn temp_path() -> PathBuf {
    static NEXT_ID: OnceLock<Mutex<u64>> = OnceLock::new();
    let mut id = NEXT_ID
        .get_or_init(|| Mutex::new(0))
        .lock()
        .expect("temp id mutex poisoned");
    *id = id.saturating_add(1);
    std::env::temp_dir().join(format!(
        "uqa-compressed-sqlite-{}-{id}.tmp",
        std::process::id()
    ))
}

fn lock_path(path: &Path) -> PathBuf {
    let mut raw = path.as_os_str().to_os_string();
    raw.push(".lock");
    PathBuf::from(raw)
}

fn open_lock_file(path: &Path) -> std::io::Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
}

#[cfg(test)]
mod tests {
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
        let chunk_size = compression.chunk_size();

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
            .write_at(0, &vec![b'a'; compression.chunk_size()])
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
}
