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
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argon2::{Argon2, Block};
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305, XNonce};
#[cfg(not(target_os = "emscripten"))]
use fs2::FileExt;

/// Browser (emscripten) builds run a single process against a virtual
/// filesystem, so the inter-process byte locks `fs2` provides are
/// vacuously held; this shim keeps the SQLite VFS locking protocol
/// call sites identical across targets.
#[cfg(target_os = "emscripten")]
struct FileExt;

#[cfg(target_os = "emscripten")]
impl FileExt {
    fn try_lock_exclusive(_file: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }

    fn try_lock_shared(_file: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }

    fn unlock(_file: &std::fs::File) -> std::io::Result<()> {
        Ok(())
    }
}
use rusqlite::ffi;

pub const VFS_NAME: &str = "uqa_compressed";

const VFS_NAME_C: &[u8] = b"uqa_compressed\0";
pub(crate) const MAGIC: &[u8; 8] = b"UQACDB1\0";
const VERSION: u32 = 1;
const HEADER_SIZE: usize = 128;
const ENTRY_SIZE: usize = 80;
pub(crate) const FLAG_ENCRYPTED: u32 = 1;
/// Byte offset of the little-endian `flags` word inside the container
/// header. Kept next to `build_header` / `parse_header` so every piece
/// of on-disk layout knowledge stays in this file.
pub(crate) const HEADER_FLAGS_OFFSET: usize = 12;
const CHUNK_COMPRESSED: u32 = 1;
const CHUNK_ENCRYPTED: u32 = 2;
const CHUNK_COMMIT: u32 = 4;
const COMMIT_CHUNK_ID: u64 = u64::MAX;
const MIN_COMPACT_STALE_BYTES: u64 = 4 * 1024;
const MAX_COMPACT_STALE_BYTES: u64 = 8 * 1024 * 1024;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const AEAD_TAG_LEN: usize = 16;
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

    pub fn chunk_size(self) -> Result<usize, String> {
        let validated = self.validate()?;
        let page_size = usize::try_from(validated.page_size)
            .map_err(|_| "page_size exceeds the addressable range".to_string())?;
        let chunk_pages = usize::try_from(validated.chunk_pages)
            .map_err(|_| "chunk_pages exceeds the addressable range".to_string())?;
        page_size
            .checked_mul(chunk_pages)
            .ok_or_else(|| "chunk size exceeds the addressable range".to_string())
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
    let normalized = normalize_path(path).map_err(|error| error.to_string())?;
    registry.insert(normalized, entry);
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

fn normalize_path(path: &Path) -> std::io::Result<String> {
    let full = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
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
    Ok(out.to_string_lossy().into_owned())
}

fn options_for_path(path: &Path) -> std::io::Result<OpenOptionsEntry> {
    let normalized = normalize_path(path)?;
    let registry = registry()
        .lock()
        .map_err(|_| invalid_data("vfs registry poisoned"))?;
    if let Some(options) = registry.get(&normalized) {
        return Ok(options.clone());
    }
    for suffix in ["-journal", "-wal", "-shm"] {
        if let Some(base) = normalized.strip_suffix(suffix) {
            if let Some(options) = registry.get(base) {
                return Ok(options.clone());
            }
        }
    }
    Ok(OpenOptionsEntry {
        compression: SQLiteCompressionOptions::default(),
        key: None,
    })
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
        let offset = usize_to_u64(offset, "plain-file read offset")?;
        self.file.seek(SeekFrom::Start(offset))?;
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
        let offset = usize_to_u64(offset, "plain-file write offset")?;
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.write_all(source)
    }

    fn truncate_to(&mut self, size: usize) -> std::io::Result<()> {
        self.file
            .set_len(usize_to_u64(size, "plain-file truncate size")?)
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
            fill_random(&mut salt)?;
        }
        Ok(Self {
            path,
            logical_len: 0,
            append_offset: usize_to_u64(HEADER_SIZE, "container header size")?,
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
        let chunk_size = header.compression.chunk_size().map_err(invalid_data)?;
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
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut record_offset = usize_to_u64(HEADER_SIZE, "container header size")?;
        let mut entries = Vec::new();
        loop {
            let entry_end = record_offset
                .checked_add(entry_size)
                .ok_or_else(|| invalid_data("container entry offset overflow"))?;
            if entry_end > file_len {
                break;
            }
            file.seek(SeekFrom::Start(record_offset))?;
            let mut entry_bytes = [0_u8; ENTRY_SIZE];
            file.read_exact(&mut entry_bytes)?;
            let entry = parse_entry(&entry_bytes)?;
            if entry.flags & CHUNK_COMMIT != 0 {
                if entry.chunk_id != COMMIT_CHUNK_ID
                    || entry.flags != CHUNK_COMMIT
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
                record_offset = entry_end;
                continue;
            }
            validate_chunk_entry(&entry, chunk_size, encrypted)?;
            let payload_offset = entry_end;
            let allocated_len = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
            let payload_end = payload_offset
                .checked_add(allocated_len)
                .ok_or_else(|| invalid_data("chunk payload offset overflow"))?;
            if payload_end > file_len {
                break;
            }
            if entry.offset != payload_offset {
                return Err(invalid_data("chunk payload offset mismatch"));
            }
            entries.push(entry);
            record_offset = payload_end;
        }
        let expected_chunk_count = chunk_count_for(committed_logical_len, chunk_size);
        if committed_chunk_count != expected_chunk_count {
            return Err(invalid_data(
                "compressed container commit chunk count mismatch",
            ));
        }
        let committed_chunk_count = usize_to_u64(committed_chunk_count, "committed chunk count")?;
        let mut chunks = BTreeMap::new();
        for entry in entries {
            if entry.generation <= committed_generation && entry.chunk_id < committed_chunk_count {
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
            fill_random(&mut self.salt)?;
        }
        let next_generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| invalid_data("container generation overflow"))?;
        let cipher = self
            .key
            .as_deref()
            .map(|key| cipher_from_key(key, &self.salt))
            .transpose()?;
        let encrypted = cipher.is_some();
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        self.ensure_header(&mut file, encrypted)?;
        file.set_len(self.append_offset)?;
        file.seek(SeekFrom::Start(self.append_offset))?;
        let active_chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let dirty_chunks: Vec<u64> = self
            .dirty_chunks
            .iter()
            .copied()
            .filter(|chunk_id| *chunk_id < active_chunk_count)
            .collect();
        let mut append_offset = self.append_offset;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut pending_entries = Vec::with_capacity(dirty_chunks.len());
        for chunk_id in dirty_chunks {
            let (entry, stored) =
                self.encode_dirty_chunk(chunk_id, append_offset, next_generation, cipher.as_ref())?;
            file.write_all(&build_entry(&entry)?)?;
            file.write_all(&stored)?;
            let allocated_len = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
            append_offset = append_offset
                .checked_add(entry_size)
                .and_then(|offset| offset.checked_add(allocated_len))
                .ok_or_else(|| invalid_data("container append offset overflow"))?;
            pending_entries.push(entry);
        }
        let commit = build_commit_entry(next_generation, self.logical_len, self.chunk_count()?)?;
        file.write_all(&build_entry(&commit)?)?;
        append_offset = append_offset
            .checked_add(entry_size)
            .ok_or_else(|| invalid_data("container commit offset overflow"))?;
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
        let header_size = usize_to_u64(HEADER_SIZE, "container header size")?;
        if file.metadata()?.len() >= header_size {
            return Ok(());
        }
        file.set_len(header_size)?;
        file.seek(SeekFrom::Start(0))?;
        let header_chunk_count = if self.generation == 0 {
            0
        } else {
            self.chunk_count()?
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
        )?)
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
            fill_random(&mut nonce)?;
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
                offset: append_offset
                    .checked_add(usize_to_u64(ENTRY_SIZE, "container entry size")?)
                    .ok_or_else(|| invalid_data("chunk payload offset overflow"))?,
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
        let chunk_size = self.compression.chunk_size().map_err(invalid_data)?;
        let mut copied = 0;
        while copied < available {
            let logical_offset = offset + copied;
            let chunk_id = usize_to_u64(logical_offset / chunk_size, "read chunk id")?;
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
        let chunk_size = self.compression.chunk_size().map_err(invalid_data)?;
        let mut copied = 0;
        while copied < source.len() {
            let logical_offset = offset + copied;
            let chunk_id = usize_to_u64(logical_offset / chunk_size, "write chunk id")?;
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
        let old_chunk_count = self.chunk_count()?;
        self.logical_len = size;
        self.dirty_header = true;
        let new_chunk_count = self.chunk_count()?;
        let active_chunk_count = usize_to_u64(new_chunk_count, "active chunk count")?;
        self.chunks
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.cache
            .retain(|chunk_id, _| *chunk_id < active_chunk_count);
        self.dirty_chunks
            .retain(|chunk_id| *chunk_id < active_chunk_count);
        if size > 0 {
            let chunk_size = self.compression.chunk_size().map_err(invalid_data)?;
            let changed_chunk = if size < old_len && size % chunk_size != 0 {
                Some(usize_to_u64(new_chunk_count - 1, "changed chunk id")?)
            } else if old_len > 0 && old_len % chunk_size != 0 {
                Some(usize_to_u64(old_chunk_count - 1, "changed chunk id")?)
            } else if old_chunk_count == new_chunk_count {
                Some(usize_to_u64(new_chunk_count - 1, "changed chunk id")?)
            } else {
                None
            };
            if let Some(chunk_id) = changed_chunk {
                let expected_len = self.expected_chunk_len(chunk_id)?;
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
        let expected_len = self.expected_chunk_len(chunk_id)?;
        let chunk = self.cache.get_mut(&chunk_id).ok_or_else(|| {
            invalid_data(format!(
                "chunk {chunk_id} disappeared from the cache after loading"
            ))
        })?;
        chunk.resize(expected_len, 0);
        Ok(chunk)
    }

    fn read_chunk_from_disk(&self, chunk_id: u64) -> std::io::Result<Vec<u8>> {
        let expected_len = self.expected_chunk_len(chunk_id)?;
        if expected_len == 0 {
            return Ok(Vec::new());
        }
        let Some(entry) = self.chunks.get(&chunk_id) else {
            return Ok(vec![0_u8; expected_len]);
        };
        let mut file = File::open(&self.path)?;
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut payload = allocate_payload(entry.stored_len, "chunk stored payload")?;
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
            decompress_chunk(self.compression.codec, &payload, entry.raw_len)?
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

    fn chunk_count(&self) -> std::io::Result<usize> {
        Ok(chunk_count_for(
            self.logical_len,
            self.compression.chunk_size().map_err(invalid_data)?,
        ))
    }

    fn expected_chunk_len(&self, chunk_id: u64) -> std::io::Result<usize> {
        Ok(expected_chunk_len_for(
            self.logical_len,
            self.compression.chunk_size().map_err(invalid_data)?,
            usize::try_from(chunk_id).map_err(|_| invalid_data("chunk id exceeds usize"))?,
        ))
    }

    fn active_record_bytes(&self) -> std::io::Result<u64> {
        let chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut total = 0_u64;
        for entry in self
            .chunks
            .iter()
            .filter(|(chunk_id, _)| **chunk_id < chunk_count)
            .map(|(_, entry)| entry)
        {
            let allocated_len = usize_to_u64(entry.allocated_len, "chunk allocated length")?;
            total = total
                .checked_add(entry_size)
                .and_then(|value| value.checked_add(allocated_len))
                .ok_or_else(|| invalid_data("active container byte count overflow"))?;
        }
        Ok(total)
    }

    fn compact_if_needed(&mut self) -> std::io::Result<()> {
        let active_record_bytes = self.active_record_bytes()?;
        let compact_len = usize_to_u64(HEADER_SIZE, "container header size")?
            .checked_add(active_record_bytes)
            .ok_or_else(|| invalid_data("compacted container length overflow"))?;
        let stale_bytes = self.append_offset.saturating_sub(compact_len);
        let compact_threshold = (active_record_bytes.saturating_mul(2))
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
            self.chunk_count()?,
            self.logical_len,
            self.generation,
            self.salt,
        )?)?;
        let chunk_count = usize_to_u64(self.chunk_count()?, "active chunk count")?;
        let entry_size = usize_to_u64(ENTRY_SIZE, "container entry size")?;
        let mut append_offset = usize_to_u64(HEADER_SIZE, "container header size")?;
        let mut compacted = BTreeMap::new();
        for (&chunk_id, entry) in self.chunks.iter().filter(|(id, _)| **id < chunk_count) {
            let mut payload = allocate_payload(entry.stored_len, "compacted chunk payload")?;
            source.seek(SeekFrom::Start(entry.offset))?;
            source.read_exact(&mut payload)?;
            let mut compacted_entry = entry.clone();
            compacted_entry.offset = append_offset
                .checked_add(entry_size)
                .ok_or_else(|| invalid_data("compacted payload offset overflow"))?;
            compacted_entry.allocated_len = compacted_entry.stored_len;
            tmp.write_all(&build_entry(&compacted_entry)?)?;
            tmp.write_all(&payload)?;
            append_offset = compacted_entry
                .offset
                .checked_add(usize_to_u64(
                    compacted_entry.allocated_len,
                    "compacted chunk allocated length",
                )?)
                .ok_or_else(|| invalid_data("compacted append offset overflow"))?;
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

fn build_commit_entry(
    generation: u64,
    logical_len: usize,
    chunk_count: usize,
) -> std::io::Result<ChunkEntry> {
    Ok(ChunkEntry {
        chunk_id: COMMIT_CHUNK_ID,
        offset: usize_to_u64(logical_len, "commit logical length")?,
        stored_len: 0,
        raw_len: chunk_count,
        flags: CHUNK_COMMIT,
        crc32: 0,
        nonce: [0_u8; NONCE_LEN],
        generation,
        allocated_len: 0,
    })
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
    let argon2 = Argon2::default();
    let mut memory_blocks = vec![Block::default(); argon2.params().block_count()];
    argon2
        .hash_password_into_with_memory(key.as_bytes(), salt, &mut derived, &mut memory_blocks)
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

fn decompress_chunk(
    codec: SQLiteCompressionCodec,
    payload: &[u8],
    expected_len: usize,
) -> std::io::Result<Vec<u8>> {
    match codec {
        SQLiteCompressionCodec::Zstd => {
            let decoder = zstd::stream::read::Decoder::new(payload)?;
            let read_limit = u64::try_from(expected_len)
                .ok()
                .and_then(|length| length.checked_add(1))
                .ok_or_else(|| invalid_data("zstd decoded length limit overflow"))?;
            let mut output = Vec::new();
            output.try_reserve_exact(expected_len).map_err(|error| {
                invalid_data(format!(
                    "unable to allocate zstd output of {expected_len} bytes: {error}"
                ))
            })?;
            decoder.take(read_limit).read_to_end(&mut output)?;
            if output.len() != expected_len {
                return Err(invalid_data("zstd decoded length mismatch"));
            }
            Ok(output)
        }
        SQLiteCompressionCodec::LZ4 => {
            let encoded_len = payload
                .get(..4)
                .ok_or_else(|| invalid_data("lz4 payload is missing its decoded length"))?;
            let mut encoded = [0_u8; 4];
            encoded.copy_from_slice(encoded_len);
            let declared_len = usize::try_from(u32::from_le_bytes(encoded))
                .map_err(|_| invalid_data("lz4 decoded length is outside address space"))?;
            if declared_len != expected_len {
                return Err(invalid_data("lz4 decoded length mismatch"));
            }
            let mut output = allocate_payload(expected_len, "lz4 decoded chunk")?;
            let decoded = lz4_flex::decompress_into(&payload[4..], &mut output)
                .map_err(|error| invalid_data(error.to_string()))?;
            if decoded != expected_len {
                return Err(invalid_data("lz4 decoded byte count mismatch"));
            }
            Ok(output)
        }
    }
}

fn allocate_payload(length: usize, context: &str) -> std::io::Result<Vec<u8>> {
    let mut payload = Vec::new();
    payload.try_reserve_exact(length).map_err(|error| {
        invalid_data(format!(
            "unable to allocate {context} of {length} bytes: {error}"
        ))
    })?;
    payload.resize(length, 0);
    Ok(payload)
}

fn validate_chunk_entry(
    entry: &ChunkEntry,
    chunk_size: usize,
    container_encrypted: bool,
) -> std::io::Result<()> {
    if entry.flags & !(CHUNK_COMPRESSED | CHUNK_ENCRYPTED) != 0 {
        return Err(invalid_data("chunk entry contains unsupported flags"));
    }
    let entry_encrypted = entry.flags & CHUNK_ENCRYPTED != 0;
    if entry_encrypted != container_encrypted {
        return Err(invalid_data(
            "chunk encryption flag does not match the container header",
        ));
    }
    if entry.raw_len == 0 || entry.raw_len > chunk_size {
        return Err(invalid_data(
            "chunk raw length is outside the configured chunk size",
        ));
    }
    let overhead = if entry_encrypted { AEAD_TAG_LEN } else { 0 };
    let uncompressed_stored_len = entry
        .raw_len
        .checked_add(overhead)
        .ok_or_else(|| invalid_data("chunk stored length limit overflow"))?;
    if entry.flags & CHUNK_COMPRESSED != 0 {
        if entry.stored_len >= uncompressed_stored_len {
            return Err(invalid_data(
                "compressed chunk payload is not smaller than its raw payload",
            ));
        }
        if entry_encrypted && entry.stored_len < AEAD_TAG_LEN {
            return Err(invalid_data(
                "encrypted chunk payload is shorter than its tag",
            ));
        }
    } else if entry.stored_len != uncompressed_stored_len {
        return Err(invalid_data(
            "uncompressed chunk stored length does not match its raw payload",
        ));
    }
    if entry.allocated_len != entry.stored_len {
        return Err(invalid_data(
            "chunk allocation length does not match its stored payload",
        ));
    }
    Ok(())
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
    if flags & !FLAG_ENCRYPTED != 0 {
        return Err(invalid_data("unsupported compressed container flags"));
    }
    let page_size = read_u32(bytes, 16)?;
    let chunk_pages = read_u32(bytes, 20)?;
    let level = read_i32(bytes, 24)?;
    let header_size = read_u32(bytes, 28)?;
    let entry_size = read_u32(bytes, 32)?;
    if usize::try_from(header_size).ok() != Some(HEADER_SIZE)
        || usize::try_from(entry_size).ok() != Some(ENTRY_SIZE)
    {
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
    let expected_chunk_count =
        chunk_count_for(logical_len, compression.chunk_size().map_err(invalid_data)?);
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
) -> std::io::Result<[u8; HEADER_SIZE]> {
    let mut out = [0_u8; HEADER_SIZE];
    out[..MAGIC.len()].copy_from_slice(MAGIC);
    write_u32(&mut out, 8, VERSION);
    write_u32(&mut out, 12, flags);
    write_u32(&mut out, 16, compression.page_size);
    write_u32(&mut out, 20, compression.chunk_pages);
    write_i32(&mut out, 24, compression.level);
    write_u32(
        &mut out,
        28,
        u32::try_from(HEADER_SIZE).map_err(|_| invalid_data("container header size"))?,
    );
    write_u32(
        &mut out,
        32,
        u32::try_from(ENTRY_SIZE).map_err(|_| invalid_data("container entry size"))?,
    );
    write_u64(
        &mut out,
        36,
        usize_to_u64(chunk_count, "header chunk count")?,
    );
    write_u64(
        &mut out,
        44,
        usize_to_u64(logical_len, "header logical length")?,
    );
    write_u64(&mut out, 52, generation);
    out[60..60 + SALT_LEN].copy_from_slice(&salt);
    write_u32(&mut out, 76, compression.codec.id());
    Ok(out)
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

fn build_entry(entry: &ChunkEntry) -> std::io::Result<[u8; ENTRY_SIZE]> {
    let mut out = [0_u8; ENTRY_SIZE];
    write_u64(&mut out, 0, entry.chunk_id);
    write_u64(&mut out, 8, entry.offset);
    write_u64(
        &mut out,
        16,
        usize_to_u64(entry.stored_len, "entry stored length")?,
    );
    write_u64(
        &mut out,
        24,
        usize_to_u64(entry.raw_len, "entry raw length")?,
    );
    write_u32(&mut out, 32, entry.flags);
    write_u32(&mut out, 36, entry.crc32);
    out[40..40 + NONCE_LEN].copy_from_slice(&entry.nonce);
    write_u64(&mut out, 64, entry.generation);
    write_u64(
        &mut out,
        72,
        usize_to_u64(entry.allocated_len, "entry allocated length")?,
    );
    Ok(out)
}

fn read_u32(bytes: &[u8], offset: usize) -> std::io::Result<u32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_data("u32 field truncated"))?;
    let encoded: [u8; 4] = slice
        .try_into()
        .map_err(|_| invalid_data("u32 field has an invalid width"))?;
    Ok(u32::from_le_bytes(encoded))
}

fn read_i32(bytes: &[u8], offset: usize) -> std::io::Result<i32> {
    let slice = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| invalid_data("i32 field truncated"))?;
    let encoded: [u8; 4] = slice
        .try_into()
        .map_err(|_| invalid_data("i32 field has an invalid width"))?;
    Ok(i32::from_le_bytes(encoded))
}

fn read_u64(bytes: &[u8], offset: usize) -> std::io::Result<u64> {
    let slice = bytes
        .get(offset..offset + 8)
        .ok_or_else(|| invalid_data("u64 field truncated"))?;
    let encoded: [u8; 8] = slice
        .try_into()
        .map_err(|_| invalid_data("u64 field has an invalid width"))?;
    Ok(u64::from_le_bytes(encoded))
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

fn usize_to_u64(value: usize, context: &str) -> std::io::Result<u64> {
    u64::try_from(value).map_err(|_| invalid_data(format!("{context} exceeds u64")))
}

fn fill_random(dest: &mut [u8]) -> std::io::Result<()> {
    getrandom::fill(dest)
        .map_err(|err| std::io::Error::other(format!("failed to obtain OS randomness: {err}")))
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
    let unlock = FileExt::unlock(&handle.lock_file);
    let delete = if handle.delete_on_close {
        match fs::remove_file(handle.file.path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    } else {
        Ok(())
    };
    unsafe {
        (*compressed).base.pMethods = ptr::null();
    }
    if flush.is_ok() && unlock.is_ok() && delete.is_ok() {
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
    if out.is_null() || amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_READ;
    }
    let (Ok(amount), Ok(offset)) = (usize::try_from(amount), usize::try_from(offset)) else {
        return ffi::SQLITE_IOERR_READ;
    };
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
    if input.is_null() || amount < 0 || offset < 0 {
        return ffi::SQLITE_IOERR_WRITE;
    }
    let (Ok(amount), Ok(offset)) = (usize::try_from(amount), usize::try_from(offset)) else {
        return ffi::SQLITE_IOERR_WRITE;
    };
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
    let Ok(size) = usize::try_from(size) else {
        return ffi::SQLITE_IOERR_TRUNCATE;
    };
    match handle.file.truncate_to(size) {
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
    if size_out.is_null() {
        return ffi::SQLITE_IOERR_FSTAT;
    }
    let Some(handle) = (unsafe { file_from_sqlite(file) }) else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    let Ok(size) = handle.file.size() else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    let Ok(size) = ffi::sqlite3_int64::try_from(size) else {
        return ffi::SQLITE_IOERR_FSTAT;
    };
    unsafe {
        *size_out = size;
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
        if handle.lock_state != SQLITE_LOCK_NONE && FileExt::unlock(&handle.lock_file).is_err() {
            return ffi::SQLITE_IOERR_UNLOCK;
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
        match temp_path() {
            Ok(path) => path,
            Err(_) => return ffi::SQLITE_CANTOPEN,
        }
    } else {
        match unsafe { CStr::from_ptr(name) }.to_str() {
            Ok(path) => PathBuf::from(path),
            Err(_) => return ffi::SQLITE_CANTOPEN,
        }
    };
    let normalized = match normalize_path(&path) {
        Ok(path) => PathBuf::from(path),
        Err(_) => return ffi::SQLITE_CANTOPEN,
    };
    let Ok(options) = options_for_path(&normalized) else {
        return ffi::SQLITE_CANTOPEN;
    };
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
    let normalized = match normalize_path(Path::new(path)) {
        Ok(path) => PathBuf::from(path),
        Err(_) => return ffi::SQLITE_IOERR_DELETE,
    };
    let remove = |path: &Path| match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    };
    let file_result = remove(&normalized);
    let lock_result = remove(&lock_path(&normalized));
    if file_result.is_ok() && lock_result.is_ok() {
        ffi::SQLITE_OK
    } else {
        ffi::SQLITE_IOERR_DELETE
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
    let Ok(normalized) = normalize_path(Path::new(path)) else {
        return ffi::SQLITE_IOERR_ACCESS;
    };
    unsafe {
        *out = i32::from(Path::new(&normalized).exists());
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
    let Ok(normalized) = normalize_path(Path::new(path)) else {
        return ffi::SQLITE_CANTOPEN;
    };
    let bytes = normalized.as_bytes();
    let Ok(output_len) = usize::try_from(output_len) else {
        return ffi::SQLITE_CANTOPEN;
    };
    if bytes
        .len()
        .checked_add(1)
        .is_none_or(|len| len > output_len)
    {
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
    let Ok(len) = usize::try_from(amount) else {
        return 0;
    };
    let dest = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), len) };
    if fill_random(dest).is_ok() {
        amount
    } else {
        0
    }
}

unsafe extern "C" fn vfs_sleep(_vfs: *mut ffi::sqlite3_vfs, microseconds: c_int) -> c_int {
    let Ok(microseconds_u64) = u64::try_from(microseconds) else {
        return 0;
    };
    std::thread::sleep(Duration::from_micros(microseconds_u64));
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

fn temp_path() -> std::io::Result<PathBuf> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(0);
    let id = NEXT_ID
        .fetch_update(AtomicOrdering::Relaxed, AtomicOrdering::Relaxed, |id| {
            id.checked_add(1)
        })
        .map_err(|_| invalid_data("compressed VFS temporary-file id overflow"))?
        + 1;
    Ok(std::env::temp_dir().join(format!(
        "uqa-compressed-sqlite-{}-{id}.tmp",
        std::process::id()
    )))
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
        let lz4_error =
            decompress_chunk(SQLiteCompressionCodec::LZ4, &oversized_lz4, 512).unwrap_err();
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
}
