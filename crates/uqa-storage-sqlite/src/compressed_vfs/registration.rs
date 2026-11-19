//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Per-database options registry and process-wide `SQLite` VFS registration.

use std::sync::Arc;

use super::{
    c_char, c_int, ffi, invalid_data, ptr, vfs_access, vfs_current_time, vfs_delete,
    vfs_full_pathname, vfs_get_last_error, vfs_open, vfs_open_pool, vfs_randomness, vfs_sleep,
    BTreeMap, Component, CompressedSQLiteFile, ContainerFile, Mutex, OpenOptionsEntry, Path,
    PathBuf, SQLiteCompressedContainerAnchor, SQLiteCompressionOptions, POOL_VFS_NAME_C, REGISTRY,
    VFS_NAME_C, VFS_REGISTERED,
};

pub fn register_database(
    path: &Path,
    compression: SQLiteCompressionOptions,
    key: Option<&str>,
) -> Result<(), String> {
    register_database_options(path, compression, key, None)
}

pub fn register_database_with_anchor(
    path: &Path,
    compression: SQLiteCompressionOptions,
    key: &str,
    trusted_anchor: SQLiteCompressedContainerAnchor,
) -> Result<(), String> {
    register_database_options(path, compression, Some(key), Some(trusted_anchor))
}

fn register_database_options(
    path: &Path,
    compression: SQLiteCompressionOptions,
    key: Option<&str>,
    trusted_anchor: Option<SQLiteCompressedContainerAnchor>,
) -> Result<(), String> {
    let compression = compression.validate()?;
    ensure_registered().map_err(|code| format!("sqlite3_vfs_register failed with code {code}"))?;
    validate_existing_container(path, key, trusted_anchor)?;
    let mut entry = OpenOptionsEntry {
        compression,
        key: key.map(str::to_string),
        trusted_anchor,
        live_anchor: Arc::default(),
    };
    let mut registry = registry().lock().map_err(|_| "vfs registry poisoned")?;
    let normalized = normalize_path(path).map_err(|error| error.to_string())?;
    if let Some(existing) = registry.get(&normalized) {
        if existing.key != entry.key {
            return Err("compressed database is already registered with a different key".into());
        }
        entry.trusted_anchor = merge_anchors(existing.trusted_anchor, entry.trusted_anchor)?;
        entry.live_anchor = existing.live_anchor.clone();
    }
    if trusted_anchor.is_some() && trusted_anchor != entry.trusted_anchor {
        return Err("trusted anchor is older than the registered generation".into());
    }
    {
        let mut live = entry
            .live_anchor
            .lock()
            .map_err(|_| "compressed container anchor poisoned")?;
        let admitted = merge_anchors(*live, entry.trusted_anchor)?;
        if trusted_anchor.is_some() && trusted_anchor != admitted {
            return Err("trusted anchor is older than the admitted live generation".into());
        }
        *live = admitted;
    }
    registry.insert(normalized, entry);
    Ok(())
}

fn validate_existing_container(
    path: &Path,
    key: Option<&str>,
    trusted_anchor: Option<SQLiteCompressedContainerAnchor>,
) -> Result<(), String> {
    let exists = match path.metadata() {
        Ok(metadata) => metadata.len() > 0,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => return Err(error.to_string()),
    };
    if !exists {
        return if trusted_anchor.is_some() {
            Err("trusted anchor cannot be applied to a missing or empty container".into())
        } else {
            Ok(())
        };
    }
    let container =
        ContainerFile::load(path.to_path_buf(), key).map_err(|error| error.to_string())?;
    if let Some(trusted) = trusted_anchor {
        if container.keys.is_none() {
            return Err("trusted anchors require an encrypted compressed container".into());
        }
        container
            .require_trusted_anchor(trusted)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn merge_anchors(
    existing: Option<SQLiteCompressedContainerAnchor>,
    requested: Option<SQLiteCompressedContainerAnchor>,
) -> Result<Option<SQLiteCompressedContainerAnchor>, String> {
    match (existing, requested) {
        (None, anchor) | (anchor, None) => Ok(anchor),
        (Some(existing), Some(requested)) => {
            if existing.database_id != requested.database_id {
                return Err("trusted anchor database identities disagree".into());
            }
            if existing.generation == requested.generation
                && existing.state_tag != requested.state_tag
            {
                return Err("trusted anchors disagree for the same generation".into());
            }
            Ok(Some(if existing.generation >= requested.generation {
                existing
            } else {
                requested
            }))
        }
    }
}

type OpenCallback = unsafe extern "C" fn(
    *mut ffi::sqlite3_vfs,
    ffi::sqlite3_filename,
    *mut ffi::sqlite3_file,
    c_int,
    *mut c_int,
) -> c_int;

fn ensure_registered() -> std::result::Result<(), c_int> {
    VFS_REGISTERED
        .get_or_init(|| {
            for (name, open) in [
                (VFS_NAME_C, vfs_open as OpenCallback),
                (POOL_VFS_NAME_C, vfs_open_pool as OpenCallback),
            ] {
                let vfs = Box::new(ffi::sqlite3_vfs {
                    iVersion: 1,
                    szOsFile: std::mem::size_of::<CompressedSQLiteFile>() as c_int,
                    mxPathname: 4096,
                    pNext: ptr::null_mut(),
                    zName: name.as_ptr().cast::<c_char>(),
                    pAppData: ptr::null_mut(),
                    xOpen: Some(open),
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
                if rc != ffi::SQLITE_OK {
                    return Err(rc);
                }
            }
            Ok(())
        })
        .to_owned()
}

fn registry() -> &'static Mutex<BTreeMap<String, OpenOptionsEntry>> {
    REGISTRY.get_or_init(|| Mutex::new(BTreeMap::new()))
}

pub(super) fn normalize_path(path: &Path) -> std::io::Result<String> {
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

pub(super) fn options_for_path(path: &Path) -> std::io::Result<OpenOptionsEntry> {
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
        trusted_anchor: None,
        live_anchor: Arc::default(),
    })
}
